use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::Duration,
};

use maestro_domain::RunId;
use portable_pty::{
    Child, CommandBuilder, MasterPty, PtySize as PortablePtySize, native_pty_system,
};
use tokio::{sync::OwnedSemaphorePermit, task::spawn_blocking};

use crate::{
    ExitCause, ProcessError, ProcessSpec,
    lifecycle::{LifecycleOutcome, ProcessLifecycle},
    process_group::{OwnedProcessGroup, wait_for_leader_exit_blocking},
};

type PtyReader = Box<dyn Read + Send>;
type PtyWriter = Box<dyn Write + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    pub rows: u16,
    pub columns: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        Self {
            rows: 24,
            columns: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl From<PtySize> for PortablePtySize {
    fn from(value: PtySize) -> Self {
        Self {
            rows: value.rows,
            cols: value.columns,
            pixel_width: value.pixel_width,
            pixel_height: value.pixel_height,
        }
    }
}

/// A real PTY with independently cloneable read/write paths. Blocking native
/// calls are moved onto Tokio's blocking pool.
pub struct PtyProcess {
    run_id: RunId,
    pid: u32,
    group: OwnedProcessGroup,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    reader: Arc<Mutex<PtyReader>>,
    writer: Arc<Mutex<PtyWriter>>,
    lifecycle: ProcessLifecycle,
}

impl std::fmt::Debug for PtyProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PtyProcess")
            .field("run_id", &self.run_id)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl PtyProcess {
    pub(crate) fn spawn(
        run_id: RunId,
        spec: &ProcessSpec,
        size: PtySize,
        permit: OwnedSemaphorePermit,
    ) -> Result<Self, ProcessError> {
        Self::spawn_with_acquirers(
            run_id,
            spec,
            size,
            permit,
            |master, _| {
                master
                    .try_clone_reader()
                    .map_err(|error| ProcessError::Pty(error.to_string()))
            },
            |master, _| {
                master
                    .take_writer()
                    .map_err(|error| ProcessError::Pty(error.to_string()))
            },
        )
    }

    fn spawn_with_acquirers<AcquireReader, AcquireWriter>(
        run_id: RunId,
        spec: &ProcessSpec,
        size: PtySize,
        permit: OwnedSemaphorePermit,
        acquire_reader: AcquireReader,
        acquire_writer: AcquireWriter,
    ) -> Result<Self, ProcessError>
    where
        AcquireReader: FnOnce(&dyn MasterPty, u32) -> Result<PtyReader, ProcessError>,
        AcquireWriter: FnOnce(&dyn MasterPty, u32) -> Result<PtyWriter, ProcessError>,
    {
        let pair = native_pty_system()
            .openpty(size.into())
            .map_err(|error| ProcessError::Pty(error.to_string()))?;
        let mut command = CommandBuilder::new(&spec.executable);
        command.args(&spec.arguments);
        command.cwd(&spec.working_directory);
        command.env_clear();
        for (name, value) in spec.environment.values() {
            command.env(name, value);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| ProcessError::Pty(error.to_string()))?;
        let mut guarded = PtyChildGuard::new(child);
        let Some(pid) = guarded.process_id() else {
            guarded.cleanup_blocking();
            return Err(ProcessError::MissingProcessId);
        };
        let group = match OwnedProcessGroup::claim(pid) {
            Ok(group) => group,
            Err(error) => {
                guarded.cleanup_blocking();
                return Err(error);
            }
        };
        guarded.set_group(group.clone());
        drop(pair.slave);
        let reader = match acquire_reader(pair.master.as_ref(), pid) {
            Ok(reader) => reader,
            Err(error) => {
                guarded.cleanup_blocking();
                return Err(error);
            }
        };
        let writer = match acquire_writer(pair.master.as_ref(), pid) {
            Ok(writer) => writer,
            Err(error) => {
                guarded.cleanup_blocking();
                return Err(error);
            }
        };
        let (outcome, lifecycle) = ProcessLifecycle::channel();
        drop(spawn_blocking(move || {
            supervise_child(&mut guarded, permit, &outcome);
        }));

        Ok(Self {
            run_id,
            pid,
            group,
            master: Arc::new(Mutex::new(pair.master)),
            reader: Arc::new(Mutex::new(reader)),
            writer: Arc::new(Mutex::new(writer)),
            lifecycle,
        })
    }

    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Reads at most `maximum_bytes` from the PTY master.
    ///
    /// # Errors
    ///
    /// Returns an error when the blocking task or native read fails.
    pub async fn read(&self, maximum_bytes: usize) -> Result<Vec<u8>, ProcessError> {
        if maximum_bytes == 0 {
            return Ok(Vec::new());
        }
        let reader = Arc::clone(&self.reader);
        spawn_blocking(move || {
            let mut bytes = vec![0_u8; maximum_bytes];
            let count = reader
                .lock()
                .map_err(|_| ProcessError::Pty("PTY reader lock was poisoned".to_owned()))?
                .read(&mut bytes)?;
            bytes.truncate(count);
            Ok(bytes)
        })
        .await
        .map_err(|error| ProcessError::Pty(error.to_string()))?
    }

    /// Writes ordered input bytes to the PTY.
    ///
    /// # Errors
    ///
    /// Returns an error when the blocking task or native write fails.
    pub async fn write(&self, bytes: &[u8]) -> Result<(), ProcessError> {
        let writer = Arc::clone(&self.writer);
        let bytes = bytes.to_vec();
        spawn_blocking(move || {
            let mut writer = writer
                .lock()
                .map_err(|_| ProcessError::Pty("PTY writer lock was poisoned".to_owned()))?;
            writer.write_all(&bytes)?;
            writer.flush()?;
            Ok(())
        })
        .await
        .map_err(|error| ProcessError::Pty(error.to_string()))?
    }

    /// Applies a terminal-size update to the real PTY.
    ///
    /// # Errors
    ///
    /// Returns an error when the native resize operation fails.
    pub async fn resize(&self, size: PtySize) -> Result<(), ProcessError> {
        let master = Arc::clone(&self.master);
        spawn_blocking(move || {
            master
                .lock()
                .map_err(|_| ProcessError::Pty("PTY master lock was poisoned".to_owned()))?
                .resize(size.into())
                .map_err(|error| ProcessError::Pty(error.to_string()))
        })
        .await
        .map_err(|error| ProcessError::Pty(error.to_string()))?
    }

    /// Waits for the PTY child to exit.
    ///
    /// # Errors
    ///
    /// Returns an error when the blocking task or native wait fails.
    pub async fn wait(&self) -> Result<ExitCause, ProcessError> {
        self.lifecycle.wait().await
    }

    /// Terminates the PTY process group, escalating after `grace`.
    ///
    /// # Errors
    ///
    /// Returns an error when signaling or waiting fails.
    pub async fn terminate(&self, grace: Duration) -> Result<ExitCause, ProcessError> {
        if let Some(result) = self.lifecycle.completed_result() {
            return result;
        }
        let _ = self.group.terminate()?;
        if let Ok(result) = tokio::time::timeout(grace, self.lifecycle.wait()).await {
            result
        } else {
            self.group.seal()?;
            self.lifecycle.wait().await
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        if self.lifecycle.is_running() {
            self.group.try_seal();
        }
    }
}

struct PtyChildGuard {
    child: Box<dyn Child + Send + Sync>,
    group: Option<OwnedProcessGroup>,
    finished: bool,
}

impl std::fmt::Debug for PtyChildGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PtyChildGuard")
            .field("pid", &self.child.process_id())
            .field("group", &self.group)
            .field("finished", &self.finished)
            .finish()
    }
}

