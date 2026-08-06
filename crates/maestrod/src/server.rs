use std::{
    fs, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Duration,
};

use maestro_adapter::{ProcessLaunchSpec, ProcessTransport, TuiLaunchPlan};
use maestro_domain::{
    ErrorCode, EventEnvelope, EventSource, IntegrationMode, MaestroError, NormalizedEvent,
    ProjectId, RunId, SessionId, SessionState, TerminalId,
};
use maestro_process::{ExitCause, ProcessSpawner};
use maestro_protocol::{
    ClientFrame, MAX_FAKE_EVENT_VOLUME, MAX_HELLO_FRAME_BYTES, MAX_SESSION_ACTION_BYTES,
    MAX_SESSION_EVENT_WAIT_MILLISECONDS, MAX_SESSION_EVENTS_PER_READ, MAX_SESSION_INDEX_ENTRIES,
    MAX_SESSION_RAW_READ_BYTES, MAX_SETTING_KEY_BYTES, MAX_SETTING_SCOPE_BYTES,
    MAX_SETTING_SCOPE_REFERENCE_BYTES, MAX_SETTING_VALUE_BYTES, PROTOCOL_VERSION,
    ProjectWindowLayout, RecentProject, Request, Response, ResponseEnvelope, SensitiveBytes,
    SensitiveString, ServerFrame, ServerHello, SessionErrorSummary, SessionEventBatch, SessionExit,
    SessionIndexEntry, SessionPermissionDecision, SessionRawBatch, SessionReplayGap,
    SessionRunAttached, SessionRunStarted, SessionSnapshot, SessionTerminalAttached,
    SessionTerminalStarted, StorageStatus, SystemSnapshot, TerminalState, TerminalStatus,
};
use maestro_redaction::redact_text;
use maestro_storage::PersistedRunExit;
use serde_json::json;
use thiserror::Error;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{Notify, OwnedSemaphorePermit, RwLock, Semaphore, watch},
    task::JoinSet,
    time::{Instant, MissedTickBehavior, interval, sleep_until, timeout},
};
use tracing::{debug, info, warn};

use crate::{
    DaemonPaths, IpcError, SecretToken,
    fake_session::{
        FakeSessionLimits, FakeSessionSupervisor, FakeSubscriptionError, PermissionDecision,
    },
    ipc::{read_frame, read_frame_with_limit, write_frame},
    project::ProjectManager,
    storage_runtime::{StorageRuntime, StorageRuntimeError},
    terminal::TerminalManager,
};

#[cfg(not(test))]
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const HELLO_TIMEOUT: Duration = Duration::from_millis(100);
const STORAGE_MAINTENANCE_INTERVAL: Duration = Duration::from_hours(1);
const MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION: usize = 64;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub maximum_connections: usize,
    pub maximum_processes: usize,
    pub maximum_terminals: usize,
    pub idle_shutdown_grace: Duration,
    pub fake_agent_executable: Option<PathBuf>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            maximum_connections: 32,
            maximum_processes: maestro_process::DEFAULT_PROCESS_LIMIT,
            maximum_terminals: 16,
            idle_shutdown_grace: Duration::from_secs(30),
            fake_agent_executable: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("another maestrod instance is already listening")]
    AlreadyRunning,
    #[error("could not determine the per-user {0}")]
    PathUnavailable(&'static str),
    #[error("the daemon authentication token file is invalid")]
    InvalidTokenFile,
    #[error("daemon configuration is invalid: {0}")]
    InvalidConfiguration(&'static str),
    #[error("encrypted daemon storage failed to initialize")]
    StorageInitialization,
    #[error("daemon I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("daemon IPC failed: {0}")]
    Ipc(#[from] IpcError),
}

#[derive(Debug)]
struct DaemonState {
    active_sessions: Arc<AtomicU32>,
    connected_clients: AtomicU32,
    shutting_down: AtomicBool,
    shutdown: watch::Sender<bool>,
    activity_changed: Arc<Notify>,
    process_spawner: ProcessSpawner,
    sessions: Arc<FakeSessionSupervisor>,
    fake_agent_executable: PathBuf,
    projects: Arc<ProjectManager>,
    storage: Arc<StorageRuntime>,
    terminals: Arc<TerminalManager>,
    tui_sessions: Arc<RwLock<std::collections::HashMap<SessionId, TerminalId>>>,
    #[cfg(test)]
    retained_connection_tasks: std::sync::atomic::AtomicUsize,
}

impl DaemonState {
    fn new(
        maximum_processes: usize,
        maximum_terminals: usize,
        storage: StorageRuntime,
        fake_agent_executable: PathBuf,
    ) -> Self {
        let (shutdown, _receiver) = watch::channel(false);
        let activity_changed = Arc::new(Notify::new());
        let process_spawner = ProcessSpawner::new(maximum_processes);
        let storage = Arc::new(storage);
        Self {
            active_sessions: Arc::new(AtomicU32::new(0)),
            connected_clients: AtomicU32::new(0),
            shutting_down: AtomicBool::new(false),
            shutdown,
            activity_changed: Arc::clone(&activity_changed),
            sessions: Arc::new(
                FakeSessionSupervisor::new(process_spawner.clone(), FakeSessionLimits::default())
                    .expect("default fake-session limits must be valid"),
            ),
            fake_agent_executable,
            projects: Arc::new(ProjectManager::default()),
            storage: Arc::clone(&storage),
            terminals: Arc::new(TerminalManager::with_storage(
                process_spawner.clone(),
                maximum_terminals,
                activity_changed,
                storage,
            )),
            tui_sessions: Arc::new(RwLock::new(std::collections::HashMap::new())),
            process_spawner,
            #[cfg(test)]
            retained_connection_tasks: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn request_shutdown(&self) {
        if !self.shutting_down.swap(true, Ordering::SeqCst) {
            self.shutdown.send_replace(true);
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShutdownHandle {
    state: Arc<DaemonState>,
}

impl ShutdownHandle {
    pub fn request(&self) {
        self.state.request_shutdown();
    }
}

#[derive(Debug)]
pub struct DaemonServer {
    paths: DaemonPaths,
    token: SecretToken,
    listener: UnixListener,
    config: DaemonConfig,
    state: Arc<DaemonState>,
    connections: Arc<Semaphore>,
}

impl DaemonServer {
    /// Claims the per-user socket and initializes daemon-owned resources.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid limits, unsafe runtime paths, token
    /// failures, or when another daemon already owns the socket.
    pub async fn bind(paths: DaemonPaths, config: DaemonConfig) -> Result<Self, DaemonError> {
        if config.maximum_connections == 0 {
            return Err(DaemonError::InvalidConfiguration(
                "connection limit must be nonzero",
            ));
        }
        if config.maximum_processes == 0 {
            return Err(DaemonError::InvalidConfiguration(
                "process limit must be nonzero",
            ));
        }
        if config.maximum_terminals == 0 || config.maximum_terminals > config.maximum_processes {
            return Err(DaemonError::InvalidConfiguration(
                "terminal limit must be nonzero and no greater than the process limit",
            ));
        }
        paths.prepare()?;
        let storage_paths = paths.clone();
        let storage =
            tokio::task::spawn_blocking(move || StorageRuntime::initialize(storage_paths))
                .await
                .map_err(|_| DaemonError::StorageInitialization)?
                .map_err(|_| DaemonError::StorageInitialization)?;
        let token = paths.load_or_create_token()?;
        let listener = bind_single_instance(&paths).await?;
        restrict_socket(&paths.socket)?;
        let fake_agent_executable = match config.fake_agent_executable.clone() {
            Some(path) => path,
            None => std::env::current_exe()?.with_file_name("maestro-fake-agent"),
        };
        Ok(Self {
            paths,
            token,
            state: Arc::new(DaemonState::new(
                config.maximum_processes,
                config.maximum_terminals,
                storage,
                fake_agent_executable,
            )),
            connections: Arc::new(Semaphore::new(config.maximum_connections)),
            config,
            listener,
        })
    }

    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            state: Arc::clone(&self.state),
        }
    }

    pub fn paths(&self) -> &DaemonPaths {
        &self.paths
    }

    pub fn process_spawner(&self) -> ProcessSpawner {
        self.state.process_spawner.clone()
    }

    /// Runs the accept loop until explicit shutdown or the idle grace expires.
    ///
    /// # Errors
    ///
    /// Returns an error when accepting connections or cleaning the owned socket
    /// fails.
    pub async fn run(self) -> Result<(), DaemonError> {
        let mut tasks = JoinSet::new();
        let mut idle_since = Some(Instant::now());
        let mut shutdown = self.state.shutdown.subscribe();
        let maintenance = tokio::spawn(run_storage_maintenance(
            Arc::clone(&self.state.storage),
            self.state.shutdown.subscribe(),
        ));
        info!(socket = %self.paths.socket.display(), "maestrod is accepting local connections");

        loop {
            let idle_deadline = idle_since.map(|start| start + self.config.idle_shutdown_grace);
            tokio::select! {
                result = shutdown.changed() => {
                    if result.is_err() || *shutdown.borrow() {
                        break;
                    }
                },
                () = self.state.activity_changed.notified() => {
                    idle_since = if self.is_idle() {
                        Some(Instant::now())
                    } else {
                        None
                    };
                },
                () = wait_for_deadline(idle_deadline), if idle_deadline.is_some() => {
                    if self.is_idle() {
                        info!("idle grace period elapsed");
                        break;
                    }
                    idle_since = None;
                },
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result) = joined {
                        record_connection_task_reaped(&self.state);
                        if let Err(error) = result {
                            warn!(%error, "connection task failed");
                        }
                    }
                },
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted?;
                    if let Ok(permit) = Arc::clone(&self.connections).try_acquire_owned() {
                        let token = self.token.clone();
                        let state = Arc::clone(&self.state);
                        state.connected_clients.fetch_add(1, Ordering::SeqCst);
                        state.activity_changed.notify_one();
                        record_connection_task_spawned(&self.state);
                        tasks.spawn(async move {
                            let _client_guard = ClientGuard(Arc::clone(&state));
                            if let Err(error) = serve_connection(stream, token, state, permit).await {
                                debug!(%error, "local client disconnected with an error");
                            }
                        });
                    } else {
                        record_connection_task_spawned(&self.state);
                        tasks.spawn(reject_capacity(stream));
                    }
                }
            }
        }

        self.state.shutting_down.store(true, Ordering::SeqCst);
        self.state.shutdown.send_replace(true);
        if let Err(error) = maintenance.await
            && !error.is_cancelled()
        {
            warn!(%error, "storage maintenance task failed during shutdown");
        }
        self.state.terminals.close_all().await;
        let grace = self.config.idle_shutdown_grace;
        if timeout(grace, async {
            while let Some(result) = tasks.join_next().await {
                record_connection_task_reaped(&self.state);
                if let Err(error) = result {
                    warn!(%error, "connection task failed during shutdown");
                }
            }
        })
        .await
        .is_err()
        {
            warn!("client connection grace period expired; aborting connection tasks");
            tasks.abort_all();
            while let Some(result) = tasks.join_next().await {
                record_connection_task_reaped(&self.state);
                if let Err(error) = result
                    && !error.is_cancelled()
                {
                    warn!(%error, "aborted connection task failed");
                }
            }
        }
        remove_owned_socket(&self.paths.socket)?;
        info!("maestrod stopped cleanly");
        Ok(())
    }

    fn is_idle(&self) -> bool {
        self.state.connected_clients.load(Ordering::SeqCst) == 0
            && self.state.active_sessions.load(Ordering::SeqCst) == 0
            && self.state.process_spawner.active_count() == 0
    }
}

async fn run_storage_maintenance(
    storage: Arc<StorageRuntime>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut schedule = interval(STORAGE_MAINTENANCE_INTERVAL);
    schedule.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            _ = schedule.tick() => {
                let storage = Arc::clone(&storage);
                match tokio::task::spawn_blocking(move || storage.maintain()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => warn!(%error, "encrypted storage maintenance failed"),
                    Err(error) => warn!(%error, "encrypted storage maintenance task failed"),
                }
            }
        }
    }
}

#[cfg(test)]
fn record_connection_task_spawned(state: &DaemonState) {
    state
        .retained_connection_tasks
        .fetch_add(1, Ordering::SeqCst);
}

#[cfg(not(test))]
fn record_connection_task_spawned(_state: &DaemonState) {}

#[cfg(test)]
fn record_connection_task_reaped(state: &DaemonState) {
    state
        .retained_connection_tasks
        .fetch_sub(1, Ordering::SeqCst);
}

#[cfg(not(test))]
fn record_connection_task_reaped(_state: &DaemonState) {}

