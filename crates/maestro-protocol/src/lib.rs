//! Length-prefixed `MessagePack` protocol shared by the Maestro desktop host and
//! daemon. Authentication is mandatory before ordinary requests are accepted.

use std::io::Cursor;

use maestro_domain::{
    AgentKind, ErrorCode, EventEnvelope, IntegrationMode, MaestroError, ProjectId, RequestId,
    RunId, SessionId, SessionState, TerminalId,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const PROTOCOL_VERSION: u16 = 9;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_HELLO_FRAME_BYTES: usize = 16 * 1024;
pub const MAX_TERMINAL_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_TERMINAL_POLL_BYTES: u32 = 256 * 1024;
pub const MIN_TERMINAL_POLL_BYTES: u32 = 4 * 1024;
pub const MAX_TERMINAL_READ_WAIT_MILLISECONDS: u32 = 30_000;
pub const MAX_TERMINAL_PATH_BYTES: usize = 4 * 1024;
pub const MAX_TERMINAL_DIMENSION: u16 = 1_000;
pub const MAX_TERMINAL_INDEX_ENTRIES: usize = 64;
pub const MAX_RECENT_PROJECTS: usize = 100;
pub const MAX_WINDOW_KEY_BYTES: usize = 256;
pub const MAX_WINDOW_LAYOUT_BYTES: usize = 256 * 1024;
pub const MAX_SESSION_EVENTS_PER_READ: usize = 1_024;
pub const MAX_SESSION_INDEX_ENTRIES: usize = 256;
pub const MAX_SESSION_EVENT_WAIT_MILLISECONDS: u32 = 30_000;
pub const MAX_SESSION_ACTION_BYTES: usize = 256 * 1024;
pub const MAX_SESSION_RAW_READ_BYTES: u32 = 256 * 1024;
pub const MAX_FAKE_EVENT_VOLUME: usize = 2_048;
pub const MAX_SETTING_SCOPE_BYTES: usize = 32;
pub const MAX_SETTING_SCOPE_REFERENCE_BYTES: usize = 256;
pub const MAX_SETTING_KEY_BYTES: usize = 128;
pub const MAX_SETTING_VALUE_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AuthenticationToken(String);

impl AuthenticationToken {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AuthenticationToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthenticationToken([REDACTED])")
    }
}

