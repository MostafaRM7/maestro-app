use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use maestrod::{DaemonPaths, IpcError, MultiplexedDaemonClient};
use tokio::time::{Instant, sleep};

const DAEMON_EXECUTABLE: &str = "maestrod";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);
const STARTUP_RETRY_INTERVAL: Duration = Duration::from_millis(25);

/// Discovers and starts the trusted Maestro daemon executable when IPC is not
/// already available. Authentication and protocol negotiation still happen on
/// every returned connection.
#[derive(Debug, Clone)]
pub(crate) struct DaemonLauncher {
    candidates: Arc<[PathBuf]>,
    arguments: Arc<[OsString]>,
    environment: Arc<[(OsString, OsString)]>,
    startup_timeout: Duration,
    retry_interval: Duration,
}

impl DaemonLauncher {
    pub(crate) fn discover(resource_directory: Option<&Path>) -> Self {
        let mut candidates = Vec::new();
        if let Ok(current_executable) = std::env::current_exe()
            && let Some(binary_directory) = current_executable.parent()
        {
            push_unique(&mut candidates, binary_directory.join(DAEMON_EXECUTABLE));
            if binary_directory
                .file_name()
                .is_some_and(|name| name == "deps")
                && let Some(profile_directory) = binary_directory.parent()
            {
                push_unique(&mut candidates, profile_directory.join(DAEMON_EXECUTABLE));
            }
        }
        if let Some(resource_directory) = resource_directory {
            push_unique(&mut candidates, resource_directory.join(DAEMON_EXECUTABLE));
        }
        #[cfg(debug_assertions)]
        push_unique(
            &mut candidates,
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .join("target/debug")
                .join(DAEMON_EXECUTABLE),
        );

        Self {
            candidates: candidates.into(),
            arguments: Arc::from([]),
            environment: Arc::from([]),
            startup_timeout: STARTUP_TIMEOUT,
            retry_interval: STARTUP_RETRY_INTERVAL,
        }
    }

    pub(crate) async fn connect(
        &self,
        paths: &DaemonPaths,
    ) -> Result<MultiplexedDaemonClient, IpcError> {
        let initial = connect_client(paths).await;
        match initial {
            Ok(client) => return Ok(client),
            Err(error) if should_launch_after(&error) => {}
            Err(error) => return Err(error),
        }

        self.spawn()?;
        let deadline = Instant::now() + self.startup_timeout;
        loop {
            match connect_client(paths).await {
                Ok(client) => return Ok(client),
                Err(error) if !should_launch_after(&error) => return Err(error),
                Err(_) if Instant::now() >= deadline => {
                    return Err(IpcError::Io(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "maestrod did not become ready before the startup deadline",
                    )));
                }
                Err(_) => sleep(self.retry_interval).await,
            }
        }
    }

    fn spawn(&self) -> Result<(), IpcError> {
        let executable = self.resolve_executable().map_err(IpcError::Io)?;
        let mut command = tokio::process::Command::new(executable);
        command
            .args(self.arguments.iter())
            .envs(self.environment.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(false);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(IpcError::Io)?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }

    fn resolve_executable(&self) -> Result<PathBuf, io::Error> {
        for candidate in self.candidates.iter() {
            match validate_executable(candidate) {
                Ok(executable) => return Ok(executable),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "a packaged or development maestrod executable was not found",
        ))
    }

    #[cfg(test)]
    fn for_test(
        executable: PathBuf,
        arguments: Vec<OsString>,
        environment: Vec<(OsString, OsString)>,
    ) -> Self {
        Self {
            candidates: Arc::from([executable]),
            arguments: arguments.into(),
            environment: environment.into(),
            startup_timeout: Duration::from_secs(5),
            retry_interval: Duration::from_millis(10),
        }
    }
}

async fn connect_client(paths: &DaemonPaths) -> Result<MultiplexedDaemonClient, IpcError> {
    MultiplexedDaemonClient::connect(paths, "maestro-desktop", env!("CARGO_PKG_VERSION")).await
}