async fn wait_for_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn bind_single_instance(paths: &DaemonPaths) -> Result<UnixListener, DaemonError> {
    match UnixListener::bind(&paths.socket) {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(&paths.socket).await.is_ok() {
                Err(DaemonError::AlreadyRunning)
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    let file_type = fs::symlink_metadata(&paths.socket)?.file_type();
                    if !file_type.is_socket() {
                        return Err(DaemonError::Io(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "refusing to replace a non-socket daemon path",
                        )));
                    }
                }
                fs::remove_file(&paths.socket)?;
                UnixListener::bind(&paths.socket).map_err(DaemonError::from)
            }
        }
        Err(error) => Err(error.into()),
    }
}

async fn reject_capacity(mut stream: UnixStream) {
    let mut error = MaestroError::new(ErrorCode::DaemonLocked, "daemon connection limit reached");
    error.retryable = true;
    let _ = write_frame(&mut stream, &ServerFrame::Fatal(error)).await;
}

async fn serve_connection(
    mut stream: UnixStream,
    token: SecretToken,
    state: Arc<DaemonState>,
    _permit: OwnedSemaphorePermit,
) -> Result<(), IpcError> {
    if !authenticate_connection(&mut stream, &token).await? {
        return Ok(());
    }

    let mut shutdown = state.shutdown.subscribe();
    let mut requests = JoinSet::new();
    while !state.shutting_down.load(Ordering::SeqCst) {
        tokio::select! {
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
            },
            joined = requests.join_next(), if !requests.is_empty() => {
                match joined {
                    Some(Ok(response)) => {
                        write_frame(&mut stream, &ServerFrame::Response(response)).await?;
                    }
                    Some(Err(error)) => {
                        warn!(%error, "correlated request task failed");
                    }
                    None => {}
                }
            },
            frame = read_frame::<ClientFrame, _>(&mut stream) => {
                let Some(frame) = frame? else {
                    requests.detach_all();
                    break;
                };
                let ClientFrame::Request(envelope) = frame else {
                    let error = MaestroError::new(
                        ErrorCode::InvalidRequest,
                        "system.hello was already completed",
                    );
                    write_frame(&mut stream, &ServerFrame::Fatal(error)).await?;
                    requests.detach_all();
                    break;
                };
                if requests.len() >= MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION {
                    let mut error = MaestroError::new(
                        ErrorCode::InvalidRequest,
                        "this client has too many requests in flight",
                    );
                    error.retryable = true;
                    error.details = Some(json!({
                        "maximum_in_flight_requests": MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION,
                    }));
                    write_frame(
                        &mut stream,
                        &ServerFrame::Response(ResponseEnvelope {
                            request_id: envelope.request_id,
                            response: Err(error),
                        }),
                    )
                    .await?;
                    continue;
                }
                let request_state = Arc::clone(&state);
                requests.spawn(async move {
                    let response = handle_request(&envelope.request, &request_state).await;
                    ResponseEnvelope {
                        request_id: envelope.request_id,
                        response,
                    }
                });
            },
        }
    }
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    Ok(())
}

async fn authenticate_connection(
    stream: &mut UnixStream,
    token: &SecretToken,
) -> Result<bool, IpcError> {
    let first = timeout(
        HELLO_TIMEOUT,
        read_frame_with_limit::<ClientFrame, _>(stream, MAX_HELLO_FRAME_BYTES),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "client hello timed out"))??;
    let Some(ClientFrame::Hello(hello)) = first else {
        let error = MaestroError::new(
            ErrorCode::AuthenticationRequired,
            "system.hello must be the first frame",
        );
        write_frame(stream, &ServerFrame::Fatal(error)).await?;
        return Ok(false);
    };

    if !token.matches(hello.authentication_token.expose()) {
        let error = MaestroError::new(
            ErrorCode::AuthenticationRequired,
            "daemon authentication failed",
        );
        write_frame(stream, &ServerFrame::Fatal(error)).await?;
        return Ok(false);
    }
    if hello.protocol_version != PROTOCOL_VERSION {
        let mut error = MaestroError::new(
            ErrorCode::CliProtocolIncompatible,
            "the client and daemon protocol versions are incompatible",
        );
        error.user_action = Some("UPDATE_MAESTRO".to_owned());
        error.details = Some(json!({
            "client_protocol_version": hello.protocol_version,
            "daemon_protocol_version": PROTOCOL_VERSION,
        }));
        write_frame(stream, &ServerFrame::Fatal(error)).await?;
        return Ok(false);
    }

    write_frame(
        stream,
        &ServerFrame::Hello(ServerHello {
            protocol_version: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            connection_id: uuid::Uuid::new_v4(),
        }),
    )
    .await?;
    Ok(true)
}

#[expect(
    clippy::too_many_lines,
    reason = "the top-level protocol dispatcher keeps storage and terminal routing explicit"
)]
async fn handle_request(request: &Request, state: &DaemonState) -> Result<Response, MaestroError> {
    let (storage_status, storage_schema_version) = state.storage.snapshot();
    if storage_status.is_locked()
        && !matches!(
            request,
            Request::Ping
                | Request::SystemSnapshot
                | Request::StopAllWork
                | Request::StorageUnlock { .. }
        )
    {
        return Err(locked_storage_error(storage_status));
    }
    if is_session_request(request) {
        return handle_session_request(request, state).await;
    }
    if is_project_request(request) {
        return handle_project_request(
            request,
            Arc::clone(&state.projects),
            Arc::clone(&state.storage),
        )
        .await;
    }

    match request {
        Request::Ping => Ok(Response::Pong),
        Request::SystemSnapshot => Ok(Response::SystemSnapshot(SystemSnapshot {
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            locked: storage_status.is_locked(),
            storage: storage_status,
            storage_schema_version,
            active_sessions: state.active_sessions.load(Ordering::SeqCst),
            active_terminals: u32::try_from(state.terminals.count()).unwrap_or(u32::MAX),
            installed_agents: Vec::new(),
        })),
        Request::StopAllWork => {
            let terminals_closed = u32::try_from(state.terminals.count()).unwrap_or(u32::MAX);
            let (structured_sessions_stopped, ()) =
                tokio::join!(state.sessions.stop_all(), state.terminals.close_all());
            Ok(Response::BackgroundWorkStopped {
                structured_sessions_stopped: u32::try_from(structured_sessions_stopped)
                    .unwrap_or(u32::MAX),
                terminals_closed,
            })
        }
        Request::StorageUnlock { passphrase } => {
            let storage = Arc::clone(&state.storage);
            let passphrase = passphrase.clone().into_inner();
            tokio::task::spawn_blocking(move || storage.unlock(passphrase))
                .await
                .map_err(|_| database_unavailable_error())?
                .map_err(|error| storage_unlock_error(&error))?;
            Ok(Response::StorageUnlocked)
        }
        Request::SettingLoad {
            scope,
            scope_reference,
            key,
        } => {
            validate_setting_identity(scope, scope_reference, key)?;
            let storage = Arc::clone(&state.storage);
            let scope = scope.clone();
            let scope_reference = scope_reference.clone();
            let key = key.clone();
            let value_json = tokio::task::spawn_blocking({
                let scope = scope.clone();
                let scope_reference = scope_reference.clone();
                let key = key.clone();
                move || storage.setting(&scope, &scope_reference, &key)
            })
            .await
            .map_err(|_| database_unavailable_error())?
            .map_err(|_| database_unavailable_error())?;
            Ok(Response::SettingValue {
                scope,
                scope_reference,
                key,
                value_json: value_json.map(SensitiveString::new),
            })
        }
        Request::SettingSave {
            scope,
            scope_reference,
            key,
            value_json,
        } => {
            validate_setting_identity(scope, scope_reference, key)?;
            let value_json = value_json.clone().into_inner();
            validate_setting_json(&value_json)?;
            let storage = Arc::clone(&state.storage);
            let scope = scope.clone();
            let scope_reference = scope_reference.clone();
            let key = key.clone();
            tokio::task::spawn_blocking({
                let scope = scope.clone();
                let scope_reference = scope_reference.clone();
                let key = key.clone();
                move || storage.save_setting(&scope, &scope_reference, &key, &value_json)
            })
            .await
            .map_err(|_| database_unavailable_error())?
            .map_err(|_| database_unavailable_error())?;
            Ok(Response::SettingSaved {
                scope,
                scope_reference,
                key,
            })
        }
        Request::ProjectRegister { .. }
        | Request::ProjectRecentList { .. }
        | Request::ProjectSetFavorite { .. }
        | Request::ProjectWindowLayoutLoad { .. }
        | Request::ProjectWindowLayoutSave { .. }
        | Request::ProjectDirectoryList { .. }
        | Request::ProjectFileRead { .. }
        | Request::ProjectFileSave { .. }
        | Request::ProjectSearch { .. }
        | Request::ProjectSearchCancel { .. }
        | Request::ProjectGitStatus { .. }
        | Request::ProjectGitBranch { .. }
        | Request::ProjectGitDiff { .. }
        | Request::ProjectGitWorktrees { .. } => unreachable!("project requests handled above"),
        Request::FakeSessionStart { .. }
        | Request::FakeSessionResume { .. }
        | Request::FakeTuiStart { .. }
        | Request::SessionTerminalAttach { .. }
        | Request::SessionStructuredAttach { .. }
        | Request::SessionList { .. }
        | Request::SessionSnapshot { .. }
        | Request::SessionEventsRead { .. }
        | Request::SessionRawRead { .. }
        | Request::SessionPermissionRespond { .. }
        | Request::SessionUserInputRespond { .. }
        | Request::SessionGuiAction { .. }
        | Request::SubscribeSession { .. }
        | Request::UnsubscribeSession { .. }
        | Request::StopSession { .. } => unreachable!("session requests handled above"),
        Request::TerminalOpen {
            project_id,
            cwd,
            columns,
            rows,
        } => {
            let canonical = state.projects.terminal_cwd(*project_id, cwd)?;
            state
                .terminals
                .open_for_project(
                    *project_id,
                    canonical.to_str().ok_or_else(|| {
                        MaestroError::new(
                            ErrorCode::InvalidPath,
                            "terminal working directory cannot be represented safely",
                        )
                    })?,
                    *columns,
                    *rows,
                )
                .await
                .map(Response::TerminalOpened)
        }
        Request::TerminalList {
            project_id,
            maximum_terminals,
        } => {
            state.projects.primary_root(*project_id)?;
            state
                .terminals
                .list_for_project(*project_id, *maximum_terminals)
                .await
                .map(Response::TerminalList)
        }
        Request::TerminalAttach {
            project_id,
            terminal_id,
        } => {
            state.projects.primary_root(*project_id)?;
            state
                .terminals
                .attach_shell_for_project(*project_id, *terminal_id)
                .await
                .map(Response::TerminalAttached)
        }
        Request::TerminalWrite { terminal_id, data } => {
            state.terminals.write(*terminal_id, data).await?;
            Ok(Response::TerminalWriteAccepted {
                terminal_id: *terminal_id,
            })
        }
        Request::TerminalResize {
            terminal_id,
            columns,
            rows,
        } => {
            state
                .terminals
                .resize(*terminal_id, *columns, *rows)
                .await?;
            Ok(Response::TerminalResized {
                terminal_id: *terminal_id,
            })
        }
        Request::TerminalRead {
            terminal_id,
            after_sequence,
            maximum_bytes,
            wait_milliseconds,
        } => state
            .terminals
            .read(
                *terminal_id,
                *after_sequence,
                *maximum_bytes,
                *wait_milliseconds,
            )
            .await
            .map(Response::TerminalRead),
        Request::TerminalState { terminal_id } => state
            .terminals
            .status(*terminal_id)
            .await
            .map(Response::TerminalState),
        Request::TerminalClose { terminal_id } => state
            .terminals
            .close(*terminal_id)
            .await
            .map(Response::TerminalClosed),
    }
}

fn validate_setting_identity(
    scope: &str,
    scope_reference: &str,
    key: &str,
) -> Result<(), MaestroError> {
    if scope.is_empty() || scope.len() > MAX_SETTING_SCOPE_BYTES {
        return Err(invalid_setting_request(format!(
            "Setting scopes must contain between 1 and {MAX_SETTING_SCOPE_BYTES} bytes."
        )));
    }
    if scope_reference.len() > MAX_SETTING_SCOPE_REFERENCE_BYTES {
        return Err(invalid_setting_request(format!(
            "Setting scope references cannot exceed {MAX_SETTING_SCOPE_REFERENCE_BYTES} bytes."
        )));
    }
    if key.is_empty() || key.len() > MAX_SETTING_KEY_BYTES {
        return Err(invalid_setting_request(format!(
            "Setting keys must contain between 1 and {MAX_SETTING_KEY_BYTES} bytes."
        )));
    }
    Ok(())
}

fn validate_setting_json(value_json: &str) -> Result<(), MaestroError> {
    if value_json.len() > MAX_SETTING_VALUE_BYTES {
        return Err(invalid_setting_request(format!(
            "Setting values cannot exceed {MAX_SETTING_VALUE_BYTES} bytes."
        )));
    }
    serde_json::from_str::<serde_json::Value>(value_json)
        .map(|_| ())
        .map_err(|_| invalid_setting_request("Setting values must contain valid JSON."))
}

