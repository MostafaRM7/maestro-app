use std::{
    collections::{HashMap, VecDeque},
    ffi::OsStr,
    future::{Future, poll_fn},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::Poll,
    time::Duration,
};

use maestro_adapter::{AdapterWriterLease, ProcessLaunchSpec, ProcessTransport, TuiLaunchPlan};
use maestro_domain::{ErrorCode, MaestroError, ProjectId, RunId, TerminalId};
use maestro_process::{
    ControlledEnvironment, EnvironmentPolicy, ExitCause, ProcessSpawner, ProcessSpec, PtyProcess,
    PtySize,
};
use maestro_protocol::{
    MAX_TERMINAL_DIMENSION, MAX_TERMINAL_INDEX_ENTRIES, MAX_TERMINAL_INPUT_BYTES,
    MAX_TERMINAL_PATH_BYTES, MAX_TERMINAL_POLL_BYTES, MAX_TERMINAL_READ_WAIT_MILLISECONDS,
    MIN_TERMINAL_POLL_BYTES, TerminalExit, TerminalIndexEntry, TerminalOpened, TerminalOutputChunk,
    TerminalReadResult, TerminalState, TerminalStatus,
};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, RwLock, Semaphore, mpsc};

use crate::storage_runtime::{StorageRuntime, StorageRuntimeError};

const TERMINAL_READ_CHUNK_BYTES: usize = MIN_TERMINAL_POLL_BYTES as usize;
const MAX_BUFFERED_TERMINAL_BYTES: usize = 1024 * 1024;
const PERSISTED_SEGMENT_BYTES: usize = 64 * 1024;
const PERSISTENCE_CHANNEL_CAPACITY: usize = 64;
const PERSISTENCE_FLUSH_DELAY: Duration = Duration::from_millis(500);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
const CLOSE_ALL_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) struct TerminalManager {
    terminals: RwLock<HashMap<TerminalId, Arc<TerminalSession>>>,
    completed: Arc<Mutex<VecDeque<TerminalId>>>,
    slots: Arc<Semaphore>,
    limit: usize,
    process_spawner: ProcessSpawner,
    activity_changed: Arc<Notify>,
    storage: Option<Arc<StorageRuntime>>,
}

#[derive(Debug)]
struct TerminalLaunchOwnership {
    run_id: RunId,
    writer_lease: Option<Box<dyn AdapterWriterLease>>,
    persistence: Option<(ProjectId, String, String)>,
}

impl TerminalManager {
    pub(crate) fn new(
        process_spawner: ProcessSpawner,
        limit: usize,
        activity_changed: Arc<Notify>,
    ) -> Self {
        Self {
            terminals: RwLock::new(HashMap::new()),
            completed: Arc::new(Mutex::new(VecDeque::new())),
            slots: Arc::new(Semaphore::new(limit)),
            limit,
            process_spawner,
            activity_changed,
            storage: None,
        }
    }

    pub(crate) fn with_storage(
        process_spawner: ProcessSpawner,
        limit: usize,
        activity_changed: Arc<Notify>,
        storage: Arc<StorageRuntime>,
    ) -> Self {
        let mut manager = Self::new(process_spawner, limit, activity_changed);
        manager.storage = Some(storage);
        manager
    }

