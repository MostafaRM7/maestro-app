use std::{
    ffi::OsString,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    time::Duration,
};

use maestro_domain::RunId;
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::OwnedSemaphorePermit,
    time::timeout,
};

use crate::{
    ControlledEnvironment, ProcessError,
    lifecycle::{LifecycleOutcome, ProcessLifecycle},
    process_group::{OwnedProcessGroup, wait_for_leader_exit},
};

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub working_directory: PathBuf,
    pub environment: ControlledEnvironment,
}

impl ProcessSpec {
    pub fn new(executable: impl Into<PathBuf>, working_directory: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            working_directory: working_directory.into(),
            environment: ControlledEnvironment::empty(),
        }
    }

    #[must_use]
    pub fn argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    #[must_use]
    pub fn arguments<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn environment(mut self, environment: ControlledEnvironment) -> Self {
        self.environment = environment;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCause {
    Exited(i32),
    Signaled(i32),
    Unknown,
}

impl ExitCause {
    pub fn from_status(status: ExitStatus) -> Self {
        if let Some(code) = status.code() {
            return Self::Exited(code);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if let Some(signal) = status.signal() {
                return Self::Signaled(signal);
            }
        }
        Self::Unknown
    }
}

#[derive(Debug)]
pub struct StructuredProcess {
    run_id: RunId,
    pid: u32,
    group: OwnedProcessGroup,
    lifecycle: ProcessLifecycle,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

impl StructuredProcess {
    pub(crate) fn spawn(
        run_id: RunId,
        spec: &ProcessSpec,
        permit: OwnedSemaphorePermit,
    ) -> Result<Self, ProcessError> {
        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.arguments)
            .current_dir(&spec.working_directory)
            .env_clear()
            .envs(spec.environment.values())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn()?;
        let pid = child.id().ok_or(ProcessError::MissingProcessId)?;
        let group = OwnedProcessGroup::claim(pid)?;
        let stdin = child.stdin.take().map(BufWriter::new);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (outcome, lifecycle) = ProcessLifecycle::channel();
        drop(tokio::spawn(supervise_child(
            StructuredChildGuard::new(child, group.clone()),
            permit,
            outcome,
        )));

        Ok(Self {
            run_id,
            pid,
            group,
            lifecycle,
            stdin,
            stdout,
            stderr,
        })
    }

    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Takes ownership of the captured stdout stream.
    ///
    /// # Errors
    ///
    /// Returns an error when stdout was already taken.
    pub fn take_stdout(&mut self) -> Result<ChildStdout, ProcessError> {
        self.stdout
            .take()
            .ok_or(ProcessError::MissingStream("stdout"))
    }

    /// Takes ownership of the captured stderr stream.
    ///
    /// # Errors
    ///
    /// Returns an error when stderr was already taken.
    pub fn take_stderr(&mut self) -> Result<ChildStderr, ProcessError> {
        self.stderr
            .take()
            .ok_or(ProcessError::MissingStream("stderr"))
    }

    /// Writes bytes to the child stdin without shell interpretation.
    ///
    /// # Errors
    ///
    /// Returns an error when stdin is closed or the write fails.
    pub async fn write(&mut self, bytes: &[u8]) -> Result<(), ProcessError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(ProcessError::MissingStream("stdin"))?;
        stdin.write_all(bytes).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Closes stdin, delivering EOF to the child.
    ///
    /// # Errors
    ///
    /// Returns an error when shutting down the pipe fails.
    pub async fn close_stdin(&mut self) -> Result<(), ProcessError> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin.shutdown().await?;
        }
        Ok(())
    }

    /// Waits for process exit and attributes an exit code or signal.
    ///
    /// # Errors
    ///
    /// Returns an error when the OS wait operation fails.
    pub async fn wait(&mut self) -> Result<ExitCause, ProcessError> {
        self.lifecycle.wait().await
    }

    /// Signals the owned process group, escalating after `grace`.
    ///
    /// # Errors
    ///
    /// Returns an error when signaling or waiting fails.
    pub async fn terminate(&mut self, grace: Duration) -> Result<ExitCause, ProcessError> {
        if let Some(result) = self.lifecycle.completed_result() {
            return result;
        }
        let _ = self.group.terminate()?;
        if let Ok(result) = timeout(grace, self.lifecycle.wait()).await {
            result
        } else {
            self.group.seal()?;
            self.lifecycle.wait().await
        }
    }
}