fn invalid_setting_request(message: impl Into<String>) -> MaestroError {
    MaestroError::new(ErrorCode::InvalidRequest, message)
}

fn is_session_request(request: &Request) -> bool {
    matches!(
        request,
        Request::FakeSessionStart { .. }
            | Request::FakeSessionResume { .. }
            | Request::FakeTuiStart { .. }
            | Request::SessionTerminalAttach { .. }
            | Request::SessionStructuredAttach { .. }
            | Request::SessionList { .. }
            | Request::SessionSnapshot { .. }
            | Request::SessionEventsRead { .. }
            | Request::SessionRawRead { .. }
            | Request::SessionPermissionRespond { .. }
            | Request::SessionUserInputRespond { .. }
            | Request::SessionGuiAction { .. }
            | Request::SubscribeSession { .. }
            | Request::UnsubscribeSession { .. }
            | Request::StopSession { .. }
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "typed session protocol variants are mapped explicitly at one IPC boundary"
)]
async fn handle_session_request(
    request: &Request,
    state: &DaemonState,
) -> Result<Response, MaestroError> {
    match request {
        Request::FakeSessionStart {
            project_id,
            scenario,
            binding,
            volume,
            capture_raw_protocol,
        } => {
            validate_fake_session_request(scenario, *volume)?;
            let cwd = state.projects.primary_root(*project_id)?;
            let run = state
                .sessions
                .start_with_volume_and_raw_capture(
                    state.fake_agent_executable.clone(),
                    scenario.clone(),
                    cwd,
                    binding.clone(),
                    *volume,
                    *capture_raw_protocol,
                )
                .await?;
            let invocation = json!({
                "fixture": "maestro-fake-agent",
                "scenario": scenario,
                "binding": binding,
                "volume": volume,
                "raw_protocol_capture": capture_raw_protocol,
            });
            if let Err(error) = persist_started_session(
                Arc::clone(&state.storage),
                *project_id,
                run.session_id,
                run.run_id,
                run.process_id,
                format!("Fake · {scenario}"),
                invocation,
            )
            .await
            {
                let _ = state.sessions.stop(run.session_id, run.run_id).await;
                return Err(storage_session_error(&error));
            }
            start_session_monitor(state, run.session_id, run.run_id, 0);
            Ok(Response::SessionRunStarted(SessionRunStarted {
                session_id: run.session_id,
                run_id: run.run_id,
                process_id: run.process_id,
            }))
        }
        Request::FakeSessionResume {
            session_id,
            project_id,
            scenario,
            binding,
            capture_raw_protocol,
        } => {
            validate_fake_session_request(scenario, None)?;
            let after_sequence = state.sessions.snapshot(*session_id).await?.latest_sequence;
            let cwd = state.projects.primary_root(*project_id)?;
            let run = state
                .sessions
                .resume_with_raw_capture(
                    *session_id,
                    state.fake_agent_executable.clone(),
                    scenario.clone(),
                    cwd,
                    binding.clone(),
                    *capture_raw_protocol,
                )
                .await?;
            let invocation = json!({
                "fixture": "maestro-fake-agent",
                "scenario": scenario,
                "binding": binding,
                "resume": true,
                "raw_protocol_capture": capture_raw_protocol,
            });
            if let Err(error) = persist_started_session(
                Arc::clone(&state.storage),
                *project_id,
                run.session_id,
                run.run_id,
                run.process_id,
                format!("Fake · {scenario}"),
                invocation,
            )
            .await
            {
                let _ = state.sessions.stop(run.session_id, run.run_id).await;
                return Err(storage_session_error(&error));
            }
            start_session_monitor(state, run.session_id, run.run_id, after_sequence);
            Ok(Response::SessionRunStarted(SessionRunStarted {
                session_id: run.session_id,
                run_id: run.run_id,
                process_id: run.process_id,
            }))
        }
        Request::FakeTuiStart {
            project_id,
            scenario,
            columns,
            rows,
        } => {
            validate_fake_tui_request(scenario)?;
            let cwd = state.projects.primary_root(*project_id)?;
            let session_id = SessionId::new();
            let run_id = RunId::new();
            let plan = TuiLaunchPlan::new(
                ProcessLaunchSpec {
                    executable: state.fake_agent_executable.clone(),
                    arguments: vec!["--scenario".into(), scenario.as_str().into()],
                    working_directory: cwd.clone(),
                    transport: ProcessTransport::Pty,
                    requested_environment_variables: Vec::new(),
                },
                None,
            );
            let terminal = state
                .terminals
                .open_tui_plan_for_project(
                    *project_id,
                    run_id,
                    plan,
                    *columns,
                    *rows,
                    &format!("Fake TUI · {scenario}"),
                )
                .await?;
            let invocation = json!({
                "fixture": "maestro-fake-agent",
                "scenario": scenario,
                "terminal_id": terminal.terminal_id,
            });
            if let Err(error) = persist_started_tui_session(
                Arc::clone(&state.storage),
                *project_id,
                session_id,
                terminal.clone(),
                format!("Fake TUI · {scenario}"),
                invocation,
            )
            .await
            {
                let _ = state.terminals.close(terminal.terminal_id).await;
                return Err(storage_session_error(&error));
            }
            state
                .tui_sessions
                .write()
                .await
                .insert(session_id, terminal.terminal_id);
            start_tui_session_monitor(state, session_id, &terminal);
            Ok(Response::SessionTerminalStarted(SessionTerminalStarted {
                session_id,
                terminal,
            }))
        }
        Request::SessionTerminalAttach {
            session_id,
            project_id,
        } => {
            state.projects.primary_root(*project_id)?;
            require_session_ownership(
                Arc::clone(&state.storage),
                *session_id,
                *project_id,
                IntegrationMode::PtyTui,
            )
            .await?;
            let terminal_id = state
                .tui_sessions
                .read()
                .await
                .get(session_id)
                .copied()
                .ok_or_else(|| {
                    MaestroError::new(
                        ErrorCode::TerminalNotRunning,
                        "The exact TUI session is not currently running.",
                    )
                })?;
            let terminal = state
                .terminals
                .attach_tui_for_project(*project_id, terminal_id)
                .await?;
            Ok(Response::SessionTerminalAttached(SessionTerminalAttached {
                session_id: *session_id,
                terminal,
            }))
        }
        Request::SessionStructuredAttach {
            session_id,
            project_id,
        } => {
            state.projects.primary_root(*project_id)?;
            require_session_ownership(
                Arc::clone(&state.storage),
                *session_id,
                *project_id,
                IntegrationMode::Structured,
            )
            .await?;
            let run = state.sessions.attach_active(*session_id).await?;
            Ok(Response::SessionRunAttached(SessionRunAttached {
                session_id: run.session_id,
                run_id: run.run_id,
                process_id: run.process_id,
            }))
        }
        Request::SessionList {
            project_id,
            maximum_sessions,
        } => {
            if *maximum_sessions == 0 || *maximum_sessions > MAX_SESSION_INDEX_ENTRIES {
                return Err(invalid_session_request(
                    "The session index limit is outside its supported range.",
                ));
            }
            // Require the project to be registered in this daemon before its
            // durable session index is exposed to the connection.
            state.projects.primary_root(*project_id)?;
            let storage = Arc::clone(&state.storage);
            let project_id = *project_id;
            let maximum_sessions = *maximum_sessions;
            let sessions = tokio::task::spawn_blocking(move || {
                storage.persisted_sessions(project_id, maximum_sessions)
            })
            .await
            .map_err(|_| database_unavailable_error())?
            .map_err(|error| storage_session_error(&error))?;
            Ok(Response::SessionList(
                sessions
                    .into_iter()
                    .map(|session| SessionIndexEntry {
                        session_id: session.session_id,
                        project_id: session.project_id,
                        agent_kind: session.agent_kind,
                        integration_mode: session.integration_mode,
                        state: session.state,
                        title: session.title,
                        active_run_id: session.active_run_id,
                        latest_sequence: session.latest_sequence,
                        updated_at: session.updated_at.to_rfc3339(),
                    })
                    .collect(),
            ))
        }
        Request::SessionSnapshot { session_id } => {
            match state.sessions.snapshot(*session_id).await {
                Ok(snapshot) => Ok(Response::SessionSnapshot(protocol_session_snapshot(
                    snapshot,
                ))),
                Err(error) if error.code == ErrorCode::SessionNotFound => {
                    persisted_session_snapshot(Arc::clone(&state.storage), *session_id)
                        .await
                        .map(Response::SessionSnapshot)
                }
                Err(error) => Err(error),
            }
        }
        Request::SessionEventsRead {
            session_id,
            after_sequence,
            maximum_events,
            wait_milliseconds,
        } => {
            if *maximum_events == 0 || *maximum_events > MAX_SESSION_EVENTS_PER_READ {
                return Err(invalid_session_request(
                    "The session event read limit is outside its supported range.",
                ));
            }
            if *wait_milliseconds > MAX_SESSION_EVENT_WAIT_MILLISECONDS {
                return Err(invalid_session_request(
                    "The session event wait duration is outside its supported range.",
                ));
            }
            read_session_events(
                Arc::clone(&state.sessions),
                Arc::clone(&state.storage),
                *session_id,
                *after_sequence,
                *maximum_events,
                *wait_milliseconds,
            )
            .await
            .map(Response::SessionEvents)
        }
        Request::SessionRawRead {
            session_id,
            run_id,
            after_offset,
            maximum_bytes,
        } => {
            if *maximum_bytes == 0 || *maximum_bytes > MAX_SESSION_RAW_READ_BYTES {
                return Err(invalid_session_request(
                    "The raw protocol read limit is outside its supported range.",
                ));
            }
            read_session_raw(
                Arc::clone(&state.sessions),
                Arc::clone(&state.storage),
                *session_id,
                *run_id,
                *after_offset,
                *maximum_bytes,
            )
            .await
            .map(Response::SessionRaw)
        }
        Request::SessionPermissionRespond {
            session_id,
            run_id,
            request_id,
            decision,
        } => {
            let decision = match decision {
                SessionPermissionDecision::Allow => PermissionDecision::Allow,
                SessionPermissionDecision::Deny => PermissionDecision::Deny,
                SessionPermissionDecision::Cancel => PermissionDecision::Cancel,
            };
            state
                .sessions
                .respond_permission(*session_id, *run_id, request_id, decision)
                .await?;
            Ok(Response::SessionPermissionAccepted {
                session_id: *session_id,
                request_id: request_id.clone(),
            })
        }
        Request::SessionUserInputRespond {
            session_id,
            run_id,
            request_id,
            value_json,
        } => {
            let value_json = value_json.clone().into_inner();
            if value_json.len() > MAX_SESSION_ACTION_BYTES {
                return Err(invalid_session_request(
                    "The session input exceeds the supported size.",
                ));
            }
            let value = serde_json::from_str(&value_json).map_err(|_| {
                invalid_session_request("The session input must contain valid JSON.")
            })?;
            state
                .sessions
                .respond_user_input(*session_id, *run_id, request_id, value)
                .await?;
            Ok(Response::SessionUserInputAccepted {
                session_id: *session_id,
                request_id: request_id.clone(),
            })
        }
        Request::SessionGuiAction {
            session_id,
            run_id,
            action,
            payload_json,
        } => {
            let payload_json = payload_json.clone().into_inner();
            if payload_json.len() > MAX_SESSION_ACTION_BYTES {
                return Err(invalid_session_request(
                    "The GUI action payload exceeds the supported size.",
                ));
            }
            let payload = serde_json::from_str(&payload_json).map_err(|_| {
                invalid_session_request("The GUI action payload must contain valid JSON.")
            })?;
            let action_id = state
                .sessions
                .send_gui_action(*session_id, *run_id, action, payload)
                .await?;
            Ok(Response::SessionGuiActionAccepted {
                session_id: *session_id,
                action_id,
            })
        }
        Request::SubscribeSession {
            session_id,
            after_sequence,
        } => {
            match state.sessions.subscribe(*session_id, *after_sequence).await {
                Ok(_subscription) => {}
                Err(error) if error.code == ErrorCode::SessionNotFound => {
                    let snapshot =
                        persisted_session_snapshot(Arc::clone(&state.storage), *session_id).await?;
                    if *after_sequence > snapshot.latest_sequence {
                        return Err(invalid_session_request(
                            "The session event cursor is newer than the durable event stream.",
                        ));
                    }
                }
                Err(error) => return Err(error),
            }
            Ok(Response::Subscribed {
                session_id: *session_id,
            })
        }
        Request::UnsubscribeSession { session_id } => Ok(Response::Unsubscribed {
            session_id: *session_id,
        }),
        Request::StopSession { session_id } => {
            match state.sessions.snapshot(*session_id).await {
                Ok(snapshot) => {
                    if let Some(run_id) = snapshot.active_run_id {
                        state.sessions.stop(*session_id, run_id).await?;
                    }
                }
                Err(error) if error.code == ErrorCode::SessionNotFound => {
                    let terminal_id = state.tui_sessions.read().await.get(session_id).copied();
                    if let Some(terminal_id) = terminal_id {
                        state.terminals.close(terminal_id).await?;
                    } else {
                        // Distinguish an already-finished persisted TUI from an
                        // unknown logical session without inventing state.
                        persisted_session_snapshot(Arc::clone(&state.storage), *session_id).await?;
                    }
                }
                Err(error) => return Err(error),
            }
            Ok(Response::SessionStopped {
                session_id: *session_id,
            })
        }
        _ => Err(invalid_session_request(
            "The request is not a session operation.",
        )),
    }
}