    #[cfg(test)]
    pub(crate) async fn open(
        &self,
        cwd: &str,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalOpened, MaestroError> {
        let (canonical_cwd, canonical, environment, terminal_permit) =
            self.prepare_launch(cwd, columns, rows).await?;
        let shell = resolve_shell(&environment).await;
        let spec = ProcessSpec::new(&shell.executable, canonical)
            .arguments(shell.arguments)
            .environment(environment);
        self.spawn(spec, canonical_cwd, terminal_permit, columns, rows)
            .await
    }

    pub(crate) async fn open_for_project(
        &self,
        project_id: ProjectId,
        cwd: &str,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalOpened, MaestroError> {
        let (canonical_cwd, canonical, environment, terminal_permit) =
            self.prepare_launch(cwd, columns, rows).await?;
        let shell = resolve_shell(&environment).await;
        let spec = ProcessSpec::new(&shell.executable, canonical)
            .arguments(shell.arguments)
            .environment(environment);
        self.spawn_persisted(
            spec,
            canonical_cwd,
            terminal_permit,
            columns,
            rows,
            project_id,
            "shell",
            "Shell terminal",
        )
        .await
    }

    /// Launches only the daemon-configured fake fixture in exact TUI mode.
    /// The caller chooses a scenario, never an executable or arbitrary args.
    #[cfg(test)]
    pub(crate) async fn open_fake_tui(
        &self,
        executable: &Path,
        scenario: &str,
        cwd: &str,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalOpened, MaestroError> {
        let (canonical_cwd, canonical, environment, terminal_permit) =
            self.prepare_launch(cwd, columns, rows).await?;
        let spec = ProcessSpec::new(executable, canonical)
            .arguments(["--scenario".to_owned(), scenario.to_owned()])
            .environment(environment);
        self.spawn(spec, canonical_cwd, terminal_permit, columns, rows)
            .await
    }

    pub(crate) async fn open_tui_plan_for_project(
        &self,
        project_id: ProjectId,
        run_id: RunId,
        plan: TuiLaunchPlan,
        columns: u16,
        rows: u16,
        title: &str,
    ) -> Result<TerminalOpened, MaestroError> {
        if plan.process().transport != ProcessTransport::Pty {
            return Err(MaestroError::new(
                ErrorCode::InvalidRequest,
                "the adapter terminal launch plan must use PTY transport",
            ));
        }
        let cwd = plan
            .process()
            .working_directory
            .to_str()
            .ok_or_else(invalid_path_error)?
            .to_owned();
        let (canonical_cwd, canonical, environment, terminal_permit) =
            self.prepare_launch(&cwd, columns, rows).await?;
        let (launch, writer_lease) = plan.into_parts();
        let ProcessLaunchSpec {
            executable,
            arguments,
            working_directory: _,
            transport: _,
            requested_environment_variables: _,
        } = launch;
        let spec = ProcessSpec::new(executable, canonical)
            .arguments(arguments)
            .environment(environment);
        self.spawn_inner(
            spec,
            canonical_cwd,
            terminal_permit,
            columns,
            rows,
            TerminalLaunchOwnership {
                run_id,
                writer_lease,
                persistence: Some((project_id, "agent_tui".to_owned(), title.to_owned())),
            },
        )
        .await
    }

    async fn prepare_launch(
        &self,
        cwd: &str,
        columns: u16,
        rows: u16,
    ) -> Result<(String, PathBuf, ControlledEnvironment, OwnedSemaphorePermit), MaestroError> {
        validate_dimensions(columns, rows)?;
        if cwd.is_empty() || cwd.len() > MAX_TERMINAL_PATH_BYTES {
            return Err(invalid_path_error());
        }
        let requested = Path::new(cwd);
        if !requested.is_absolute() {
            return Err(invalid_path_error());
        }
        let canonical = tokio::fs::canonicalize(requested)
            .await
            .map_err(|_| invalid_path_error())?;
        let metadata = tokio::fs::metadata(&canonical)
            .await
            .map_err(|_| invalid_path_error())?;
        if !metadata.is_dir() {
            return Err(invalid_path_error());
        }
        let canonical_cwd = canonical
            .to_str()
            .ok_or_else(invalid_path_error)?
            .to_owned();

        let terminal_permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| terminal_limit_error(self.limit))?;
        self.prune_completed_for_insert().await;
        let environment = EnvironmentPolicy::default()
            .override_value("TERM", "xterm-256color")
            .override_value("COLORTERM", "truecolor")
            .evaluate_current();
        Ok((canonical_cwd, canonical, environment, terminal_permit))
    }

    #[cfg(test)]
    async fn spawn(
        &self,
        spec: ProcessSpec,
        canonical_cwd: String,
        terminal_permit: OwnedSemaphorePermit,
        columns: u16,
        rows: u16,
    ) -> Result<TerminalOpened, MaestroError> {
        self.spawn_inner(
            spec,
            canonical_cwd,
            terminal_permit,
            columns,
            rows,
            TerminalLaunchOwnership {
                run_id: RunId::new(),
                writer_lease: None,
                persistence: None,
            },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn spawn_persisted(
        &self,
        spec: ProcessSpec,
        canonical_cwd: String,
        terminal_permit: OwnedSemaphorePermit,
        columns: u16,
        rows: u16,
        project_id: ProjectId,
        kind: &str,
        title: &str,
    ) -> Result<TerminalOpened, MaestroError> {
        self.spawn_inner(
            spec,
            canonical_cwd,
            terminal_permit,
            columns,
            rows,
            TerminalLaunchOwnership {
                run_id: RunId::new(),
                writer_lease: None,
                persistence: Some((project_id, kind.to_owned(), title.to_owned())),
            },
        )
        .await
    }

    async fn spawn_inner(
        &self,
        spec: ProcessSpec,
        canonical_cwd: String,
        terminal_permit: OwnedSemaphorePermit,
        columns: u16,
        rows: u16,
        ownership: TerminalLaunchOwnership,
    ) -> Result<TerminalOpened, MaestroError> {
        let TerminalLaunchOwnership {
            run_id,
            mut writer_lease,
            persistence,
        } = ownership;
        let process = Arc::new(
            self.process_spawner
                .spawn_pty(
                    run_id,
                    spec,
                    PtySize {
                        rows,
                        columns,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                )
                .await
                .map_err(|error| process_error(&error))?,
        );
        let terminal_id = TerminalId::new();
        debug_assert_eq!(process.run_id(), run_id);
        let process_id = process.pid();
        let metadata = persistence
            .as_ref()
            .map(|(project_id, kind, title)| TerminalMetadata {
                project_id: *project_id,
                kind: kind.clone(),
                title: title.clone(),
            });
        let persistence_sender =
            if let (Some(storage), Some(metadata)) = (self.storage.as_ref(), metadata.as_ref()) {
                let registration_storage = Arc::clone(storage);
                let project_id = metadata.project_id;
                let kind = metadata.kind.clone();
                let title = metadata.title.clone();
                let registration = tokio::task::spawn_blocking(move || {
                    registration_storage.register_terminal(project_id, terminal_id, &kind, &title)
                })
                .await;
                if !matches!(registration, Ok(Ok(()))) {
                    if process.terminate(TERMINATION_GRACE).await.is_err() {
                        retain_writer_lease_until_process_completion(
                            Arc::clone(&process),
                            writer_lease.take(),
                        );
                    }
                    return Err(terminal_storage_error());
                }
                Some(spawn_persistence_writer(terminal_id, Arc::clone(storage)))
            } else {
                None
            };
        let session = Arc::new(TerminalSession::new(
            process,
            writer_lease,
            terminal_permit,
            canonical_cwd.clone(),
            metadata,
        ));
        self.terminals
            .write()
            .await
            .insert(terminal_id, Arc::clone(&session));
        self.activity_changed.notify_one();
        spawn_output_reader(
            terminal_id,
            session,
            Arc::clone(&self.completed),
            Arc::clone(&self.activity_changed),
            persistence_sender,
            self.storage.clone(),
        );

        Ok(TerminalOpened {
            terminal_id,
            run_id,
            process_id,
            canonical_cwd,
            state: TerminalState::Running,
        })
    }

    /// Lists only retained terminals owned by one project. The bounded result
    /// is suitable for view reattachment and never changes process ownership.
    pub(crate) async fn list_for_project(
        &self,
        project_id: ProjectId,
        maximum_terminals: usize,
    ) -> Result<Vec<TerminalIndexEntry>, MaestroError> {
        if maximum_terminals == 0 || maximum_terminals > MAX_TERMINAL_INDEX_ENTRIES {
            return Err(MaestroError::new(
                ErrorCode::InvalidRequest,
                "terminal index limit is outside the supported range",
            ));
        }
        let sessions = self.terminals.read().await;
        let mut matching = sessions
            .iter()
            .filter_map(|(terminal_id, session)| {
                session
                    .metadata
                    .as_ref()
                    .filter(|metadata| {
                        metadata.project_id == project_id && metadata.kind == "shell"
                    })
                    .map(|metadata| (*terminal_id, Arc::clone(session), metadata.clone()))
            })
            .collect::<Vec<_>>();
        drop(sessions);
        matching.sort_by_key(|(terminal_id, _, _)| terminal_id.to_string());
        matching.truncate(maximum_terminals);

        let mut entries = Vec::with_capacity(matching.len());
        for (terminal_id, session, metadata) in matching {
            let terminal = session.opened(terminal_id).await;
            let exit = session.runtime.lock().await.exit;
            entries.push(TerminalIndexEntry {
                project_id,
                terminal,
                kind: metadata.kind,
                title: metadata.title,
                exit,
            });
        }
        Ok(entries)
    }

    /// Attaches only when the daemon-owned terminal belongs to the supplied
    /// persisted project. This check is independent of webview capabilities.
    pub(crate) async fn attach_shell_for_project(
        &self,
        project_id: ProjectId,
        terminal_id: TerminalId,
    ) -> Result<TerminalOpened, MaestroError> {
        self.attach_for_project_kind(project_id, terminal_id, "shell")
            .await
    }

    pub(crate) async fn attach_tui_for_project(
        &self,
        project_id: ProjectId,
        terminal_id: TerminalId,
    ) -> Result<TerminalOpened, MaestroError> {
        self.attach_for_project_kind(project_id, terminal_id, "agent_tui")
            .await
    }

    async fn attach_for_project_kind(
        &self,
        project_id: ProjectId,
        terminal_id: TerminalId,
        expected_kind: &str,
    ) -> Result<TerminalOpened, MaestroError> {
        let session = self.session(terminal_id).await?;
        if session.metadata.as_ref().is_none_or(|metadata| {
            metadata.project_id != project_id || metadata.kind != expected_kind
        }) {
            return Err(MaestroError::new(
                ErrorCode::PermissionDenied,
                "terminal does not belong to the requested project or integration mode",
            ));
        }
        Ok(session.opened(terminal_id).await)
    }

    pub(crate) async fn write(
        &self,
        terminal_id: TerminalId,
        data: &[u8],
    ) -> Result<(), MaestroError> {
        if data.len() > MAX_TERMINAL_INPUT_BYTES {
            let mut error = MaestroError::new(
                ErrorCode::InputTooLarge,
                "terminal input exceeds the maximum request size",
            );
            error.details = Some(serde_json::json!({
                "actual_bytes": data.len(),
                "maximum_bytes": MAX_TERMINAL_INPUT_BYTES,
            }));
            return Err(error);
        }
        let session = self.session(terminal_id).await?;
        let _operation_guard = session.operation_guard.lock().await;
        session.require_running().await?;
        session
            .process
            .write(data)
            .await
            .map_err(|error| process_error(&error))
    }

    pub(crate) async fn resize(
        &self,
        terminal_id: TerminalId,
        columns: u16,
        rows: u16,
    ) -> Result<(), MaestroError> {
        validate_dimensions(columns, rows)?;
        let session = self.session(terminal_id).await?;
        let _operation_guard = session.operation_guard.lock().await;
        session.require_running().await?;
        session
            .process
            .resize(PtySize {
                rows,
                columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .await
            .map_err(|error| process_error(&error))
    }

    pub(crate) async fn read(
        &self,
        terminal_id: TerminalId,
        after_sequence: u64,
        maximum_bytes: u32,
        wait_milliseconds: u32,
    ) -> Result<TerminalReadResult, MaestroError> {
        if !(MIN_TERMINAL_POLL_BYTES..=MAX_TERMINAL_POLL_BYTES).contains(&maximum_bytes) {
            let mut error = MaestroError::new(
                ErrorCode::InvalidRequest,
                "terminal poll size is outside the supported range",
            );
            error.details = Some(serde_json::json!({
                "minimum_bytes": MIN_TERMINAL_POLL_BYTES,
                "maximum_bytes": MAX_TERMINAL_POLL_BYTES,
            }));
            return Err(error);
        }
        if wait_milliseconds > MAX_TERMINAL_READ_WAIT_MILLISECONDS {
            let mut error = MaestroError::new(
                ErrorCode::InvalidRequest,
                "terminal read wait exceeds the supported maximum",
            );
            error.details = Some(serde_json::json!({
                "maximum_wait_milliseconds": MAX_TERMINAL_READ_WAIT_MILLISECONDS,
            }));
            return Err(error);
        }
        let session = self.session(terminal_id).await?;
        let output_changed = session.output_changed.notified();
        tokio::pin!(output_changed);
        output_changed.as_mut().enable();

        let initial = terminal_read_snapshot(
            &session,
            terminal_id,
            after_sequence,
            maximum_bytes as usize,
        )
        .await?;
        if wait_milliseconds == 0
            || !initial.chunks.is_empty()
            || initial.overflowed
            || initial.state != TerminalState::Running
        {
            return Ok(initial);
        }

        let _ = tokio::time::timeout(
            Duration::from_millis(u64::from(wait_milliseconds)),
            output_changed,
        )
        .await;
        terminal_read_snapshot(
            &session,
            terminal_id,
            after_sequence,
            maximum_bytes as usize,
        )
        .await
    }

    pub(crate) async fn status(
        &self,
        terminal_id: TerminalId,
    ) -> Result<TerminalStatus, MaestroError> {
        Ok(self.session(terminal_id).await?.status(terminal_id).await)
    }

    /// Waits without polling until a retained terminal reaches a final state.
    pub(crate) async fn wait_for_completion(
        &self,
        terminal_id: TerminalId,
    ) -> Result<TerminalStatus, MaestroError> {
        let session = self.session(terminal_id).await?;
        loop {
            let changed = session.output_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let status = session.status(terminal_id).await;
            if !matches!(
                status.state,
                TerminalState::Running | TerminalState::Closing
            ) {
                return Ok(status);
            }
            changed.await;
        }
    }

    pub(crate) async fn close(
        &self,
        terminal_id: TerminalId,
    ) -> Result<TerminalStatus, MaestroError> {
        let session = self.session(terminal_id).await?;
        let _operation_guard = session.operation_guard.lock().await;
        if !self.terminals.read().await.contains_key(&terminal_id) {
            return Err(terminal_not_found(terminal_id));
        }

        {
            let mut runtime = session.runtime.lock().await;
            runtime.close_requested = true;
            if !matches!(runtime.state, TerminalState::Exited | TerminalState::Closed) {
                runtime.state = TerminalState::Closing;
            }
        }
        if !session.is_finalized().await {
            match session.process.terminate(TERMINATION_GRACE).await {
                Ok(cause) => {
                    session
                        .finalize_reaped(
                            terminal_id,
                            Ok(exit_from_cause(cause)),
                            &self.completed,
                            &self.activity_changed,
                        )
                        .await;
                }
                Err(error) => {
                    if session.note_termination_failure().await {
                        return Err(process_error(&error));
                    }
                }
            }
        }
        session.runtime.lock().await.state = TerminalState::Closed;
        session.mark_removed(terminal_id, &self.completed).await;
        let status = session.status(terminal_id).await;
        self.terminals.write().await.remove(&terminal_id);
        self.activity_changed.notify_one();
        Ok(status)
    }

    pub(crate) async fn close_all(&self) {
        let terminal_ids = self
            .terminals
            .read()
            .await
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let mut closes = terminal_ids
            .into_iter()
            .map(|terminal_id| Some(Box::pin(self.close(terminal_id))))
            .collect::<Vec<_>>();
        let mut failures = 0_usize;
        let all_closed = poll_fn(move |context| {
            let mut pending = false;
            for close in &mut closes {
                let Some(future) = close.as_mut() else {
                    continue;
                };
                match Future::poll(Pin::as_mut(future), context) {
                    Poll::Ready(result) => {
                        if result.is_err() {
                            failures += 1;
                        }
                        *close = None;
                    }
                    Poll::Pending => pending = true,
                }
            }
            if pending {
                Poll::Pending
            } else {
                Poll::Ready(failures)
            }
        });
        match tokio::time::timeout(CLOSE_ALL_DEADLINE, all_closed).await {
            Ok(0) => {}
            Ok(failures) => tracing::warn!(
                failures,
                "one or more terminal close operations failed; conclusive reader reaping remains armed"
            ),
            Err(_) => tracing::warn!(
                deadline_seconds = CLOSE_ALL_DEADLINE.as_secs(),
                "terminal shutdown deadline expired; process Drop cleanup remains armed"
            ),
        }
    }

    pub(crate) fn count(&self) -> usize {
        self.limit - self.slots.available_permits()
    }

    async fn session(&self, terminal_id: TerminalId) -> Result<Arc<TerminalSession>, MaestroError> {
        self.terminals
            .read()
            .await
            .get(&terminal_id)
            .cloned()
            .ok_or_else(|| terminal_not_found(terminal_id))
    }

    async fn prune_completed_for_insert(&self) {
        while self.terminals.read().await.len() >= self.limit {
            let Some(terminal_id) = self.completed.lock().await.pop_front() else {
                break;
            };
            if let Some(session) = self.terminals.write().await.remove(&terminal_id) {
                session
                    .retained
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }
}

async fn terminal_read_snapshot(
    session: &TerminalSession,
    terminal_id: TerminalId,
    after_sequence: u64,
    maximum_bytes: usize,
) -> Result<TerminalReadResult, MaestroError> {
    let status = session.status(terminal_id).await;
    let read = session
        .output
        .lock()
        .await
        .poll(after_sequence, maximum_bytes)?;
    Ok(TerminalReadResult {
        terminal_id,
        chunks: read.chunks,
        next_sequence: read.next_sequence,
        latest_sequence: read.latest_sequence,
        overflowed: read.overflowed,
        dropped_through_sequence: read.dropped_through_sequence,
        state: status.state,
        exit: status.exit,
    })
}

#[derive(Debug)]
struct TerminalSession {
    // Keep the process before the writer lease so defensive field-drop order
    // cannot release a vendor binding before PTY process cleanup runs.
    process: Arc<PtyProcess>,
    writer_lease: Mutex<Option<Box<dyn AdapterWriterLease>>>,
    canonical_cwd: String,
    output: Mutex<OutputBuffer>,
    output_changed: Notify,
    runtime: Mutex<TerminalRuntime>,
    operation_guard: Mutex<()>,
    finalization_guard: Mutex<()>,
    terminal_permit: Mutex<Option<OwnedSemaphorePermit>>,
    retained: std::sync::atomic::AtomicBool,
    metadata: Option<TerminalMetadata>,
}

#[derive(Debug, Clone)]
struct TerminalMetadata {
    project_id: ProjectId,
    kind: String,
    title: String,
}

impl TerminalSession {
    fn new(
        process: Arc<PtyProcess>,
        writer_lease: Option<Box<dyn AdapterWriterLease>>,
        terminal_permit: OwnedSemaphorePermit,
        canonical_cwd: String,
        metadata: Option<TerminalMetadata>,
    ) -> Self {
        Self {
            process,
            writer_lease: Mutex::new(writer_lease),
            canonical_cwd,
            output: Mutex::new(OutputBuffer::default()),
            output_changed: Notify::new(),
            runtime: Mutex::new(TerminalRuntime::default()),
            operation_guard: Mutex::new(()),
            finalization_guard: Mutex::new(()),
            terminal_permit: Mutex::new(Some(terminal_permit)),
            retained: std::sync::atomic::AtomicBool::new(true),
            metadata,
        }
    }

    async fn opened(&self, terminal_id: TerminalId) -> TerminalOpened {
        TerminalOpened {
            terminal_id,
            run_id: self.process.run_id(),
            process_id: self.process.pid(),
            canonical_cwd: self.canonical_cwd.clone(),
            state: self.runtime.lock().await.state,
        }
    }

    async fn require_running(&self) -> Result<(), MaestroError> {
        if self.runtime.lock().await.state == TerminalState::Running {
            Ok(())
        } else {
            Err(MaestroError::new(
                ErrorCode::TerminalNotRunning,
                "terminal is not running",
            ))
        }
    }

    async fn status(&self, terminal_id: TerminalId) -> TerminalStatus {
        let runtime = self.runtime.lock().await;
        TerminalStatus {
            terminal_id,
            state: runtime.state,
            exit: runtime.exit,
        }
    }

    async fn is_finalized(&self) -> bool {
        let _finalization_guard = self.finalization_guard.lock().await;
        self.terminal_permit.lock().await.is_none()
    }

    async fn note_termination_failure(&self) -> bool {
        let _finalization_guard = self.finalization_guard.lock().await;
        if self.terminal_permit.lock().await.is_none() {
            return false;
        }
        let mut runtime = self.runtime.lock().await;
        runtime.state = TerminalState::Failed;
        runtime.exit = None;
        self.output_changed.notify_waiters();
        true
    }

    async fn finalize_reaped(
        &self,
        terminal_id: TerminalId,
        completion: Result<TerminalExit, ()>,
        completed: &Mutex<VecDeque<TerminalId>>,
        activity_changed: &Notify,
    ) {
        let _finalization_guard = self.finalization_guard.lock().await;
        // Reaping has already completed when this method is called. Release
        // the adapter writer before publishing a final terminal state so an
        // observer can never see Exited/Closed while the binding is claimed.
        drop(self.writer_lease.lock().await.take());
        {
            let mut runtime = self.runtime.lock().await;
            if let Ok(exit) = completion {
                runtime.exit = Some(exit);
                runtime.state = if runtime.close_requested {
                    TerminalState::Closed
                } else {
                    TerminalState::Exited
                };
            } else {
                runtime.state = TerminalState::Failed;
                runtime.exit = None;
            }
        }

        let permit = self.terminal_permit.lock().await.take();
        self.output_changed.notify_waiters();
        if permit.is_some() {
            let mut completed = completed.lock().await;
            if self.retained.load(std::sync::atomic::Ordering::SeqCst) {
                completed.push_back(terminal_id);
            }
            drop(completed);
            drop(permit);
            activity_changed.notify_one();
        }
    }

    async fn mark_removed(&self, terminal_id: TerminalId, completed: &Mutex<VecDeque<TerminalId>>) {
        let mut completed = completed.lock().await;
        self.retained
            .store(false, std::sync::atomic::Ordering::SeqCst);
        completed.retain(|candidate| *candidate != terminal_id);
    }
}

#[derive(Debug, Clone)]
struct TerminalRuntime {
    state: TerminalState,
    exit: Option<TerminalExit>,
    close_requested: bool,
}

impl Default for TerminalRuntime {
    fn default() -> Self {
        Self {
            state: TerminalState::Running,
            exit: None,
            close_requested: false,
        }
    }
}

#[derive(Debug, Default)]
struct OutputBuffer {
    chunks: VecDeque<TerminalOutputChunk>,
    buffered_bytes: usize,
    latest_sequence: u64,
    dropped_through_sequence: u64,
}

impl OutputBuffer {
    fn push(&mut self, data: Vec<u8>) -> Option<u64> {
        if data.is_empty() {
            return None;
        }
        self.latest_sequence = self.latest_sequence.saturating_add(1);
        let sequence = self.latest_sequence;
        self.buffered_bytes += data.len();
        self.chunks
            .push_back(TerminalOutputChunk { sequence, data });
        while self.buffered_bytes > MAX_BUFFERED_TERMINAL_BYTES {
            let Some(dropped) = self.chunks.pop_front() else {
                break;
            };
            self.buffered_bytes = self.buffered_bytes.saturating_sub(dropped.data.len());
            self.dropped_through_sequence = dropped.sequence;
        }
        Some(sequence)
    }

    fn poll(&self, after_sequence: u64, maximum_bytes: usize) -> Result<PollResult, MaestroError> {
        if after_sequence > self.latest_sequence {
            return Err(MaestroError::new(
                ErrorCode::InvalidRequest,
                "terminal cursor is newer than the available output",
            ));
        }
        let overflowed = after_sequence < self.dropped_through_sequence;
        let cursor = after_sequence.max(self.dropped_through_sequence);
        let mut bytes = 0_usize;
        let mut chunks = Vec::new();
        for chunk in self.chunks.iter().filter(|chunk| chunk.sequence > cursor) {
            if bytes + chunk.data.len() > maximum_bytes {
                break;
            }
            bytes += chunk.data.len();
            chunks.push(chunk.clone());
        }
        let next_sequence = chunks.last().map_or(cursor, |chunk| chunk.sequence);
        Ok(PollResult {
            chunks,
            next_sequence,
            latest_sequence: self.latest_sequence,
            overflowed,
            dropped_through_sequence: overflowed.then_some(self.dropped_through_sequence),
        })
    }
}

#[derive(Debug)]
struct PollResult {
    chunks: Vec<TerminalOutputChunk>,
    next_sequence: u64,
    latest_sequence: u64,
    overflowed: bool,
    dropped_through_sequence: Option<u64>,
}

fn spawn_output_reader(
    terminal_id: TerminalId,
    session: Arc<TerminalSession>,
    completed: Arc<Mutex<VecDeque<TerminalId>>>,
    activity_changed: Arc<Notify>,
    persistence: Option<mpsc::Sender<PersistedTerminalChunk>>,
    storage: Option<Arc<StorageRuntime>>,
) {
    tokio::spawn(async move {
        let mut read_failed = false;
        loop {
            match session.process.read(TERMINAL_READ_CHUNK_BYTES).await {
                Ok(data) if data.is_empty() => break,
                Ok(data) => {
                    let persisted_data = persistence.as_ref().map(|_| data.clone());
                    let sequence = session.output.lock().await.push(data);
                    session.output_changed.notify_waiters();
                    if let (Some(sender), Some(data), Some(sequence)) =
                        (persistence.as_ref(), persisted_data, sequence)
                    {
                        let _ = sender.send(PersistedTerminalChunk { sequence, data }).await;
                    }
                }
                Err(_) => {
                    read_failed = true;
                    break;
                }
            }
        }
        let process_completion = if read_failed {
            session.process.terminate(TERMINATION_GRACE).await
        } else {
            session.process.wait().await
        };
        let completion = match process_completion {
            Ok(cause) => Ok(exit_from_cause(cause)),
            Err(_) => Err(()),
        };

        session
            .finalize_reaped(terminal_id, completion, &completed, &activity_changed)
            .await;
        drop(persistence);
        if let Some(storage) = storage {
            let state = terminal_state_name(session.status(terminal_id).await.state);
            let _ = tokio::task::spawn_blocking(move || {
                storage.update_terminal_state(terminal_id, state)
            })
            .await;
        }
    });
}

fn retain_writer_lease_until_process_completion(
    process: Arc<PtyProcess>,
    writer_lease: Option<Box<dyn AdapterWriterLease>>,
) {
    if writer_lease.is_none() {
        return;
    }
    tokio::spawn(async move {
        // A failed termination request is not proof that the child is gone.
        // The process supervisor resolves this wait only after its reap path.
        let _ = process.wait().await;
        drop(writer_lease);
    });
}

#[derive(Debug)]
struct PersistedTerminalChunk {
    sequence: u64,
    data: Vec<u8>,
}

fn spawn_persistence_writer(
    terminal_id: TerminalId,
    storage: Arc<StorageRuntime>,
) -> mpsc::Sender<PersistedTerminalChunk> {
    let (sender, mut receiver) =
        mpsc::channel::<PersistedTerminalChunk>(PERSISTENCE_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        let mut sequence_start = None;
        let mut sequence_end = 0_u64;
        let mut buffer = Vec::with_capacity(PERSISTED_SEGMENT_BYTES);
        loop {
            if buffer.is_empty() {
                let Some(chunk) = receiver.recv().await else {
                    break;
                };
                append_persisted_chunk(&chunk, &mut sequence_start, &mut sequence_end, &mut buffer);
                continue;
            }

            let flush_now = tokio::select! {
                chunk = receiver.recv() => {
                    let Some(chunk) = chunk else {
                        break;
                    };
                    append_persisted_chunk(
                        &chunk,
                        &mut sequence_start,
                        &mut sequence_end,
                        &mut buffer,
                    );
                    buffer.len() >= PERSISTED_SEGMENT_BYTES
                }
                () = tokio::time::sleep(PERSISTENCE_FLUSH_DELAY) => true
            };
            if flush_now
                && persist_buffer(
                    Arc::clone(&storage),
                    terminal_id,
                    &mut sequence_start,
                    sequence_end,
                    &mut buffer,
                )
                .await
                .is_err()
            {
                tracing::warn!(%terminal_id, "encrypted terminal scrollback persistence failed");
            }
        }
        if !buffer.is_empty()
            && persist_buffer(
                storage,
                terminal_id,
                &mut sequence_start,
                sequence_end,
                &mut buffer,
            )
            .await
            .is_err()
        {
            tracing::warn!(%terminal_id, "final encrypted terminal scrollback persistence failed");
        }
    });
    sender
}

fn append_persisted_chunk(
    chunk: &PersistedTerminalChunk,
    sequence_start: &mut Option<u64>,
    sequence_end: &mut u64,
    buffer: &mut Vec<u8>,
) {
    sequence_start.get_or_insert(chunk.sequence);
    *sequence_end = chunk.sequence;
    buffer.extend_from_slice(&chunk.data);
}

async fn persist_buffer(
    storage: Arc<StorageRuntime>,
    terminal_id: TerminalId,
    sequence_start: &mut Option<u64>,
    sequence_end: u64,
    buffer: &mut Vec<u8>,
) -> Result<(), StorageRuntimeError> {
    let start = sequence_start.take().unwrap_or(sequence_end);
    let data = std::mem::take(buffer);
    let result = tokio::task::spawn_blocking(move || {
        storage.persist_terminal_segment(terminal_id, start, sequence_end, &data)
    })
    .await
    .map_err(|_| StorageRuntimeError::Unavailable)?;
    *buffer = Vec::with_capacity(PERSISTED_SEGMENT_BYTES);
    result
}

fn terminal_state_name(state: TerminalState) -> &'static str {
    match state {
        TerminalState::Running => "running",
        TerminalState::Exited => "exited",
        TerminalState::Closing => "closing",
        TerminalState::Closed => "closed",
        TerminalState::Failed => "failed",
    }
}

#[derive(Debug)]
struct ShellInvocation {
    executable: PathBuf,
    arguments: Vec<&'static str>,
}

async fn resolve_shell(environment: &ControlledEnvironment) -> ShellInvocation {
    let configured = environment
        .values()
        .get(OsStr::new("SHELL"))
        .map(PathBuf::from);
    if let Some(shell) = configured
        && shell.is_absolute()
        && tokio::fs::metadata(&shell)
            .await
            .is_ok_and(|metadata| metadata.is_file())
    {
        return ShellInvocation {
            arguments: shell_arguments(&shell),
            executable: shell,
        };
    }
    ShellInvocation {
        executable: PathBuf::from("/bin/sh"),
        arguments: Vec::new(),
    }
}

fn shell_arguments(shell: &Path) -> Vec<&'static str> {
    match shell.file_name().and_then(OsStr::to_str) {
        Some("zsh") => vec!["-f"],
        Some("bash") => vec!["--noprofile", "--norc"],
        Some("fish") => vec!["--no-config"],
        _ => Vec::new(),
    }
}

fn validate_dimensions(columns: u16, rows: u16) -> Result<(), MaestroError> {
    if columns == 0
        || rows == 0
        || columns > MAX_TERMINAL_DIMENSION
        || rows > MAX_TERMINAL_DIMENSION
    {
        Err(MaestroError::new(
            ErrorCode::InvalidRequest,
            "terminal dimensions are outside the supported range",
        ))
    } else {
        Ok(())
    }
}

fn invalid_path_error() -> MaestroError {
    MaestroError::new(
        ErrorCode::InvalidPath,
        "terminal working directory must be an existing absolute directory",
    )
}

fn terminal_limit_error(limit: usize) -> MaestroError {
    let mut error = MaestroError::new(
        ErrorCode::TerminalLimitReached,
        "terminal limit has been reached",
    );
    error.details = Some(serde_json::json!({ "limit": limit }));
    error
}

fn terminal_not_found(terminal_id: TerminalId) -> MaestroError {
    let mut error = MaestroError::new(ErrorCode::TerminalNotFound, "terminal does not exist");
    error.details = Some(serde_json::json!({ "terminal_id": terminal_id }));
    error
}

fn terminal_storage_error() -> MaestroError {
    MaestroError::new(
        ErrorCode::DatabaseUnavailable,
        "encrypted terminal history is unavailable",
    )
}

fn process_error(error: &maestro_process::ProcessError) -> MaestroError {
    let mut result = MaestroError::new(
        ErrorCode::ProcessCrashed,
        "terminal process operation failed",
    );
    result.retryable = false;
    result.details = Some(serde_json::json!({
        "category": match error {
            maestro_process::ProcessError::Capacity { .. } => "capacity",
            maestro_process::ProcessError::Io(_) => "io",
            maestro_process::ProcessError::Pty(_) => "pty",
            maestro_process::ProcessError::MissingStream(_)
            | maestro_process::ProcessError::AlreadyExited
            | maestro_process::ProcessError::MissingProcessId
            | maestro_process::ProcessError::Termination(_) => "lifecycle",
        }
    }));
    result
}

fn exit_from_cause(cause: ExitCause) -> TerminalExit {
    match cause {
        ExitCause::Exited(code) => TerminalExit {
            code: Some(code),
            signal: None,
        },
        ExitCause::Signaled(signal) => TerminalExit {
            code: None,
            signal: Some(signal),
        },
        ExitCause::Unknown => TerminalExit {
            code: None,
            signal: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc, time::Duration};

    use maestro_adapter::{
        AdapterErrorKind, AgentAdapter, FakeAdapter, ResumeSessionRequest, RunStopReason,
        SessionOptions, StartSessionRequest, TuiLaunchRequest, VendorBinding,
    };
    use maestro_domain::{
        AgentKind, ErrorCode, IntegrationMode, ProjectId, RunId, SessionId, TerminalId,
    };
    use maestro_process::{ProcessError, ProcessSpawner};
    use maestro_protocol::{MIN_TERMINAL_POLL_BYTES, TerminalState};
    use maestro_storage::DatabaseKey;
    use tokio::sync::{Mutex, Notify};

    use super::{MAX_BUFFERED_TERMINAL_BYTES, OutputBuffer, TerminalManager, process_error};
    use crate::{DaemonPaths, storage_runtime::StorageRuntime};

    async fn wait_for_file(path: &Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {}", path.display());
    }

    async fn wait_for_terminal_exit(
        manager: &TerminalManager,
        terminal_id: maestro_domain::TerminalId,
    ) {
        for _ in 0..200 {
            if manager.status(terminal_id).await.expect("status").state == TerminalState::Exited {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("terminal did not exit");
    }

    async fn read_until(
        manager: &TerminalManager,
        terminal_id: TerminalId,
        mut cursor: u64,
        expected: &[u8],
    ) -> (u64, Vec<u8>) {
        let mut output = Vec::new();
        for _ in 0..100 {
            let read = manager
                .read(terminal_id, cursor, MIN_TERMINAL_POLL_BYTES, 250)
                .await
                .expect("fake TUI output reads");
            cursor = read.next_sequence;
            for chunk in read.chunks {
                output.extend(chunk.data);
            }
            if output
                .windows(expected.len())
                .any(|window| window == expected)
            {
                return (cursor, output);
            }
        }
        panic!(
            "timed out waiting for fake TUI output: {}",
            String::from_utf8_lossy(expected)
        );
    }

    #[test]
    fn output_overflow_advances_cursor_explicitly() {
        let mut output = OutputBuffer::default();
        for _ in 0..=(MAX_BUFFERED_TERMINAL_BYTES / 4096) {
            output.push(vec![b'x'; 4096]);
        }

        let poll = output.poll(0, 4096).expect("poll");
        assert!(poll.overflowed);
        assert!(poll.dropped_through_sequence.is_some());
        assert_eq!(poll.chunks.len(), 1);
        assert!(poll.next_sequence > 1);
    }

    #[test]
    fn frontend_process_errors_never_include_raw_internal_details() {
        let secret = "secret-token-and-private-path";
        let error = process_error(&ProcessError::Pty(secret.to_owned()));
        let serialized = serde_json::to_string(&error).expect("error serializes");

        assert!(!serialized.contains(secret));
        assert!(serialized.contains("pty"));
    }

    #[tokio::test]
    async fn natural_exit_releases_capacity_while_metadata_stays_bounded() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let spawner = ProcessSpawner::new(1);
        let manager = TerminalManager::new(spawner.clone(), 1, Arc::new(Notify::new()));

        for _ in 0..5 {
            let opened = manager
                .open(&temporary.path().to_string_lossy(), 80, 24)
                .await
                .expect("terminal opens after prior natural exit");
            manager
                .write(opened.terminal_id, b"exit 0\n")
                .await
                .expect("exit writes");

            for _ in 0..200 {
                if manager
                    .status(opened.terminal_id)
                    .await
                    .expect("status")
                    .state
                    == TerminalState::Exited
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(
                manager
                    .status(opened.terminal_id)
                    .await
                    .expect("status")
                    .state,
                TerminalState::Exited
            );
            assert_eq!(spawner.active_count(), 0);
            assert_eq!(manager.count(), 0);
            assert!(manager.terminals.read().await.len() <= 1);
        }
    }

    #[tokio::test]
    async fn project_terminal_flushes_encrypted_scrollback_while_process_is_running() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let storage = Arc::new(
            StorageRuntime::persistent_for_test(paths, DatabaseKey::generate())
                .expect("persistent test storage"),
        );
        let project_id = ProjectId::new();
        storage
            .upsert_project(
                &project_id.to_string(),
                "Terminal integration",
                &[temporary.path().to_string_lossy().into_owned()],
            )
            .expect("project persists");
        let manager = TerminalManager::with_storage(
            ProcessSpawner::new(1),
            1,
            Arc::new(Notify::new()),
            Arc::clone(&storage),
        );
        let opened = manager
            .open_for_project(project_id, &temporary.path().to_string_lossy(), 80, 24)
            .await
            .expect("terminal opens");
        manager
            .write(
                opened.terminal_id,
                b"printf 'persisted-terminal-secret-marker\\n'\n",
            )
            .await
            .expect("terminal command writes");

        let mut persisted = Vec::new();
        for _ in 0..100 {
            persisted = storage
                .terminal_segment_paths(opened.terminal_id)
                .expect("segment metadata reads");
            if !persisted.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            !persisted.is_empty(),
            "scrollback was not flushed while running"
        );
        let encrypted = std::fs::read(&persisted[0]).expect("encrypted segment reads");
        assert!(
            !encrypted
                .windows(b"persisted-terminal-secret-marker".len())
                .any(|window| window == b"persisted-terminal-secret-marker")
        );
        assert_eq!(
            manager
                .status(opened.terminal_id)
                .await
                .expect("terminal status")
                .state,
            TerminalState::Running
        );
        manager
            .close(opened.terminal_id)
            .await
            .expect("terminal closes");
    }

    #[tokio::test]
    async fn terminal_discovery_and_attach_are_bounded_and_project_scoped() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let storage = Arc::new(
            StorageRuntime::persistent_for_test(paths, DatabaseKey::generate())
                .expect("persistent test storage"),
        );
        let first_project = ProjectId::new();
        let second_project = ProjectId::new();
        for (project_id, name) in [(first_project, "First"), (second_project, "Second")] {
            storage
                .upsert_project(
                    &project_id.to_string(),
                    name,
                    &[temporary.path().to_string_lossy().into_owned()],
                )
                .expect("project persists");
        }
        let manager = TerminalManager::with_storage(
            ProcessSpawner::new(2),
            2,
            Arc::new(Notify::new()),
            storage,
        );
        let first = manager
            .open_for_project(first_project, &temporary.path().to_string_lossy(), 80, 24)
            .await
            .expect("first terminal opens");
        let second = manager
            .open_for_project(second_project, &temporary.path().to_string_lossy(), 80, 24)
            .await
            .expect("second terminal opens");

        let first_entries = manager
            .list_for_project(first_project, 1)
            .await
            .expect("first project lists");
        assert_eq!(first_entries.len(), 1);
        assert_eq!(first_entries[0].terminal.terminal_id, first.terminal_id);
        assert_eq!(first_entries[0].project_id, first_project);
        assert_eq!(
            manager
                .attach_shell_for_project(first_project, first.terminal_id)
                .await
                .expect("owner attaches"),
            first
        );
        let denied = manager
            .attach_shell_for_project(second_project, first.terminal_id)
            .await
            .expect_err("another project cannot attach");
        assert_eq!(denied.code, ErrorCode::PermissionDenied);
        assert!(manager.list_for_project(first_project, 0).await.is_err());

        manager
            .close(first.terminal_id)
            .await
            .expect("first closes");
        manager
            .close(second.terminal_id)
            .await
            .expect("second closes");
    }

    #[tokio::test]
    async fn idle_read_waits_without_polling_and_wakes_on_output() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = Arc::new(TerminalManager::new(
            ProcessSpawner::new(1),
            1,
            Arc::new(Notify::new()),
        ));
        let opened = manager
            .open(&temporary.path().to_string_lossy(), 80, 24)
            .await
            .expect("terminal opens");
        manager
            .write(opened.terminal_id, b"cat\n")
            .await
            .expect("cat starts");

        let mut cursor = 0;
        loop {
            let drained = manager
                .read(opened.terminal_id, cursor, MIN_TERMINAL_POLL_BYTES, 100)
                .await
                .expect("terminal drains");
            cursor = drained.next_sequence;
            if drained.chunks.is_empty() {
                break;
            }
        }

        let read_manager = Arc::clone(&manager);
        let started = std::time::Instant::now();
        let pending_read = tokio::spawn(async move {
            read_manager
                .read(opened.terminal_id, cursor, MIN_TERMINAL_POLL_BYTES, 2_000)
                .await
        });
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(
            !pending_read.is_finished(),
            "idle read returned before output"
        );

        manager
            .write(opened.terminal_id, b"maestro-wake\n")
            .await
            .expect("terminal input writes");
        let read = tokio::time::timeout(Duration::from_secs(1), pending_read)
            .await
            .expect("output wakes read before timeout")
            .expect("read task joins")
            .expect("terminal reads");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(read.chunks.iter().any(|chunk| {
            chunk
                .data
                .windows(b"maestro-wake".len())
                .any(|window| window == b"maestro-wake")
        }));

        manager
            .close(opened.terminal_id)
            .await
            .expect("terminal closes");
    }

    #[tokio::test]
    async fn concurrent_operations_are_fifo_and_close_is_race_safe() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = Arc::new(TerminalManager::new(
            ProcessSpawner::new(1),
            1,
            Arc::new(Notify::new()),
        ));
        let opened = manager
            .open(&temporary.path().to_string_lossy(), 80, 24)
            .await
            .expect("terminal opens");
        let session = manager
            .session(opened.terminal_id)
            .await
            .expect("session exists");
        let blocker = session.operation_guard.lock().await;
        let completions = Arc::new(Mutex::new(Vec::new()));

        let first_manager = Arc::clone(&manager);
        let first_completions = Arc::clone(&completions);
        let first = tokio::spawn(async move {
            first_manager
                .write(opened.terminal_id, b"printf 'first\\n'\n")
                .await
                .expect("first write");
            first_completions.lock().await.push("first");
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let second_manager = Arc::clone(&manager);
        let second_completions = Arc::clone(&completions);
        let second = tokio::spawn(async move {
            second_manager
                .write(opened.terminal_id, b"printf 'second\\n'\n")
                .await
                .expect("second write");
            second_completions.lock().await.push("second");
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        let close_manager = Arc::clone(&manager);
        let close_completions = Arc::clone(&completions);
        let close = tokio::spawn(async move {
            let status = close_manager
                .close(opened.terminal_id)
                .await
                .expect("close succeeds");
            close_completions.lock().await.push("close");
            status
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(blocker);

        first.await.expect("first task");
        second.await.expect("second task");
        let closed = close.await.expect("close task");
        assert_eq!(closed.state, TerminalState::Closed);
        assert_eq!(*completions.lock().await, ["first", "second", "close"]);
        assert!(matches!(
            manager.write(opened.terminal_id, b"late").await,
            Err(error) if error.code == ErrorCode::TerminalNotFound
        ));
    }

    #[tokio::test]
    async fn conclusive_reap_finalizes_capacity_even_after_a_prior_failure_state() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = TerminalManager::new(ProcessSpawner::new(1), 1, Arc::new(Notify::new()));
        let opened = manager
            .open(&temporary.path().to_string_lossy(), 80, 24)
            .await
            .expect("terminal opens");
        let session = manager
            .session(opened.terminal_id)
            .await
            .expect("session exists");
        assert!(session.note_termination_failure().await);
        session
            .process
            .write(b"exit 0\n")
            .await
            .expect("exit writes directly after simulated failure");

        wait_for_terminal_exit(&manager, opened.terminal_id).await;
        assert_eq!(manager.count(), 0);
        assert_eq!(manager.completed.lock().await.len(), 1);

        manager
            .close(opened.terminal_id)
            .await
            .expect("finalized terminal closes");
        assert!(manager.completed.lock().await.is_empty());
        assert!(manager.terminals.read().await.is_empty());
    }

    #[tokio::test]
    async fn natural_exit_then_close_does_not_accumulate_completed_queue_entries() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = TerminalManager::new(ProcessSpawner::new(1), 1, Arc::new(Notify::new()));

        for _ in 0..20 {
            let opened = manager
                .open(&temporary.path().to_string_lossy(), 80, 24)
                .await
                .expect("terminal opens");
            manager
                .write(opened.terminal_id, b"exit 0\n")
                .await
                .expect("exit writes");
            wait_for_terminal_exit(&manager, opened.terminal_id).await;
            manager
                .close(opened.terminal_id)
                .await
                .expect("terminal closes");

            assert!(manager.completed.lock().await.is_empty());
            assert!(manager.terminals.read().await.is_empty());
            assert_eq!(manager.count(), 0);
        }
    }

    #[tokio::test]
    async fn close_all_terminates_term_ignoring_terminals_concurrently() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = TerminalManager::new(ProcessSpawner::new(3), 3, Arc::new(Notify::new()));

        for index in 0..3 {
            let opened = manager
                .open(&temporary.path().to_string_lossy(), 80, 24)
                .await
                .expect("terminal opens");
            let ready = temporary.path().join(format!("terminal-{index}-ready"));
            let command = format!("trap '' TERM; : > '{}'\n", ready.display());
            manager
                .write(opened.terminal_id, command.as_bytes())
                .await
                .expect("trap command writes");
            wait_for_file(&ready).await;
        }

        let started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(4), manager.close_all())
            .await
            .expect("close_all uses one concurrent deadline");
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(manager.completed.lock().await.is_empty());
        assert!(manager.terminals.read().await.is_empty());
        assert_eq!(manager.count(), 0);
    }

    #[tokio::test]
    async fn adapter_tui_writer_lease_is_retained_until_pty_reap() {
        let Some(fake_agent) = std::env::var_os("MAESTRO_FAKE_AGENT") else {
            return;
        };
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = TerminalManager::new(ProcessSpawner::new(1), 1, Arc::new(Notify::new()));
        let adapter = FakeAdapter::new();
        let binding = VendorBinding {
            agent_kind: AgentKind::Fake,
            vendor_session_id: "daemon-retained-tui-binding".to_owned(),
        };
        let tui_session_id = SessionId::new();
        let tui_run_id = RunId::new();
        let plan = adapter
            .launch_tui(TuiLaunchRequest {
                start: StartSessionRequest {
                    session_id: tui_session_id,
                    run_id: tui_run_id,
                    project_id: ProjectId::new(),
                    executable: fake_agent.clone().into(),
                    working_directory: temporary.path().to_path_buf(),
                    integration_mode: IntegrationMode::PtyTui,
                    options: SessionOptions::default(),
                    capture_raw_protocol: false,
                },
                binding: Some(binding.clone()),
            })
            .await
            .expect("adapter creates exact-TUI plan");
        let opened = manager
            .open_tui_plan_for_project(
                ProjectId::new(),
                tui_run_id,
                plan,
                40,
                6,
                "Lease retention fixture",
            )
            .await
            .expect("daemon starts adapter TUI plan");
        assert_eq!(opened.run_id, tui_run_id);
        let (cursor, _) = read_until(
            &manager,
            opened.terminal_id,
            0,
            "Maestro fake TUI ✓".as_bytes(),
        )
        .await;

        let structured_session_id = SessionId::new();
        let structured_request = |run_id| ResumeSessionRequest {
            start: StartSessionRequest {
                session_id: structured_session_id,
                run_id,
                project_id: ProjectId::new(),
                executable: fake_agent.clone().into(),
                working_directory: temporary.path().to_path_buf(),
                integration_mode: IntegrationMode::Structured,
                options: SessionOptions::default(),
                capture_raw_protocol: false,
            },
            binding: binding.clone(),
        };
        let blocked = adapter
            .resume_session(structured_request(RunId::new()))
            .await
            .expect_err("live daemon TUI retains the vendor writer");
        assert_eq!(blocked.kind(), AdapterErrorKind::BindingInUse);

        manager
            .write(opened.terminal_id, b"release\n")
            .await
            .expect("TUI input reaches child");
        let _ = read_until(&manager, opened.terminal_id, cursor, b"echo: release").await;
        let completed = manager
            .wait_for_completion(opened.terminal_id)
            .await
            .expect("daemon observes conclusively reaped TUI");
        assert_eq!(completed.state, TerminalState::Exited);

        let resumed = adapter
            .resume_session(structured_request(RunId::new()))
            .await
            .expect("reaped TUI releases the vendor writer");
        resumed
            .stop_run(RunStopReason::UserRequested)
            .await
            .expect("structured fixture releases its writer");
        manager
            .close(opened.terminal_id)
            .await
            .expect("completed TUI closes");
    }

    #[tokio::test]
    async fn fake_tui_vt_alternate_and_hostile_osc_bytes_survive_the_real_pty() {
        const VT_PREFIX: &[u8] = b"\x1b[2J\x1b[H\x1b[32m";

        let Some(fake_agent) = std::env::var_os("MAESTRO_FAKE_AGENT") else {
            return;
        };
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = TerminalManager::new(ProcessSpawner::new(1), 1, Arc::new(Notify::new()));

        let baseline = manager
            .open_fake_tui(
                Path::new(&fake_agent),
                "tui/vt-baseline",
                &temporary.path().to_string_lossy(),
                40,
                6,
            )
            .await
            .expect("VT fixture starts");
        let (cursor, mut baseline_output) = read_until(
            &manager,
            baseline.terminal_id,
            0,
            "Maestro fake TUI ✓".as_bytes(),
        )
        .await;
        manager
            .write(baseline.terminal_id, b"hello\n")
            .await
            .expect("VT input writes");
        let (_, tail) = read_until(&manager, baseline.terminal_id, cursor, b"echo: hello").await;
        baseline_output.extend(tail);
        assert!(
            baseline_output
                .windows(VT_PREFIX.len())
                .any(|bytes| bytes == VT_PREFIX)
        );
        wait_for_terminal_exit(&manager, baseline.terminal_id).await;
        manager
            .close(baseline.terminal_id)
            .await
            .expect("VT fixture closes");

        let alternate = manager
            .open_fake_tui(
                Path::new(&fake_agent),
                "tui/alternate-screen",
                &temporary.path().to_string_lossy(),
                40,
                6,
            )
            .await
            .expect("alternate fixture starts");
        let (_, alternate_output) =
            read_until(&manager, alternate.terminal_id, 0, b"\x1b[?1049lmain-after").await;
        assert!(
            alternate_output
                .windows(8)
                .any(|bytes| bytes == b"\x1b[?1049h")
        );
        wait_for_terminal_exit(&manager, alternate.terminal_id).await;
        manager
            .close(alternate.terminal_id)
            .await
            .expect("alternate fixture closes");

        let hostile = manager
            .open_fake_tui(
                Path::new(&fake_agent),
                "tui/osc-security",
                &temporary.path().to_string_lossy(),
                80,
                4,
            )
            .await
            .expect("OSC fixture starts");
        let (_, hostile_output) =
            read_until(&manager, hostile.terminal_id, 0, b"\x1b]8;;\x07").await;
        assert!(hostile_output.windows(4).any(|bytes| bytes == b"\x1b]0;"));
        assert!(hostile_output.windows(5).any(|bytes| bytes == b"\x1b]52;"));
        assert!(hostile_output.windows(5).any(|bytes| bytes == b"\x1b]8;;"));
        wait_for_terminal_exit(&manager, hostile.terminal_id).await;
        manager
            .close(hostile.terminal_id)
            .await
            .expect("OSC fixture closes");
    }

    #[tokio::test]
    async fn fake_tui_resize_and_exact_sgr_mouse_bytes_reach_the_child() {
        const REPORTS: &[u8] = b"\x1b[<0;5;3M\x1b[<0;5;3m\x1b[<32;6;4M\x1b[<64;7;5M\x1b[<65;8;6M";

        let Some(fake_agent) = std::env::var_os("MAESTRO_FAKE_AGENT") else {
            return;
        };
        let temporary = tempfile::tempdir().expect("temporary directory");
        let manager = TerminalManager::new(ProcessSpawner::new(1), 1, Arc::new(Notify::new()));
        let opened = manager
            .open_fake_tui(
                Path::new(&fake_agent),
                "tui/resize-mouse",
                &temporary.path().to_string_lossy(),
                20,
                10,
            )
            .await
            .expect("mouse fixture starts");
        let (cursor, ready) = read_until(&manager, opened.terminal_id, 0, b"mouse-ready").await;
        assert!(ready.windows(8).any(|bytes| bytes == b"\x1b[?1003h"));
        assert!(ready.windows(8).any(|bytes| bytes == b"\x1b[?1006h"));
        manager
            .resize(opened.terminal_id, 120, 40)
            .await
            .expect("real PTY resizes");
        manager
            .write(opened.terminal_id, REPORTS)
            .await
            .expect("mouse reports write");
        let (_, response) = read_until(
            &manager,
            opened.terminal_id,
            cursor,
            b"\x1b[?1006l\x1b[?1003l",
        )
        .await;
        assert!(response.windows(11).any(|bytes| bytes == b"size:40 120"));
        let mouse = response
            .windows(b"mouse:".len())
            .position(|bytes| bytes == b"mouse:")
            .map(|position| position + b"mouse:".len())
            .expect("fixture echoes mouse reports");
        assert_eq!(&response[mouse..mouse + REPORTS.len()], REPORTS);
        wait_for_terminal_exit(&manager, opened.terminal_id).await;
        manager
            .close(opened.terminal_id)
            .await
            .expect("mouse fixture closes");
    }
}
