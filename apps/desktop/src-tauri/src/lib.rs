use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::Path,
    sync::Mutex,
    time::Duration,
};

use maestro_domain::{ErrorCode, MaestroError, ProjectId, RequestId, RunId, SessionId, TerminalId};
use maestro_protocol::{
    ProjectBranchState, ProjectDiffScope, ProjectDirectoryPage, ProjectFileSaved, ProjectGitDiff,
    ProjectGitStatusEntry, ProjectSearchOptions, ProjectSearchResult, ProjectTextFile,
    ProjectWorktree, RecentProject, Request, Response, SensitiveString, SessionEventBatch,
    SessionIndexEntry, SessionPermissionDecision, SessionRawBatch, SessionRunAttached,
    SessionRunStarted, SessionSnapshot, SessionTerminalAttached, SessionTerminalStarted,
    StorageStatus, StorageUnlockMode, TerminalIndexEntry, TerminalOpened, TerminalReadResult,
    TerminalStatus,
};
#[cfg(test)]
use maestrod::DaemonClient;
use maestrod::{DaemonPaths, MultiplexedDaemonClient};
use serde::{Deserialize, Serialize};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

mod background_menu;
mod daemon_launcher;

use daemon_launcher::DaemonLauncher;

const TERMINAL_READ_WAIT_MILLISECONDS: u32 = 20_000;
const SESSION_READ_WAIT_MILLISECONDS: u32 = 20_000;
const SESSION_EVENTS_PER_READ: usize = 256;
const MAXIMUM_RECENT_PROJECTS: usize = 100;
const MAXIMUM_WINDOW_LAYOUT_BYTES: usize = 16 * 1024;
const MAXIMUM_TERMINAL_LINK_BYTES: usize = 2_048;
const PROJECT_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(20);
const SHORTCUT_SETTING_SCOPE: &str = "global";
const SHORTCUT_SETTING_SCOPE_REFERENCE: &str = "";
const SHORTCUT_SETTING_KEY: &str = "keyboard.shortcuts";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DaemonSnapshot {
    status: DaemonStatus,
    detail: String,
    storage_status: DaemonStorageStatus,
    storage_schema_version: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackgroundStopSummary {
    structured_sessions_stopped: u32,
    terminals_closed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum DaemonStatus {
    Connected,
    NotConnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum DaemonStorageStatus {
    Ready,
    PassphraseCreateRequired,
    PassphraseUnlockRequired,
    Unavailable,
}

impl From<StorageStatus> for DaemonStorageStatus {
    fn from(status: StorageStatus) -> Self {
        match status {
            StorageStatus::Ready => Self::Ready,
            StorageStatus::PassphraseRequired {
                mode: StorageUnlockMode::Create,
            } => Self::PassphraseCreateRequired,
            StorageStatus::PassphraseRequired {
                mode: StorageUnlockMode::Unlock,
            } => Self::PassphraseUnlockRequired,
            StorageStatus::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopCommandError {
    code: ErrorCode,
    message: String,
    retryable: bool,
    user_action: Option<String>,
    correlation_id: String,
    details: Option<serde_json::Value>,
}

impl From<MaestroError> for DesktopCommandError {
    fn from(error: MaestroError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            retryable: error.retryable,
            user_action: error.user_action,
            correlation_id: error.correlation_id.to_string(),
            details: error.details,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectSelection {
    id: ProjectId,
    name: String,
    roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShortcutBindings {
    open_new_window: String,
    open_project: String,
    toggle_bottom_panel: String,
    toggle_command_palette: String,
    toggle_inspector: String,
    toggle_sidebar: String,
}

impl ShortcutBindings {
    fn values(&self) -> [&str; 6] {
        [
            &self.open_new_window,
            &self.open_project,
            &self.toggle_bottom_panel,
            &self.toggle_command_palette,
            &self.toggle_inspector,
            &self.toggle_sidebar,
        ]
    }

    fn validate(&self) -> Result<(), DesktopCommandError> {
        let mut unique = HashSet::with_capacity(self.values().len());
        for shortcut in self.values() {
            if !is_canonical_shortcut(shortcut) {
                return Err(invalid_request(
                    "Every shortcut must use canonical Mod+[Shift+] plus one letter or number.",
                ));
            }
            if !unique.insert(shortcut) {
                return Err(invalid_request("Keyboard shortcuts cannot conflict."));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedProject {
    canonical_path: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectGrant {
    canonical_path: String,
    persisted_project_id: ProjectId,
    window_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalGrant {
    project_grant: ProjectId,
    persisted_terminal_id: TerminalId,
    discovery_only: bool,
    window_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionGrant {
    project_grant: ProjectId,
    terminal_id: Option<TerminalId>,
    window_label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectRegistrationAttempt {
    project_id: ProjectId,
    correlation_id: RequestId,
}

#[derive(Debug)]
struct DesktopHostState {
    projects: Mutex<HashMap<ProjectId, ProjectGrant>>,
    sessions: Mutex<HashMap<SessionId, SessionGrant>>,
    terminals: Mutex<HashMap<TerminalId, TerminalGrant>>,
    project_registrations: Mutex<HashMap<Vec<String>, ProjectId>>,
    daemon: tokio::sync::Mutex<Option<MultiplexedDaemonClient>>,
    daemon_launcher: DaemonLauncher,
}

impl Default for DesktopHostState {
    fn default() -> Self {
        Self::new(DaemonLauncher::discover(None))
    }
}

impl DesktopHostState {
    fn new(daemon_launcher: DaemonLauncher) -> Self {
        Self {
            projects: Mutex::default(),
            sessions: Mutex::default(),
            terminals: Mutex::default(),
            project_registrations: Mutex::default(),
            daemon: tokio::sync::Mutex::default(),
            daemon_launcher,
        }
    }

    async fn request_daemon(
        &self,
        paths: &DaemonPaths,
        request: Request,
    ) -> Result<Response, DesktopCommandError> {
        self.request_daemon_correlated(paths, RequestId::new(), request)
            .await
    }

    async fn request_daemon_correlated(
        &self,
        paths: &DaemonPaths,
        request_id: RequestId,
        request: Request,
    ) -> Result<Response, DesktopCommandError> {
        let client = {
            let mut connection = self.daemon.lock().await;
            if connection.is_none() {
                let client =
                    self.daemon_launcher
                        .connect(paths)
                        .await
                        .map_err(|error| match error {
                            maestrod::IpcError::Fatal(error) => error.into(),
                            _ => daemon_unavailable(),
                        })?;
                *connection = Some(client);
            }
            connection
                .as_ref()
                .cloned()
                .ok_or_else(daemon_unavailable)?
        };
        match client.request_correlated(request_id, request).await {
            Ok(response) => Ok(response),
            Err(maestrod::IpcError::Fatal(error)) => Err(error.into()),
            Err(_) => {
                self.invalidate_daemon_client(&client).await;
                Err(daemon_unavailable())
            }
        }
    }

    async fn invalidate_daemon_client(&self, failed: &MultiplexedDaemonClient) {
        let mut connection = self.daemon.lock().await;
        if connection
            .as_ref()
            .is_some_and(|current| current.is_same_connection(failed))
        {
            *connection = None;
        }
    }

    fn begin_project_registration(
        &self,
        canonical_roots: &[String],
    ) -> Result<ProjectRegistrationAttempt, DesktopCommandError> {
        let key = project_registration_key(canonical_roots);
        let mut registrations = self.project_registrations.lock().map_err(|_| {
            internal_error("Maestro could not prepare the selected project registration.")
        })?;
        let project_id = *registrations.entry(key).or_insert_with(ProjectId::new);
        Ok(ProjectRegistrationAttempt {
            project_id,
            correlation_id: RequestId::new(),
        })
    }

    fn finish_project_registration(
        &self,
        canonical_roots: &[String],
        attempt: ProjectRegistrationAttempt,
    ) {
        let key = project_registration_key(canonical_roots);
        if let Ok(mut registrations) = self.project_registrations.lock()
            && registrations.get(&key) == Some(&attempt.project_id)
        {
            registrations.remove(&key);
        }
    }

    fn grant_registered_project(
        &self,
        persisted_project_id: ProjectId,
        name: String,
        canonical_roots: Vec<String>,
        window_label: &str,
    ) -> Result<ProjectSelection, DesktopCommandError> {
        let canonical_path = canonical_roots.first().cloned().ok_or_else(|| {
            invalid_path("The saved project does not contain a usable workspace root.")
        })?;
        let (grant_id, replaced_projects) = {
            let mut projects = self
                .projects
                .lock()
                .map_err(|_| internal_error("Maestro could not register the selected project."))?;
            let replaced = projects
                .iter()
                .filter_map(|(project_id, grant)| {
                    (grant.window_label == window_label).then_some(*project_id)
                })
                .collect::<Vec<_>>();
            projects.retain(|_, grant| grant.window_label != window_label);
            let grant_id = loop {
                let candidate = ProjectId::new();
                if candidate != persisted_project_id && !projects.contains_key(&candidate) {
                    break candidate;
                }
            };
            projects.insert(
                grant_id,
                ProjectGrant {
                    canonical_path,
                    persisted_project_id,
                    window_label: window_label.to_owned(),
                },
            );
            (grant_id, replaced)
        };
        if !replaced_projects.is_empty() {
            self.terminals
                .lock()
                .map_err(|_| {
                    internal_error("Maestro could not revoke the previous project grant.")
                })?
                .retain(|_, grant| !replaced_projects.contains(&grant.project_grant));
            self.sessions
                .lock()
                .map_err(|_| {
                    internal_error("Maestro could not revoke the previous session grant.")
                })?
                .retain(|_, grant| !replaced_projects.contains(&grant.project_grant));
        }
        Ok(ProjectSelection {
            id: grant_id,
            name,
            roots: canonical_roots,
        })
    }

    fn resolve_project(
        &self,
        id: ProjectId,
        window_label: &str,
    ) -> Result<ProjectGrant, DesktopCommandError> {
        let projects = self.projects.lock().map_err(|_| project_grant_denied())?;
        let grant = projects.get(&id).ok_or_else(project_grant_denied)?;
        if grant.window_label != window_label {
            return Err(project_grant_denied());
        }
        Ok(grant.clone())
    }

    fn register_terminal(
        &self,
        persisted_terminal_id: TerminalId,
        project_id: ProjectId,
        window_label: &str,
        discovery_only: bool,
    ) -> Result<TerminalId, DesktopCommandError> {
        let mut terminals = self.terminals.lock().map_err(|_| terminal_grant_denied())?;
        terminals.retain(|_, grant| {
            if grant.window_label != window_label
                || grant.persisted_terminal_id != persisted_terminal_id
            {
                true
            } else {
                discovery_only && !grant.discovery_only
            }
        });
        let grant_id = loop {
            let candidate = TerminalId::new();
            if candidate != persisted_terminal_id && !terminals.contains_key(&candidate) {
                break candidate;
            }
        };
        terminals.insert(
            grant_id,
            TerminalGrant {
                project_grant: project_id,
                persisted_terminal_id,
                discovery_only,
                window_label: window_label.to_owned(),
            },
        );
        Ok(grant_id)
    }

    fn authorize_terminal(
        &self,
        terminal_id: TerminalId,
        window_label: &str,
    ) -> Result<TerminalGrant, DesktopCommandError> {
        let terminals = self.terminals.lock().map_err(|_| terminal_grant_denied())?;
        let grant = terminals
            .get(&terminal_id)
            .ok_or_else(terminal_grant_denied)?;
        if grant.window_label != window_label {
            return Err(terminal_grant_denied());
        }
        Ok(grant.clone())
    }

    fn unregister_terminal(&self, terminal_id: TerminalId) {
        if let Ok(mut terminals) = self.terminals.lock() {
            terminals.remove(&terminal_id);
        }
    }

    fn unregister_terminal_process(&self, persisted_terminal_id: TerminalId) {
        if let Ok(mut terminals) = self.terminals.lock() {
            terminals.retain(|_, grant| grant.persisted_terminal_id != persisted_terminal_id);
        }
    }

    fn register_session(
        &self,
        session_id: SessionId,
        project_grant: ProjectId,
        window_label: &str,
    ) -> Result<(), DesktopCommandError> {
        let mut sessions = self.sessions.lock().map_err(|_| session_grant_denied())?;
        let terminal_id = sessions
            .get(&session_id)
            .filter(|grant| {
                grant.project_grant == project_grant && grant.window_label == window_label
            })
            .and_then(|grant| grant.terminal_id);
        sessions.insert(
            session_id,
            SessionGrant {
                project_grant,
                terminal_id,
                window_label: window_label.to_owned(),
            },
        );
        Ok(())
    }

    fn register_tui_session(
        &self,
        session_id: SessionId,
        terminal_id: TerminalId,
        project_grant: ProjectId,
        window_label: &str,
    ) -> Result<TerminalId, DesktopCommandError> {
        let terminal_grant =
            self.register_terminal(terminal_id, project_grant, window_label, false)?;
        let result = self.sessions.lock().map_err(|_| session_grant_denied());
        let Ok(mut sessions) = result else {
            self.unregister_terminal(terminal_grant);
            return Err(session_grant_denied());
        };
        let previous_terminal = sessions
            .insert(
                session_id,
                SessionGrant {
                    project_grant,
                    terminal_id: Some(terminal_grant),
                    window_label: window_label.to_owned(),
                },
            )
            .and_then(|grant| grant.terminal_id);
        drop(sessions);
        if let Some(previous_terminal) = previous_terminal
            && previous_terminal != terminal_grant
        {
            self.unregister_terminal(previous_terminal);
        }
        Ok(terminal_grant)
    }

    fn unregister_session(&self, session_id: SessionId) -> Option<TerminalId> {
        self.sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&session_id))
            .and_then(|grant| grant.terminal_id)
    }

    fn authorize_session(
        &self,
        session_id: SessionId,
        window_label: &str,
    ) -> Result<SessionGrant, DesktopCommandError> {
        let sessions = self.sessions.lock().map_err(|_| session_grant_denied())?;
        let grant = sessions.get(&session_id).ok_or_else(session_grant_denied)?;
        if grant.window_label != window_label {
            return Err(session_grant_denied());
        }
        Ok(grant.clone())
    }

    fn revoke_window(&self, window_label: &str) {
        if let Ok(mut projects) = self.projects.lock() {
            projects.retain(|_, grant| grant.window_label != window_label);
        }
        if let Ok(mut terminals) = self.terminals.lock() {
            terminals.retain(|_, grant| grant.window_label != window_label);
        }
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.retain(|_, grant| grant.window_label != window_label);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SystemSnapshot {
    app_version: &'static str,
    platform: &'static str,
    architecture: &'static str,
    window_label: String,
    daemon: DaemonSnapshot,
}

impl SystemSnapshot {
    #[cfg(test)]
    async fn capture_with_paths(paths: &DaemonPaths) -> Self {
        let daemon = match request_daemon(paths, Request::SystemSnapshot).await {
            Ok(Response::SystemSnapshot(snapshot)) => DaemonSnapshot {
                status: DaemonStatus::Connected,
                detail: daemon_detail(&snapshot),
                storage_status: snapshot.storage.into(),
                storage_schema_version: snapshot.storage_schema_version,
            },
            _ => DaemonSnapshot {
                status: DaemonStatus::NotConnected,
                detail: "The Maestro service is not running or could not be authenticated."
                    .to_owned(),
                storage_status: DaemonStorageStatus::Unavailable,
                storage_schema_version: None,
            },
        };
        Self {
            app_version: env!("CARGO_PKG_VERSION"),
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            window_label: "test".to_owned(),
            daemon,
        }
    }

    async fn capture_with_host(
        state: &DesktopHostState,
        paths: &DaemonPaths,
        window_label: &str,
    ) -> Self {
        let daemon = match state.request_daemon(paths, Request::SystemSnapshot).await {
            Ok(Response::SystemSnapshot(snapshot)) => DaemonSnapshot {
                status: DaemonStatus::Connected,
                detail: daemon_detail(&snapshot),
                storage_status: snapshot.storage.into(),
                storage_schema_version: snapshot.storage_schema_version,
            },
            _ => DaemonSnapshot {
                status: DaemonStatus::NotConnected,
                detail: "The Maestro service is not running or could not be authenticated."
                    .to_owned(),
                storage_status: DaemonStorageStatus::Unavailable,
                storage_schema_version: None,
            },
        };
        Self {
            app_version: env!("CARGO_PKG_VERSION"),
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            window_label: window_label.to_owned(),
            daemon,
        }
    }

    fn offline(window_label: &str) -> Self {
        Self {
            app_version: env!("CARGO_PKG_VERSION"),
            platform: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            window_label: window_label.to_owned(),
            daemon: DaemonSnapshot {
                status: DaemonStatus::NotConnected,
                detail: "The Maestro service is not running or could not be authenticated."
                    .to_owned(),
                storage_status: DaemonStorageStatus::Unavailable,
                storage_schema_version: None,
            },
        }
    }
}

fn daemon_detail(snapshot: &maestro_protocol::SystemSnapshot) -> String {
    match snapshot.storage {
        StorageStatus::Ready => format!(
            "Maestro service {} is connected ({} terminals active).",
            snapshot.daemon_version, snapshot.active_terminals
        ),
        StorageStatus::PassphraseRequired { .. } => {
            "Encrypted storage requires a passphrase before Maestro can continue.".to_owned()
        }
        StorageStatus::Unavailable => {
            "Encrypted storage is unavailable; Maestro has not opened project data.".to_owned()
        }
    }
}

/// Returns non-sensitive desktop-host state used during application bootstrap.
///
/// This command starts the packaged daemon on demand when no authenticated
/// instance is available. The webview never receives process handles,
/// authentication material, or database keys.
#[tauri::command]
async fn system_snapshot(
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SystemSnapshot, DesktopCommandError> {
    Ok(match DaemonPaths::discover() {
        Ok(paths) => SystemSnapshot::capture_with_host(&state, &paths, window.label()).await,
        Err(_) => SystemSnapshot::offline(window.label()),
    })
}

/// Opens another native Maestro window. Each window receives an independent
/// project capability and frontend state while sharing the same daemon.
#[tauri::command]
#[expect(
    clippy::needless_pass_by_value,
    reason = "Tauri injects AppHandle as an owned command parameter"
)]
fn open_new_window(app: tauri::AppHandle) -> Result<String, DesktopCommandError> {
    let label = format!("project-{}", uuid::Uuid::new_v4());
    let window = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("index.html".into()))
        .title("Maestro")
        .inner_size(1360.0, 860.0)
        .min_inner_size(960.0, 640.0)
        .resizable(true)
        .build()
        .map_err(|_| internal_error("Maestro could not create a new window."))?;
    let _ = window.set_focus();
    Ok(label)
}

/// Explicitly terminates every daemon-owned structured run and PTY process.
#[tauri::command]
async fn background_stop_all(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<BackgroundStopSummary, DesktopCommandError> {
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state.request_daemon(&paths, Request::StopAllWork).await? {
        Response::BackgroundWorkStopped {
            structured_sessions_stopped,
            terminals_closed,
        } => Ok(BackgroundStopSummary {
            structured_sessions_stopped,
            terminals_closed,
        }),
        _ => Err(unexpected_response()),
    }
}

/// Unlocks only Maestro-owned encrypted storage. The passphrase is wrapped in
/// a redacting, zeroizing protocol value and is never persisted by the host.
#[tauri::command]
async fn storage_unlock(
    passphrase: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SystemSnapshot, DesktopCommandError> {
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::StorageUnlock {
                passphrase: SensitiveString::new(passphrase),
            },
        )
        .await?
    {
        Response::StorageUnlocked => {
            Ok(SystemSnapshot::capture_with_host(&state, &paths, window.label()).await)
        }
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_recent_list(
    maximum_projects: usize,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<RecentProject>, DesktopCommandError> {
    validate_recent_project_limit(maximum_projects)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(&paths, Request::ProjectRecentList { maximum_projects })
        .await?
    {
        Response::ProjectRecentList(projects) => Ok(projects),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn open_recent_project(
    project_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ProjectSelection, DesktopCommandError> {
    let project_id = parse_project_id_for_request(&project_id)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    open_recent_project_with_paths(&paths, &state, project_id, window.label()).await
}

#[tauri::command]
async fn project_set_favorite(
    project_id: String,
    favorite: bool,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<bool, DesktopCommandError> {
    let project_id = parse_project_id_for_request(&project_id)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectSetFavorite {
                project_id,
                favorite,
            },
        )
        .await?
    {
        Response::ProjectFavoriteUpdated {
            project_id: updated_project_id,
            favorite: updated_favorite,
        } if updated_project_id == project_id && updated_favorite == favorite => {
            Ok(updated_favorite)
        }
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_window_layout_load(
    project_grant: String,
    window_key: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Option<String>, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    validate_window_key(&window_key, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectWindowLayoutLoad {
                project_id,
                window_key: window_key.clone(),
            },
        )
        .await?
    {
        Response::ProjectWindowLayout(layout)
            if layout.project_id == project_id && layout.window_key == window_key =>
        {
            Ok(layout.layout_json)
        }
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_window_layout_save(
    project_grant: String,
    window_key: String,
    layout_json: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    validate_window_key(&window_key, window.label())?;
    validate_window_layout_json(&layout_json)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectWindowLayoutSave {
                project_id,
                window_key: window_key.clone(),
                layout_json,
            },
        )
        .await?
    {
        Response::ProjectWindowLayoutSaved {
            project_id: saved_project_id,
            window_key: saved_window_key,
        } if saved_project_id == project_id && saved_window_key == window_key => Ok(()),
        _ => Err(unexpected_response()),
    }
}

/// Loads global keyboard shortcuts from Maestro's encrypted application
/// settings. Vendor configuration and credentials are never involved.
#[tauri::command]
async fn shortcut_settings_load(
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Option<ShortcutBindings>, DesktopCommandError> {
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    shortcut_settings_load_with_paths(&paths, &state).await
}

/// Validates and saves the complete keyboard shortcut object in Maestro's
/// encrypted application settings.
#[tauri::command]
async fn shortcut_settings_save(
    bindings: ShortcutBindings,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    shortcut_settings_save_with_paths(&paths, &state, bindings).await
}

/// Starts Maestro's deterministic local fake agent. This is an integration
/// harness only and never represents an installed vendor CLI.
#[tauri::command]
async fn fake_session_start(
    project_grant: String,
    scenario: String,
    binding: Option<String>,
    volume: Option<usize>,
    capture_raw_protocol: bool,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SessionRunStarted, DesktopCommandError> {
    let project_grant = parse_project_id(&project_grant)?;
    let project_id = state
        .resolve_project(project_grant, window.label())?
        .persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::SessionRunStarted(started) = state
        .request_daemon(
            &paths,
            Request::FakeSessionStart {
                project_id,
                scenario,
                binding,
                volume,
                capture_raw_protocol,
            },
        )
        .await?
    else {
        return Err(unexpected_response());
    };
    if let Err(error) = state.register_session(started.session_id, project_grant, window.label()) {
        let _ = state
            .request_daemon(
                &paths,
                Request::StopSession {
                    session_id: started.session_id,
                },
            )
            .await;
        return Err(error);
    }
    Ok(started)
}

/// Starts one exact fake TUI through the daemon-owned PTY and grants both its
/// logical session and terminal to the requesting project window.
#[tauri::command]
async fn fake_tui_start(
    project_grant: String,
    scenario: String,
    columns: u16,
    rows: u16,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SessionTerminalStarted, DesktopCommandError> {
    let project_grant = parse_project_id(&project_grant)?;
    let project_id = state
        .resolve_project(project_grant, window.label())?
        .persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::SessionTerminalStarted(mut started) = state
        .request_daemon(
            &paths,
            Request::FakeTuiStart {
                project_id,
                scenario,
                columns,
                rows,
            },
        )
        .await?
    else {
        return Err(unexpected_response());
    };
    let persisted_terminal_id = started.terminal.terminal_id;
    let terminal_grant = match state.register_tui_session(
        started.session_id,
        persisted_terminal_id,
        project_grant,
        window.label(),
    ) {
        Ok(terminal_grant) => terminal_grant,
        Err(error) => {
            let _ = state
                .request_daemon(
                    &paths,
                    Request::StopSession {
                        session_id: started.session_id,
                    },
                )
                .await;
            return Err(error);
        }
    };
    started.terminal.terminal_id = terminal_grant;
    Ok(started)
}

/// Attaches this window to an already-running daemon-owned fake TUI without
/// creating a second process or replacing its logical session.
#[tauri::command]
async fn fake_tui_attach(
    session_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SessionTerminalAttached, DesktopCommandError> {
    let session_id = parse_session_id(&session_id)?;
    let grant = state.authorize_session(session_id, window.label())?;
    let project_id = state
        .resolve_project(grant.project_grant, window.label())?
        .persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::SessionTerminalAttached(mut attached) = state
        .request_daemon(
            &paths,
            Request::SessionTerminalAttach {
                session_id,
                project_id,
            },
        )
        .await?
    else {
        return Err(unexpected_response());
    };
    if attached.session_id != session_id {
        return Err(unexpected_response());
    }
    let terminal_grant = state.register_tui_session(
        session_id,
        attached.terminal.terminal_id,
        grant.project_grant,
        window.label(),
    )?;
    attached.terminal.terminal_id = terminal_grant;
    Ok(attached)
}

/// Attaches this window to an already-running structured fake session without
/// launching or resuming its daemon-owned process.
#[tauri::command]
async fn fake_session_attach(
    project_grant: String,
    session_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SessionRunAttached, DesktopCommandError> {
    let project_grant = parse_project_id(&project_grant)?;
    let session_id = parse_session_id(&session_id)?;
    let session_grant = state.authorize_session(session_id, window.label())?;
    if session_grant.project_grant != project_grant || session_grant.terminal_id.is_some() {
        return Err(session_grant_denied());
    }
    let project_id = state
        .resolve_project(project_grant, window.label())?
        .persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::SessionStructuredAttach {
                session_id,
                project_id,
            },
        )
        .await?
    {
        Response::SessionRunAttached(attached) if attached.session_id == session_id => Ok(attached),
        _ => Err(unexpected_response()),
    }
}

/// Restores the bounded logical-session index for one authorized project and
/// grants the returned session identifiers only to this native window.
#[tauri::command]
async fn session_list(
    project_grant: String,
    maximum_sessions: usize,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<SessionIndexEntry>, DesktopCommandError> {
    let project_grant = parse_project_id(&project_grant)?;
    let project_id = state
        .resolve_project(project_grant, window.label())?
        .persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::SessionList(sessions) = state
        .request_daemon(
            &paths,
            Request::SessionList {
                project_id,
                maximum_sessions,
            },
        )
        .await?
    else {
        return Err(unexpected_response());
    };
    for session in &sessions {
        if session.project_id != project_id {
            return Err(unexpected_response());
        }
        state.register_session(session.session_id, project_grant, window.label())?;
    }
    Ok(sessions)
}

/// Resumes a fake run only when the same window still owns both the logical
/// session grant and its project grant.
#[tauri::command]
async fn fake_session_resume(
    project_grant: String,
    session_id: String,
    scenario: String,
    binding: Option<String>,
    capture_raw_protocol: bool,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SessionRunStarted, DesktopCommandError> {
    let project_grant = parse_project_id(&project_grant)?;
    let session_id = parse_session_id(&session_id)?;
    let session_grant = state.authorize_session(session_id, window.label())?;
    if session_grant.project_grant != project_grant {
        return Err(session_grant_denied());
    }
    let project_id = state
        .resolve_project(project_grant, window.label())?
        .persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::FakeSessionResume {
                session_id,
                project_id,
                scenario,
                binding,
                capture_raw_protocol,
            },
        )
        .await?
    {
        Response::SessionRunStarted(started) if started.session_id == session_id => Ok(started),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn session_snapshot(
    session_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SessionSnapshot, DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(&paths, Request::SessionSnapshot { session_id })
        .await?
    {
        Response::SessionSnapshot(snapshot) if snapshot.session_id == session_id => Ok(snapshot),
        _ => Err(unexpected_response()),
    }
}

/// Performs one bounded wake-driven read. The webview controls cancellation
/// and starts another read only after this one resolves.
#[tauri::command]
async fn session_events_read(
    session_id: String,
    after_sequence: u64,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SessionEventBatch, DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::SessionEventsRead {
                session_id,
                after_sequence,
                maximum_events: SESSION_EVENTS_PER_READ,
                wait_milliseconds: SESSION_READ_WAIT_MILLISECONDS,
            },
        )
        .await?
    {
        Response::SessionEvents(batch) if batch.session_id == session_id => Ok(batch),
        _ => Err(unexpected_response()),
    }
}

/// Reads one bounded page of explicitly opted-in, unredacted CLI stdout.
#[tauri::command]
async fn session_raw_read(
    session_id: String,
    run_id: String,
    after_offset: u64,
    maximum_bytes: u32,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<SessionRawBatch, DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let run_id = parse_run_id(&run_id)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::SessionRawRead {
                session_id,
                run_id,
                after_offset,
                maximum_bytes,
            },
        )
        .await?
    {
        Response::SessionRaw(batch) if batch.session_id == session_id && batch.run_id == run_id => {
            Ok(batch)
        }
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn session_subscribe(
    session_id: String,
    after_sequence: u64,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::SubscribeSession {
                session_id,
                after_sequence,
            },
        )
        .await?
    {
        Response::Subscribed {
            session_id: subscribed,
        } if subscribed == session_id => Ok(()),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn session_unsubscribe(
    session_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(&paths, Request::UnsubscribeSession { session_id })
        .await?
    {
        Response::Unsubscribed {
            session_id: unsubscribed,
        } if unsubscribed == session_id => Ok(()),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn session_stop(
    session_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(&paths, Request::StopSession { session_id })
        .await?
    {
        Response::SessionStopped {
            session_id: stopped,
        } if stopped == session_id => {
            if let Some(terminal_id) = state.unregister_session(session_id) {
                state.unregister_terminal(terminal_id);
            }
            Ok(())
        }
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn session_permission_respond(
    session_id: String,
    run_id: String,
    request_id: String,
    decision: SessionPermissionDecision,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let run_id = parse_run_id(&run_id)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::SessionPermissionRespond {
                session_id,
                run_id,
                request_id: request_id.clone(),
                decision,
            },
        )
        .await?
    {
        Response::SessionPermissionAccepted {
            session_id: accepted_session,
            request_id: accepted_request,
        } if accepted_session == session_id && accepted_request == request_id => Ok(()),
        _ => Err(unexpected_response()),
    }
}

/// Carries sensitive input through the redacting, zeroizing protocol wrapper.
/// The desktop host never logs or persists the JSON text.
#[tauri::command]
async fn session_user_input_respond(
    session_id: String,
    run_id: String,
    request_id: String,
    value_json: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let run_id = parse_run_id(&run_id)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::SessionUserInputRespond {
                session_id,
                run_id,
                request_id: request_id.clone(),
                value_json: SensitiveString::new(value_json),
            },
        )
        .await?
    {
        Response::SessionUserInputAccepted {
            session_id: accepted_session,
            request_id: accepted_request,
        } if accepted_session == session_id && accepted_request == request_id => Ok(()),
        _ => Err(unexpected_response()),
    }
}

/// Carries an opaque action payload without exposing it to host diagnostics.
#[tauri::command]
async fn session_gui_action(
    session_id: String,
    run_id: String,
    action: String,
    payload_json: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<String, DesktopCommandError> {
    let session_id = authorize_session(&state, &session_id, window.label())?;
    let run_id = parse_run_id(&run_id)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::SessionGuiAction {
                session_id,
                run_id,
                action,
                payload_json: SensitiveString::new(payload_json),
            },
        )
        .await?
    {
        Response::SessionGuiActionAccepted {
            session_id: accepted_session,
            action_id,
        } if accepted_session == session_id => Ok(action_id),
        _ => Err(unexpected_response()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalAcknowledgement {
    terminal_id: TerminalId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopTerminalIndexEntry {
    terminal_id: TerminalId,
    run_id: RunId,
    process_id: u32,
    canonical_cwd: String,
    state: maestro_protocol::TerminalState,
    kind: String,
    title: String,
    exit: Option<maestro_protocol::TerminalExit>,
}

#[tauri::command]
async fn project_directory_list(
    project_grant: String,
    directory: String,
    cursor: u64,
    maximum_entries: usize,
    include_hidden: bool,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ProjectDirectoryPage, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectDirectoryList {
                project_id,
                directory,
                cursor,
                maximum_entries,
                include_hidden,
            },
        )
        .await?
    {
        Response::ProjectDirectoryPage(page) => Ok(page),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_file_read(
    project_grant: String,
    path: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ProjectTextFile, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(&paths, Request::ProjectFileRead { project_id, path })
        .await?
    {
        Response::ProjectTextFile(file) => Ok(file),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_file_save(
    project_grant: String,
    path: String,
    text: String,
    expected_fingerprint: Vec<u8>,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ProjectFileSaved, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectFileSave {
                project_id,
                path,
                text,
                expected_fingerprint,
            },
        )
        .await?
    {
        Response::ProjectFileSaved(saved) => Ok(saved),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_file_open_external(
    project_grant: String,
    path: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::ProjectTextFile(opened) = state
        .request_daemon(&paths, Request::ProjectFileRead { project_id, path })
        .await?
    else {
        return Err(unexpected_response());
    };
    launch_external_path(&opened.path)
}

fn launch_external_path(path: &str) -> Result<(), DesktopCommandError> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = tokio::process::Command::new("/usr/bin/open");
        command.arg("--").arg(path);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = tokio::process::Command::new("/usr/bin/xdg-open");
        command.arg(path);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(invalid_request(
        "Opening files externally is not supported on this platform.",
    ));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut child = command
            .spawn()
            .map_err(|_| internal_error("The external editor could not be launched."))?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }
}

#[tauri::command]
async fn project_search(
    project_grant: String,
    search_id: String,
    options: ProjectSearchOptions,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ProjectSearchResult, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let search_id = parse_request_id(&search_id)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectSearch {
                project_id,
                search_id,
                options,
            },
        )
        .await?
    {
        Response::ProjectSearchResult(result) => Ok(result),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_search_cancel(
    project_grant: String,
    search_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<(), DesktopCommandError> {
    let _project_id = authorize_project(&state, &project_grant, window.label())?;
    let search_id = parse_request_id(&search_id)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(&paths, Request::ProjectSearchCancel { search_id })
        .await?
    {
        Response::ProjectSearchCancelled { .. } => Ok(()),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_git_status(
    project_grant: String,
    repository: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<ProjectGitStatusEntry>, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectGitStatus {
                project_id,
                repository,
            },
        )
        .await?
    {
        Response::ProjectGitStatus(status) => Ok(status),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_git_branch(
    project_grant: String,
    repository: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ProjectBranchState, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectGitBranch {
                project_id,
                repository,
            },
        )
        .await?
    {
        Response::ProjectGitBranch(branch) => Ok(branch),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_git_diff(
    project_grant: String,
    repository: String,
    scope: ProjectDiffScope,
    maximum_bytes: usize,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<ProjectGitDiff, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectGitDiff {
                project_id,
                repository,
                scope,
                maximum_bytes,
            },
        )
        .await?
    {
        Response::ProjectGitDiff(diff) => Ok(diff),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn project_git_worktrees(
    project_grant: String,
    repository: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<ProjectWorktree>, DesktopCommandError> {
    let project_id = authorize_project(&state, &project_grant, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::ProjectGitWorktrees {
                project_id,
                repository,
            },
        )
        .await?
    {
        Response::ProjectGitWorktrees(worktrees) => Ok(worktrees),
        _ => Err(unexpected_response()),
    }
}

/// Opens a daemon-owned shell PTY. Closing or disconnecting the invoking
/// window does not close the terminal.
#[tauri::command]
async fn terminal_open(
    project_grant: String,
    columns: u16,
    rows: u16,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<TerminalOpened, DesktopCommandError> {
    let project_grant = parse_project_id(&project_grant)?;
    let project = state.resolve_project(project_grant, window.label())?;
    let cwd = project.canonical_path;
    let project_id = project.persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::TerminalOpened(mut opened) = state
        .request_daemon(
            &paths,
            Request::TerminalOpen {
                project_id,
                cwd,
                columns,
                rows,
            },
        )
        .await?
    else {
        return Err(unexpected_response());
    };
    let persisted_terminal_id = opened.terminal_id;
    let terminal_grant = match state.register_terminal(
        persisted_terminal_id,
        project_grant,
        window.label(),
        false,
    ) {
        Ok(terminal_grant) => terminal_grant,
        Err(error) => {
            let _ = state
                .request_daemon(
                    &paths,
                    Request::TerminalClose {
                        terminal_id: persisted_terminal_id,
                    },
                )
                .await;
            return Err(error);
        }
    };
    opened.terminal_id = terminal_grant;
    Ok(opened)
}

/// Returns a bounded project-scoped list of daemon-owned shell terminals.
/// Every returned identifier is a disposable window capability, never the
/// daemon terminal identifier itself.
#[tauri::command]
async fn terminal_list(
    project_grant: String,
    maximum_terminals: usize,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Vec<DesktopTerminalIndexEntry>, DesktopCommandError> {
    let project_grant = parse_project_id(&project_grant)?;
    let project_id = state
        .resolve_project(project_grant, window.label())?
        .persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::TerminalList(terminals) = state
        .request_daemon(
            &paths,
            Request::TerminalList {
                project_id,
                maximum_terminals,
            },
        )
        .await?
    else {
        return Err(unexpected_response());
    };
    terminals
        .into_iter()
        .map(|entry| {
            if entry.project_id != project_id || entry.kind != "shell" {
                return Err(unexpected_response());
            }
            grant_terminal_index_entry(&state, entry, project_grant, window.label())
        })
        .collect()
}

/// Reattaches a view after independently verifying both the host capability
/// and daemon-side persisted project ownership. A fresh opaque grant is issued
/// for the attached view.
#[tauri::command]
async fn terminal_attach(
    project_grant: String,
    terminal_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<TerminalOpened, DesktopCommandError> {
    let project_grant = parse_project_id(&project_grant)?;
    let terminal_grant_id = parse_terminal_id(&terminal_id)?;
    let terminal_grant = state.authorize_terminal(terminal_grant_id, window.label())?;
    if terminal_grant.project_grant != project_grant {
        return Err(terminal_grant_denied());
    }
    let project_id = state
        .resolve_project(project_grant, window.label())?
        .persisted_project_id;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::TerminalAttached(mut opened) = state
        .request_daemon(
            &paths,
            Request::TerminalAttach {
                project_id,
                terminal_id: terminal_grant.persisted_terminal_id,
            },
        )
        .await?
    else {
        return Err(unexpected_response());
    };
    if opened.terminal_id != terminal_grant.persisted_terminal_id {
        return Err(unexpected_response());
    }
    let fresh_grant =
        state.register_terminal(opened.terminal_id, project_grant, window.label(), false)?;
    opened.terminal_id = fresh_grant;
    Ok(opened)
}

fn grant_terminal_index_entry(
    state: &DesktopHostState,
    entry: TerminalIndexEntry,
    project_grant: ProjectId,
    window_label: &str,
) -> Result<DesktopTerminalIndexEntry, DesktopCommandError> {
    let terminal_grant = state.register_terminal(
        entry.terminal.terminal_id,
        project_grant,
        window_label,
        true,
    )?;
    Ok(DesktopTerminalIndexEntry {
        terminal_id: terminal_grant,
        run_id: entry.terminal.run_id,
        process_id: entry.terminal.process_id,
        canonical_cwd: entry.terminal.canonical_cwd,
        state: entry.terminal.state,
        kind: entry.kind,
        title: entry.title,
        exit: entry.exit,
    })
}

/// Shows a native confirmation before opening one bounded, non-credentialed
/// HTTP(S) URL emitted by terminal content. The terminal cannot navigate the
/// webview or launch another scheme directly.
#[tauri::command]
async fn terminal_link_open(
    url: String,
    app: tauri::AppHandle,
    window: tauri::Window,
) -> Result<bool, DesktopCommandError> {
    let url = validate_terminal_link(&url)?;
    let (decision_sender, decision_receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(format!(
            "Terminal content requested opening this untrusted link:\n\n{}",
            url.as_str()
        ))
        .title("Open terminal link?")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Open".to_owned(),
            "Cancel".to_owned(),
        ))
        .parent(&window)
        .show(move |approved| {
            let _ = decision_sender.send(approved);
        });
    if !decision_receiver.await.unwrap_or(false) {
        return Ok(false);
    }

    launch_external_url(url.as_str())?;
    Ok(true)
}

fn validate_terminal_link(value: &str) -> Result<url::Url, DesktopCommandError> {
    if value.len() > MAXIMUM_TERMINAL_LINK_BYTES {
        return Err(invalid_request("The terminal link is too large."));
    }
    let parsed = url::Url::parse(value)
        .map_err(|_| invalid_request("The terminal link is not a valid URL."))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query_pairs().any(|(name, _)| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "access_token"
                    | "api_key"
                    | "apikey"
                    | "auth"
                    | "authorization"
                    | "key"
                    | "password"
                    | "secret"
                    | "signature"
                    | "token"
            )
        })
    {
        return Err(invalid_request(
            "The terminal link uses a blocked scheme or contains credentials.",
        ));
    }
    Ok(parsed)
}

fn launch_external_url(url: &str) -> Result<(), DesktopCommandError> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = tokio::process::Command::new("/usr/bin/open");
        command.arg("--").arg(url);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = tokio::process::Command::new("/usr/bin/xdg-open");
        command.arg(url);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(invalid_request(
        "Opening terminal links is not supported on this platform.",
    ));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut child = command
            .spawn()
            .map_err(|_| internal_error("The external browser could not be launched."))?;
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Ok(())
    }
}

#[tauri::command]
async fn terminal_write(
    terminal_id: String,
    data: Vec<u8>,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<TerminalAcknowledgement, DesktopCommandError> {
    let (terminal_grant, terminal_id) = authorize_terminal(&state, &terminal_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(&paths, Request::TerminalWrite { terminal_id, data })
        .await?
    {
        Response::TerminalWriteAccepted {
            terminal_id: accepted,
        } if accepted == terminal_id => Ok(TerminalAcknowledgement {
            terminal_id: terminal_grant,
        }),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn terminal_resize(
    terminal_id: String,
    columns: u16,
    rows: u16,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<TerminalAcknowledgement, DesktopCommandError> {
    let (terminal_grant, terminal_id) = authorize_terminal(&state, &terminal_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::TerminalResize {
                terminal_id,
                columns,
                rows,
            },
        )
        .await?
    {
        Response::TerminalResized {
            terminal_id: resized,
        } if resized == terminal_id => Ok(TerminalAcknowledgement {
            terminal_id: terminal_grant,
        }),
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn terminal_read(
    terminal_id: String,
    after_sequence: u64,
    maximum_bytes: u32,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<TerminalReadResult, DesktopCommandError> {
    let (terminal_grant, terminal_id) = authorize_terminal(&state, &terminal_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(
            &paths,
            Request::TerminalRead {
                terminal_id,
                after_sequence,
                maximum_bytes,
                wait_milliseconds: TERMINAL_READ_WAIT_MILLISECONDS,
            },
        )
        .await?
    {
        Response::TerminalRead(mut read) if read.terminal_id == terminal_id => {
            read.terminal_id = terminal_grant;
            Ok(read)
        }
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn terminal_state(
    terminal_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<TerminalStatus, DesktopCommandError> {
    let (terminal_grant, terminal_id) = authorize_terminal(&state, &terminal_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    match state
        .request_daemon(&paths, Request::TerminalState { terminal_id })
        .await?
    {
        Response::TerminalState(mut status) if status.terminal_id == terminal_id => {
            status.terminal_id = terminal_grant;
            Ok(status)
        }
        _ => Err(unexpected_response()),
    }
}

#[tauri::command]
async fn terminal_close(
    terminal_id: String,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<TerminalStatus, DesktopCommandError> {
    let (terminal_grant, terminal_id) = authorize_terminal(&state, &terminal_id, window.label())?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    let Response::TerminalClosed(status) = state
        .request_daemon(&paths, Request::TerminalClose { terminal_id })
        .await?
    else {
        return Err(unexpected_response());
    };
    if status.terminal_id != terminal_id {
        return Err(unexpected_response());
    }
    state.unregister_terminal_process(terminal_id);
    let mut status = status;
    status.terminal_id = terminal_grant;
    Ok(status)
}

fn authorize_terminal(
    state: &DesktopHostState,
    value: &str,
    window_label: &str,
) -> Result<(TerminalId, TerminalId), DesktopCommandError> {
    let terminal_grant = parse_terminal_id(value)?;
    let grant = state.authorize_terminal(terminal_grant, window_label)?;
    Ok((terminal_grant, grant.persisted_terminal_id))
}

fn authorize_session(
    state: &DesktopHostState,
    value: &str,
    window_label: &str,
) -> Result<SessionId, DesktopCommandError> {
    let session_id = parse_session_id(value)?;
    let _grant = state.authorize_session(session_id, window_label)?;
    Ok(session_id)
}

fn authorize_project(
    state: &DesktopHostState,
    value: &str,
    window_label: &str,
) -> Result<ProjectId, DesktopCommandError> {
    let project_grant = parse_project_id(value)?;
    Ok(state
        .resolve_project(project_grant, window_label)?
        .persisted_project_id)
}

async fn open_recent_project_with_paths(
    paths: &DaemonPaths,
    state: &DesktopHostState,
    project_id: ProjectId,
    window_label: &str,
) -> Result<ProjectSelection, DesktopCommandError> {
    let Response::ProjectRecentList(recent_projects) = state
        .request_daemon(
            paths,
            Request::ProjectRecentList {
                maximum_projects: MAXIMUM_RECENT_PROJECTS,
            },
        )
        .await?
    else {
        return Err(unexpected_response());
    };
    let saved = recent_projects
        .into_iter()
        .find(|project| project.project_id == project_id)
        .ok_or_else(|| invalid_request("The saved project is no longer available."))?;
    let canonical_roots = validate_persisted_roots(&saved.canonical_roots)?;

    register_project_with_paths(
        paths,
        state,
        project_id,
        RequestId::new(),
        saved.display_name,
        canonical_roots,
        window_label,
    )
    .await
}

async fn shortcut_settings_load_with_paths(
    paths: &DaemonPaths,
    state: &DesktopHostState,
) -> Result<Option<ShortcutBindings>, DesktopCommandError> {
    let response = state
        .request_daemon(
            paths,
            Request::SettingLoad {
                scope: SHORTCUT_SETTING_SCOPE.to_owned(),
                scope_reference: SHORTCUT_SETTING_SCOPE_REFERENCE.to_owned(),
                key: SHORTCUT_SETTING_KEY.to_owned(),
            },
        )
        .await?;
    let Response::SettingValue {
        scope,
        scope_reference,
        key,
        value_json,
    } = response
    else {
        return Err(unexpected_response());
    };
    if scope != SHORTCUT_SETTING_SCOPE
        || scope_reference != SHORTCUT_SETTING_SCOPE_REFERENCE
        || key != SHORTCUT_SETTING_KEY
    {
        return Err(unexpected_response());
    }
    let Some(value_json) = value_json else {
        return Ok(None);
    };
    let bindings: ShortcutBindings = serde_json::from_str(&value_json.into_inner())
        .map_err(|_| invalid_request("Saved keyboard shortcuts are not valid."))?;
    bindings.validate()?;
    Ok(Some(bindings))
}

async fn shortcut_settings_save_with_paths(
    paths: &DaemonPaths,
    state: &DesktopHostState,
    bindings: ShortcutBindings,
) -> Result<(), DesktopCommandError> {
    bindings.validate()?;
    let value_json = serde_json::to_string(&bindings)
        .map_err(|_| internal_error("Maestro could not encode keyboard shortcuts."))?;
    match state
        .request_daemon(
            paths,
            Request::SettingSave {
                scope: SHORTCUT_SETTING_SCOPE.to_owned(),
                scope_reference: SHORTCUT_SETTING_SCOPE_REFERENCE.to_owned(),
                key: SHORTCUT_SETTING_KEY.to_owned(),
                value_json: SensitiveString::new(value_json),
            },
        )
        .await?
    {
        Response::SettingSaved {
            scope,
            scope_reference,
            key,
        } if scope == SHORTCUT_SETTING_SCOPE
            && scope_reference == SHORTCUT_SETTING_SCOPE_REFERENCE
            && key == SHORTCUT_SETTING_KEY =>
        {
            Ok(())
        }
        _ => Err(unexpected_response()),
    }
}

async fn register_project_with_paths(
    paths: &DaemonPaths,
    state: &DesktopHostState,
    project_id: ProjectId,
    correlation_id: RequestId,
    display_name: String,
    canonical_roots: Vec<String>,
    window_label: &str,
) -> Result<ProjectSelection, DesktopCommandError> {
    let expected_roots = project_registration_key(&canonical_roots);
    let registered = project_registration_with_deadline(
        state.request_daemon_correlated(
            paths,
            correlation_id,
            Request::ProjectRegister {
                project_id,
                display_name,
                roots: canonical_roots,
            },
        ),
        PROJECT_REGISTRATION_TIMEOUT,
        correlation_id,
    )
    .await?;
    let Response::ProjectRegistered(project) = registered else {
        return Err(unexpected_response());
    };
    let canonical_roots = validate_persisted_roots(&project.canonical_roots)?;
    if canonical_roots != expected_roots {
        return Err(unexpected_response());
    }
    state.grant_registered_project(
        project.project_id,
        project.display_name,
        canonical_roots,
        window_label,
    )
}

async fn register_new_project_with_paths(
    paths: &DaemonPaths,
    state: &DesktopHostState,
    display_name: String,
    canonical_roots: Vec<String>,
    window_label: &str,
) -> Result<ProjectSelection, DesktopCommandError> {
    let attempt = state.begin_project_registration(&canonical_roots)?;
    let result = register_project_with_paths(
        paths,
        state,
        attempt.project_id,
        attempt.correlation_id,
        display_name,
        canonical_roots.clone(),
        window_label,
    )
    .await;
    if match &result {
        Ok(_) => true,
        Err(error) => !error.retryable,
    } {
        state.finish_project_registration(&canonical_roots, attempt);
    }
    result
}

async fn project_registration_with_deadline<F>(
    operation: F,
    deadline: Duration,
    correlation_id: RequestId,
) -> Result<Response, DesktopCommandError>
where
    F: Future<Output = Result<Response, DesktopCommandError>>,
{
    tokio::time::timeout(deadline, operation)
        .await
        .map_err(|_| project_registration_timed_out(correlation_id))?
}

fn project_registration_key(canonical_roots: &[String]) -> Vec<String> {
    let mut key = canonical_roots.to_vec();
    key.sort();
    key
}

fn is_canonical_shortcut(shortcut: &str) -> bool {
    let parts = shortcut.split('+').collect::<Vec<_>>();
    let key = match parts.as_slice() {
        ["Mod", key] | ["Mod", "Shift", key] => *key,
        _ => return false,
    };
    key.len() == 1
        && key
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn validate_recent_project_limit(maximum_projects: usize) -> Result<(), DesktopCommandError> {
    if (1..=MAXIMUM_RECENT_PROJECTS).contains(&maximum_projects) {
        Ok(())
    } else {
        Err(invalid_request(format!(
            "Recent project requests must contain between 1 and {MAXIMUM_RECENT_PROJECTS} entries."
        )))
    }
}

fn validate_persisted_roots(roots: &[String]) -> Result<Vec<String>, DesktopCommandError> {
    if roots.is_empty() {
        return Err(invalid_path(
            "The saved project does not contain a workspace root.",
        ));
    }
    roots
        .iter()
        .map(|root| {
            let validated = validate_project_path(Path::new(root)).map_err(invalid_path)?;
            if validated.canonical_path != *root {
                return Err(invalid_path(
                    "A saved project root no longer resolves to its recorded location.",
                ));
            }
            Ok(validated.canonical_path)
        })
        .collect()
}

fn validate_window_key(window_key: &str, window_label: &str) -> Result<(), DesktopCommandError> {
    if window_key == window_label {
        Ok(())
    } else {
        Err(project_grant_denied())
    }
}

fn validate_window_layout_json(layout_json: &str) -> Result<(), DesktopCommandError> {
    if layout_json.len() > MAXIMUM_WINDOW_LAYOUT_BYTES {
        return Err(invalid_request("The window layout is too large."));
    }
    let value: serde_json::Value = serde_json::from_str(layout_json)
        .map_err(|_| invalid_request("The window layout is not valid JSON."))?;
    if !value.is_object() {
        return Err(invalid_request("The window layout must be a JSON object."));
    }
    Ok(())
}

#[cfg(test)]
async fn terminal_open_with_paths(
    paths: &DaemonPaths,
    cwd: String,
    columns: u16,
    rows: u16,
) -> Result<TerminalOpened, DesktopCommandError> {
    let project_id = ProjectId::new();
    let display_name = "Terminal test project".to_owned();
    match request_daemon(
        paths,
        Request::ProjectRegister {
            project_id,
            display_name,
            roots: vec![cwd.clone()],
        },
    )
    .await?
    {
        Response::ProjectRegistered(_) => {}
        _ => return Err(unexpected_response()),
    }
    match request_daemon(
        paths,
        Request::TerminalOpen {
            project_id,
            cwd,
            columns,
            rows,
        },
    )
    .await?
    {
        Response::TerminalOpened(opened) => Ok(opened),
        _ => Err(unexpected_response()),
    }
}

#[cfg(test)]
async fn terminal_write_with_paths(
    paths: &DaemonPaths,
    terminal_id: &str,
    data: Vec<u8>,
) -> Result<TerminalAcknowledgement, DesktopCommandError> {
    let terminal_id = parse_terminal_id(terminal_id)?;
    match request_daemon(paths, Request::TerminalWrite { terminal_id, data }).await? {
        Response::TerminalWriteAccepted { terminal_id } => {
            Ok(TerminalAcknowledgement { terminal_id })
        }
        _ => Err(unexpected_response()),
    }
}

#[cfg(test)]
async fn terminal_resize_with_paths(
    paths: &DaemonPaths,
    terminal_id: &str,
    columns: u16,
    rows: u16,
) -> Result<TerminalAcknowledgement, DesktopCommandError> {
    let terminal_id = parse_terminal_id(terminal_id)?;
    match request_daemon(
        paths,
        Request::TerminalResize {
            terminal_id,
            columns,
            rows,
        },
    )
    .await?
    {
        Response::TerminalResized { terminal_id } => Ok(TerminalAcknowledgement { terminal_id }),
        _ => Err(unexpected_response()),
    }
}

#[cfg(test)]
async fn terminal_read_with_paths(
    paths: &DaemonPaths,
    terminal_id: &str,
    after_sequence: u64,
    maximum_bytes: u32,
) -> Result<TerminalReadResult, DesktopCommandError> {
    let terminal_id = parse_terminal_id(terminal_id)?;
    match request_daemon(
        paths,
        Request::TerminalRead {
            terminal_id,
            after_sequence,
            maximum_bytes,
            wait_milliseconds: TERMINAL_READ_WAIT_MILLISECONDS,
        },
    )
    .await?
    {
        Response::TerminalRead(read) => Ok(read),
        _ => Err(unexpected_response()),
    }
}

#[cfg(test)]
async fn terminal_state_with_paths(
    paths: &DaemonPaths,
    terminal_id: &str,
) -> Result<TerminalStatus, DesktopCommandError> {
    let terminal_id = parse_terminal_id(terminal_id)?;
    match request_daemon(paths, Request::TerminalState { terminal_id }).await? {
        Response::TerminalState(status) => Ok(status),
        _ => Err(unexpected_response()),
    }
}

#[cfg(test)]
async fn terminal_close_with_paths(
    paths: &DaemonPaths,
    terminal_id: &str,
) -> Result<TerminalStatus, DesktopCommandError> {
    let terminal_id = parse_terminal_id(terminal_id)?;
    match request_daemon(paths, Request::TerminalClose { terminal_id }).await? {
        Response::TerminalClosed(status) => Ok(status),
        _ => Err(unexpected_response()),
    }
}

#[cfg(test)]
async fn request_daemon(
    paths: &DaemonPaths,
    request: Request,
) -> Result<Response, DesktopCommandError> {
    let mut client = DaemonClient::connect(paths, "maestro-desktop", env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|_| daemon_unavailable())?;
    client.request(request).await.map_err(|error| match error {
        maestrod::IpcError::Fatal(error) => error.into(),
        _ => daemon_unavailable(),
    })
}

fn parse_terminal_id(value: &str) -> Result<TerminalId, DesktopCommandError> {
    value
        .parse()
        .map_err(|_| invalid_request("The terminal identifier is invalid."))
}

fn parse_session_id(value: &str) -> Result<SessionId, DesktopCommandError> {
    value
        .parse()
        .map_err(|_| invalid_request("The session identifier is invalid."))
}

fn parse_run_id(value: &str) -> Result<RunId, DesktopCommandError> {
    value
        .parse()
        .map_err(|_| invalid_request("The session run identifier is invalid."))
}

fn parse_project_id(value: &str) -> Result<ProjectId, DesktopCommandError> {
    value.parse().map_err(|_| project_grant_denied())
}

fn parse_project_id_for_request(value: &str) -> Result<ProjectId, DesktopCommandError> {
    value
        .parse()
        .map_err(|_| invalid_request("The project identifier is invalid."))
}

fn parse_request_id(value: &str) -> Result<RequestId, DesktopCommandError> {
    value
        .parse()
        .map_err(|_| invalid_request("The request identifier is invalid."))
}

fn project_grant_denied() -> DesktopCommandError {
    command_error(
        ErrorCode::PermissionDenied,
        "The project grant is not valid for this window. Select the project again.",
    )
}

fn terminal_grant_denied() -> DesktopCommandError {
    command_error(
        ErrorCode::PermissionDenied,
        "The terminal is not available to this window.",
    )
}

fn session_grant_denied() -> DesktopCommandError {
    command_error(
        ErrorCode::PermissionDenied,
        "The session is not available to this window.",
    )
}

fn daemon_unavailable() -> DesktopCommandError {
    let mut error = MaestroError::new(
        ErrorCode::Internal,
        "The Maestro service is unavailable. Start maestrod and try again.",
    );
    error.retryable = true;
    error.user_action = Some("Start the Maestro service and retry the operation.".to_owned());
    error.into()
}

fn project_registration_timed_out(correlation_id: RequestId) -> DesktopCommandError {
    let mut error = MaestroError::new(
        ErrorCode::Internal,
        "The selected project took too long to register with the Maestro service.",
    );
    error.retryable = true;
    error.user_action =
        Some("Retry opening the project. If it continues, restart the Maestro service.".to_owned());
    error.correlation_id = correlation_id.as_uuid();
    error.into()
}

fn unexpected_response() -> DesktopCommandError {
    command_error(
        ErrorCode::Internal,
        "The Maestro service returned an unexpected terminal response.",
    )
}

fn invalid_request(message: impl Into<String>) -> DesktopCommandError {
    command_error(ErrorCode::InvalidRequest, message)
}

fn invalid_path(message: impl Into<String>) -> DesktopCommandError {
    command_error(ErrorCode::InvalidPath, message)
}

fn internal_error(message: impl Into<String>) -> DesktopCommandError {
    command_error(ErrorCode::Internal, message)
}

fn command_error(code: ErrorCode, message: impl Into<String>) -> DesktopCommandError {
    MaestroError::new(code, message).into()
}

fn validate_project_path(path: &Path) -> Result<ValidatedProject, &'static str> {
    let canonical = path
        .canonicalize()
        .map_err(|_| "The selected project folder could not be resolved.")?;
    if !canonical.is_dir() {
        return Err("The selected project path is not a folder.");
    }

    let path = canonical
        .to_str()
        .ok_or("The selected project path is not valid UTF-8 on this platform.")?
        .to_owned();
    let name = canonical
        .file_name()
        .and_then(|part| part.to_str())
        .filter(|part| !part.is_empty())
        .unwrap_or(&path)
        .to_owned();

    Ok(ValidatedProject {
        canonical_path: path,
        name,
    })
}

fn validate_workspace_paths(
    paths: impl IntoIterator<Item = std::path::PathBuf>,
) -> Result<(String, Vec<String>), &'static str> {
    let projects = paths
        .into_iter()
        .map(|path| validate_project_path(&path))
        .collect::<Result<Vec<_>, _>>()?;
    let first = projects
        .first()
        .ok_or("At least one project folder must be selected.")?;
    let name = if projects.len() == 1 {
        first.name.clone()
    } else {
        format!("{} + {} more", first.name, projects.len() - 1)
    };
    Ok((
        name,
        projects
            .into_iter()
            .map(|project| project.canonical_path)
            .collect(),
    ))
}

/// Opens the operating system's folder picker and returns one or more
/// validated canonical roots plus an opaque, window-owned capability grant.
#[tauri::command]
#[allow(clippy::needless_pass_by_value)] // Tauri command injection provides an owned AppHandle.
async fn open_project_folder(
    app: tauri::AppHandle,
    window: tauri::Window,
    state: tauri::State<'_, DesktopHostState>,
) -> Result<Option<ProjectSelection>, DesktopCommandError> {
    let (selection_sender, selection_receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Open Project or Workspace in Maestro")
        .pick_folders(move |selected| {
            // The command may have been cancelled during application shutdown.
            let _ = selection_sender.send(selected);
        });
    let selected = selection_receiver
        .await
        .map_err(|_| internal_error("The project folder picker closed unexpectedly."))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let selected = selected
        .into_iter()
        .map(|path| {
            path.into_path().map_err(|_| {
                invalid_path("A selected project path is not a local filesystem path.")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (display_name, roots) = validate_workspace_paths(selected).map_err(invalid_path)?;
    let paths = DaemonPaths::discover().map_err(|_| daemon_unavailable())?;
    register_new_project_with_paths(&paths, &state, display_name, roots, window.label())
        .await
        .map(Some)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Starts the native Maestro desktop host.
///
/// # Panics
///
/// Panics when Tauri cannot initialize or run the native application event
/// loop, since the desktop process cannot recover without that host.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let resource_directory = app.path().resource_dir().ok();
            app.manage(DesktopHostState::new(DaemonLauncher::discover(
                resource_directory.as_deref(),
            )));
            background_menu::install(app)?;
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            tauri::WindowEvent::Destroyed => {
                let state = window.state::<DesktopHostState>();
                state.revoke_window(window.label());
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            background_stop_all,
            fake_session_attach,
            fake_session_resume,
            fake_session_start,
            fake_tui_attach,
            fake_tui_start,
            open_project_folder,
            open_new_window,
            open_recent_project,
            project_directory_list,
            project_file_read,
            project_file_save,
            project_file_open_external,
            project_search,
            project_search_cancel,
            project_git_status,
            project_git_branch,
            project_git_diff,
            project_git_worktrees,
            project_recent_list,
            project_set_favorite,
            project_window_layout_load,
            project_window_layout_save,
            shortcut_settings_load,
            shortcut_settings_save,
            session_events_read,
            session_gui_action,
            session_list,
            session_permission_respond,
            session_raw_read,
            session_snapshot,
            session_stop,
            session_subscribe,
            session_unsubscribe,
            session_user_input_respond,
            storage_unlock,
            system_snapshot,
            terminal_open,
            terminal_list,
            terminal_attach,
            terminal_link_open,
            terminal_write,
            terminal_resize,
            terminal_read,
            terminal_state,
            terminal_close
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Maestro desktop host");
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use maestro_domain::{ErrorCode, RequestId};
    use maestro_protocol::{MIN_TERMINAL_POLL_BYTES, Response, TerminalState};
    use maestrod::{DaemonConfig, DaemonServer, MultiplexedDaemonClient};

    use super::{
        DaemonStatus, DesktopCommandError, DesktopHostState, ShortcutBindings, SystemSnapshot,
        is_canonical_shortcut, open_recent_project_with_paths, project_registration_with_deadline,
        shortcut_settings_load_with_paths, shortcut_settings_save_with_paths,
        terminal_close_with_paths, terminal_open_with_paths, terminal_read_with_paths,
        terminal_resize_with_paths, terminal_state_with_paths, terminal_write_with_paths,
        validate_persisted_roots, validate_project_path, validate_recent_project_limit,
        validate_terminal_link, validate_window_key, validate_window_layout_json,
        validate_workspace_paths,
    };

    fn test_shortcuts() -> ShortcutBindings {
        ShortcutBindings {
            open_new_window: "Mod+Shift+N".to_owned(),
            open_project: "Mod+O".to_owned(),
            toggle_bottom_panel: "Mod+J".to_owned(),
            toggle_command_palette: "Mod+Shift+P".to_owned(),
            toggle_inspector: "Mod+Shift+B".to_owned(),
            toggle_sidebar: "Mod+B".to_owned(),
        }
    }

    #[test]
    fn shortcut_settings_require_an_exact_safe_object() {
        let bindings = test_shortcuts();
        bindings.validate().expect("defaults validate");
        assert!(is_canonical_shortcut("Mod+A"));
        assert!(is_canonical_shortcut("Mod+Shift+9"));
        assert!(!is_canonical_shortcut("Ctrl+A"));
        assert!(!is_canonical_shortcut("mod+a"));

        let mut conflicting = bindings.clone();
        conflicting.open_project = conflicting.toggle_sidebar.clone();
        assert!(conflicting.validate().is_err());
        assert!(
            serde_json::from_value::<ShortcutBindings>(serde_json::json!({
                "openNewWindow": "Mod+Shift+N",
                "openProject": "Mod+O",
                "toggleBottomPanel": "Mod+J",
                "toggleCommandPalette": "Mod+Shift+P",
                "toggleInspector": "Mod+Shift+B",
                "toggleSidebar": "Mod+B",
                "unexpected": "Mod+X"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn shortcut_settings_round_trip_through_native_validation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = maestrod::DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let state = DesktopHostState::default();
        let bindings = test_shortcuts();

        assert_eq!(
            shortcut_settings_load_with_paths(&paths, &state)
                .await
                .expect("unset settings load"),
            None
        );
        shortcut_settings_save_with_paths(&paths, &state, bindings.clone())
            .await
            .expect("settings save");
        assert_eq!(
            shortcut_settings_load_with_paths(&paths, &state)
                .await
                .expect("settings load"),
            Some(bindings)
        );

        drop(state);
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn project_registration_deadline_is_retryable_and_correlated() {
        let correlation_id = RequestId::new();
        let error = project_registration_with_deadline(
            std::future::pending::<Result<Response, DesktopCommandError>>(),
            Duration::from_millis(1),
            correlation_id,
        )
        .await
        .expect_err("stalled registration times out");

        assert_eq!(error.code, ErrorCode::Internal);
        assert!(error.retryable);
        assert!(error.user_action.is_some());
        assert_eq!(error.correlation_id, correlation_id.to_string());
    }

    #[tokio::test]
    async fn delayed_registration_retry_reuses_identity_and_refreshes_wire_correlation() {
        let state = DesktopHostState::default();
        let roots = vec!["/workspace/zeta".to_owned(), "/workspace/alpha".to_owned()];
        let first = state
            .begin_project_registration(&roots)
            .expect("registration starts");
        let error = project_registration_with_deadline(
            std::future::pending::<Result<Response, DesktopCommandError>>(),
            Duration::from_millis(1),
            first.correlation_id,
        )
        .await
        .expect_err("delayed completion times out");

        let retry = state
            .begin_project_registration(&[roots[1].clone(), roots[0].clone()])
            .expect("registration retries");
        assert_eq!(retry.project_id, first.project_id);
        assert_ne!(retry.correlation_id, first.correlation_id);
        assert_eq!(error.correlation_id, first.correlation_id.to_string());

        state.finish_project_registration(&roots, retry);
        let completed = state
            .begin_project_registration(&roots)
            .expect("completed registration starts a fresh operation");
        assert_ne!(completed.project_id, retry.project_id);
        assert_ne!(completed.correlation_id, retry.correlation_id);
    }

    #[tokio::test]
    async fn stale_request_failure_does_not_clear_a_fresh_daemon_client() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = maestrod::DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let stale = MultiplexedDaemonClient::connect(&paths, "stale-client", "0.1.0")
            .await
            .expect("stale client connects");
        let fresh = MultiplexedDaemonClient::connect(&paths, "fresh-client", "0.1.0")
            .await
            .expect("fresh client connects");
        let host = DesktopHostState::default();
        *host.daemon.lock().await = Some(fresh.clone());

        host.invalidate_daemon_client(&stale).await;
        assert!(
            host.daemon
                .lock()
                .await
                .as_ref()
                .is_some_and(|cached| cached.is_same_connection(&fresh))
        );

        host.invalidate_daemon_client(&fresh).await;
        assert!(host.daemon.lock().await.is_none());
        drop(stale);
        drop(fresh);
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[test]
    fn terminal_links_are_bounded_http_and_credential_free() {
        assert_eq!(
            validate_terminal_link("https://example.com/a path")
                .expect("safe link normalizes")
                .as_str(),
            "https://example.com/a%20path"
        );
        for blocked in [
            "javascript:alert(1)",
            "file:///tmp/private",
            "https://user:password@example.com/",
            "https://example.com/?token=secret",
            "https://example.com/?API_KEY=secret",
        ] {
            assert!(
                validate_terminal_link(blocked).is_err(),
                "accepted {blocked}"
            );
        }
        assert!(
            validate_terminal_link(&format!(
                "https://example.com/{}",
                "x".repeat(super::MAXIMUM_TERMINAL_LINK_BYTES)
            ))
            .is_err()
        );
    }

    #[tokio::test]
    async fn system_snapshot_is_honest_when_daemon_is_offline() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = maestrod::DaemonPaths::isolated(temporary.path());
        let snapshot = SystemSnapshot::capture_with_paths(&paths).await;

        assert!(!snapshot.app_version.is_empty());
        assert!(!snapshot.platform.is_empty());
        assert!(!snapshot.architecture.is_empty());
        assert_eq!(snapshot.daemon.status, DaemonStatus::NotConnected);
    }

    #[tokio::test]
    async fn terminal_commands_proxy_a_reconnectable_daemon_owned_pty() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = maestrod::DaemonPaths::isolated(temporary.path());
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());

        let opened = terminal_open_with_paths(
            &paths,
            temporary.path().to_string_lossy().into_owned(),
            80,
            24,
        )
        .await
        .expect("terminal opens");
        let terminal_id = opened.terminal_id.to_string();
        terminal_resize_with_paths(&paths, &terminal_id, 100, 30)
            .await
            .expect("terminal resizes");
        terminal_write_with_paths(
            &paths,
            &terminal_id,
            b"stty size; printf 'maestro-terminal-ready\\n'\n".to_vec(),
        )
        .await
        .expect("terminal input writes");

        let mut cursor = 0;
        let mut output = Vec::new();
        for _ in 0..100 {
            let read =
                terminal_read_with_paths(&paths, &terminal_id, cursor, MIN_TERMINAL_POLL_BYTES)
                    .await
                    .expect("terminal reads");
            cursor = read.next_sequence;
            for chunk in read.chunks {
                output.extend_from_slice(&chunk.data);
            }
            let rendered = String::from_utf8_lossy(&output);
            if rendered.contains("maestro-terminal-ready") && rendered.contains("30 100") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(String::from_utf8_lossy(&output).contains("maestro-terminal-ready"));
        assert!(
            String::from_utf8_lossy(&output).contains("30 100"),
            "resize output was: {}",
            String::from_utf8_lossy(&output)
        );
        assert_eq!(
            terminal_state_with_paths(&paths, &terminal_id)
                .await
                .expect("terminal state")
                .state,
            TerminalState::Running
        );
        assert_eq!(
            terminal_close_with_paths(&paths, &terminal_id)
                .await
                .expect("terminal closes")
                .state,
            TerminalState::Closed
        );

        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[tokio::test]
    async fn recent_project_reopen_revalidates_roots_and_preserves_persisted_identity() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let paths = maestrod::DaemonPaths::isolated(temporary.path());
        let project_root = temporary.path().join("saved-project");
        std::fs::create_dir(&project_root).expect("project root creates");
        let canonical_root = project_root
            .canonicalize()
            .expect("project root canonicalizes")
            .to_string_lossy()
            .into_owned();
        let server = DaemonServer::bind(paths.clone(), DaemonConfig::default())
            .await
            .expect("server binds");
        let shutdown = server.shutdown_handle();
        let task = tokio::spawn(server.run());
        let state = DesktopHostState::default();
        let project_id = maestro_domain::ProjectId::new();
        assert!(matches!(
            state
                .request_daemon(
                    &paths,
                    maestro_protocol::Request::ProjectRegister {
                        project_id,
                        display_name: "Saved project".to_owned(),
                        roots: vec![canonical_root.clone()],
                    },
                )
                .await
                .expect("project persists"),
            maestro_protocol::Response::ProjectRegistered(_)
        ));

        let reopened = open_recent_project_with_paths(&paths, &state, project_id, "project-window")
            .await
            .expect("recent project safely reopens");

        assert_ne!(reopened.id, project_id);
        assert_eq!(reopened.roots, vec![canonical_root.clone()]);
        let grant = state
            .resolve_project(reopened.id, "project-window")
            .expect("window receives project grant");
        assert_eq!(grant.persisted_project_id, project_id);
        assert_eq!(grant.canonical_path, canonical_root);
        assert!(state.resolve_project(reopened.id, "other-window").is_err());

        drop(state);
        shutdown.request();
        task.await.expect("server task").expect("clean shutdown");
    }

    #[test]
    fn project_path_validation_canonicalizes_directories() {
        let current = std::env::current_dir().expect("current directory should exist");
        let selection = validate_project_path(&current).expect("current directory should validate");

        assert_eq!(
            selection.canonical_path,
            current.canonicalize().unwrap().to_string_lossy()
        );
        assert!(!selection.name.is_empty());
    }

    #[test]
    fn workspace_validation_preserves_multiple_canonical_roots() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        std::fs::create_dir(&first).expect("first root creates");
        std::fs::create_dir(&second).expect("second root creates");

        let (name, roots) =
            validate_workspace_paths([first.clone(), second.clone()]).expect("workspace validates");

        assert_eq!(name, "first + 1 more");
        assert_eq!(
            roots,
            vec![
                first.canonicalize().unwrap().to_string_lossy().into_owned(),
                second
                    .canonicalize()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ]
        );
    }

    #[test]
    fn project_and_terminal_grants_are_opaque_and_window_scoped() {
        let current = std::env::current_dir().expect("current directory should exist");
        let project = validate_project_path(&current).expect("current directory should validate");
        let canonical_path = project.canonical_path.clone();
        let state = DesktopHostState::default();
        let selection = state
            .grant_registered_project(
                maestro_domain::ProjectId::new(),
                project.name,
                vec![project.canonical_path],
                "project-window",
            )
            .expect("project grant should register");

        assert_ne!(selection.id.to_string(), canonical_path);
        assert_eq!(
            state
                .resolve_project(selection.id, "project-window")
                .expect("owner resolves grant")
                .canonical_path,
            canonical_path
        );
        assert!(state.resolve_project(selection.id, "other-window").is_err());

        let terminal_id = maestro_domain::TerminalId::new();
        let terminal_grant = state
            .register_terminal(terminal_id, selection.id, "project-window", false)
            .expect("terminal grant should register");
        assert_ne!(terminal_grant, terminal_id);
        assert_eq!(
            state
                .authorize_terminal(terminal_grant, "project-window")
                .expect("owner controls terminal")
                .project_grant,
            selection.id
        );
        assert!(
            state
                .authorize_terminal(terminal_grant, "other-window")
                .is_err()
        );
        let replacement = validate_project_path(&current).expect("replacement project validates");
        let replacement = state
            .grant_registered_project(
                maestro_domain::ProjectId::new(),
                replacement.name,
                vec![replacement.canonical_path],
                "project-window",
            )
            .expect("replacement grant should register");
        assert!(
            state
                .resolve_project(selection.id, "project-window")
                .is_err()
        );
        assert!(
            state
                .authorize_terminal(terminal_grant, "project-window")
                .is_err()
        );

        let replacement_terminal = maestro_domain::TerminalId::new();
        let replacement_terminal_grant = state
            .register_terminal(
                replacement_terminal,
                replacement.id,
                "project-window",
                false,
            )
            .expect("replacement terminal grant should register");
        state.revoke_window("project-window");
        assert!(
            state
                .resolve_project(replacement.id, "project-window")
                .is_err()
        );
        assert!(
            state
                .authorize_terminal(replacement_terminal_grant, "project-window")
                .is_err()
        );
    }

    #[test]
    fn project_path_validation_rejects_files() {
        let executable = std::env::current_exe().expect("test executable should exist");

        assert_eq!(
            validate_project_path(&executable),
            Err("The selected project path is not a folder.")
        );
    }

    #[test]
    fn persisted_project_roots_are_revalidated_before_granting() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let canonical = temporary
            .path()
            .canonicalize()
            .expect("temporary directory canonicalizes")
            .to_string_lossy()
            .into_owned();

        assert_eq!(
            validate_persisted_roots(std::slice::from_ref(&canonical))
                .expect("recorded canonical root remains valid"),
            vec![canonical]
        );
        assert!(validate_persisted_roots(&[]).is_err());
        assert!(
            validate_persisted_roots(&[temporary.path().join("missing").display().to_string()])
                .is_err()
        );
    }

    #[test]
    fn saved_project_identity_is_hidden_behind_a_window_scoped_grant() {
        let state = DesktopHostState::default();
        let project_id = maestro_domain::ProjectId::new();
        let current = std::env::current_dir()
            .expect("current directory")
            .canonicalize()
            .expect("current directory canonicalizes")
            .to_string_lossy()
            .into_owned();
        let selection = state
            .grant_registered_project(
                project_id,
                "Saved project".to_owned(),
                vec![current.clone()],
                "project-window",
            )
            .expect("saved project receives a grant");

        assert_ne!(selection.id, project_id);
        assert_eq!(selection.roots, vec![current.clone()]);
        let grant = state
            .resolve_project(selection.id, "project-window")
            .expect("owner resolves saved project");
        assert_eq!(grant.persisted_project_id, project_id);
        assert_eq!(grant.canonical_path, current);
        assert!(state.resolve_project(selection.id, "other-window").is_err());
    }

    #[test]
    fn structured_sessions_are_window_and_project_capability_scoped() {
        let state = DesktopHostState::default();
        let project_grant = maestro_domain::ProjectId::new();
        let session_id = maestro_domain::SessionId::new();
        state
            .register_session(session_id, project_grant, "project-window")
            .expect("session grant registers");

        let authorized = state
            .authorize_session(session_id, "project-window")
            .expect("owner authorizes session");
        assert_eq!(authorized.project_grant, project_grant);
        assert!(state.authorize_session(session_id, "other-window").is_err());

        state.revoke_window("project-window");
        assert!(
            state
                .authorize_session(session_id, "project-window")
                .is_err()
        );
    }

    #[test]
    fn terminal_discovery_and_attachment_issue_fresh_opaque_window_grants() {
        let state = DesktopHostState::default();
        let project_grant = maestro_domain::ProjectId::new();
        let persisted_terminal = maestro_domain::TerminalId::new();
        let discovery = state
            .register_terminal(persisted_terminal, project_grant, "project-window", true)
            .expect("discovery grant registers");
        assert_ne!(discovery, persisted_terminal);
        assert_eq!(
            state
                .authorize_terminal(discovery, "project-window")
                .expect("discovery capability authorizes")
                .persisted_terminal_id,
            persisted_terminal
        );

        let attached = state
            .register_terminal(persisted_terminal, project_grant, "project-window", false)
            .expect("attached grant registers");
        assert_ne!(attached, persisted_terminal);
        assert_ne!(attached, discovery);
        assert!(
            state
                .authorize_terminal(discovery, "project-window")
                .is_err()
        );
        let authorized = state
            .authorize_terminal(attached, "project-window")
            .expect("fresh attached grant authorizes");
        assert_eq!(authorized.project_grant, project_grant);
        assert_eq!(authorized.persisted_terminal_id, persisted_terminal);
        assert!(!authorized.discovery_only);
        assert!(state.authorize_terminal(attached, "other-window").is_err());
    }

    #[test]
    fn reattaching_a_tui_replaces_only_the_windows_previous_terminal_grant() {
        let state = DesktopHostState::default();
        let project_grant = maestro_domain::ProjectId::new();
        let session_id = maestro_domain::SessionId::new();
        let first_terminal = maestro_domain::TerminalId::new();
        let attached_terminal = maestro_domain::TerminalId::new();
        let first_grant = state
            .register_tui_session(session_id, first_terminal, project_grant, "project-window")
            .expect("initial TUI grants register");
        state
            .register_session(session_id, project_grant, "project-window")
            .expect("session-list refresh preserves the active TUI grant");
        assert_eq!(
            state
                .authorize_session(session_id, "project-window")
                .expect("refreshed session grant remains active")
                .terminal_id,
            Some(first_grant)
        );
        let attached_grant = state
            .register_tui_session(
                session_id,
                attached_terminal,
                project_grant,
                "project-window",
            )
            .expect("reattached TUI grant registers");

        assert!(
            state
                .authorize_terminal(first_grant, "project-window")
                .is_err()
        );
        assert_eq!(
            state
                .authorize_terminal(attached_grant, "project-window")
                .expect("new terminal grant is active")
                .project_grant,
            project_grant
        );
        assert_eq!(
            state
                .authorize_session(session_id, "project-window")
                .expect("session grant remains active")
                .terminal_id,
            Some(attached_grant)
        );
    }

    #[test]
    fn recent_and_window_layout_inputs_are_strictly_bounded() {
        assert!(validate_recent_project_limit(1).is_ok());
        assert!(validate_recent_project_limit(100).is_ok());
        assert!(validate_recent_project_limit(0).is_err());
        assert!(validate_recent_project_limit(101).is_err());
        assert!(validate_window_key("main", "main").is_ok());
        assert!(validate_window_key("other", "main").is_err());
        assert!(validate_window_layout_json(r#"{"sidebarOpen":true}"#).is_ok());
        assert!(validate_window_layout_json("[]").is_err());
        assert!(validate_window_layout_json("not-json").is_err());
        assert!(
            validate_window_layout_json(&format!(
                "{{\"value\":\"{}\"}}",
                "x".repeat(super::MAXIMUM_WINDOW_LAYOUT_BYTES)
            ))
            .is_err()
        );
    }

    #[test]
    fn desktop_errors_preserve_the_structured_safe_contract() {
        let mut error = maestro_domain::MaestroError::new(
            maestro_domain::ErrorCode::PermissionDenied,
            "Safe permission message",
        );
        error.retryable = true;
        error.user_action = Some("Review the project grant.".to_owned());
        error.details = Some(serde_json::json!({ "scope": "window" }));
        let serialized = serde_json::to_value(super::DesktopCommandError::from(error))
            .expect("desktop error serializes");

        assert_eq!(serialized["code"], "PERMISSION_DENIED");
        assert_eq!(serialized["message"], "Safe permission message");
        assert_eq!(serialized["retryable"], true);
        assert_eq!(serialized["userAction"], "Review the project grant.");
        assert_eq!(serialized["details"]["scope"], "window");
        assert!(serialized["correlationId"].as_str().is_some());
    }
}