fn validate_fake_session_request(
    scenario: &str,
    volume: Option<usize>,
) -> Result<(), MaestroError> {
    const STRUCTURED_SCENARIOS: &[&str] = &[
        "structured/happy",
        "structured/fragmented",
        "structured/multi-frame-read",
        "structured/permission",
        "structured/user-input",
        "structured/gui-actions",
        "structured/nonzero",
        "structured/crash",
        "structured/malformed",
        "structured/incompatible",
        "structured/delay",
        "structured/stall",
        "structured/flood",
        "structured/resume",
        "structured/process-tree",
        "structured/ignore-term",
    ];
    if !STRUCTURED_SCENARIOS.contains(&scenario) {
        return Err(invalid_session_request(
            "The requested fake structured-session scenario is unsupported.",
        ));
    }
    if volume.is_some_and(|value| value == 0 || value > MAX_FAKE_EVENT_VOLUME) {
        return Err(invalid_session_request(
            "The fake-session event volume is outside its supported range.",
        ));
    }
    Ok(())
}

fn validate_fake_tui_request(scenario: &str) -> Result<(), MaestroError> {
    const TUI_SCENARIOS: &[&str] = &[
        "tui/vt-baseline",
        "tui/alternate-screen",
        "tui/resize-mouse",
        "tui/osc-security",
    ];
    if TUI_SCENARIOS.contains(&scenario) {
        Ok(())
    } else {
        Err(invalid_session_request(
            "The requested fake exact-TUI scenario is unsupported.",
        ))
    }
}

async fn persist_started_session(
    storage: Arc<StorageRuntime>,
    project_id: maestro_domain::ProjectId,
    session_id: maestro_domain::SessionId,
    run_id: maestro_domain::RunId,
    process_id: u32,
    title: String,
    invocation: serde_json::Value,
) -> Result<(), StorageRuntimeError> {
    tokio::task::spawn_blocking(move || {
        storage.start_session_run(
            project_id,
            session_id,
            run_id,
            process_id,
            &title,
            &invocation,
        )
    })
    .await
    .map_err(|_| StorageRuntimeError::Unavailable)?
}

async fn persist_started_tui_session(
    storage: Arc<StorageRuntime>,
    project_id: maestro_domain::ProjectId,
    session_id: SessionId,
    terminal: maestro_protocol::TerminalOpened,
    title: String,
    invocation: serde_json::Value,
) -> Result<(), StorageRuntimeError> {
    tokio::task::spawn_blocking(move || {
        storage.start_tui_session_run(
            project_id,
            session_id,
            terminal.run_id,
            terminal.process_id,
            &title,
            &invocation,
        )?;
        storage.persist_event(&EventEnvelope::new(
            session_id,
            Some(terminal.run_id),
            1,
            EventSource::Daemon,
            NormalizedEvent::user(
                "run_started",
                json!({
                    "channel": "pty_tui",
                    "process_id": terminal.process_id,
                    "terminal_id": terminal.terminal_id,
                }),
            ),
        ))?;
        storage.update_session_state(session_id, SessionState::Running)
    })
    .await
    .map_err(|_| StorageRuntimeError::Unavailable)?
}

fn start_tui_session_monitor(
    state: &DaemonState,
    session_id: SessionId,
    terminal: &maestro_protocol::TerminalOpened,
) {
    state.active_sessions.fetch_add(1, Ordering::AcqRel);
    state.activity_changed.notify_waiters();
    let terminals = Arc::clone(&state.terminals);
    let tui_sessions = Arc::clone(&state.tui_sessions);
    let storage = Arc::clone(&state.storage);
    let active_sessions = Arc::clone(&state.active_sessions);
    let activity_changed = Arc::clone(&state.activity_changed);
    let run_id = terminal.run_id;
    let terminal_id = terminal.terminal_id;
    tokio::spawn(async move {
        let status = terminals.wait_for_completion(terminal_id).await;
        persist_tui_completion(storage, session_id, run_id, terminal_id, status).await;
        tui_sessions.write().await.remove(&session_id);
        let _ = active_sessions.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            Some(count.saturating_sub(1))
        });
        activity_changed.notify_waiters();
    });
}

async fn persist_tui_completion(
    storage: Arc<StorageRuntime>,
    session_id: SessionId,
    run_id: maestro_domain::RunId,
    terminal_id: TerminalId,
    status: Result<TerminalStatus, MaestroError>,
) {
    let (state, exit, payload) = match status {
        Ok(status) => {
            let state = terminal_session_state(&status);
            let exit = status.exit.map_or(PersistedRunExit::Unknown, |exit| {
                if let Some(code) = exit.code {
                    PersistedRunExit::Exited(code)
                } else if let Some(signal) = exit.signal {
                    PersistedRunExit::Signaled(signal)
                } else {
                    PersistedRunExit::Unknown
                }
            });
            (
                state,
                exit,
                json!({
                    "terminal_id": terminal_id,
                    "terminal_state": status.state,
                    "exit": status.exit,
                }),
            )
        }
        Err(error) => (
            SessionState::Failed,
            PersistedRunExit::Unknown,
            json!({
                "terminal_id": terminal_id,
                "error_code": error.code,
                "correlation_id": error.correlation_id,
            }),
        ),
    };
    let result = tokio::task::spawn_blocking(move || {
        storage.persist_event(&EventEnvelope::new(
            session_id,
            Some(run_id),
            2,
            EventSource::Daemon,
            NormalizedEvent::user("process_exited", payload),
        ))?;
        storage.finish_session_run(session_id, run_id, state, exit, None)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(
            ?session_id,
            ?run_id,
            ?error,
            "TUI completion persistence failed"
        ),
        Err(error) => warn!(
            ?session_id,
            ?run_id,
            ?error,
            "TUI completion worker stopped"
        ),
    }
}

fn terminal_session_state(status: &TerminalStatus) -> SessionState {
    match status.state {
        TerminalState::Closed | TerminalState::Closing => SessionState::Stopped,
        TerminalState::Exited if status.exit.is_some_and(|exit| exit.code == Some(0)) => {
            SessionState::Completed
        }
        TerminalState::Exited | TerminalState::Failed => SessionState::Failed,
        TerminalState::Running => SessionState::Interrupted,
    }
}

fn start_session_monitor(
    state: &DaemonState,
    session_id: maestro_domain::SessionId,
    run_id: maestro_domain::RunId,
    after_sequence: u64,
) {
    state.active_sessions.fetch_add(1, Ordering::AcqRel);
    state.activity_changed.notify_waiters();
    let sessions = Arc::clone(&state.sessions);
    let storage = Arc::clone(&state.storage);
    let active_sessions = Arc::clone(&state.active_sessions);
    let activity_changed = Arc::clone(&state.activity_changed);
    tokio::spawn(async move {
        persist_session_events(
            Arc::clone(&sessions),
            Arc::clone(&storage),
            session_id,
            run_id,
            after_sequence,
        )
        .await;
        let _ = active_sessions.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            Some(count.saturating_sub(1))
        });
        activity_changed.notify_waiters();
    });
}