impl PtyChildGuard {
    fn new(child: Box<dyn Child + Send + Sync>) -> Self {
        Self {
            child,
            group: None,
            finished: false,
        }
    }

    fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    fn set_group(&mut self, group: OwnedProcessGroup) {
        self.group = Some(group);
    }

    fn cleanup_blocking(&mut self) {
        if self.finished {
            return;
        }
        let group_cleanup = self.group.as_ref().map(OwnedProcessGroup::seal);
        if !matches!(group_cleanup, Some(Ok(()))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        self.finished = true;
    }
}

impl Drop for PtyChildGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Some(group) = &self.group {
            group.try_seal();
        }
        // Never block in `Drop`; explicit initialization failures and the
        // supervisor take the reaping path through `cleanup_blocking`/`wait`.
        let _ = self.child.kill();
    }
}

fn supervise_child(
    guarded: &mut PtyChildGuard,
    permit: OwnedSemaphorePermit,
    outcome: &tokio::sync::watch::Sender<LifecycleOutcome>,
) {
    let group = guarded
        .group
        .as_ref()
        .expect("supervised PTY child has an owned process group");
    let observation = wait_for_leader_exit_blocking(group.pid());
    let cleanup = group.seal();
    if cleanup.is_err() {
        let _ = guarded.child.kill();
    }
    let reaped = guarded.child.wait();
    guarded.finished = true;
    let final_outcome = if let Err(error) = observation {
        LifecycleOutcome::Failed(error.to_string())
    } else if let Err(error) = cleanup {
        LifecycleOutcome::Failed(error.to_string())
    } else {
        match reaped {
            Ok(status) => LifecycleOutcome::Exited(ExitCause::Exited(
                i32::try_from(status.exit_code()).unwrap_or(i32::MAX),
            )),
            Err(error) => {
                LifecycleOutcome::Failed(format!("could not reap PTY process leader: {error}"))
            }
        }
    };
    drop(permit);
    outcome.send_replace(final_outcome);
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
        },
        time::Duration,
    };

    use maestro_domain::RunId;
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

    use crate::{
        EnvironmentPolicy, ExitCause, ProcessError, ProcessSpawner, ProcessSpec, PtyProcess,
        PtySize,
    };

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
    async fn pty_supports_output_input_and_resize() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let spec = ProcessSpec::new("/bin/sh", temp.path())
            .arguments([
                "-c",
                "printf '\\033[31mready ✓\\033[0m\\n'; read value; printf 'got:%s\\n' \"$value\"",
            ])
            .environment(EnvironmentPolicy::default().evaluate_current());
        let run_id = RunId::new();
        let process = ProcessSpawner::new(1)
            .spawn_pty(run_id, spec, PtySize::default())
            .await
            .expect("PTY starts");
        assert_eq!(process.run_id(), run_id);
        let first = process.read(4096).await.expect("read greeting");
        assert!(String::from_utf8_lossy(&first).contains("ready ✓"));

        process
            .resize(PtySize {
                rows: 40,
                columns: 120,
                ..PtySize::default()
            })
            .await
            .expect("PTY resizes");
        process.write(b"maestro\n").await.expect("write input");
        let mut response = Vec::new();
        while !String::from_utf8_lossy(&response).contains("got:maestro") {
            let bytes = process.read(4096).await.expect("read response");
            if bytes.is_empty() {
                break;
            }
            response.extend_from_slice(&bytes);
        }
        assert!(String::from_utf8_lossy(&response).contains("got:maestro"));
        assert_eq!(process.wait().await.expect("exit"), ExitCause::Exited(0));
    }

    #[tokio::test]
    async fn pty_termination_handles_a_leader_that_exits_before_its_descendant() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let ready = temp.path().join("pty-descendant-ready");
        let pid_file = temp.path().join("pty-descendant-pid");
        let spec = ProcessSpec::new("/bin/sh", temp.path())
            .argument("-c")
            .argument(
                "(trap '' TERM; : > \"$1\"; while :; do sleep 1; done) & \
                 child=$!; while [ ! -f \"$1\" ]; do sleep 0.01; done; printf '%s' \"$child\" > \"$2\"; exit 0",
            )
            .argument("maestro-test")
            .argument(&ready)
            .argument(&pid_file)
            .environment(EnvironmentPolicy::default().evaluate_current());
        let process = ProcessSpawner::new(1)
            .spawn_pty(RunId::new(), spec, PtySize::default())
            .await
            .expect("PTY starts");
        wait_for_file(&pid_file).await;
        let child_pid = tokio::fs::read_to_string(&pid_file)
            .await
            .expect("pid reads")
            .parse::<u32>()
            .expect("valid pid");
        assert_eq!(
            process.wait().await.expect("leader exit is observed"),
            ExitCause::Exited(0)
        );
        wait_for_process_state(child_pid, false).await;
        assert_eq!(
            process
                .terminate(Duration::from_millis(50))
                .await
                .expect("completed PTY group stays sealed"),
            ExitCause::Exited(0)
        );
    }

    #[tokio::test]
    async fn dropping_a_pty_process_kills_descendants_and_releases_capacity() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let ready = temp.path().join("pty-drop-ready");
        let pid_file = temp.path().join("pty-drop-pid");
        let spec = ProcessSpec::new("/bin/sh", temp.path())
            .argument("-c")
            .argument(
                "trap '' TERM; (trap '' TERM; : > \"$1\"; while :; do sleep 1; done) & \
                 child=$!; while [ ! -f \"$1\" ]; do sleep 0.01; done; printf '%s' \"$child\" > \"$2\"; wait",
            )
            .argument("maestro-test")
            .argument(&ready)
            .argument(&pid_file)
            .environment(EnvironmentPolicy::default().evaluate_current());
        let spawner = ProcessSpawner::new(1);
        let process = spawner
            .spawn_pty(RunId::new(), spec, PtySize::default())
            .await
            .expect("PTY starts");
        let leader_pid = process.pid();
        wait_for_file(&pid_file).await;
        let child_pid = tokio::fs::read_to_string(&pid_file)
            .await
            .expect("pid reads")
            .parse::<u32>()
            .expect("valid pid");

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

    #[test]
    fn reader_acquisition_failure_kills_and_reaps_the_spawned_child() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let spec = ProcessSpec::new("/bin/sh", temp.path())
            .arguments(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .environment(EnvironmentPolicy::default().evaluate_current());
        let spawner = ProcessSpawner::new(1);
        let permit = spawner.try_reserve().expect("capacity is available");
        let spawned_pid = Arc::new(AtomicU32::new(0));
        let captured_pid = Arc::clone(&spawned_pid);

        let result = PtyProcess::spawn_with_acquirers(
            RunId::new(),
            &spec,
            PtySize::default(),
            permit,
            move |_, pid| {
                captured_pid.store(pid, Ordering::SeqCst);
                Err(ProcessError::Pty("injected reader failure".to_owned()))
            },
            |master, _| {
                master
                    .take_writer()
                    .map_err(|error| ProcessError::Pty(error.to_string()))
            },
        );

        assert!(
            matches!(result, Err(ProcessError::Pty(message)) if message == "injected reader failure")
        );
        let pid = spawned_pid.load(Ordering::SeqCst);
        assert_ne!(pid, 0);
        assert!(!process_exists(pid));
        assert_eq!(spawner.active_count(), 0);
    }

    #[test]
    fn writer_acquisition_failure_kills_and_reaps_the_spawned_child() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let spec = ProcessSpec::new("/bin/sh", temp.path())
            .arguments(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .environment(EnvironmentPolicy::default().evaluate_current());
        let spawner = ProcessSpawner::new(1);
        let permit = spawner.try_reserve().expect("capacity is available");
        let spawned_pid = Arc::new(AtomicU32::new(0));
        let captured_pid = Arc::clone(&spawned_pid);

        let result = PtyProcess::spawn_with_acquirers(
            RunId::new(),
            &spec,
            PtySize::default(),
            permit,
            |master, _| {
                master
                    .try_clone_reader()
                    .map_err(|error| ProcessError::Pty(error.to_string()))
            },
            move |_, pid| {
                captured_pid.store(pid, Ordering::SeqCst);
                Err(ProcessError::Pty("injected writer failure".to_owned()))
            },
        );

        assert!(
            matches!(result, Err(ProcessError::Pty(message)) if message == "injected writer failure")
        );
        let pid = spawned_pid.load(Ordering::SeqCst);
        assert_ne!(pid, 0);
        assert!(!process_exists(pid));
        assert_eq!(spawner.active_count(), 0);
    }
}