impl From<String> for AuthenticationToken {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for AuthenticationToken {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// A serialized secret whose debug representation is always redacted and whose
/// allocation is cleared when the value leaves scope.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct SensitiveString(String);

impl SensitiveString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn into_inner(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl std::fmt::Debug for SensitiveString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SensitiveString([REDACTED])")
    }
}

/// Serialized unredacted protocol bytes whose debug representation is always
/// sensitive-labeled and whose allocation is cleared when dropped.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct SensitiveBytes(Vec<u8>);

impl SensitiveBytes {
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn into_inner(mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

impl std::fmt::Debug for SensitiveBytes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SensitiveBytes([SENSITIVE])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub protocol_version: u16,
    pub client_name: String,
    pub client_version: String,
    pub authentication_token: AuthenticationToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub protocol_version: u16,
    pub daemon_version: String,
    pub connection_id: uuid::Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemSnapshot {
    pub daemon_version: String,
    pub locked: bool,
    pub storage: StorageStatus,
    pub storage_schema_version: Option<i64>,
    pub active_sessions: u32,
    pub active_terminals: u32,
    pub installed_agents: Vec<AgentKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageUnlockMode {
    Create,
    Unlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum StorageStatus {
    Ready,
    PassphraseRequired { mode: StorageUnlockMode },
    Unavailable,
}

impl StorageStatus {
    pub fn is_locked(self) -> bool {
        !matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalState {
    Running,
    Closing,
    Exited,
    Failed,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOpened {
    pub terminal_id: TerminalId,
    pub run_id: RunId,
    pub process_id: u32,
    pub canonical_cwd: String,
    pub state: TerminalState,
}

/// One bounded daemon-owned terminal entry discoverable within its project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalIndexEntry {
    pub project_id: ProjectId,
    pub terminal: TerminalOpened,
    pub kind: String,
    pub title: String,
    pub exit: Option<TerminalExit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputChunk {
    pub sequence: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReadResult {
    pub terminal_id: TerminalId,
    pub chunks: Vec<TerminalOutputChunk>,
    /// Cursor to pass as `after_sequence` on the next poll.
    pub next_sequence: u64,
    pub latest_sequence: u64,
    pub overflowed: bool,
    pub dropped_through_sequence: Option<u64>,
    pub state: TerminalState,
    pub exit: Option<TerminalExit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalStatus {
    pub terminal_id: TerminalId,
    pub state: TerminalState,
    pub exit: Option<TerminalExit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRegistered {
    pub project_id: ProjectId,
    pub display_name: String,
    pub canonical_roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentProject {
    pub project_id: ProjectId,
    pub display_name: String,
    pub canonical_roots: Vec<String>,
    pub favorite: bool,
    pub last_opened_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWindowLayout {
    pub project_id: ProjectId,
    pub window_key: String,
    pub layout_json: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPermissionDecision {
    Allow,
    Deny,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cause", content = "value", rename_all = "snake_case")]
pub enum SessionExit {
    Exited(i32),
    Signaled(i32),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRunStarted {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub process_id: u32,
}

/// A new desktop view attached to an already-running structured CLI process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRunAttached {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub process_id: u32,
}

/// A daemon-owned exact-TUI process attached to one logical agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTerminalStarted {
    pub session_id: SessionId,
    pub terminal: TerminalOpened,
}

/// A new desktop view attached to an already-running daemon-owned exact TUI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTerminalAttached {
    pub session_id: SessionId,
    pub terminal: TerminalOpened,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexEntry {
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub agent_kind: AgentKind,
    pub integration_mode: IntegrationMode,
    pub state: SessionState,
    pub title: Option<String>,
    pub active_run_id: Option<RunId>,
    pub latest_sequence: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionErrorSummary {
    pub code: ErrorCode,
    pub message: String,
    pub correlation_id: uuid::Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub active_run_id: Option<RunId>,
    pub state: SessionState,
    pub binding: Option<String>,
    pub latest_sequence: u64,
    pub dropped_through_sequence: u64,
    pub stderr: String,
    pub stderr_truncated: bool,
    pub last_exit: Option<SessionExit>,
    pub last_error: Option<SessionErrorSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReplayGap {
    pub requested_after_sequence: u64,
    pub available_after_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventBatch {
    pub session_id: SessionId,
    pub events: Vec<EventEnvelope>,
    pub next_sequence: u64,
    pub latest_sequence: u64,
    pub replay_gap: Option<SessionReplayGap>,
    pub state: SessionState,
}

/// A bounded page of explicitly opted-in, unredacted CLI stdout bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRawBatch {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub data: SensitiveBytes,
    pub next_offset: u64,
    pub captured_bytes: u64,
    pub observed_bytes: u64,
    pub truncated: bool,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDirectoryEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDirectoryEntry {
    pub path: String,
    pub display_name: String,
    pub kind: ProjectDirectoryEntryKind,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDirectoryPage {
    pub directory: String,
    pub entries: Vec<ProjectDirectoryEntry>,
    pub next_cursor: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTextFile {
    pub path: String,
    pub text: String,
    pub fingerprint: Vec<u8>,
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFileSaved {
    pub fingerprint: Vec<u8>,
    pub bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectSearchMode {
    Literal,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchOptions {
    pub pattern: String,
    pub mode: ProjectSearchMode,
    pub case_sensitive: bool,
    pub include_hidden: bool,
    pub maximum_results: usize,
    pub maximum_file_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchMatch {
    pub path: String,
    pub line: u64,
    pub byte_column: usize,
    pub byte_length: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchSummary {
    pub scanned_files: u64,
    pub skipped_files: u64,
    pub matches: usize,
    pub limit_reached: bool,
    pub cancelled: bool,
    pub consumer_stopped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchResult {
    pub matches: Vec<ProjectSearchMatch>,
    pub summary: ProjectSearchSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGitPath {
    pub bytes: Vec<u8>,
    pub display: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectGitStatusKind {
    Ordinary,
    RenamedOrCopied,
    Unmerged,
    Untracked,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGitStatusEntry {
    pub path: ProjectGitPath,
    pub original_path: Option<ProjectGitPath>,
    pub index_status: char,
    pub worktree_status: char,
    pub kind: ProjectGitStatusKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum ProjectBranchState {
    Branch(String),
    Unborn(String),
    Detached { commit: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDiffScope {
    WorkingTree,
    Staged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectGitDiff {
    pub text: String,
    pub truncated: bool,
    pub contains_binary_changes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorktree {
    pub path: String,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked_reason: Option<String>,
    pub prunable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Ping,
    SystemSnapshot,
    StopAllWork,
    StorageUnlock {
        passphrase: SensitiveString,
    },
    ProjectRegister {
        project_id: ProjectId,
        display_name: String,
        roots: Vec<String>,
    },
    ProjectRecentList {
        maximum_projects: usize,
    },
    ProjectSetFavorite {
        project_id: ProjectId,
        favorite: bool,
    },
    ProjectWindowLayoutLoad {
        project_id: ProjectId,
        window_key: String,
    },
    ProjectWindowLayoutSave {
        project_id: ProjectId,
        window_key: String,
        layout_json: String,
    },
    SettingLoad {
        scope: String,
        scope_reference: String,
        key: String,
    },
    SettingSave {
        scope: String,
        scope_reference: String,
        key: String,
        value_json: SensitiveString,
    },
    FakeSessionStart {
        project_id: ProjectId,
        scenario: String,
        binding: Option<String>,
        volume: Option<usize>,
        capture_raw_protocol: bool,
    },
    FakeSessionResume {
        session_id: SessionId,
        project_id: ProjectId,
        scenario: String,
        binding: Option<String>,
        capture_raw_protocol: bool,
    },
    FakeTuiStart {
        project_id: ProjectId,
        scenario: String,
        columns: u16,
        rows: u16,
    },
    SessionTerminalAttach {
        session_id: SessionId,
        project_id: ProjectId,
    },
    SessionStructuredAttach {
        session_id: SessionId,
        project_id: ProjectId,
    },
    SessionList {
        project_id: ProjectId,
        maximum_sessions: usize,
    },
    SessionSnapshot {
        session_id: SessionId,
    },
    SessionEventsRead {
        session_id: SessionId,
        after_sequence: u64,
        maximum_events: usize,
        wait_milliseconds: u32,
    },
    SessionRawRead {
        session_id: SessionId,
        run_id: RunId,
        after_offset: u64,
        maximum_bytes: u32,
    },
    SessionPermissionRespond {
        session_id: SessionId,
        run_id: RunId,
        request_id: String,
        decision: SessionPermissionDecision,
    },
    SessionUserInputRespond {
        session_id: SessionId,
        run_id: RunId,
        request_id: String,
        value_json: SensitiveString,
    },
    SessionGuiAction {
        session_id: SessionId,
        run_id: RunId,
        action: String,
        payload_json: SensitiveString,
    },
    ProjectDirectoryList {
        project_id: ProjectId,
        directory: String,
        cursor: u64,
        maximum_entries: usize,
        include_hidden: bool,
    },
    ProjectFileRead {
        project_id: ProjectId,
        path: String,
    },
    ProjectFileSave {
        project_id: ProjectId,
        path: String,
        text: String,
        expected_fingerprint: Vec<u8>,
    },
    ProjectSearch {
        project_id: ProjectId,
        search_id: RequestId,
        options: ProjectSearchOptions,
    },
    ProjectSearchCancel {
        search_id: RequestId,
    },
    ProjectGitStatus {
        project_id: ProjectId,
        repository: String,
    },
    ProjectGitBranch {
        project_id: ProjectId,
        repository: String,
    },
    ProjectGitDiff {
        project_id: ProjectId,
        repository: String,
        scope: ProjectDiffScope,
        maximum_bytes: usize,
    },
    ProjectGitWorktrees {
        project_id: ProjectId,
        repository: String,
    },
    SubscribeSession {
        session_id: SessionId,
        after_sequence: u64,
    },
    UnsubscribeSession {
        session_id: SessionId,
    },
    StopSession {
        session_id: SessionId,
    },
    TerminalOpen {
        project_id: ProjectId,
        cwd: String,
        columns: u16,
        rows: u16,
    },
    TerminalList {
        project_id: ProjectId,
        maximum_terminals: usize,
    },
    TerminalAttach {
        project_id: ProjectId,
        terminal_id: TerminalId,
    },
    TerminalWrite {
        terminal_id: TerminalId,
        data: Vec<u8>,
    },
    TerminalResize {
        terminal_id: TerminalId,
        columns: u16,
        rows: u16,
    },
    TerminalRead {
        terminal_id: TerminalId,
        after_sequence: u64,
        maximum_bytes: u32,
        wait_milliseconds: u32,
    },
    TerminalState {
        terminal_id: TerminalId,
    },
    TerminalClose {
        terminal_id: TerminalId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub request_id: RequestId,
    pub request: Request,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", content = "data", rename_all = "snake_case")]
pub enum Response {
    Pong,
    SystemSnapshot(SystemSnapshot),
    BackgroundWorkStopped {
        structured_sessions_stopped: u32,
        terminals_closed: u32,
    },
    StorageUnlocked,
    ProjectRegistered(ProjectRegistered),
    ProjectRecentList(Vec<RecentProject>),
    ProjectFavoriteUpdated {
        project_id: ProjectId,
        favorite: bool,
    },
    ProjectWindowLayout(ProjectWindowLayout),
    ProjectWindowLayoutSaved {
        project_id: ProjectId,
        window_key: String,
    },
    SettingValue {
        scope: String,
        scope_reference: String,
        key: String,
        value_json: Option<SensitiveString>,
    },
    SettingSaved {
        scope: String,
        scope_reference: String,
        key: String,
    },
    SessionRunStarted(SessionRunStarted),
    SessionRunAttached(SessionRunAttached),
    SessionTerminalStarted(SessionTerminalStarted),
    SessionTerminalAttached(SessionTerminalAttached),
    SessionList(Vec<SessionIndexEntry>),
    SessionSnapshot(SessionSnapshot),
    SessionEvents(SessionEventBatch),
    SessionRaw(SessionRawBatch),
    SessionPermissionAccepted {
        session_id: SessionId,
        request_id: String,
    },
    SessionUserInputAccepted {
        session_id: SessionId,
        request_id: String,
    },
    SessionGuiActionAccepted {
        session_id: SessionId,
        action_id: String,
    },
    ProjectDirectoryPage(ProjectDirectoryPage),
    ProjectTextFile(ProjectTextFile),
    ProjectFileSaved(ProjectFileSaved),
    ProjectSearchResult(ProjectSearchResult),
    ProjectSearchCancelled {
        search_id: RequestId,
    },
    ProjectGitStatus(Vec<ProjectGitStatusEntry>),
    ProjectGitBranch(ProjectBranchState),
    ProjectGitDiff(ProjectGitDiff),
    ProjectGitWorktrees(Vec<ProjectWorktree>),
    Subscribed {
        session_id: SessionId,
    },
    Unsubscribed {
        session_id: SessionId,
    },
    SessionStopped {
        session_id: SessionId,
    },
    TerminalOpened(TerminalOpened),
    TerminalList(Vec<TerminalIndexEntry>),
    TerminalAttached(TerminalOpened),
    TerminalWriteAccepted {
        terminal_id: TerminalId,
    },
    TerminalResized {
        terminal_id: TerminalId,
    },
    TerminalRead(TerminalReadResult),
    TerminalState(TerminalStatus),
    TerminalClosed(TerminalStatus),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub request_id: RequestId,
    pub response: Result<Response, MaestroError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum ServerEvent {
    Agent(EventEnvelope),
    SessionStateChanged {
        session_id: SessionId,
        state: SessionState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ClientFrame {
    Hello(ClientHello),
    Request(RequestEnvelope),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ServerFrame {
    Hello(ServerHello),
    Response(ResponseEnvelope),
    Event(ServerEvent),
    Fatal(MaestroError),
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("frame length {actual} exceeds maximum {maximum}")]
    FrameTooLarge { actual: usize, maximum: usize },
    #[error("incomplete frame: expected {expected} bytes and received {actual}")]
    IncompleteFrame { expected: usize, actual: usize },
    #[error("messagepack encoding failed: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("messagepack decoding failed: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
}

/// Serializes a protocol value and prefixes it with a network-order length.
///
/// # Errors
///
/// Returns [`CodecError`] when serialization fails or the encoded payload is
/// larger than [`MAX_FRAME_BYTES`].
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    let payload = rmp_serde::to_vec_named(value)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(CodecError::FrameTooLarge {
            actual: payload.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }

    let length = u32::try_from(payload.len()).map_err(|_| CodecError::FrameTooLarge {
        actual: payload.len(),
        maximum: MAX_FRAME_BYTES,
    })?;
    let mut framed = Vec::with_capacity(payload.len() + size_of::<u32>());
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(&payload);
    Ok(framed)
}

/// Validates and decodes one complete length-prefixed protocol frame.
///
/// # Errors
///
/// Returns [`CodecError`] for an incomplete or oversized frame, or when the
/// `MessagePack` payload cannot be deserialized as `T`.
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, CodecError> {
    if frame.len() < size_of::<u32>() {
        return Err(CodecError::IncompleteFrame {
            expected: size_of::<u32>(),
            actual: frame.len(),
        });
    }

    let prefix = frame
        .get(..size_of::<u32>())
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(CodecError::IncompleteFrame {
            expected: size_of::<u32>(),
            actual: frame.len(),
        })?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(CodecError::FrameTooLarge {
            actual: length,
            maximum: MAX_FRAME_BYTES,
        });
    }

    let expected = length + size_of::<u32>();
    if frame.len() != expected {
        return Err(CodecError::IncompleteFrame {
            expected,
            actual: frame.len(),
        });
    }

    decode_payload(&frame[4..])
}

/// Decodes a validated frame payload without allocating a second framed copy.
///
/// # Errors
///
/// Returns [`CodecError`] when the payload is oversized or invalid `MessagePack`.
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, CodecError> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(CodecError::FrameTooLarge {
            actual: payload.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    rmp_serde::from_read(Cursor::new(payload)).map_err(CodecError::from)
}

#[cfg(test)]
mod tests {
    use maestro_domain::{RequestId, TerminalId};

    use super::{
        AuthenticationToken, ClientFrame, ClientHello, PROTOCOL_VERSION, Request, RequestEnvelope,
        SensitiveBytes, SensitiveString, decode_frame, encode_frame,
    };

    #[test]
    fn request_frame_round_trip_is_lossless() {
        let frame = ClientFrame::Request(RequestEnvelope {
            request_id: RequestId::new(),
            request: Request::Ping,
        });

        let encoded = encode_frame(&frame).expect("frame encodes");
        let decoded = decode_frame::<ClientFrame>(&encoded).expect("frame decodes");

        assert_eq!(decoded, frame);
    }

    #[test]
    fn truncated_frame_is_rejected() {
        let frame = ClientFrame::Request(RequestEnvelope {
            request_id: RequestId::new(),
            request: Request::Ping,
        });
        let mut encoded = encode_frame(&frame).expect("frame encodes");
        encoded.pop();

        assert!(decode_frame::<ClientFrame>(&encoded).is_err());
    }

    #[test]
    fn binary_terminal_write_round_trip_is_lossless() {
        let terminal_id = TerminalId::new();
        let frame = ClientFrame::Request(RequestEnvelope {
            request_id: RequestId::new(),
            request: Request::TerminalWrite {
                terminal_id,
                data: vec![0, 0x1b, 0xff, b'\n'],
            },
        });

        let encoded = encode_frame(&frame).expect("frame encodes");
        assert_eq!(decode_frame::<ClientFrame>(&encoded).unwrap(), frame);
    }

    #[test]
    fn terminal_results_use_frontend_camel_case_fields() {
        let value = serde_json::to_value(super::TerminalReadResult {
            terminal_id: TerminalId::new(),
            chunks: Vec::new(),
            next_sequence: 3,
            latest_sequence: 4,
            overflowed: true,
            dropped_through_sequence: Some(2),
            state: super::TerminalState::Running,
            exit: None,
        })
        .expect("serializes");

        assert!(value.get("terminalId").is_some());
        assert_eq!(value["nextSequence"], 3);
        assert_eq!(value["latestSequence"], 4);
        assert_eq!(value["droppedThroughSequence"], 2);
        assert_eq!(value["state"], "running");
    }

    #[test]
    fn authentication_tokens_are_redacted_from_derived_debug_output() {
        let raw_token = "secret-auth-token";
        let frame = ClientFrame::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "test".to_owned(),
            client_version: "0.1.0".to_owned(),
            authentication_token: AuthenticationToken::new(raw_token),
        });

        let debug = format!("{frame:?}");
        assert!(!debug.contains(raw_token));
        assert!(debug.contains("[REDACTED]"));

        let encoded = encode_frame(&frame).expect("frame encodes");
        assert_eq!(decode_frame::<ClientFrame>(&encoded).unwrap(), frame);
    }

    #[test]
    fn storage_passphrases_are_redacted_from_debug_but_survive_transport() {
        let raw_passphrase = "correct horse battery staple";
        let frame = ClientFrame::Request(RequestEnvelope {
            request_id: RequestId::new(),
            request: Request::StorageUnlock {
                passphrase: SensitiveString::new(raw_passphrase),
            },
        });

        let debug = format!("{frame:?}");
        assert!(!debug.contains(raw_passphrase));
        assert!(debug.contains("[REDACTED]"));

        let encoded = encode_frame(&frame).expect("frame encodes");
        assert_eq!(decode_frame::<ClientFrame>(&encoded).unwrap(), frame);
    }

    #[test]
    fn sensitive_session_inputs_are_redacted_from_debug_but_survive_transport() {
        let raw_value = r#"{\"password\":\"fixture-secret\"}"#;
        let frame = ClientFrame::Request(RequestEnvelope {
            request_id: RequestId::new(),
            request: Request::SessionUserInputRespond {
                session_id: maestro_domain::SessionId::new(),
                run_id: maestro_domain::RunId::new(),
                request_id: "input-1".to_owned(),
                value_json: SensitiveString::new(raw_value),
            },
        });

        let debug = format!("{frame:?}");
        assert!(!debug.contains("fixture-secret"));
        assert!(debug.contains("[REDACTED]"));

        let encoded = encode_frame(&frame).expect("frame encodes");
        assert_eq!(decode_frame::<ClientFrame>(&encoded).unwrap(), frame);
    }

    #[test]
    fn sensitive_raw_bytes_are_labeled_in_debug_and_lossless_in_transport() {
        let raw_value = b"{\"token\":\"fixture-secret\"}\n".to_vec();
        let batch = super::SessionRawBatch {
            session_id: maestro_domain::SessionId::new(),
            run_id: maestro_domain::RunId::new(),
            data: SensitiveBytes::new(raw_value.clone()),
            next_offset: u64::try_from(raw_value.len()).expect("fixture length fits"),
            captured_bytes: u64::try_from(raw_value.len()).expect("fixture length fits"),
            observed_bytes: u64::try_from(raw_value.len()).expect("fixture length fits"),
            truncated: false,
            complete: true,
        };

        let debug = format!("{batch:?}");
        assert!(!debug.contains("fixture-secret"));
        assert!(debug.contains("[SENSITIVE]"));

        let encoded = encode_frame(&batch).expect("batch encodes");
        let decoded = decode_frame::<super::SessionRawBatch>(&encoded).expect("batch decodes");
        assert_eq!(decoded.data.expose(), raw_value);
        assert_eq!(decoded, batch);
    }
}