#[expect(
    clippy::too_many_lines,
    reason = "the ordered replay, state, and finalization pipeline remains linear and auditable"
)]
async fn persist_session_events(
    sessions: Arc<FakeSessionSupervisor>,
    storage: Arc<StorageRuntime>,
    session_id: maestro_domain::SessionId,
    run_id: maestro_domain::RunId,
    after_sequence: u64,
) {
    let mut subscription = match sessions.subscribe(session_id, after_sequence).await {
        Ok(subscription) => subscription,
        Err(error) => {
            warn!(
                ?session_id,
                ?run_id,
                ?error,
                "session persistence subscription failed"
            );
            return;
        }
    };
    let mut last_persisted_state = None;
    loop {
        let event = match subscription.recv().await {
            Ok(event) => event,
            Err(error) => {
                warn!(
                    ?session_id,
                    ?run_id,
                    ?error,
                    "session persistence stream stopped"
                );
                break;
            }
        };
        let event_for_storage = event.clone();
        let event_storage = Arc::clone(&storage);
        match tokio::task::spawn_blocking(move || event_storage.persist_event(&event_for_storage))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(
                ?session_id,
                ?run_id,
                ?error,
                "session event persistence failed"
            ),
            Err(error) => warn!(
                ?session_id,
                ?run_id,
                ?error,
                "session persistence worker stopped"
            ),
        }
        match sessions.snapshot(session_id).await {
            Ok(snapshot) if snapshot.active_run_id == Some(run_id) => {
                if last_persisted_state != Some(snapshot.state) {
                    let state_storage = Arc::clone(&storage);
                    let current_state = snapshot.state;
                    match tokio::task::spawn_blocking(move || {
                        state_storage.update_session_state(session_id, current_state)
                    })
                    .await
                    {
                        Ok(Ok(())) => last_persisted_state = Some(current_state),
                        Ok(Err(error)) => warn!(
                            ?session_id,
                            ?run_id,
                            ?error,
                            "session state persistence failed"
                        ),
                        Err(error) => warn!(
                            ?session_id,
                            ?run_id,
                            ?error,
                            "session state persistence worker stopped"
                        ),
                    }
                }
            }
            Ok(_) | Err(_) => break,
        }
    }

    let Ok(snapshot) = sessions.snapshot(session_id).await else {
        return;
    };
    let exit = snapshot
        .last_exit
        .map_or(PersistedRunExit::Unknown, |exit| match exit {
            ExitCause::Exited(code) => PersistedRunExit::Exited(code),
            ExitCause::Signaled(signal) => PersistedRunExit::Signaled(signal),
            ExitCause::Unknown => PersistedRunExit::Unknown,
        });
    let recovery = snapshot.last_error.as_ref().map(|error| {
        json!({
            "code": error.code,
            "message": error.message,
            "correlation_id": error.correlation_id,
        })
    });
    if let Ok(Some(capture)) = sessions.raw_capture(session_id, run_id).await {
        let raw_storage = Arc::clone(&storage);
        match tokio::task::spawn_blocking(move || {
            raw_storage.persist_raw_protocol_capture(
                session_id,
                run_id,
                &capture.bytes,
                capture.observed_byte_count,
                capture.truncated,
                capture.complete,
            )
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(
                ?session_id,
                ?run_id,
                ?error,
                "raw protocol persistence failed"
            ),
            Err(error) => warn!(
                ?session_id,
                ?run_id,
                ?error,
                "raw protocol persistence worker stopped"
            ),
        }
    }
    let final_storage = Arc::clone(&storage);
    match tokio::task::spawn_blocking(move || {
        final_storage.finish_session_run(
            session_id,
            run_id,
            snapshot.state,
            exit,
            recovery.as_ref(),
        )
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(
            ?session_id,
            ?run_id,
            ?error,
            "session finalization persistence failed"
        ),
        Err(error) => warn!(
            ?session_id,
            ?run_id,
            ?error,
            "session finalization worker stopped"
        ),
    }
}

async fn read_session_events(
    sessions: Arc<FakeSessionSupervisor>,
    storage: Arc<StorageRuntime>,
    session_id: maestro_domain::SessionId,
    after_sequence: u64,
    maximum_events: usize,
    wait_milliseconds: u32,
) -> Result<SessionEventBatch, MaestroError> {
    let mut subscription = match sessions.subscribe(session_id, after_sequence).await {
        Ok(subscription) => subscription,
        Err(error) if error.code == ErrorCode::SessionNotFound => {
            return read_persisted_session_events(
                storage,
                session_id,
                after_sequence,
                maximum_events,
            )
            .await;
        }
        Err(error) => return Err(error),
    };
    let replay_gap = subscription.replay_gap().map(|gap| SessionReplayGap {
        requested_after_sequence: gap.requested_after_sequence,
        available_after_sequence: gap.available_after_sequence,
    });
    let mut events = Vec::with_capacity(maximum_events.min(64));
    while events.len() < maximum_events {
        match subscription
            .try_recv()
            .map_err(session_subscription_error)?
        {
            Some(event) => events.push(event),
            None => break,
        }
    }
    if events.is_empty() && wait_milliseconds > 0 {
        if let Ok(result) = timeout(
            Duration::from_millis(u64::from(wait_milliseconds)),
            subscription.recv(),
        )
        .await
        {
            events.push(result.map_err(session_subscription_error)?);
        }
        while events.len() < maximum_events {
            match subscription
                .try_recv()
                .map_err(session_subscription_error)?
            {
                Some(event) => events.push(event),
                None => break,
            }
        }
    }
    let snapshot = sessions.snapshot(session_id).await?;
    let next_sequence = events.last().map_or(after_sequence, |event| event.sequence);
    Ok(SessionEventBatch {
        session_id,
        events,
        next_sequence,
        latest_sequence: snapshot.latest_sequence,
        replay_gap,
        state: snapshot.state,
    })
}

async fn read_persisted_session_events(
    storage: Arc<StorageRuntime>,
    session_id: maestro_domain::SessionId,
    after_sequence: u64,
    maximum_events: usize,
) -> Result<SessionEventBatch, MaestroError> {
    let (metadata, events) = tokio::task::spawn_blocking(move || {
        let metadata = storage.persisted_session_metadata(session_id)?;
        let events = storage.persisted_events(session_id, after_sequence, maximum_events)?;
        Ok::<_, StorageRuntimeError>((metadata, events))
    })
    .await
    .map_err(|_| database_unavailable_error())?
    .map_err(|error| storage_session_read_error(&error, session_id))?;
    let Some(metadata) = metadata else {
        return Err(session_not_found_error(session_id));
    };
    if after_sequence > metadata.latest_sequence {
        return Err(invalid_session_request(
            "The session event cursor is newer than the durable event stream.",
        ));
    }
    let next_sequence = events.last().map_or(after_sequence, |event| event.sequence);
    Ok(SessionEventBatch {
        session_id,
        events,
        next_sequence,
        latest_sequence: metadata.latest_sequence,
        replay_gap: None,
        state: metadata.state,
    })
}

async fn read_session_raw(
    sessions: Arc<FakeSessionSupervisor>,
    storage: Arc<StorageRuntime>,
    session_id: maestro_domain::SessionId,
    run_id: maestro_domain::RunId,
    after_offset: u64,
    maximum_bytes: u32,
) -> Result<SessionRawBatch, MaestroError> {
    let capture = match sessions.raw_capture(session_id, run_id).await {
        Ok(Some(capture)) => {
            let capture_for_storage = capture.clone();
            let raw_storage = Arc::clone(&storage);
            tokio::task::spawn_blocking(move || {
                raw_storage.persist_raw_protocol_capture(
                    session_id,
                    run_id,
                    &capture_for_storage.bytes,
                    capture_for_storage.observed_byte_count,
                    capture_for_storage.truncated,
                    capture_for_storage.complete,
                )
            })
            .await
            .map_err(|_| database_unavailable_error())?
            .map_err(|error| storage_session_error(&error))?;
            let metadata = (
                capture.observed_byte_count,
                capture.truncated,
                capture.complete,
            );
            Some((capture.into_bytes(), metadata.0, metadata.1, metadata.2))
        }
        Ok(None) => None,
        Err(error) if error.code == ErrorCode::SessionNotFound => None,
        Err(error) => return Err(error),
    };
    let capture = if let Some(capture) = capture {
        Some(capture)
    } else {
        tokio::task::spawn_blocking(move || {
            storage.persisted_raw_protocol_capture(session_id, run_id)
        })
        .await
        .map_err(|_| database_unavailable_error())?
        .map_err(|error| storage_session_read_error(&error, session_id))?
        .map(|capture| {
            let metadata = (
                capture.observed_byte_count,
                capture.truncated,
                capture.completed,
            );
            (capture.into_bytes(), metadata.0, metadata.1, metadata.2)
        })
    };
    let Some((bytes, observed_bytes, truncated, complete)) = capture else {
        return Err(invalid_session_request(
            "Raw protocol capture was not enabled for this run.",
        ));
    };
    raw_batch(
        session_id,
        run_id,
        &bytes,
        observed_bytes,
        truncated,
        complete,
        after_offset,
        maximum_bytes,
    )
}

#[allow(clippy::too_many_arguments)]
fn raw_batch(
    session_id: SessionId,
    run_id: maestro_domain::RunId,
    bytes: &[u8],
    observed_bytes: u64,
    truncated: bool,
    complete: bool,
    after_offset: u64,
    maximum_bytes: u32,
) -> Result<SessionRawBatch, MaestroError> {
    let captured_bytes = u64::try_from(bytes.len())
        .map_err(|_| invalid_session_request("The raw capture size is unsupported."))?;
    if after_offset > captured_bytes {
        return Err(invalid_session_request(
            "The raw protocol cursor is newer than the retained capture.",
        ));
    }
    let start = usize::try_from(after_offset)
        .map_err(|_| invalid_session_request("The raw protocol cursor is unsupported."))?;
    let maximum = usize::try_from(maximum_bytes)
        .map_err(|_| invalid_session_request("The raw protocol read limit is unsupported."))?;
    let end = start.saturating_add(maximum).min(bytes.len());
    let next_offset = u64::try_from(end)
        .map_err(|_| invalid_session_request("The raw protocol cursor is unsupported."))?;
    Ok(SessionRawBatch {
        session_id,
        run_id,
        data: SensitiveBytes::new(bytes[start..end].to_vec()),
        next_offset,
        captured_bytes,
        observed_bytes,
        truncated,
        complete,
    })
}

async fn persisted_session_snapshot(
    storage: Arc<StorageRuntime>,
    session_id: SessionId,
) -> Result<SessionSnapshot, MaestroError> {
    let metadata =
        tokio::task::spawn_blocking(move || storage.persisted_session_metadata(session_id))
            .await
            .map_err(|_| database_unavailable_error())?
            .map_err(|error| storage_session_read_error(&error, session_id))?
            .ok_or_else(|| session_not_found_error(session_id))?;
    Ok(SessionSnapshot {
        session_id,
        active_run_id: metadata.active_run_id,
        state: metadata.state,
        binding: None,
        latest_sequence: metadata.latest_sequence,
        dropped_through_sequence: 0,
        stderr: String::new(),
        stderr_truncated: false,
        last_exit: None,
        last_error: None,
    })
}

async fn require_session_ownership(
    storage: Arc<StorageRuntime>,
    session_id: SessionId,
    project_id: ProjectId,
    integration_mode: IntegrationMode,
) -> Result<(), MaestroError> {
    let metadata =
        tokio::task::spawn_blocking(move || storage.persisted_session_metadata(session_id))
            .await
            .map_err(|_| database_unavailable_error())?
            .map_err(|error| storage_session_read_error(&error, session_id))?
            .ok_or_else(|| session_not_found_error(session_id))?;
    if metadata.project_id != project_id || metadata.integration_mode != integration_mode {
        return Err(MaestroError::new(
            ErrorCode::PermissionDenied,
            "The session does not belong to this project or integration mode.",
        ));
    }
    Ok(())
}

fn protocol_session_snapshot(
    snapshot: crate::fake_session::FakeSessionSnapshot,
) -> SessionSnapshot {
    let stderr = String::from_utf8_lossy(&snapshot.stderr);
    SessionSnapshot {
        session_id: snapshot.session_id,
        active_run_id: snapshot.active_run_id,
        state: snapshot.state,
        binding: snapshot.binding,
        latest_sequence: snapshot.latest_sequence,
        dropped_through_sequence: snapshot.dropped_through_sequence,
        stderr: redact_text(&stderr).into_owned(),
        stderr_truncated: snapshot.stderr_truncated,
        last_exit: snapshot.last_exit.map(|exit| match exit {
            ExitCause::Exited(code) => SessionExit::Exited(code),
            ExitCause::Signaled(signal) => SessionExit::Signaled(signal),
            ExitCause::Unknown => SessionExit::Unknown,
        }),
        last_error: snapshot.last_error.map(|error| SessionErrorSummary {
            code: error.code,
            message: redact_text(&error.message).into_owned(),
            correlation_id: error.correlation_id,
        }),
    }
}

fn session_subscription_error(error: FakeSubscriptionError) -> MaestroError {
    let mut result = MaestroError::new(
        ErrorCode::Internal,
        "The session event subscription could not continue.",
    );
    result.retryable = matches!(error, FakeSubscriptionError::Lagged { .. });
    if let FakeSubscriptionError::Lagged { missed } = error {
        result.details = Some(json!({ "missed_events": missed }));
    }
    result
}

fn invalid_session_request(message: &str) -> MaestroError {
    MaestroError::new(ErrorCode::InvalidRequest, message)
}

fn session_not_found_error(session_id: SessionId) -> MaestroError {
    let mut error = MaestroError::new(
        ErrorCode::SessionNotFound,
        "The logical session was not found.",
    );
    error.details = Some(json!({ "session_id": session_id }));
    error
}

fn storage_session_error(_error: &StorageRuntimeError) -> MaestroError {
    database_unavailable_error()
}

fn storage_session_read_error(
    error: &StorageRuntimeError,
    session_id: maestro_domain::SessionId,
) -> MaestroError {
    if matches!(
        error,
        StorageRuntimeError::Session(maestro_storage::SessionStoreError::SessionNotFound(_))
    ) {
        let mut result = MaestroError::new(
            ErrorCode::SessionNotFound,
            "The requested session does not exist.",
        );
        result.details = Some(json!({ "session_id": session_id }));
        result
    } else {
        database_unavailable_error()
    }
}

fn is_project_request(request: &Request) -> bool {
    matches!(
        request,
        Request::ProjectRegister { .. }
            | Request::ProjectRecentList { .. }
            | Request::ProjectSetFavorite { .. }
            | Request::ProjectWindowLayoutLoad { .. }
            | Request::ProjectWindowLayoutSave { .. }
            | Request::ProjectDirectoryList { .. }
            | Request::ProjectFileRead { .. }
            | Request::ProjectFileSave { .. }
            | Request::ProjectSearch { .. }
            | Request::ProjectSearchCancel { .. }
            | Request::ProjectGitStatus { .. }
            | Request::ProjectGitBranch { .. }
            | Request::ProjectGitDiff { .. }
            | Request::ProjectGitWorktrees { .. }
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "typed project protocol variants are mapped explicitly at one IPC boundary"
)]
async fn handle_project_request(
    request: &Request,
    projects: Arc<ProjectManager>,
    storage: Arc<StorageRuntime>,
) -> Result<Response, MaestroError> {
    match request {
        Request::ProjectRegister {
            project_id,
            display_name,
            roots,
        } => {
            let project_id = *project_id;
            let display_name = display_name.clone();
            let roots = roots.clone();
            run_project_operation(move || {
                projects
                    .register_with_persistence(project_id, display_name, &roots, |registered| {
                        let persisted_id = storage
                            .upsert_project_registration(
                                &registered.project_id.to_string(),
                                &registered.display_name,
                                &registered.canonical_roots,
                            )
                            .map_err(|_| database_unavailable_error())?;
                        persisted_id
                            .parse()
                            .map_err(|_| database_unavailable_error())
                    })
                    .map(Response::ProjectRegistered)
            })
            .await
        }
        Request::ProjectRecentList { maximum_projects } => {
            let maximum_projects = *maximum_projects;
            run_project_operation(move || {
                let persisted = storage
                    .recent_projects(maximum_projects)
                    .map_err(|error| storage_project_error(&error))?;
                let projects = persisted
                    .into_iter()
                    .map(|project| {
                        let project_id = project
                            .id
                            .parse()
                            .map_err(|_| database_unavailable_error())?;
                        Ok(RecentProject {
                            project_id,
                            display_name: project.display_name,
                            canonical_roots: project.canonical_roots,
                            favorite: project.favorite,
                            last_opened_at: project.last_opened_at,
                        })
                    })
                    .collect::<Result<Vec<_>, MaestroError>>()?;
                Ok(Response::ProjectRecentList(projects))
            })
            .await
        }
        Request::ProjectSetFavorite {
            project_id,
            favorite,
        } => {
            let project_id = *project_id;
            let favorite = *favorite;
            run_project_operation(move || {
                storage
                    .set_project_favorite(&project_id.to_string(), favorite)
                    .map_err(|error| storage_project_error(&error))?;
                Ok(Response::ProjectFavoriteUpdated {
                    project_id,
                    favorite,
                })
            })
            .await
        }
        Request::ProjectWindowLayoutLoad {
            project_id,
            window_key,
        } => {
            let project_id = *project_id;
            let window_key = window_key.clone();
            run_project_operation(move || {
                let layout_json = storage
                    .window_layout(&project_id.to_string(), &window_key)
                    .map_err(|error| storage_project_error(&error))?;
                Ok(Response::ProjectWindowLayout(ProjectWindowLayout {
                    project_id,
                    window_key,
                    layout_json,
                }))
            })
            .await
        }
        Request::ProjectWindowLayoutSave {
            project_id,
            window_key,
            layout_json,
        } => {
            let project_id = *project_id;
            let window_key = window_key.clone();
            let layout_json = layout_json.clone();
            run_project_operation(move || {
                storage
                    .save_window_layout(&project_id.to_string(), &window_key, &layout_json)
                    .map_err(|error| storage_project_error(&error))?;
                Ok(Response::ProjectWindowLayoutSaved {
                    project_id,
                    window_key,
                })
            })
            .await
        }
        Request::ProjectDirectoryList {
            project_id,
            directory,
            cursor,
            maximum_entries,
            include_hidden,
        } => {
            let project_id = *project_id;
            let directory = directory.clone();
            let cursor = *cursor;
            let maximum_entries = *maximum_entries;
            let include_hidden = *include_hidden;
            run_project_operation(move || {
                projects
                    .list_directory(
                        project_id,
                        &directory,
                        cursor,
                        maximum_entries,
                        include_hidden,
                    )
                    .map(Response::ProjectDirectoryPage)
            })
            .await
        }
        Request::ProjectFileRead { project_id, path } => {
            let project_id = *project_id;
            let path = path.clone();
            run_project_operation(move || {
                projects
                    .read_file(project_id, &path)
                    .map(Response::ProjectTextFile)
            })
            .await
        }
        Request::ProjectFileSave {
            project_id,
            path,
            text,
            expected_fingerprint,
        } => {
            let project_id = *project_id;
            let path = path.clone();
            let text = text.clone();
            let expected_fingerprint = expected_fingerprint.clone();
            run_project_operation(move || {
                projects
                    .save_file(project_id, &path, &text, &expected_fingerprint)
                    .map(Response::ProjectFileSaved)
            })
            .await
        }
        Request::ProjectSearch {
            project_id,
            search_id,
            options,
        } => {
            let project_id = *project_id;
            let search_id = *search_id;
            let options = options.clone();
            run_project_operation(move || {
                projects
                    .search(project_id, search_id, &options)
                    .map(Response::ProjectSearchResult)
            })
            .await
        }
        Request::ProjectSearchCancel { search_id } => {
            projects.cancel_search(*search_id)?;
            Ok(Response::ProjectSearchCancelled {
                search_id: *search_id,
            })
        }
        Request::ProjectGitStatus {
            project_id,
            repository,
        } => {
            let project_id = *project_id;
            let repository = repository.clone();
            run_project_operation(move || {
                projects
                    .git_status(project_id, &repository)
                    .map(Response::ProjectGitStatus)
            })
            .await
        }
        Request::ProjectGitBranch {
            project_id,
            repository,
        } => {
            let project_id = *project_id;
            let repository = repository.clone();
            run_project_operation(move || {
                projects
                    .git_branch(project_id, &repository)
                    .map(Response::ProjectGitBranch)
            })
            .await
        }
        Request::ProjectGitDiff {
            project_id,
            repository,
            scope,
            maximum_bytes,
        } => {
            let project_id = *project_id;
            let repository = repository.clone();
            let scope = *scope;
            let maximum_bytes = *maximum_bytes;
            run_project_operation(move || {
                projects
                    .git_diff(project_id, &repository, scope, maximum_bytes)
                    .map(Response::ProjectGitDiff)
            })
            .await
        }
        Request::ProjectGitWorktrees {
            project_id,
            repository,
        } => {
            let project_id = *project_id;
            let repository = repository.clone();
            run_project_operation(move || {
                projects
                    .git_worktrees(project_id, &repository)
                    .map(Response::ProjectGitWorktrees)
            })
            .await
        }
        _ => Err(MaestroError::new(
            ErrorCode::InvalidRequest,
            "The request is not a project operation.",
        )),
    }
}

async fn run_project_operation<F>(operation: F) -> Result<Response, MaestroError>
where
    F: FnOnce() -> Result<Response, MaestroError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation).await.map_err(|_| {
        MaestroError::new(
            ErrorCode::Internal,
            "The daemon project worker stopped unexpectedly.",
        )
    })?
}