impl Drop for StructuredProcess {
    fn drop(&mut self) {
        if self.lifecycle.is_running() {
            self.group.try_seal();
        }
    }
}

#[derive(Debug)]
struct StructuredChildGuard {
    child: Child,
    group: OwnedProcessGroup,
}

impl StructuredChildGuard {
    fn new(child: Child, group: OwnedProcessGroup) -> Self {
        Self { child, group }
    }
}

impl Drop for StructuredChildGuard {
    fn drop(&mut self) {
        // This runs before `child` is dropped, so the process-group leader
        // still prevents PGID reuse while the final signal is issued.
        self.group.try_seal();
    }
}

async fn supervise_child(
    mut guarded: StructuredChildGuard,
    permit: OwnedSemaphorePermit,
    outcome: tokio::sync::watch::Sender<LifecycleOutcome>,
) {
    let observation = wait_for_leader_exit(guarded.group.pid()).await;
    let cleanup = guarded.group.seal();
    if cleanup.is_err() {
        let _ = guarded.child.start_kill();
    }
    let reaped = guarded.child.wait().await;
    let final_outcome = if let Err(error) = observation {
        LifecycleOutcome::Failed(error.to_string())
    } else if let Err(error) = cleanup {
        LifecycleOutcome::Failed(error.to_string())
    } else {
        match reaped {
            Ok(status) => LifecycleOutcome::Exited(ExitCause::from_status(status)),
            Err(error) => {
                LifecycleOutcome::Failed(format!("could not reap process leader: {error}"))
            }
        }
    };
    drop(permit);
    outcome.send_replace(final_outcome);
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use maestro_domain::RunId;
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    use crate::{EnvironmentPolicy, ExitCause, ProcessSpawner, ProcessSpec};

    fn process_exists(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
    }

    async fn wait_for_process_state(pid: u32, expected: bool) {
        for _ in 0..200 {
            if process_exists(pid) == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            process_exists(pid),
            expected,
            "unexpected state for pid {pid}"
        );
    }

    async fn wait_for_file(path: &Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {}", path.display());
    }

    #[tokio::test]
    async fn arguments_are_not_interpreted_by_a_shell() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let spec = ProcessSpec::new("/bin/echo", temp.path())
            .argument("$(printf injected)")
            .environment(EnvironmentPolicy::default().evaluate_current());
        let run_id = RunId::new();
        let mut process = ProcessSpawner::new(1)
            .spawn_structured(run_id, spec)
            .await
            .expect("echo starts");
        assert_eq!(process.run_id(), run_id);
        let mut stdout = process.take_stdout().expect("stdout");
        let mut output = String::new();
        stdout
            .read_to_string(&mut output)
            .await
            .expect("read output");

        assert_eq!(process.wait().await.expect("exit"), ExitCause::Exited(0));
        assert_eq!(output, "$(printf injected)\n");
    }

    #[tokio::test]
    async fn graceful_termination_stops_owned_process_group() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let spec = ProcessSpec::new("/bin/sh", temp.path())
            .arguments(["-c", "sleep 30 & wait"])
            .environment(EnvironmentPolicy::default().evaluate_current());
        let mut process = ProcessSpawner::new(1)
            .spawn_structured(RunId::new(), spec)
            .await
            .expect("shell starts");

        let cause = process
            .terminate(Duration::from_secs(2))
            .await
            .expect("process group terminates");
        assert!(matches!(
            cause,
            ExitCause::Signaled(_) | ExitCause::Exited(_)
        ));
    }

    #[tokio::test]
    async fn termination_handles_an_exited_leader_and_does_not_touch_an_unrelated_group() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let unrelated_ready = temp.path().join("unrelated-ready");
        let unrelated_spec = ProcessSpec::new("/bin/sh", temp.path())
            .argument("-c")
            .argument("trap '' TERM; : > \"$1\"; while :; do sleep 1; done")
            .argument("maestro-test")
            .argument(&unrelated_ready)
            .environment(EnvironmentPolicy::default().evaluate_current());
        let spawner = ProcessSpawner::new(2);
        let unrelated = spawner
            .spawn_structured(RunId::new(), unrelated_spec)
            .await
            .expect("unrelated process starts");
        wait_for_file(&unrelated_ready).await;

        let descendant_ready = temp.path().join("descendant-ready");
        let owned_spec = ProcessSpec::new("/bin/sh", temp.path())
            .argument("-c")
            .argument(
                "(trap '' TERM; : > \"$1\"; while :; do sleep 1; done) & \
                 child=$!; while [ ! -f \"$1\" ]; do sleep 0.01; done; printf '%s\\n' \"$child\"; exit 0",
            )
            .argument("maestro-test")
            .argument(&descendant_ready)
            .environment(EnvironmentPolicy::default().evaluate_current());
        let mut owned = spawner
            .spawn_structured(RunId::new(), owned_spec)
            .await
            .expect("owned process starts");
        let mut child_pid = String::new();
        BufReader::new(owned.take_stdout().expect("stdout"))
            .read_line(&mut child_pid)
            .await
            .expect("descendant pid reads");
        let child_pid = child_pid.trim().parse::<u32>().expect("valid child pid");
        assert!(process_exists(child_pid));

        assert_eq!(
            owned.wait().await.expect("leader exit is observed"),
            ExitCause::Exited(0)
        );
        wait_for_process_state(child_pid, false).await;
        assert!(process_exists(unrelated.pid()));
        assert_eq!(
            owned
                .terminate(Duration::from_millis(50))
                .await
                .expect("completed group stays sealed"),
            ExitCause::Exited(0)
        );
        assert!(process_exists(unrelated.pid()));

        let unrelated_pid = unrelated.pid();
        drop(unrelated);
        wait_for_process_state(unrelated_pid, false).await;
    }

    #[tokio::test]
    async fn cancelled_termination_then_drop_cannot_orphan_descendants() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let ready = temp.path().join("drop-ready");
        let spec = ProcessSpec::new("/bin/sh", temp.path())
            .argument("-c")
            .argument(
                "trap '' TERM; (trap '' TERM; : > \"$1\"; while :; do sleep 1; done) & \
                 child=$!; while [ ! -f \"$1\" ]; do sleep 0.01; done; printf '%s\\n' \"$child\"; wait",
            )
            .argument("maestro-test")
            .argument(&ready)
            .environment(EnvironmentPolicy::default().evaluate_current());
        let spawner = ProcessSpawner::new(1);
        let mut process = spawner
            .spawn_structured(RunId::new(), spec)
            .await
            .expect("process starts");
        let leader_pid = process.pid();
        let mut child_pid = String::new();
        BufReader::new(process.take_stdout().expect("stdout"))
            .read_line(&mut child_pid)
            .await
            .expect("descendant pid reads");
        let child_pid = child_pid.trim().parse::<u32>().expect("valid child pid");

        assert!(
            tokio::time::timeout(
                Duration::from_millis(25),
                process.terminate(Duration::from_secs(30)),
            )
            .await
            .is_err()
        );
        drop(process);
        wait_for_process_state(leader_pid, false).await;
        wait_for_process_state(child_pid, false).await;
        for _ in 0..200 {
            if spawner.active_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(spawner.active_count(), 0);
    }

    #[tokio::test]
    async fn ten_concurrent_processes_keep_output_separate() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let spawner = ProcessSpawner::new(10);
        let mut processes = Vec::new();
        for index in 0..10 {
            let spec = ProcessSpec::new("/bin/echo", temp.path())
                .argument(format!("session-{index}"))
                .environment(EnvironmentPolicy::default().evaluate_current());
            processes.push(
                spawner
                    .spawn_structured(RunId::new(), spec)
                    .await
                    .expect("process starts"),
            );
        }
        assert_eq!(spawner.active_count(), 10);

        for (index, process) in processes.iter_mut().enumerate() {
            let mut stdout = process.take_stdout().expect("stdout");
            let mut output = String::new();
            stdout.read_to_string(&mut output).await.expect("output");
            assert_eq!(output, format!("session-{index}\n"));
            assert_eq!(process.wait().await.expect("exit"), ExitCause::Exited(0));
        }
    }
}