fn should_launch_after(error: &IpcError) -> bool {
    matches!(error, IpcError::Io(_) | IpcError::Disconnected)
}

fn validate_executable(path: &Path) -> Result<PathBuf, io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing an unsafe maestrod executable path",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the maestrod executable does not have an execute bit",
            ));
        }
    }
    fs::canonicalize(path)
}

fn push_unique(candidates: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs, path::PathBuf, sync::Arc, time::Duration};

    use maestro_domain::{ErrorCode, MaestroError};
    use maestro_protocol::{Request, Response};
    use maestrod::{DaemonConfig, DaemonError, DaemonPaths, DaemonServer, IpcError};

    use super::{DaemonLauncher, should_launch_after};

    const HELPER_BASE: &str = "MAESTRO_TEST_DAEMON_BASE";

    #[test]
    #[ignore = "launched explicitly by the daemon lifecycle race test"]
    fn daemon_subprocess_helper() {
        let Some(base) = std::env::var_os(HELPER_BASE) else {
            return;
        };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("helper runtime builds");
        runtime.block_on(async move {
            let paths = DaemonPaths::isolated(PathBuf::from(base));
            let server = match DaemonServer::bind(
                paths,
                DaemonConfig {
                    idle_shutdown_grace: Duration::from_millis(100),
                    ..DaemonConfig::default()
                },
            )
            .await
            {
                Ok(server) => server,
                Err(DaemonError::AlreadyRunning) => return,
                Err(error) => panic!("helper daemon failed to bind: {error}"),
            };
            server.run().await.expect("helper daemon runs");
        });
    }

    #[tokio::test]
    async fn concurrent_launchers_converge_on_one_authenticated_daemon() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let launcher = test_launcher(temporary.path().as_os_str().to_owned());
        let competitor = launcher.clone();

        let (first, second) = tokio::join!(launcher.connect(&paths), competitor.connect(&paths));
        let first = first.expect("first desktop connects");
        let second = second.expect("second desktop connects");
        assert_eq!(
            first.request(Request::Ping).await.expect("first ping"),
            Response::Pong
        );
        assert_eq!(
            second.request(Request::Ping).await.expect("second ping"),
            Response::Pong
        );

        drop(first);
        drop(second);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    #[test]
    fn authentication_or_protocol_rejection_never_triggers_a_spawn() {
        assert!(!should_launch_after(&IpcError::Fatal(MaestroError::new(
            ErrorCode::AuthenticationRequired,
            "authentication rejected",
        ))));
        assert!(!should_launch_after(&IpcError::Codec(
            "invalid protocol frame".to_owned(),
        )));
        assert!(should_launch_after(&IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "daemon is not listening",
        ))));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_first_candidate_fails_closed_instead_of_falling_back() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temporary = tempfile::tempdir().expect("temporary directory");
        let executable = temporary.path().join("executable");
        let alias = temporary.path().join("maestrod");
        fs::write(&executable, "#!/bin/sh\nexit 0\n").expect("fixture writes");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("fixture becomes executable");
        symlink(&executable, &alias).expect("symlink creates");
        let launcher = DaemonLauncher {
            candidates: vec![alias, executable].into(),
            arguments: Arc::from([]),
            environment: Arc::from([]),
            startup_timeout: Duration::from_secs(1),
            retry_interval: Duration::from_millis(10),
        };

        assert_eq!(
            launcher
                .resolve_executable()
                .expect_err("unsafe candidate is rejected")
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    fn test_launcher(base: OsString) -> DaemonLauncher {
        DaemonLauncher::for_test(
            std::env::current_exe().expect("test executable resolves"),
            vec![
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from("daemon_launcher::tests::daemon_subprocess_helper"),
                OsString::from("--test-threads=1"),
            ],
            vec![(OsString::from(HELPER_BASE), base)],
        )
    }
}