fn locked_storage_error(status: StorageStatus) -> MaestroError {
    match status {
        StorageStatus::PassphraseRequired { .. } => MaestroError::new(
            ErrorCode::DaemonLocked,
            "Maestro encrypted storage must be unlocked before this action can run",
        )
        .with_user_action("Unlock Maestro from the desktop application."),
        StorageStatus::Unavailable | StorageStatus::Ready => database_unavailable_error(),
    }
}

fn storage_unlock_error(error: &StorageRuntimeError) -> MaestroError {
    match error {
        StorageRuntimeError::Key(
            maestro_storage::KeyStoreError::IncorrectPassphraseOrCorruptEnvelope
            | maestro_storage::KeyStoreError::EmptyPassphrase,
        ) => MaestroError::new(
            ErrorCode::AuthenticationRequired,
            "The Maestro storage passphrase was not accepted",
        )
        .with_user_action("Check the passphrase and try again."),
        StorageRuntimeError::Unavailable
        | StorageRuntimeError::Key(_)
        | StorageRuntimeError::Storage(_)
        | StorageRuntimeError::Session(_)
        | StorageRuntimeError::Backup(_)
        | StorageRuntimeError::Retention(_)
        | StorageRuntimeError::Io(_)
        | StorageRuntimeError::UnsafeRetentionPath(_)
        | StorageRuntimeError::SegmentEncryption => database_unavailable_error(),
    }
}

fn storage_project_error(error: &StorageRuntimeError) -> MaestroError {
    match error {
        StorageRuntimeError::Storage(
            maestro_storage::StorageError::InvalidLimit
            | maestro_storage::StorageError::ProjectNotFound
            | maestro_storage::StorageError::Json(_),
        ) => MaestroError::new(ErrorCode::InvalidRequest, error.to_string()),
        StorageRuntimeError::Unavailable
        | StorageRuntimeError::Key(_)
        | StorageRuntimeError::Storage(_)
        | StorageRuntimeError::Session(_)
        | StorageRuntimeError::Backup(_)
        | StorageRuntimeError::Retention(_)
        | StorageRuntimeError::Io(_)
        | StorageRuntimeError::UnsafeRetentionPath(_)
        | StorageRuntimeError::SegmentEncryption => database_unavailable_error(),
    }
}

fn database_unavailable_error() -> MaestroError {
    MaestroError::new(
        ErrorCode::DatabaseUnavailable,
        "Maestro encrypted storage is unavailable",
    )
    .with_user_action(
        "Restart Maestro. If the problem continues, preserve the data directory for recovery.",
    )
}

struct ClientGuard(Arc<DaemonState>);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.connected_clients.fetch_sub(1, Ordering::SeqCst);
        self.0.activity_changed.notify_one();
    }
}

#[cfg(unix)]
fn restrict_socket(path: &std::path::Path) -> Result<(), io::Error> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_socket(_path: &std::path::Path) -> Result<(), io::Error> {
    Ok(())
}

fn remove_owned_socket(path: &std::path::Path) -> Result<(), io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};

    use maestro_domain::{ErrorCode, ProjectId, RequestId};
    use maestro_protocol::{
        ClientFrame, ClientHello, MAX_HELLO_FRAME_BYTES, MAX_SETTING_KEY_BYTES,
        MAX_SETTING_SCOPE_BYTES, MAX_SETTING_SCOPE_REFERENCE_BYTES, MAX_SETTING_VALUE_BYTES,
        MAX_TERMINAL_INPUT_BYTES, MIN_TERMINAL_POLL_BYTES, PROTOCOL_VERSION, Request,
        RequestEnvelope, Response, SensitiveString, ServerFrame, TerminalState,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::UnixStream,
    };

    use crate::{
        DaemonClient, DaemonConfig, DaemonPaths, DaemonServer, IpcError, MultiplexedDaemonClient,
        ipc::{read_frame, write_frame},
    };

    async fn register_test_project(client: &mut DaemonClient, root: &std::path::Path) -> ProjectId {
        let project_id = ProjectId::new();
        let response = client
            .request(Request::ProjectRegister {
                project_id,
                display_name: "Terminal test project".to_owned(),
                roots: vec![root.to_string_lossy().into_owned()],
            })
            .await
            .expect("test project registers");
        assert!(matches!(response, Response::ProjectRegistered(_)));
        project_id
    }

    #[tokio::test]
    async fn authenticated_client_can_ping_and_read_snapshot() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&paths.socket)
                    .expect("socket metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let mut client = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("client authenticates");
        assert_eq!(
            client.request(Request::Ping).await.expect("ping"),
            Response::Pong
        );
        assert!(matches!(
            client
                .request(Request::SystemSnapshot)
                .await
                .expect("snapshot"),
            Response::SystemSnapshot(_)
        ));
        drop(client);
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn encrypted_settings_round_trip_through_the_daemon() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let mut client = DaemonClient::connect(&paths, "settings-test", "0.1.0")
            .await
            .expect("client authenticates");
        let value = r#"{"openProject":"Mod+O"}"#;

        assert!(matches!(
            client
                .request(Request::SettingSave {
                    scope: "global".to_owned(),
                    scope_reference: String::new(),
                    key: "keyboard.shortcuts".to_owned(),
                    value_json: SensitiveString::new(value),
                })
                .await
                .expect("setting saves"),
            Response::SettingSaved { .. }
        ));
        let Response::SettingValue {
            value_json: Some(stored),
            ..
        } = client
            .request(Request::SettingLoad {
                scope: "global".to_owned(),
                scope_reference: String::new(),
                key: "keyboard.shortcuts".to_owned(),
            })
            .await
            .expect("setting loads")
        else {
            panic!("unexpected setting response");
        };
        assert_eq!(stored.into_inner(), value);

        drop(client);
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[test]
    fn setting_requests_are_bounded_and_require_valid_json() {
        assert!(super::validate_setting_identity("global", "", "keyboard.shortcuts").is_ok());
        assert!(super::validate_setting_json(r#"{"valid":true}"#).is_ok());
        assert!(super::validate_setting_identity("", "", "key").is_err());
        assert!(
            super::validate_setting_identity(&"s".repeat(MAX_SETTING_SCOPE_BYTES + 1), "", "key")
                .is_err()
        );
        assert!(
            super::validate_setting_identity(
                "global",
                &"r".repeat(MAX_SETTING_SCOPE_REFERENCE_BYTES + 1),
                "key"
            )
            .is_err()
        );
        assert!(
            super::validate_setting_identity("global", "", &"k".repeat(MAX_SETTING_KEY_BYTES + 1))
                .is_err()
        );
        assert!(super::validate_setting_json("not-json").is_err());
        assert!(
            super::validate_setting_json(&format!("\"{}\"", "v".repeat(MAX_SETTING_VALUE_BYTES)))
                .is_err()
        );
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "the registration persistence regression keeps identity, favorite, and layout assertions together"
    )]
    async fn successful_project_registration_is_persisted() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let project_root = temporary.path().join("project");
        let documentation_root = temporary.path().join("documentation");
        std::fs::create_dir(&project_root).expect("project root creates");
        std::fs::create_dir(&documentation_root).expect("documentation root creates");
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let storage = std::sync::Arc::clone(&server.state.storage);
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let mut client = DaemonClient::connect(&paths, "project-persistence-test", "0.1.0")
            .await
            .expect("client authenticates");
        let project_id = ProjectId::new();
        let requested_roots = vec![
            project_root.to_string_lossy().into_owned(),
            documentation_root.to_string_lossy().into_owned(),
        ];

        let Response::ProjectRegistered(registered) = client
            .request(Request::ProjectRegister {
                project_id,
                display_name: "Maestro".to_owned(),
                roots: requested_roots.clone(),
            })
            .await
            .expect("project registers")
        else {
            panic!("unexpected project registration response");
        };
        let retry_project_id = ProjectId::new();
        let Response::ProjectRegistered(retried) = client
            .request(Request::ProjectRegister {
                project_id: retry_project_id,
                display_name: "Maestro retried".to_owned(),
                roots: requested_roots.into_iter().rev().collect(),
            })
            .await
            .expect("project registration retry completes")
        else {
            panic!("unexpected project registration retry response");
        };
        assert_ne!(retry_project_id, project_id);
        assert_eq!(retried.project_id, project_id);
        let persisted = tokio::task::spawn_blocking(move || storage.recent_projects(10))
            .await
            .expect("storage worker joins")
            .expect("project loads from storage");

        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].id, project_id.to_string());
        assert_eq!(persisted[0].display_name, "Maestro retried");
        assert_eq!(persisted[0].canonical_roots, registered.canonical_roots);

        let Response::ProjectRecentList(recent) = client
            .request(Request::ProjectRecentList {
                maximum_projects: 10,
            })
            .await
            .expect("recent projects load")
        else {
            panic!("unexpected recent projects response");
        };
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].project_id, project_id);
        assert!(!recent[0].favorite);

        assert_eq!(
            client
                .request(Request::ProjectSetFavorite {
                    project_id,
                    favorite: true,
                })
                .await
                .expect("favorite updates"),
            Response::ProjectFavoriteUpdated {
                project_id,
                favorite: true,
            }
        );
        let layout_json = r#"{"version":1,"leftWidth":280}"#.to_owned();
        assert_eq!(
            client
                .request(Request::ProjectWindowLayoutSave {
                    project_id,
                    window_key: "main".to_owned(),
                    layout_json: layout_json.clone(),
                })
                .await
                .expect("window layout saves"),
            Response::ProjectWindowLayoutSaved {
                project_id,
                window_key: "main".to_owned(),
            }
        );
        let Response::ProjectWindowLayout(layout) = client
            .request(Request::ProjectWindowLayoutLoad {
                project_id,
                window_key: "main".to_owned(),
            })
            .await
            .expect("window layout loads")
        else {
            panic!("unexpected window layout response");
        };
        assert_eq!(layout.layout_json.as_deref(), Some(layout_json.as_str()));
        let Response::ProjectRecentList(recent) = client
            .request(Request::ProjectRecentList {
                maximum_projects: 10,
            })
            .await
            .expect("favorite project reloads")
        else {
            panic!("unexpected recent projects response");
        };
        assert!(recent[0].favorite);
        drop(client);
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "the multiplexed IPC regression keeps the concurrent request transcript together"
    )]
    async fn persistent_connection_correlates_requests_while_a_terminal_read_waits() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let client = MultiplexedDaemonClient::connect(&paths, "multiplex-test", "0.1.0")
            .await
            .expect("client authenticates");
        let project_id = ProjectId::new();
        assert!(matches!(
            client
                .request(Request::ProjectRegister {
                    project_id,
                    display_name: "Multiplex terminal test".to_owned(),
                    roots: vec![temporary.path().to_string_lossy().into_owned()],
                })
                .await
                .expect("project registers"),
            Response::ProjectRegistered(_)
        ));

        let Response::TerminalOpened(opened) = client
            .request(Request::TerminalOpen {
                project_id,
                cwd: temporary.path().to_string_lossy().into_owned(),
                columns: 80,
                rows: 24,
            })
            .await
            .expect("terminal opens")
        else {
            panic!("unexpected open response");
        };
        let Response::TerminalList(terminals) = client
            .request(Request::TerminalList {
                project_id,
                maximum_terminals: 8,
            })
            .await
            .expect("project terminal index reads")
        else {
            panic!("unexpected terminal list response");
        };
        assert_eq!(terminals.len(), 1);
        assert_eq!(terminals[0].terminal, opened);
        let Response::TerminalAttached(attached) = client
            .request(Request::TerminalAttach {
                project_id,
                terminal_id: opened.terminal_id,
            })
            .await
            .expect("owner reattaches without opening another PTY")
        else {
            panic!("unexpected terminal attach response");
        };
        assert_eq!(attached, opened);
        let other_project = ProjectId::new();
        client
            .request(Request::ProjectRegister {
                project_id: other_project,
                display_name: "Other project".to_owned(),
                roots: vec![temporary.path().to_string_lossy().into_owned()],
            })
            .await
            .expect("other project registers");
        let denied = client
            .request(Request::TerminalAttach {
                project_id: other_project,
                terminal_id: opened.terminal_id,
            })
            .await
            .expect_err("another project cannot attach by terminal identifier");
        assert!(
            matches!(denied, IpcError::Fatal(error) if error.code == ErrorCode::PermissionDenied)
        );
        client
            .request(Request::TerminalWrite {
                terminal_id: opened.terminal_id,
                data: b"cat\n".to_vec(),
            })
            .await
            .expect("cat starts");

        let mut cursor = 0;
        loop {
            let Response::TerminalRead(read) = client
                .request(Request::TerminalRead {
                    terminal_id: opened.terminal_id,
                    after_sequence: cursor,
                    maximum_bytes: MIN_TERMINAL_POLL_BYTES,
                    wait_milliseconds: 100,
                })
                .await
                .expect("terminal drains")
            else {
                panic!("unexpected read response");
            };
            cursor = read.next_sequence;
            if read.chunks.is_empty() {
                break;
            }
        }

        let read_client = client.clone();
        let waiting = tokio::spawn(async move {
            read_client
                .request(Request::TerminalRead {
                    terminal_id: opened.terminal_id,
                    after_sequence: cursor,
                    maximum_bytes: MIN_TERMINAL_POLL_BYTES,
                    wait_milliseconds: 2_000,
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(75)).await;
        assert!(!waiting.is_finished(), "terminal read should still be idle");
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), client.request(Request::Ping))
                .await
                .expect("ping is not head-of-line blocked")
                .expect("ping succeeds"),
            Response::Pong
        );
        client
            .request(Request::TerminalWrite {
                terminal_id: opened.terminal_id,
                data: b"wake\n".to_vec(),
            })
            .await
            .expect("write succeeds while read waits");
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), waiting)
                .await
                .expect("read wakes")
                .expect("read task joins")
                .expect("read succeeds"),
            Response::TerminalRead(read) if !read.chunks.is_empty()
        ));
        client
            .request(Request::TerminalClose {
                terminal_id: opened.terminal_id,
            })
            .await
            .expect("terminal closes");
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn completed_connection_tasks_are_reaped_during_normal_operation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let state = std::sync::Arc::clone(&server.state);
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());

        for _ in 0..200 {
            let client = DaemonClient::connect(&paths, "stress", "0.1.0")
                .await
                .expect("client authenticates");
            drop(client);
        }

        for _ in 0..200 {
            if state.retained_connection_tasks.load(Ordering::SeqCst) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            state.retained_connection_tasks.load(Ordering::SeqCst),
            0,
            "completed JoinSet tasks must not be retained until daemon shutdown"
        );
        assert_eq!(state.connected_clients.load(Ordering::SeqCst), 0);

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn authentication_and_version_mismatch_fail_closed() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());

        let wrong_token = DaemonClient::connect_with(
            &paths.socket,
            ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client_name: "test".to_owned(),
                client_version: "0.1.0".to_owned(),
                authentication_token: "wrong".into(),
            },
        )
        .await;
        assert!(matches!(wrong_token, Err(IpcError::Fatal(_))));

        let token = paths.load_or_create_token().expect("token");
        let incompatible = DaemonClient::connect_with(
            &paths.socket,
            ClientHello {
                protocol_version: PROTOCOL_VERSION + 1,
                client_name: "test".to_owned(),
                client_version: "0.1.0".to_owned(),
                authentication_token: token.expose().into(),
            },
        )
        .await;
        assert!(matches!(incompatible, Err(IpcError::Fatal(_))));

        let mut authenticated = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("valid client still authenticates");
        let Response::SystemSnapshot(snapshot) = authenticated
            .request(Request::SystemSnapshot)
            .await
            .expect("snapshot")
        else {
            panic!("unexpected snapshot response");
        };
        assert_eq!(snapshot.active_terminals, 0);

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn request_before_hello_is_rejected() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let mut stream = UnixStream::connect(&paths.socket).await.expect("connect");
        write_frame(
            &mut stream,
            &ClientFrame::Request(RequestEnvelope {
                request_id: RequestId::new(),
                request: Request::Ping,
            }),
        )
        .await
        .expect("request writes");

        assert!(matches!(
            read_frame::<ServerFrame, _>(&mut stream)
                .await
                .expect("fatal frame"),
            Some(ServerFrame::Fatal(_))
        ));
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn pre_auth_connections_have_a_small_frame_limit_and_deadline() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());

        let mut oversized = UnixStream::connect(&paths.socket).await.expect("connect");
        let length = u32::try_from(MAX_HELLO_FRAME_BYTES + 1).expect("length fits");
        oversized
            .write_all(&length.to_be_bytes())
            .await
            .expect("prefix writes");
        let mut byte = [0_u8; 1];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), oversized.read(&mut byte))
                .await
                .expect("oversized connection closes")
                .expect("read succeeds"),
            0
        );

        let mut stalled = UnixStream::connect(&paths.socket).await.expect("connect");
        stalled
            .write_all(&[0])
            .await
            .expect("partial prefix writes");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), stalled.read(&mut byte))
                .await
                .expect("stalled connection closes")
                .expect("read succeeds"),
            0
        );

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn a_second_daemon_cannot_bind() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let _server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("first server binds");

        assert!(
            DaemonServer::bind(paths, DaemonConfig::default())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn idle_daemon_stops_after_configured_grace() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(
            paths.clone(),
            DaemonConfig {
                idle_shutdown_grace: Duration::from_millis(25),
                ..DaemonConfig::default()
            },
        )
        .await
        .expect("server binds");

        tokio::time::timeout(Duration::from_secs(1), server.run())
            .await
            .expect("server exits before deadline")
            .expect("server exits cleanly");
        assert!(!paths.socket.exists());
    }

    #[tokio::test]
    async fn explicit_shutdown_closes_connected_clients_without_waiting_for_grace() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let _client = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("client authenticates");

        shutdown.request();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown does not consume full grace")
            .expect("server task")
            .expect("clean shutdown");
    }

    #[tokio::test]
    async fn terminal_survives_client_disconnect_and_reports_bounded_overflow() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());

        let mut first_client = DaemonClient::connect(&paths, "first-window", "0.1.0")
            .await
            .expect("first client authenticates");
        let project_id = register_test_project(&mut first_client, temporary.path()).await;
        let Response::TerminalOpened(opened) = first_client
            .request(Request::TerminalOpen {
                project_id,
                cwd: temporary.path().to_string_lossy().into_owned(),
                columns: 80,
                rows: 24,
            })
            .await
            .expect("terminal opens")
        else {
            panic!("unexpected open response");
        };
        drop(first_client);

        let mut reconnected = DaemonClient::connect(&paths, "second-window", "0.1.0")
            .await
            .expect("second client authenticates");
        assert!(matches!(
            reconnected
                .request(Request::TerminalResize {
                    terminal_id: opened.terminal_id,
                    columns: 120,
                    rows: 40,
                })
                .await
                .expect("resize"),
            Response::TerminalResized { .. }
        ));
        reconnected
            .request(Request::TerminalWrite {
                terminal_id: opened.terminal_id,
                data: b"yes maestro-flood | head -c 1500000\n".to_vec(),
            })
            .await
            .expect("flood command writes");

        let mut saw_overflow = false;
        for _ in 0..500 {
            let Response::TerminalRead(read) = reconnected
                .request(Request::TerminalRead {
                    terminal_id: opened.terminal_id,
                    after_sequence: 0,
                    maximum_bytes: MIN_TERMINAL_POLL_BYTES,
                    wait_milliseconds: 0,
                })
                .await
                .expect("poll")
            else {
                panic!("unexpected read response");
            };
            if read.overflowed {
                assert!(read.dropped_through_sequence.is_some());
                assert!(read.next_sequence > 0);
                saw_overflow = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            saw_overflow,
            "terminal output did not reach the bounded overflow threshold"
        );

        let Response::TerminalClosed(status) = reconnected
            .request(Request::TerminalClose {
                terminal_id: opened.terminal_id,
            })
            .await
            .expect("terminal closes")
        else {
            panic!("unexpected close response");
        };
        assert_eq!(status.state, TerminalState::Closed);
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn terminal_limits_input_count_and_working_directory() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(
            paths.clone(),
            DaemonConfig {
                maximum_terminals: 1,
                ..DaemonConfig::default()
            },
        )
        .await
        .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let mut client = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("client authenticates");
        let project_id = register_test_project(&mut client, temporary.path()).await;

        let invalid_path = client
            .request(Request::TerminalOpen {
                project_id,
                cwd: "relative/path".to_owned(),
                columns: 80,
                rows: 24,
            })
            .await;
        assert!(matches!(
            invalid_path,
            Err(IpcError::Fatal(error)) if error.code == ErrorCode::InvalidPath
        ));

        let Response::TerminalOpened(opened) = client
            .request(Request::TerminalOpen {
                project_id,
                cwd: temporary.path().to_string_lossy().into_owned(),
                columns: 80,
                rows: 24,
            })
            .await
            .expect("terminal opens")
        else {
            panic!("unexpected open response");
        };
        let too_large = client
            .request(Request::TerminalWrite {
                terminal_id: opened.terminal_id,
                data: vec![b'x'; MAX_TERMINAL_INPUT_BYTES + 1],
            })
            .await;
        assert!(matches!(
            too_large,
            Err(IpcError::Fatal(error)) if error.code == ErrorCode::InputTooLarge
        ));
        let over_limit = client
            .request(Request::TerminalOpen {
                project_id,
                cwd: temporary.path().to_string_lossy().into_owned(),
                columns: 80,
                rows: 24,
            })
            .await;
        assert!(matches!(
            over_limit,
            Err(IpcError::Fatal(error)) if error.code == ErrorCode::TerminalLimitReached
        ));
        client
            .request(Request::TerminalClose {
                terminal_id: opened.terminal_id,
            })
            .await
            .expect("terminal closes");

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn terminal_reports_natural_exit_state_and_code() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let mut client = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("client authenticates");
        let project_id = register_test_project(&mut client, temporary.path()).await;
        let Response::TerminalOpened(opened) = client
            .request(Request::TerminalOpen {
                project_id,
                cwd: temporary.path().to_string_lossy().into_owned(),
                columns: 80,
                rows: 24,
            })
            .await
            .expect("terminal opens")
        else {
            panic!("unexpected open response");
        };
        client
            .request(Request::TerminalWrite {
                terminal_id: opened.terminal_id,
                data: b"exit 7\n".to_vec(),
            })
            .await
            .expect("exit writes");

        let mut observed = None;
        for _ in 0..200 {
            let Response::TerminalState(status) = client
                .request(Request::TerminalState {
                    terminal_id: opened.terminal_id,
                })
                .await
                .expect("state reads")
            else {
                panic!("unexpected state response");
            };
            if status.state == TerminalState::Exited {
                observed = status.exit;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(observed.and_then(|exit| exit.code), Some(7));
        let Response::TerminalClosed(closed) = client
            .request(Request::TerminalClose {
                terminal_id: opened.terminal_id,
            })
            .await
            .expect("terminal closes")
        else {
            panic!("unexpected close response");
        };
        assert_eq!(closed.state, TerminalState::Closed);
        assert_eq!(closed.exit.and_then(|exit| exit.code), Some(7));

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn naturally_exited_terminal_does_not_block_idle_shutdown() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(
            paths.clone(),
            DaemonConfig {
                maximum_processes: 1,
                maximum_terminals: 1,
                idle_shutdown_grace: Duration::from_millis(50),
                ..DaemonConfig::default()
            },
        )
        .await
        .expect("server binds");
        let task = tokio::spawn(server.run());
        let mut client = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("client authenticates");
        let project_id = register_test_project(&mut client, temporary.path()).await;
        let Response::TerminalOpened(opened) = client
            .request(Request::TerminalOpen {
                project_id,
                cwd: temporary.path().to_string_lossy().into_owned(),
                columns: 80,
                rows: 24,
            })
            .await
            .expect("terminal opens")
        else {
            panic!("unexpected open response");
        };
        client
            .request(Request::TerminalWrite {
                terminal_id: opened.terminal_id,
                data: b"exit 0\n".to_vec(),
            })
            .await
            .expect("exit writes");

        for _ in 0..200 {
            let Response::TerminalState(status) = client
                .request(Request::TerminalState {
                    terminal_id: opened.terminal_id,
                })
                .await
                .expect("status reads")
            else {
                panic!("unexpected status response");
            };
            if status.state == TerminalState::Exited {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(client);

        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("idle daemon exits")
            .expect("server task")
            .expect("clean shutdown");
        assert!(!paths.socket.exists());
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "structured events and opted-in raw IPC are verified together"
    )]
    async fn fake_session_streams_redacted_events_through_authenticated_ipc() {
        let Some(fake_agent) = std::env::var_os("MAESTRO_FAKE_AGENT").map(PathBuf::from) else {
            return;
        };
        let temporary = tempfile::tempdir().expect("temporary directory");
        let project_root = temporary.path().join("project");
        std::fs::create_dir(&project_root).expect("project directory creates");
        let paths = DaemonPaths::isolated(temporary.path().join("daemon"));
        let server = DaemonServer::bind(
            paths.clone(),
            DaemonConfig {
                fake_agent_executable: Some(fake_agent),
                idle_shutdown_grace: Duration::from_secs(5),
                ..DaemonConfig::default()
            },
        )
        .await
        .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let mut client = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("client authenticates");
        let project_id = ProjectId::new();
        client
            .request(Request::ProjectRegister {
                project_id,
                display_name: "Fixture".to_owned(),
                roots: vec![project_root.to_string_lossy().into_owned()],
            })
            .await
            .expect("project registers");
        let Response::SessionRunStarted(started) = client
            .request(Request::FakeSessionStart {
                project_id,
                scenario: "structured/happy".to_owned(),
                binding: None,
                volume: None,
                capture_raw_protocol: true,
            })
            .await
            .expect("fake session starts")
        else {
            panic!("unexpected fake-session response");
        };

        let mut cursor = 0;
        let mut events = Vec::new();
        for _ in 0..20 {
            let Response::SessionEvents(batch) = client
                .request(Request::SessionEventsRead {
                    session_id: started.session_id,
                    after_sequence: cursor,
                    maximum_events: 128,
                    wait_milliseconds: 500,
                })
                .await
                .expect("session events read")
            else {
                panic!("unexpected session-event response");
            };
            cursor = batch.next_sequence;
            events.extend(batch.events);
            if batch.state == maestro_domain::SessionState::Completed {
                break;
            }
        }
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].sequence < pair[1].sequence)
        );
        assert!(
            events
                .iter()
                .any(|event| event.event.kind == "message_delta")
        );
        let encoded = serde_json::to_string(&events).expect("events serialize");
        assert!(!encoded.contains("sk-test-secret"));

        let mut raw_offset = 0;
        let mut raw_bytes = Vec::new();
        let mut final_metadata = None;
        for _ in 0..64 {
            let Response::SessionRaw(batch) = client
                .request(Request::SessionRawRead {
                    session_id: started.session_id,
                    run_id: started.run_id,
                    after_offset: raw_offset,
                    maximum_bytes: 64,
                })
                .await
                .expect("raw protocol reads through authenticated IPC")
            else {
                panic!("unexpected raw-protocol response");
            };
            let debug = format!("{batch:?}");
            assert!(debug.contains("[SENSITIVE]"));
            raw_bytes.extend_from_slice(batch.data.expose());
            raw_offset = batch.next_offset;
            let at_end = batch.next_offset == batch.captured_bytes;
            final_metadata = Some((
                batch.captured_bytes,
                batch.observed_bytes,
                batch.truncated,
                batch.complete,
            ));
            if at_end && batch.complete {
                break;
            }
        }
        let (captured_bytes, observed_bytes, truncated, complete) =
            final_metadata.expect("raw metadata returned");
        assert_eq!(
            u64::try_from(raw_bytes.len()).expect("fixture size fits"),
            captured_bytes
        );
        assert_eq!(observed_bytes, captured_bytes);
        assert!(!truncated);
        assert!(complete);
        assert_eq!(
            raw_bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            8
        );

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn active_structured_session_attach_preserves_run_and_enforces_project_and_mode() {
        let Some(fake_agent) = std::env::var_os("MAESTRO_FAKE_AGENT").map(PathBuf::from) else {
            return;
        };
        let temporary = tempfile::tempdir().expect("temporary directory");
        let project_root = temporary.path().join("project");
        std::fs::create_dir(&project_root).expect("project directory creates");
        let paths = DaemonPaths::isolated(temporary.path().join("daemon"));
        let server = DaemonServer::bind(
            paths.clone(),
            DaemonConfig {
                fake_agent_executable: Some(fake_agent),
                idle_shutdown_grace: Duration::from_secs(5),
                ..DaemonConfig::default()
            },
        )
        .await
        .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let mut client = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("client authenticates");
        let project_id = register_test_project(&mut client, &project_root).await;
        let other_project = register_test_project(&mut client, &project_root).await;
        let Response::SessionRunStarted(started) = client
            .request(Request::FakeSessionStart {
                project_id,
                scenario: "structured/stall".to_owned(),
                binding: None,
                volume: None,
                capture_raw_protocol: false,
            })
            .await
            .expect("structured session starts")
        else {
            panic!("unexpected structured start response");
        };

        let Response::SessionRunAttached(attached) = client
            .request(Request::SessionStructuredAttach {
                session_id: started.session_id,
                project_id,
            })
            .await
            .expect("owner attaches to active structured run")
        else {
            panic!("unexpected structured attach response");
        };
        assert_eq!(attached.session_id, started.session_id);
        assert_eq!(attached.run_id, started.run_id);
        assert_eq!(attached.process_id, started.process_id);

        let denied = client
            .request(Request::SessionStructuredAttach {
                session_id: started.session_id,
                project_id: other_project,
            })
            .await
            .expect_err("another project cannot attach by session identifier");
        assert!(
            matches!(denied, IpcError::Fatal(error) if error.code == ErrorCode::PermissionDenied)
        );
        client
            .request(Request::StopSession {
                session_id: started.session_id,
            })
            .await
            .expect("structured session stops");

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "the authenticated exact-TUI contract is exercised end to end in one fixture"
    )]
    async fn fake_exact_tui_is_a_persisted_logical_session_over_the_daemon_pty() {
        let Some(fake_agent) = std::env::var_os("MAESTRO_FAKE_AGENT").map(PathBuf::from) else {
            return;
        };
        let temporary = tempfile::tempdir().expect("temporary directory");
        let project_root = temporary.path().join("project");
        std::fs::create_dir(&project_root).expect("project directory creates");
        let paths = DaemonPaths::isolated(temporary.path().join("daemon"));
        let server = DaemonServer::bind(
            paths.clone(),
            DaemonConfig {
                fake_agent_executable: Some(fake_agent),
                idle_shutdown_grace: Duration::from_secs(5),
                ..DaemonConfig::default()
            },
        )
        .await
        .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let mut client = DaemonClient::connect(&paths, "test", "0.1.0")
            .await
            .expect("client authenticates");
        let project_id = ProjectId::new();
        client
            .request(Request::ProjectRegister {
                project_id,
                display_name: "Fixture".to_owned(),
                roots: vec![project_root.to_string_lossy().into_owned()],
            })
            .await
            .expect("project registers");

        let Response::SessionTerminalStarted(started) = client
            .request(Request::FakeTuiStart {
                project_id,
                scenario: "tui/vt-baseline".to_owned(),
                columns: 80,
                rows: 24,
            })
            .await
            .expect("fake TUI starts")
        else {
            panic!("unexpected fake-TUI response");
        };
        let Response::SessionSnapshot(snapshot) = client
            .request(Request::SessionSnapshot {
                session_id: started.session_id,
            })
            .await
            .expect("TUI session snapshot reads")
        else {
            panic!("unexpected session snapshot response");
        };
        assert_eq!(snapshot.active_run_id, Some(started.terminal.run_id));
        assert_eq!(snapshot.state, maestro_domain::SessionState::Running);
        let Response::SessionList(session_index) = client
            .request(Request::SessionList {
                project_id,
                maximum_sessions: 20,
            })
            .await
            .expect("project session index reads")
        else {
            panic!("unexpected session-list response");
        };
        assert_eq!(session_index.len(), 1);
        assert_eq!(session_index[0].session_id, started.session_id);
        assert_eq!(
            session_index[0].active_run_id,
            Some(started.terminal.run_id)
        );
        let Response::SessionTerminalAttached(attached) = client
            .request(Request::SessionTerminalAttach {
                session_id: started.session_id,
                project_id,
            })
            .await
            .expect("a new view reattaches to the existing TUI")
        else {
            panic!("unexpected session-terminal attach response");
        };
        assert_eq!(attached.session_id, started.session_id);
        assert_eq!(attached.terminal, started.terminal);
        let wrong_mode = client
            .request(Request::SessionStructuredAttach {
                session_id: started.session_id,
                project_id,
            })
            .await
            .expect_err("an exact-TUI session cannot use structured attachment");
        assert!(
            matches!(wrong_mode, IpcError::Fatal(error) if error.code == ErrorCode::PermissionDenied)
        );

        let mut terminal_cursor = 0;
        let mut output = Vec::new();
        for _ in 0..20 {
            let Response::TerminalRead(read) = client
                .request(Request::TerminalRead {
                    terminal_id: started.terminal.terminal_id,
                    after_sequence: terminal_cursor,
                    maximum_bytes: MIN_TERMINAL_POLL_BYTES,
                    wait_milliseconds: 250,
                })
                .await
                .expect("TUI output reads")
            else {
                panic!("unexpected terminal read response");
            };
            terminal_cursor = read.next_sequence;
            for chunk in read.chunks {
                output.extend(chunk.data);
            }
            if String::from_utf8_lossy(&output).contains("Maestro fake TUI") {
                break;
            }
        }
        assert!(String::from_utf8_lossy(&output).contains("Maestro fake TUI"));
        client
            .request(Request::TerminalWrite {
                terminal_id: started.terminal.terminal_id,
                data: b"hello from test\n".to_vec(),
            })
            .await
            .expect("TUI input writes");

        let mut event_cursor = 0;
        let mut events = Vec::new();
        for _ in 0..40 {
            let Response::SessionEvents(batch) = client
                .request(Request::SessionEventsRead {
                    session_id: started.session_id,
                    after_sequence: event_cursor,
                    maximum_events: 16,
                    wait_milliseconds: 50,
                })
                .await
                .expect("TUI lifecycle events read")
            else {
                panic!("unexpected session events response");
            };
            event_cursor = batch.next_sequence;
            events.extend(batch.events);
            if batch.state == maestro_domain::SessionState::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            events
                .iter()
                .map(|event| event.event.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["run_started", "process_exited"]
        );

        let error = client
            .request(Request::FakeTuiStart {
                project_id,
                scenario: "structured/happy".to_owned(),
                columns: 80,
                rows: 24,
            })
            .await
            .expect_err("structured scenario is not accepted by trusted TUI path");
        assert!(matches!(error, IpcError::Fatal(error) if error.code == ErrorCode::InvalidRequest));

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }
}
