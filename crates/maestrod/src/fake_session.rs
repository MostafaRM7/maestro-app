//! Structured-session supervision for the deterministic fake CLI.
//!
//! This module is intentionally isolated until the daemon session IPC and
//! durable event repository contracts land. It owns real fake-agent child
//! processes through `maestro-process`; it never invokes a shell.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use maestro_domain::{
    ErrorCode, EventEnvelope, EventSource, EventVisibility, MaestroError, NormalizedEvent, RunId,
    SessionId, SessionState,
};
use maestro_process::{ExitCause, ProcessError, ProcessSpawner, ProcessSpec, StructuredProcess};
use maestro_redaction::redact_json;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::AsyncReadExt,
    process::{ChildStderr, ChildStdout},
    sync::{Mutex, RwLock, broadcast},
    time::Instant,
};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const READ_CHUNK_BYTES: usize = 8 * 1024;
const SUPPORTED_FAKE_PROTOCOL_VERSION: u64 = 1;
const MAX_LAUNCH_TEXT_BYTES: usize = 1024;

/// Independent limits for every untrusted or fan-out-controlled buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeSessionLimits {
    pub maximum_frame_bytes: usize,
    pub replay_events: usize,
    pub broadcast_events: usize,
    pub stderr_bytes: usize,
    pub pending_requests: usize,
    pub maximum_input_bytes: usize,
    pub maximum_sessions: usize,
    pub maximum_raw_protocol_bytes: usize,
    pub request_timeout: Duration,
    pub termination_grace: Duration,
}

impl Default for FakeSessionLimits {
    fn default() -> Self {
        Self {
            maximum_frame_bytes: 1024 * 1024,
            replay_events: 2_048,
            broadcast_events: 256,
            stderr_bytes: 256 * 1024,
            pending_requests: 128,
            maximum_input_bytes: 1024 * 1024,
            maximum_sessions: 256,
            maximum_raw_protocol_bytes: 1024 * 1024,
            request_timeout: Duration::from_mins(5),
            termination_grace: Duration::from_secs(2),
        }
    }
}

impl FakeSessionLimits {
    fn validate(self) -> Result<Self, MaestroError> {
        if self.maximum_frame_bytes == 0
            || self.replay_events == 0
            || self.broadcast_events == 0
            || self.pending_requests == 0
            || self.maximum_input_bytes == 0
            || self.maximum_sessions == 0
            || self.maximum_raw_protocol_bytes == 0
            || self.request_timeout.is_zero()
        {
            return Err(invalid_request(
                "fake-session limits other than stderr capacity must be nonzero",
            ));
        }
        Ok(self)
    }
}

/// Identifies the real child process backing a logical session run.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeRunHandle {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub process_id: u32,
}

/// A bounded, non-sensitive view of current and last-run state.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeSessionSnapshot {
    pub session_id: SessionId,
    pub active_run_id: Option<RunId>,
    pub state: SessionState,
    pub binding: Option<String>,
    pub latest_sequence: u64,
    pub dropped_through_sequence: u64,
    pub stderr: Vec<u8>,
    pub stderr_truncated: bool,
    pub last_exit: Option<ExitCause>,
    pub last_error: Option<MaestroError>,
}

/// Exact stdout protocol bytes for one explicitly opted-in structured run.
///
/// The payload is intentionally unredacted and must only be exposed through a
/// sensitive raw inspector. Its [`Debug`] representation never includes bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct FakeRawProtocolCapture {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub bytes: Vec<u8>,
    pub observed_byte_count: u64,
    pub truncated: bool,
    pub complete: bool,
}

impl FakeRawProtocolCapture {
    pub fn into_bytes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}

impl fmt::Debug for FakeRawProtocolCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeRawProtocolCapture")
            .field("session_id", &self.session_id)
            .field("run_id", &self.run_id)
            .field("bytes", &"[SENSITIVE RAW PROTOCOL BYTES]")
            .field("captured_byte_count", &self.bytes.len())
            .field("observed_byte_count", &self.observed_byte_count)
            .field("truncated", &self.truncated)
            .field("complete", &self.complete)
            .finish()
    }
}

impl Drop for FakeRawProtocolCapture {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
    Cancel,
}

impl PermissionDecision {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayGap {
    pub requested_after_sequence: u64,
    pub available_after_sequence: u64,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum FakeSubscriptionError {
    #[error("live session subscription lagged by {missed} events")]
    Lagged { missed: u64 },
    #[error("live session subscription closed")]
    Closed,
}

/// Replay is delivered first; future events come from the same ordered stream.
#[derive(Debug)]
pub struct FakeSessionSubscription {
    replay: VecDeque<EventEnvelope>,
    receiver: broadcast::Receiver<EventEnvelope>,
    gap: Option<ReplayGap>,
}

impl FakeSessionSubscription {
    pub const fn replay_gap(&self) -> Option<ReplayGap> {
        self.gap
    }

    /// Returns an immediately available replayed or live event without waiting.
    ///
    /// # Errors
    ///
    /// Returns [`FakeSubscriptionError`] when the live receiver lags or closes.
    pub fn try_recv(&mut self) -> Result<Option<EventEnvelope>, FakeSubscriptionError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(Some(event));
        }
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(broadcast::error::TryRecvError::Empty) => Ok(None),
            Err(broadcast::error::TryRecvError::Lagged(missed)) => {
                Err(FakeSubscriptionError::Lagged { missed })
            }
            Err(broadcast::error::TryRecvError::Closed) => Err(FakeSubscriptionError::Closed),
        }
    }

    /// Receives the next replayed or live normalized event.
    ///
    /// # Errors
    ///
    /// Returns [`FakeSubscriptionError`] when the live receiver lags or closes.
    pub async fn recv(&mut self) -> Result<EventEnvelope, FakeSubscriptionError> {
        if let Some(event) = self.replay.pop_front() {
            return Ok(event);
        }
        self.receiver.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Lagged(missed) => FakeSubscriptionError::Lagged { missed },
            broadcast::error::RecvError::Closed => FakeSubscriptionError::Closed,
        })
    }
}

/// Owns logical fake sessions, real child runs, writer leases, and event fan-out.
#[derive(Debug, Clone)]
pub struct FakeSessionSupervisor {
    inner: Arc<SupervisorInner>,
}

#[derive(Debug)]
struct SupervisorInner {
    process_spawner: ProcessSpawner,
    limits: FakeSessionLimits,
    sessions: RwLock<HashMap<SessionId, Arc<SessionRecord>>>,
    binding_writers: StdMutex<HashMap<String, BindingWriter>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BindingWriter {
    token: Uuid,
    session_id: SessionId,
}

#[derive(Debug)]
struct SessionRecord {
    session_id: SessionId,
    launching: AtomicBool,
    data: Mutex<SessionData>,
    events: Mutex<EventLog>,
    sender: broadcast::Sender<EventEnvelope>,
    stderr: Mutex<BoundedBytes>,
    raw_capture: Mutex<Option<Arc<Mutex<RawProtocolCaptureState>>>>,
}

#[derive(Debug)]
struct SessionData {
    state: SessionState,
    binding: Option<String>,
    active_run: Option<Arc<RunControl>>,
    last_exit: Option<ExitCause>,
    last_error: Option<MaestroError>,
}

#[derive(Debug)]
struct RunControl {
    run_id: RunId,
    process_id: u32,
    lease_token: Uuid,
    leased_binding: Mutex<Option<String>>,
    process: Mutex<StructuredProcess>,
    pending: Mutex<PendingTracker>,
    stop_requested: AtomicBool,
    finalized: AtomicBool,
    raw_capture: Option<Arc<Mutex<RawProtocolCaptureState>>>,
}

struct RawProtocolCaptureState {
    run_id: RunId,
    bytes: Zeroizing<Vec<u8>>,
    observed_byte_count: u64,
    truncated: bool,
    complete: bool,
}

impl fmt::Debug for RawProtocolCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawProtocolCaptureState")
            .field("run_id", &self.run_id)
            .field("bytes", &"[SENSITIVE RAW PROTOCOL BYTES]")
            .field("captured_byte_count", &self.bytes.len())
            .field("observed_byte_count", &self.observed_byte_count)
            .field("truncated", &self.truncated)
            .field("complete", &self.complete)
            .finish()
    }
}

#[derive(Debug)]
struct EventLog {
    retained: VecDeque<EventEnvelope>,
    latest_sequence: u64,
    dropped_through_sequence: u64,
}

#[derive(Debug, Default)]
struct BoundedBytes {
    bytes: VecDeque<u8>,
    truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PendingKind {
    Permission,
    UserInput,
    GuiAction,
}

impl PendingKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::UserInput => "user_input",
            Self::GuiAction => "gui_action",
        }
    }
}

#[derive(Debug, Clone)]
struct PendingRequest {
    kind: PendingKind,
    deadline: Instant,
}

#[derive(Debug)]
struct PendingTracker {
    active: HashMap<String, PendingRequest>,
    in_flight: HashMap<String, PendingRequest>,
    resolved_order: VecDeque<(String, PendingKind)>,
    resolved: HashSet<(String, PendingKind)>,
    delivery_uncertain: HashSet<(String, PendingKind)>,
    capacity: usize,
}

#[derive(Debug)]
enum StructuredWriteError {
    Definite(MaestroError),
    Uncertain(MaestroError),
}

impl StructuredWriteError {
    fn into_error(self) -> MaestroError {
        match self {
            Self::Definite(error) | Self::Uncertain(error) => error,
        }
    }
}

#[derive(Debug)]
struct LaunchGuard {
    inner: Arc<SupervisorInner>,
    record: Arc<SessionRecord>,
    lease_token: Uuid,
    reserved_binding: Option<String>,
    committed: bool,
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        self.record.launching.store(false, Ordering::Release);
        if !self.committed
            && let Some(binding) = self.reserved_binding.take()
        {
            self.inner.release_binding(&binding, self.lease_token);
        }
    }
}

impl FakeSessionSupervisor {
    /// Creates a supervisor around the shared process admission controller.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when any configured bound is invalid.
    pub fn new(
        process_spawner: ProcessSpawner,
        limits: FakeSessionLimits,
    ) -> Result<Self, MaestroError> {
        Ok(Self {
            inner: Arc::new(SupervisorInner {
                process_spawner,
                limits: limits.validate()?,
                sessions: RwLock::new(HashMap::new()),
                binding_writers: StdMutex::new(HashMap::new()),
            }),
        })
    }

    /// Launches a new logical session and one real fake-agent process.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] for invalid launch data, exhausted capacity,
    /// binding conflicts, or process startup failures.
    pub async fn start(
        &self,
        executable: impl Into<PathBuf>,
        scenario: impl Into<String>,
        cwd: impl Into<PathBuf>,
        binding: Option<String>,
    ) -> Result<FakeRunHandle, MaestroError> {
        self.start_with_raw_capture(executable, scenario, cwd, binding, false)
            .await
    }

    /// Launches a new logical session with an explicit raw-protocol opt-in.
    ///
    /// Raw capture is disabled unless `capture_raw_protocol` is true before
    /// launch. Captured stdout remains bounded by the supervisor limits.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] for invalid launch data, exhausted capacity,
    /// binding conflicts, or process startup failures.
    pub async fn start_with_raw_capture(
        &self,
        executable: impl Into<PathBuf>,
        scenario: impl Into<String>,
        cwd: impl Into<PathBuf>,
        binding: Option<String>,
        capture_raw_protocol: bool,
    ) -> Result<FakeRunHandle, MaestroError> {
        self.start_with_volume_and_raw_capture(
            executable,
            scenario,
            cwd,
            binding,
            None,
            capture_raw_protocol,
        )
        .await
    }

    /// Launches a new session with an explicit flood volume for deterministic tests.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] for invalid launch data, exhausted capacity,
    /// binding conflicts, or process startup failures.
    pub async fn start_with_volume(
        &self,
        executable: impl Into<PathBuf>,
        scenario: impl Into<String>,
        cwd: impl Into<PathBuf>,
        binding: Option<String>,
        volume: Option<usize>,
    ) -> Result<FakeRunHandle, MaestroError> {
        self.start_with_volume_and_raw_capture(executable, scenario, cwd, binding, volume, false)
            .await
    }

    /// Launches a deterministic-volume run with an explicit raw capture opt-in.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] for invalid launch data, exhausted capacity,
    /// binding conflicts, or process startup failures.
    pub async fn start_with_volume_and_raw_capture(
        &self,
        executable: impl Into<PathBuf>,
        scenario: impl Into<String>,
        cwd: impl Into<PathBuf>,
        binding: Option<String>,
        volume: Option<usize>,
        capture_raw_protocol: bool,
    ) -> Result<FakeRunHandle, MaestroError> {
        let session_id = SessionId::new();
        let record = Arc::new(SessionRecord::new(
            session_id,
            self.inner.limits.broadcast_events,
        ));
        {
            let mut sessions = self.inner.sessions.write().await;
            if sessions.len() >= self.inner.limits.maximum_sessions {
                return Err(invalid_request(
                    "fake-session retention limit has been reached",
                ));
            }
            sessions.insert(session_id, Arc::clone(&record));
        }
        let result = self
            .launch(
                record,
                executable.into(),
                scenario.into(),
                cwd.into(),
                binding,
                volume,
                capture_raw_protocol,
            )
            .await;
        if result.is_err() {
            let remove_unstarted = self
                .inner
                .sessions
                .read()
                .await
                .get(&session_id)
                .is_some_and(|record| {
                    record
                        .data
                        .try_lock()
                        .is_ok_and(|data| data.state == SessionState::Created)
                });
            if remove_unstarted {
                self.inner.sessions.write().await.remove(&session_id);
            }
        }
        result
    }

    /// Starts a second run in an existing logical session.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when the session is unknown or active, the
    /// binding is unavailable, or the process cannot be started.
    pub async fn resume(
        &self,
        session_id: SessionId,
        executable: impl Into<PathBuf>,
        scenario: impl Into<String>,
        cwd: impl Into<PathBuf>,
        binding: Option<String>,
    ) -> Result<FakeRunHandle, MaestroError> {
        self.resume_with_raw_capture(session_id, executable, scenario, cwd, binding, false)
            .await
    }

    /// Starts a second run with an explicit raw-protocol capture opt-in.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when the session is unknown or active, the
    /// binding is unavailable, or the process cannot be started.
    pub async fn resume_with_raw_capture(
        &self,
        session_id: SessionId,
        executable: impl Into<PathBuf>,
        scenario: impl Into<String>,
        cwd: impl Into<PathBuf>,
        binding: Option<String>,
        capture_raw_protocol: bool,
    ) -> Result<FakeRunHandle, MaestroError> {
        let record = self.session(session_id).await?;
        let effective_binding = {
            let data = record.data.lock().await;
            if data.active_run.is_some() {
                return Err(invalid_request(
                    "the logical session already has an active writer",
                ));
            }
            match (&data.binding, binding) {
                (Some(stored), Some(requested)) if stored != &requested => {
                    return Err(invalid_request(
                        "the requested binding does not match the logical session",
                    ));
                }
                (Some(stored), _) => Some(stored.clone()),
                (None, requested) => requested,
            }
        };
        self.launch(
            record,
            executable.into(),
            scenario.into(),
            cwd.into(),
            effective_binding,
            None,
            capture_raw_protocol,
        )
        .await
    }

    /// Returns a replay/live subscription created under the event ordering lock.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when the session is unknown or the cursor is
    /// ahead of the session event stream.
    pub async fn subscribe(
        &self,
        session_id: SessionId,
        after_sequence: u64,
    ) -> Result<FakeSessionSubscription, MaestroError> {
        let record = self.session(session_id).await?;
        let events = record.events.lock().await;
        if after_sequence > events.latest_sequence {
            return Err(invalid_request(
                "the replay cursor is newer than the session event stream",
            ));
        }
        let receiver = record.sender.subscribe();
        let gap = (after_sequence < events.dropped_through_sequence).then_some(ReplayGap {
            requested_after_sequence: after_sequence,
            available_after_sequence: events.dropped_through_sequence,
        });
        let cursor = after_sequence.max(events.dropped_through_sequence);
        let replay = events
            .retained
            .iter()
            .filter(|event| event.sequence > cursor)
            .cloned()
            .collect();
        Ok(FakeSessionSubscription {
            replay,
            receiver,
            gap,
        })
    }

    /// Returns the current bounded logical-session snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when the session is unknown.
    pub async fn snapshot(
        &self,
        session_id: SessionId,
    ) -> Result<FakeSessionSnapshot, MaestroError> {
        let record = self.session(session_id).await?;
        let data = record.data.lock().await;
        let events = record.events.lock().await;
        let stderr = record.stderr.lock().await;
        Ok(FakeSessionSnapshot {
            session_id,
            active_run_id: data.active_run.as_ref().map(|run| run.run_id),
            state: data.state,
            binding: data.binding.clone(),
            latest_sequence: events.latest_sequence,
            dropped_through_sequence: events.dropped_through_sequence,
            stderr: stderr.bytes.iter().copied().collect(),
            stderr_truncated: stderr.truncated,
            last_exit: data.last_exit,
            last_error: data.last_error.clone(),
        })
    }

    /// Returns the immutable identity of the currently active structured run
    /// without launching, resuming, or taking ownership of the process.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when the session is unknown or has no active,
    /// non-finalized structured process to attach.
    pub async fn attach_active(
        &self,
        session_id: SessionId,
    ) -> Result<FakeRunHandle, MaestroError> {
        let record = self.session(session_id).await?;
        let run =
            record.data.lock().await.active_run.clone().ok_or_else(|| {
                invalid_request("the structured session does not have an active run")
            })?;
        if run.finalized.load(Ordering::Acquire) {
            return Err(invalid_request(
                "the structured session run has already finalized",
            ));
        }
        Ok(FakeRunHandle {
            session_id,
            run_id: run.run_id,
            process_id: run.process_id,
        })
    }

    /// Returns the bounded exact-byte stdout capture for one opted-in run.
    ///
    /// The returned bytes are intentionally unredacted and sensitive. A run
    /// launched without raw capture returns `None`.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when the logical session is unknown.
    pub async fn raw_capture(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<FakeRawProtocolCapture>, MaestroError> {
        let record = self.session(session_id).await?;
        let capture = record.raw_capture.lock().await.clone();
        let Some(capture) = capture else {
            return Ok(None);
        };
        let capture = capture.lock().await;
        if capture.run_id != run_id {
            return Ok(None);
        }
        Ok(Some(FakeRawProtocolCapture {
            session_id,
            run_id,
            bytes: capture.bytes.to_vec(),
            observed_byte_count: capture.observed_byte_count,
            truncated: capture.truncated,
            complete: capture.complete,
        }))
    }

    /// Sends a single-use, correlated permission decision to the exact run.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when identifiers are invalid, the request is
    /// unavailable, or the process write fails.
    pub async fn respond_permission(
        &self,
        session_id: SessionId,
        run_id: RunId,
        request_id: &str,
        decision: PermissionDecision,
    ) -> Result<(), MaestroError> {
        validate_identifier(request_id, "permission request ID")?;
        let (record, run) = self.active_run(session_id, run_id).await?;
        self.deliver_pending_response(
            record,
            run,
            request_id,
            PendingKind::Permission,
            json!({ "request_id": request_id, "decision": decision.as_str() }),
            user_event(
                "gui_permission_response",
                json!({ "request_id": request_id, "decision": decision.as_str() }),
            ),
        )
        .await
    }

    /// Sends sensitive user input without copying its value into the audit event.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when identifiers are invalid, the request is
    /// unavailable, the value is oversized, or the process write fails.
    pub async fn respond_user_input(
        &self,
        session_id: SessionId,
        run_id: RunId,
        request_id: &str,
        value: Value,
    ) -> Result<(), MaestroError> {
        validate_identifier(request_id, "user-input request ID")?;
        let (record, run) = self.active_run(session_id, run_id).await?;
        self.deliver_pending_response(
            record,
            run,
            request_id,
            PendingKind::UserInput,
            json!({ "request_id": request_id, "value": value }),
            NormalizedEvent {
                kind: "gui_user_input_response".to_owned(),
                visibility: EventVisibility::Sensitive,
                payload: json!({ "request_id": request_id, "value_recorded": false }),
                vendor_event_id: None,
                raw_segment_reference: None,
            },
        )
        .await
    }

    async fn deliver_pending_response(
        &self,
        record: Arc<SessionRecord>,
        run: Arc<RunControl>,
        request_id: &str,
        kind: PendingKind,
        response: Value,
        audit_event: NormalizedEvent,
    ) -> Result<(), MaestroError> {
        claim_pending(&run, request_id, kind).await?;
        let bytes = match self.inner.encode_json(&response) {
            Ok(bytes) => bytes,
            Err(error) => {
                if restore_pending(&run, request_id, kind).await {
                    expire_request(&self.inner, &record, &run, request_id, kind).await;
                }
                return Err(annotate_delivery_error(error, "not_delivered", true));
            }
        };

        match self.inner.write_json_bytes(&run, &bytes).await {
            Ok(()) => complete_pending(&run, request_id, kind)
                .await
                .map_err(|error| annotate_delivery_error(error, "delivered", false))?,
            Err(StructuredWriteError::Definite(error)) => {
                if restore_pending(&run, request_id, kind).await {
                    expire_request(&self.inner, &record, &run, request_id, kind).await;
                }
                return Err(annotate_delivery_error(error, "not_delivered", true));
            }
            Err(StructuredWriteError::Uncertain(error)) => {
                if !mark_pending_delivery_uncertain(&run, request_id, kind).await {
                    update_waiting_state(&record, &run).await;
                    return Err(annotate_delivery_error(error, "uncertain", false));
                }
            }
        }

        update_waiting_state(&record, &run).await;
        self.inner
            .publish(&record, Some(run.run_id), EventSource::Gui, audit_event)
            .await
            .map_err(|error| annotate_delivery_error(error, "delivered", false))?;
        Ok(())
    }

    /// Sends an annotated GUI action and returns its generated correlation ID.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when the action is invalid, request capacity is
    /// exhausted, the payload is oversized, or the process write fails.
    pub async fn send_gui_action(
        &self,
        session_id: SessionId,
        run_id: RunId,
        action: &str,
        payload: Value,
    ) -> Result<String, MaestroError> {
        validate_identifier(action, "GUI action name")?;
        let (record, run) = self.active_run(session_id, run_id).await?;
        let action_id = Uuid::new_v4().to_string();
        register_pending(
            &self.inner,
            &record,
            &run,
            action_id.clone(),
            PendingKind::GuiAction,
        )
        .await?;
        self.inner
            .publish(
                &record,
                Some(run_id),
                EventSource::Gui,
                user_event(
                    "gui_action",
                    json!({ "action_id": action_id, "action": action }),
                ),
            )
            .await?;
        if let Err(error) = self
            .inner
            .write_json(
                &run,
                &json!({ "action_id": action_id, "action": action, "payload": payload }),
            )
            .await
        {
            run.pending
                .lock()
                .await
                .force_resolve(&action_id, PendingKind::GuiAction);
            return Err(error);
        }
        Ok(action_id)
    }

    /// Stops the complete process group through `StructuredProcess::terminate`.
    ///
    /// # Errors
    ///
    /// Returns [`MaestroError`] when the run is unavailable, already stopping,
    /// or cannot be terminated.
    pub async fn stop(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<ExitCause, MaestroError> {
        let (record, run) = self.active_run(session_id, run_id).await?;
        if run.stop_requested.swap(true, Ordering::AcqRel) {
            return Err(invalid_request("the run is already stopping"));
        }
        set_state(&record, SessionState::Interrupting).await;
        self.inner
            .publish(
                &record,
                Some(run_id),
                EventSource::Gui,
                user_event("gui_stop_requested", json!({ "run_id": run_id })),
            )
            .await?;
        let result = run
            .process
            .lock()
            .await
            .terminate(self.inner.limits.termination_grace)
            .await
            .map_err(|error| safe_process_error(&error, "terminate"))?;
        Ok(result)
    }

    /// Requests concurrent termination of every currently active structured
    /// fake run and returns the number for which termination was initiated.
    pub async fn stop_all(&self) -> usize {
        let records = self
            .inner
            .sessions
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut active_runs = Vec::new();
        for record in records {
            if let Some(run) = record.data.lock().await.active_run.clone()
                && !run.finalized.load(Ordering::Acquire)
            {
                active_runs.push((record.session_id, run.run_id));
            }
        }

        let mut terminations = tokio::task::JoinSet::new();
        for (session_id, run_id) in active_runs {
            let supervisor = self.clone();
            terminations.spawn(async move { supervisor.stop(session_id, run_id).await.is_ok() });
        }
        let mut stopped = 0_usize;
        while let Some(result) = terminations.join_next().await {
            if result.unwrap_or(false) {
                stopped += 1;
            }
        }
        stopped
    }

    pub async fn session_count(&self) -> usize {
        self.inner.sessions.read().await.len()
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn launch(
        &self,
        record: Arc<SessionRecord>,
        executable: PathBuf,
        scenario: String,
        cwd: PathBuf,
        binding: Option<String>,
        volume: Option<usize>,
        capture_raw_protocol: bool,
    ) -> Result<FakeRunHandle, MaestroError> {
        validate_launch(&executable, &scenario, &cwd, binding.as_deref())?;
        if record
            .launching
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(invalid_request(
                "the logical session is already launching a run",
            ));
        }

        let lease_token = Uuid::new_v4();
        if let Some(value) = binding.as_deref()
            && let Err(error) = self
                .inner
                .claim_binding(value, lease_token, record.session_id)
        {
            record.launching.store(false, Ordering::Release);
            return Err(error);
        }
        let mut guard = LaunchGuard {
            inner: Arc::clone(&self.inner),
            record: Arc::clone(&record),
            lease_token,
            reserved_binding: binding.clone(),
            committed: false,
        };
        set_state(&record, SessionState::Starting).await;
        {
            let mut stderr = record.stderr.lock().await;
            *stderr = BoundedBytes::default();
        }
        {
            let mut data = record.data.lock().await;
            data.last_exit = None;
            data.last_error = None;
            if binding.is_some() {
                data.binding.clone_from(&binding);
            }
        }

        let mut spec = ProcessSpec::new(executable, cwd)
            .arguments(["--scenario".to_owned(), scenario.clone()]);
        if let Some(value) = binding.as_deref() {
            spec = spec.arguments(["--binding".to_owned(), value.to_owned()]);
        }
        if let Some(value) = volume {
            spec = spec.arguments(["--volume".to_owned(), value.to_string()]);
        }

        let run_id = RunId::new();
        let mut process = match self
            .inner
            .process_spawner
            .spawn_structured(run_id, spec)
            .await
        {
            Ok(process) => process,
            Err(error) => {
                let result = safe_process_error(&error, "spawn");
                self.inner
                    .record_launch_failure(&record, result.clone())
                    .await;
                return Err(result);
            }
        };
        debug_assert_eq!(process.run_id(), run_id);
        let process_id = process.pid();
        let raw_capture = capture_raw_protocol
            .then(|| Arc::new(Mutex::new(RawProtocolCaptureState::new(run_id))));
        let stdout = match process.take_stdout() {
            Ok(stdout) => stdout,
            Err(error) => {
                let _ = process.terminate(self.inner.limits.termination_grace).await;
                let result = safe_process_error(&error, "take_stdout");
                self.inner
                    .record_launch_failure(&record, result.clone())
                    .await;
                return Err(result);
            }
        };
        let stderr = match process.take_stderr() {
            Ok(stderr) => stderr,
            Err(error) => {
                let _ = process.terminate(self.inner.limits.termination_grace).await;
                let result = safe_process_error(&error, "take_stderr");
                self.inner
                    .record_launch_failure(&record, result.clone())
                    .await;
                return Err(result);
            }
        };
        let run = Arc::new(RunControl {
            run_id,
            process_id,
            lease_token,
            leased_binding: Mutex::new(binding),
            process: Mutex::new(process),
            pending: Mutex::new(PendingTracker::new(self.inner.limits.pending_requests)),
            stop_requested: AtomicBool::new(false),
            finalized: AtomicBool::new(false),
            raw_capture: raw_capture.clone(),
        });
        {
            let mut data = record.data.lock().await;
            if data.active_run.is_some() {
                let _ = run
                    .process
                    .lock()
                    .await
                    .terminate(self.inner.limits.termination_grace)
                    .await;
                return Err(invalid_request(
                    "the logical session already has an active run",
                ));
            }
            data.active_run = Some(Arc::clone(&run));
        }
        *record.raw_capture.lock().await = raw_capture;
        guard.committed = true;
        drop(guard);
        self.inner
            .publish(
                &record,
                Some(run_id),
                EventSource::Daemon,
                user_event(
                    "run_started",
                    json!({ "run_id": run_id, "process_id": process_id, "scenario": scenario }),
                ),
            )
            .await?;
        tokio::spawn(supervise_run(
            Arc::clone(&self.inner),
            Arc::clone(&record),
            Arc::clone(&run),
            stdout,
            stderr,
        ));
        Ok(FakeRunHandle {
            session_id: record.session_id,
            run_id,
            process_id,
        })
    }

    async fn session(&self, session_id: SessionId) -> Result<Arc<SessionRecord>, MaestroError> {
        self.inner
            .sessions
            .read()
            .await
            .get(&session_id)
            .cloned()
            .ok_or_else(|| session_not_found(session_id))
    }

    async fn active_run(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<(Arc<SessionRecord>, Arc<RunControl>), MaestroError> {
        let record = self.session(session_id).await?;
        let run = record
            .data
            .lock()
            .await
            .active_run
            .clone()
            .filter(|candidate| candidate.run_id == run_id)
            .ok_or_else(|| invalid_request("the requested run is not active in this session"))?;
        if run.finalized.load(Ordering::Acquire) {
            return Err(invalid_request("the requested run has already finalized"));
        }
        Ok((record, run))
    }
}

impl SessionRecord {
    fn new(session_id: SessionId, broadcast_capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(broadcast_capacity);
        Self {
            session_id,
            launching: AtomicBool::new(false),
            data: Mutex::new(SessionData {
                state: SessionState::Created,
                binding: None,
                active_run: None,
                last_exit: None,
                last_error: None,
            }),
            events: Mutex::new(EventLog {
                retained: VecDeque::new(),
                latest_sequence: 0,
                dropped_through_sequence: 0,
            }),
            sender,
            stderr: Mutex::new(BoundedBytes::default()),
            raw_capture: Mutex::new(None),
        }
    }
}

impl SupervisorInner {
    fn claim_binding(
        &self,
        binding: &str,
        token: Uuid,
        session_id: SessionId,
    ) -> Result<(), MaestroError> {
        validate_identifier(binding, "vendor binding")?;
        let mut writers = self
            .binding_writers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(owner) = writers.get(binding) {
            let mut error = invalid_request("the vendor binding already has an active writer");
            error.details = Some(json!({
                "binding": binding,
                "owner_session_id": owner.session_id,
            }));
            return Err(error);
        }
        writers.insert(binding.to_owned(), BindingWriter { token, session_id });
        Ok(())
    }

    fn release_binding(&self, binding: &str, token: Uuid) {
        let mut writers = self
            .binding_writers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if writers
            .get(binding)
            .is_some_and(|owner| owner.token == token)
        {
            writers.remove(binding);
        }
    }

    async fn publish(
        &self,
        record: &SessionRecord,
        run_id: Option<RunId>,
        source: EventSource,
        mut event: NormalizedEvent,
    ) -> Result<EventEnvelope, MaestroError> {
        // The replay queue and live channel are display/persistence candidates.
        // Both receive the same redacted payload; this module stores no raw frames.
        event.payload = redact_json(&event.payload);
        let mut log = record.events.lock().await;
        let sequence = log
            .latest_sequence
            .checked_add(1)
            .ok_or_else(|| MaestroError::new(ErrorCode::Internal, "session sequence exhausted"))?;
        let envelope = EventEnvelope::new(record.session_id, run_id, sequence, source, event);
        log.latest_sequence = sequence;
        log.retained.push_back(envelope.clone());
        while log.retained.len() > self.limits.replay_events {
            if let Some(dropped) = log.retained.pop_front() {
                log.dropped_through_sequence = dropped.sequence;
            }
        }
        // Replay is updated before fan-out. No receivers is a normal detached state.
        let _ = record.sender.send(envelope.clone());
        Ok(envelope)
    }

    fn encode_json(&self, value: &Value) -> Result<Vec<u8>, MaestroError> {
        let mut bytes = serde_json::to_vec(value)
            .map_err(|_| invalid_request("the structured input could not be encoded"))?;
        bytes.push(b'\n');
        if bytes.len() > self.limits.maximum_input_bytes {
            let mut error = MaestroError::new(
                ErrorCode::InputTooLarge,
                "structured input exceeds the configured maximum",
            );
            error.details = Some(json!({
                "actual_bytes": bytes.len(),
                "maximum_bytes": self.limits.maximum_input_bytes,
            }));
            return Err(error);
        }
        Ok(bytes)
    }

    async fn write_json_bytes(
        &self,
        run: &RunControl,
        bytes: &[u8],
    ) -> Result<(), StructuredWriteError> {
        if run.finalized.load(Ordering::Acquire) || run.stop_requested.load(Ordering::Acquire) {
            return Err(StructuredWriteError::Definite(invalid_request(
                "the run no longer accepts input",
            )));
        }
        let mut process = run.process.lock().await;
        if run.finalized.load(Ordering::Acquire) || run.stop_requested.load(Ordering::Acquire) {
            return Err(StructuredWriteError::Definite(invalid_request(
                "the run no longer accepts input",
            )));
        }
        match process.write(bytes).await {
            Ok(()) => Ok(()),
            Err(error @ ProcessError::MissingStream(_)) => Err(StructuredWriteError::Definite(
                safe_process_error(&error, "stdin_write"),
            )),
            Err(error) => Err(StructuredWriteError::Uncertain(safe_process_error(
                &error,
                "stdin_write",
            ))),
        }
    }

    async fn write_json(&self, run: &RunControl, value: &Value) -> Result<(), MaestroError> {
        let bytes = self.encode_json(value)?;
        self.write_json_bytes(run, &bytes)
            .await
            .map_err(StructuredWriteError::into_error)
    }

    async fn record_launch_failure(&self, record: &SessionRecord, error: MaestroError) {
        {
            let mut data = record.data.lock().await;
            data.state = SessionState::Failed;
            data.last_error = Some(error.clone());
        }
        let _ = self
            .publish(
                record,
                None,
                EventSource::Daemon,
                user_event(
                    "run_launch_failed",
                    json!({ "correlation_id": error.correlation_id }),
                ),
            )
            .await;
    }
}

async fn supervise_run(
    inner: Arc<SupervisorInner>,
    record: Arc<SessionRecord>,
    run: Arc<RunControl>,
    stdout: ChildStdout,
    stderr: ChildStderr,
) {
    let stderr_reader = tokio::spawn(read_stderr(
        Arc::clone(&record),
        stderr,
        inner.limits.stderr_bytes,
    ));
    let protocol_failure = read_stdout(
        Arc::clone(&inner),
        Arc::clone(&record),
        Arc::clone(&run),
        stdout,
    )
    .await
    .err();
    if let Some(failure) = protocol_failure.as_ref() {
        record_protocol_failure(&inner, &record, &run, failure).await;
        let _ = run
            .process
            .lock()
            .await
            .terminate(inner.limits.termination_grace)
            .await;
    }
    let stderr_failed = stderr_reader.await.unwrap_or(true);
    let exit = run.process.lock().await.wait().await;
    finalize_run(&inner, &record, &run, protocol_failure, stderr_failed, exit).await;
}

async fn read_stdout(
    inner: Arc<SupervisorInner>,
    record: Arc<SessionRecord>,
    run: Arc<RunControl>,
    mut stdout: ChildStdout,
) -> Result<(), ProtocolFailure> {
    let mut decoder = JsonLineDecoder::new(inner.limits.maximum_frame_bytes);
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        let count = stdout
            .read(&mut chunk)
            .await
            .map_err(|_| ProtocolFailure::new("stdout_io", false))?;
        if count == 0 {
            return decoder.finish();
        }
        if let Some(capture) = run.raw_capture.as_ref() {
            capture
                .lock()
                .await
                .push(&chunk[..count], inner.limits.maximum_raw_protocol_bytes);
        }
        let batch = decoder.push(&chunk[..count]);
        for value in batch.frames {
            handle_frame(&inner, &record, &run, value).await?;
        }
        if let Some(failure) = batch.failure {
            return Err(failure);
        }
    }
}

async fn read_stderr(record: Arc<SessionRecord>, mut stderr: ChildStderr, limit: usize) -> bool {
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) => return false,
            Err(_) => return true,
            Ok(count) => record.stderr.lock().await.push(&chunk[..count], limit),
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn handle_frame(
    inner: &Arc<SupervisorInner>,
    record: &Arc<SessionRecord>,
    run: &Arc<RunControl>,
    value: Value,
) -> Result<(), ProtocolFailure> {
    let object = value
        .as_object()
        .ok_or_else(|| ProtocolFailure::new("non_object_frame", false))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ProtocolFailure::new("invalid_event_type", false))?
        .to_owned();
    if let Some(version) = object.get("protocol_version")
        && version.as_u64() != Some(SUPPORTED_FAKE_PROTOCOL_VERSION)
    {
        return Err(ProtocolFailure::new("unsupported_protocol_version", true));
    }
    if !is_supported_event_kind(&kind) {
        return Err(ProtocolFailure::new("unknown_event_type", false));
    }

    match kind.as_str() {
        "binding" | "resumed" => {
            let binding = required_string(object.get("binding_id"), "binding_id")?;
            validate_identifier(binding, "vendor binding")
                .map_err(|_| ProtocolFailure::new("invalid_binding", false))?;
            let existing = run.leased_binding.lock().await.clone();
            if let Some(existing) = existing {
                if existing != binding {
                    return Err(ProtocolFailure::new("binding_mismatch", false));
                }
            } else {
                inner
                    .claim_binding(binding, run.lease_token, record.session_id)
                    .map_err(|_| ProtocolFailure::new("binding_writer_conflict", false))?;
                *run.leased_binding.lock().await = Some(binding.to_owned());
            }
            record.data.lock().await.binding = Some(binding.to_owned());
        }
        "permission_request" => {
            let request_id = required_string(object.get("request_id"), "request_id")?;
            register_pending(
                inner,
                record,
                run,
                request_id.to_owned(),
                PendingKind::Permission,
            )
            .await
            .map_err(|_| ProtocolFailure::new("invalid_permission_request", false))?;
            set_state(record, SessionState::AwaitingPermission).await;
        }
        "user_input_request" => {
            let request_id = required_string(object.get("request_id"), "request_id")?;
            register_pending(
                inner,
                record,
                run,
                request_id.to_owned(),
                PendingKind::UserInput,
            )
            .await
            .map_err(|_| ProtocolFailure::new("invalid_user_input_request", false))?;
            set_state(record, SessionState::AwaitingUserInput).await;
        }
        "permission_result" => {
            let request_id = required_string(object.get("request_id"), "request_id")?;
            if !run
                .pending
                .lock()
                .await
                .confirm_resolved(request_id, PendingKind::Permission)
            {
                return Err(ProtocolFailure::new(
                    "uncorrelated_permission_result",
                    false,
                ));
            }
            update_waiting_state(record, run).await;
        }
        "user_input_result" => {
            let request_id = required_string(object.get("request_id"), "request_id")?;
            if !run
                .pending
                .lock()
                .await
                .confirm_resolved(request_id, PendingKind::UserInput)
            {
                return Err(ProtocolFailure::new(
                    "uncorrelated_user_input_result",
                    false,
                ));
            }
            update_waiting_state(record, run).await;
        }
        "action_ack" => {
            let action_id = object
                .get("action")
                .and_then(Value::as_object)
                .and_then(|action| action.get("action_id"))
                .and_then(Value::as_str)
                .ok_or_else(|| ProtocolFailure::new("uncorrelated_action_ack", false))?;
            run.pending
                .lock()
                .await
                .resolve_active(action_id, PendingKind::GuiAction)
                .map_err(|_| ProtocolFailure::new("uncorrelated_action_ack", false))?;
        }
        "init" | "message_delta" | "tool_start" | "tool_end" | "artifact" | "usage" | "result"
        | "ready" | "message" | "delta" | "child_started" | "signal_ignored" => {}
        _ => return Err(ProtocolFailure::new("unknown_event_type", false)),
    }

    let visibility = if matches!(kind.as_str(), "user_input_result" | "action_ack") {
        EventVisibility::Sensitive
    } else {
        EventVisibility::User
    };
    let vendor_event_id = object
        .get("event_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    inner
        .publish(
            record,
            Some(run.run_id),
            EventSource::Cli,
            NormalizedEvent {
                kind: kind.clone(),
                visibility,
                payload: value,
                vendor_event_id,
                raw_segment_reference: None,
            },
        )
        .await
        .map_err(|_| ProtocolFailure::new("event_sequence_failure", false))?;
    let current = record.data.lock().await.state;
    if current == SessionState::Starting {
        set_state(
            record,
            if kind == "ready" {
                SessionState::Ready
            } else {
                SessionState::Running
            },
        )
        .await;
    }
    Ok(())
}

async fn register_pending(
    inner: &Arc<SupervisorInner>,
    record: &Arc<SessionRecord>,
    run: &Arc<RunControl>,
    request_id: String,
    kind: PendingKind,
) -> Result<(), MaestroError> {
    validate_identifier(&request_id, "request ID")?;
    let deadline = Instant::now() + inner.limits.request_timeout;
    run.pending
        .lock()
        .await
        .register(request_id.clone(), kind, deadline)?;
    let weak_inner = Arc::downgrade(inner);
    let weak_record = Arc::downgrade(record);
    let weak_run = Arc::downgrade(run);
    tokio::spawn(async move {
        tokio::time::sleep_until(deadline).await;
        if let (Some(inner), Some(record), Some(run)) = (
            weak_inner.upgrade(),
            weak_record.upgrade(),
            weak_run.upgrade(),
        ) {
            expire_request(&inner, &record, &run, &request_id, kind).await;
        }
    });
    Ok(())
}

async fn expire_request(
    inner: &SupervisorInner,
    record: &SessionRecord,
    run: &RunControl,
    request_id: &str,
    kind: PendingKind,
) {
    if run.finalized.load(Ordering::Acquire) {
        return;
    }
    let expired = run
        .pending
        .lock()
        .await
        .expire(request_id, kind, Instant::now());
    if !expired {
        return;
    }
    let _ = inner
        .publish(
            record,
            Some(run.run_id),
            EventSource::Daemon,
            user_event(
                "request_expired",
                json!({ "request_id": request_id, "request_kind": kind.label() }),
            ),
        )
        .await;
    let response = match kind {
        PendingKind::Permission => Some(json!({ "request_id": request_id, "decision": "cancel" })),
        PendingKind::UserInput => Some(json!({ "request_id": request_id, "value": null })),
        PendingKind::GuiAction => None,
    };
    if let Some(response) = response {
        let _ = inner.write_json(run, &response).await;
    }
    update_waiting_state(record, run).await;
}

async fn claim_pending(
    run: &RunControl,
    request_id: &str,
    kind: PendingKind,
) -> Result<(), MaestroError> {
    run.pending.lock().await.claim_active(request_id, kind)
}

async fn complete_pending(
    run: &RunControl,
    request_id: &str,
    kind: PendingKind,
) -> Result<(), MaestroError> {
    run.pending.lock().await.resolve_in_flight(request_id, kind)
}

async fn restore_pending(run: &RunControl, request_id: &str, kind: PendingKind) -> bool {
    run.pending.lock().await.restore_in_flight(request_id, kind)
}

async fn mark_pending_delivery_uncertain(
    run: &RunControl,
    request_id: &str,
    kind: PendingKind,
) -> bool {
    run.pending
        .lock()
        .await
        .mark_delivery_uncertain(request_id, kind)
}

async fn update_waiting_state(record: &SessionRecord, run: &RunControl) {
    let next = run.pending.lock().await.waiting_state();
    set_state(record, next.unwrap_or(SessionState::Running)).await;
}

async fn record_protocol_failure(
    inner: &SupervisorInner,
    record: &SessionRecord,
    run: &RunControl,
    failure: &ProtocolFailure,
) {
    let mut error = MaestroError::new(
        if failure.incompatible {
            ErrorCode::CliProtocolIncompatible
        } else {
            ErrorCode::ProcessCrashed
        },
        if failure.incompatible {
            "the fake CLI protocol version is unsupported"
        } else {
            "the fake CLI emitted an invalid structured frame"
        },
    );
    error.details = Some(json!({ "category": failure.category }));
    if failure.incompatible {
        error.user_action = Some("Use the exact TUI compatibility mode".to_owned());
    }
    {
        let mut data = record.data.lock().await;
        data.state = if failure.incompatible {
            SessionState::Incompatible
        } else {
            SessionState::Failed
        };
        data.last_error = Some(error.clone());
    }
    let _ = inner
        .publish(
            record,
            Some(run.run_id),
            EventSource::Daemon,
            user_event(
                if failure.incompatible {
                    "protocol_incompatible"
                } else {
                    "protocol_error"
                },
                json!({
                    "category": failure.category,
                    "correlation_id": error.correlation_id,
                }),
            ),
        )
        .await;
}

async fn finalize_run(
    inner: &SupervisorInner,
    record: &SessionRecord,
    run: &RunControl,
    protocol_failure: Option<ProtocolFailure>,
    stderr_failed: bool,
    exit: Result<ExitCause, ProcessError>,
) {
    if run.finalized.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(capture) = run.raw_capture.as_ref() {
        capture.lock().await.complete = true;
    }
    run.pending.lock().await.clear_active();
    let leased_binding = run.leased_binding.lock().await.take();
    if let Some(binding) = leased_binding.as_deref() {
        inner.release_binding(binding, run.lease_token);
    }

    let stop_requested = run.stop_requested.load(Ordering::Acquire);
    let (state, exit_cause, error) = classify_completion(
        stop_requested,
        protocol_failure.as_ref(),
        stderr_failed,
        exit,
    );
    {
        let mut data = record.data.lock().await;
        if data
            .active_run
            .as_ref()
            .is_some_and(|active| active.run_id == run.run_id)
        {
            data.active_run = None;
        }
        data.state = state;
        data.last_exit = exit_cause;
        if let Some(error) = error.as_ref() {
            data.last_error = Some(error.clone());
        }
    }
    let event = match state {
        SessionState::Completed => user_event("run_completed", json!({ "run_id": run.run_id })),
        SessionState::Stopped => user_event("run_stopped", json!({ "run_id": run.run_id })),
        SessionState::Incompatible => return,
        _ => user_event(
            "run_failed",
            json!({
                "run_id": run.run_id,
                "correlation_id": error.as_ref().map(|value| value.correlation_id),
                "exit": exit_cause.map(exit_json),
            }),
        ),
    };
    let _ = inner
        .publish(record, Some(run.run_id), EventSource::Daemon, event)
        .await;
}

fn classify_completion(
    stop_requested: bool,
    protocol_failure: Option<&ProtocolFailure>,
    stderr_failed: bool,
    exit: Result<ExitCause, ProcessError>,
) -> (SessionState, Option<ExitCause>, Option<MaestroError>) {
    let exit_cause = exit.ok();
    if stop_requested {
        return (SessionState::Stopped, exit_cause, None);
    }
    if let Some(failure) = protocol_failure {
        let mut error = MaestroError::new(
            if failure.incompatible {
                ErrorCode::CliProtocolIncompatible
            } else {
                ErrorCode::ProcessCrashed
            },
            "the structured fake CLI run failed protocol validation",
        );
        error.details = Some(json!({ "category": failure.category }));
        return (
            if failure.incompatible {
                SessionState::Incompatible
            } else {
                SessionState::Failed
            },
            exit_cause,
            Some(error),
        );
    }
    if stderr_failed {
        return (
            SessionState::Failed,
            exit_cause,
            Some(MaestroError::new(
                ErrorCode::ProcessCrashed,
                "the fake CLI stderr stream could not be read",
            )),
        );
    }
    match exit_cause {
        Some(ExitCause::Exited(0)) => (SessionState::Completed, exit_cause, None),
        Some(cause) => {
            let mut error = MaestroError::new(
                ErrorCode::ProcessCrashed,
                "the fake CLI process exited unsuccessfully",
            );
            error.details = Some(json!({ "exit": exit_json(cause) }));
            (SessionState::Failed, exit_cause, Some(error))
        }
        None => (
            SessionState::Failed,
            None,
            Some(MaestroError::new(
                ErrorCode::ProcessCrashed,
                "the fake CLI process could not be reaped",
            )),
        ),
    }
}

fn exit_json(cause: ExitCause) -> Value {
    match cause {
        ExitCause::Exited(code) => json!({ "code": code }),
        ExitCause::Signaled(signal) => json!({ "signal": signal }),
        ExitCause::Unknown => json!({ "unknown": true }),
    }
}

async fn set_state(record: &SessionRecord, target: SessionState) {
    let mut data = record.data.lock().await;
    if data.state == target {
        return;
    }
    let direct = data.state.transition_to(target).is_ok();
    let bridged = matches!(
        (data.state, target),
        (
            SessionState::Starting,
            SessionState::AwaitingPermission | SessionState::AwaitingUserInput
        ) | (
            SessionState::Ready
                | SessionState::AwaitingPermission
                | SessionState::AwaitingUserInput,
            SessionState::Completed
        )
    );
    if direct || bridged || target == SessionState::Starting {
        data.state = target;
    }
}

impl PendingTracker {
    fn new(capacity: usize) -> Self {
        Self {
            active: HashMap::new(),
            in_flight: HashMap::new(),
            resolved_order: VecDeque::new(),
            resolved: HashSet::new(),
            delivery_uncertain: HashSet::new(),
            capacity,
        }
    }

    fn register(
        &mut self,
        request_id: String,
        kind: PendingKind,
        deadline: Instant,
    ) -> Result<(), MaestroError> {
        if self.active.len() + self.in_flight.len() >= self.capacity {
            return Err(invalid_request("too many pending structured requests"));
        }
        let key = (request_id.clone(), kind);
        if self.active.contains_key(&request_id)
            || self.in_flight.contains_key(&request_id)
            || self.resolved.contains(&key)
            || self.delivery_uncertain.contains(&key)
        {
            return Err(invalid_request("the structured request ID is not unique"));
        }
        self.active
            .insert(request_id, PendingRequest { kind, deadline });
        Ok(())
    }

    fn claim_active(&mut self, request_id: &str, kind: PendingKind) -> Result<(), MaestroError> {
        let Some(pending) = self.active.get(request_id) else {
            return Err(invalid_request(
                "the structured request is unknown or already used",
            ));
        };
        if pending.kind != kind || pending.deadline <= Instant::now() {
            return Err(invalid_request(
                "the structured request is expired or has the wrong type",
            ));
        }
        let pending = self
            .active
            .remove(request_id)
            .expect("the claimed pending request was just validated");
        self.in_flight.insert(request_id.to_owned(), pending);
        Ok(())
    }

    fn restore_in_flight(&mut self, request_id: &str, kind: PendingKind) -> bool {
        if !self
            .in_flight
            .get(request_id)
            .is_some_and(|pending| pending.kind == kind)
        {
            return false;
        }
        let pending = self
            .in_flight
            .remove(request_id)
            .expect("the in-flight request was just validated");
        self.active.insert(request_id.to_owned(), pending);
        true
    }

    fn resolve_in_flight(
        &mut self,
        request_id: &str,
        kind: PendingKind,
    ) -> Result<(), MaestroError> {
        let key = (request_id.to_owned(), kind);
        if self.resolved.contains(&key) {
            return Ok(());
        }
        if !self
            .in_flight
            .get(request_id)
            .is_some_and(|pending| pending.kind == kind)
        {
            return Err(invalid_request(
                "the structured request is no longer in flight",
            ));
        }
        self.in_flight.remove(request_id);
        self.remember_resolved(request_id.to_owned(), kind);
        Ok(())
    }

    /// Returns true when a child result already proved that delivery completed.
    fn mark_delivery_uncertain(&mut self, request_id: &str, kind: PendingKind) -> bool {
        let key = (request_id.to_owned(), kind);
        if self.resolved.contains(&key) {
            return true;
        }
        if self
            .in_flight
            .get(request_id)
            .is_some_and(|pending| pending.kind == kind)
        {
            self.in_flight.remove(request_id);
            self.remember_delivery_uncertain(request_id.to_owned(), kind);
        }
        false
    }

    fn resolve_active(&mut self, request_id: &str, kind: PendingKind) -> Result<(), MaestroError> {
        let Some(pending) = self.active.get(request_id) else {
            return Err(invalid_request(
                "the structured request is unknown or already used",
            ));
        };
        if pending.kind != kind || pending.deadline <= Instant::now() {
            return Err(invalid_request(
                "the structured request is expired or has the wrong type",
            ));
        }
        self.active.remove(request_id);
        self.remember_resolved(request_id.to_owned(), kind);
        Ok(())
    }

    fn force_resolve(&mut self, request_id: &str, kind: PendingKind) {
        if self
            .active
            .get(request_id)
            .is_some_and(|pending| pending.kind == kind)
        {
            self.active.remove(request_id);
            self.remember_resolved(request_id.to_owned(), kind);
        }
    }

    fn expire(&mut self, request_id: &str, kind: PendingKind, now: Instant) -> bool {
        if !self
            .active
            .get(request_id)
            .is_some_and(|pending| pending.kind == kind && pending.deadline <= now)
        {
            return false;
        }
        self.active.remove(request_id);
        self.remember_resolved(request_id.to_owned(), kind);
        true
    }

    #[cfg(test)]
    fn was_resolved(&self, request_id: &str, kind: PendingKind) -> bool {
        self.resolved.contains(&(request_id.to_owned(), kind))
    }

    fn confirm_resolved(&mut self, request_id: &str, kind: PendingKind) -> bool {
        let key = (request_id.to_owned(), kind);
        if self.resolved.contains(&key) {
            return true;
        }
        if self.delivery_uncertain.remove(&key) {
            self.resolved.insert(key);
            return true;
        }
        if self
            .in_flight
            .get(request_id)
            .is_some_and(|pending| pending.kind == kind)
        {
            self.in_flight.remove(request_id);
            self.remember_resolved(request_id.to_owned(), kind);
            return true;
        }
        false
    }

    fn waiting_state(&self) -> Option<SessionState> {
        if self
            .active
            .values()
            .chain(self.in_flight.values())
            .any(|pending| pending.kind == PendingKind::Permission)
        {
            Some(SessionState::AwaitingPermission)
        } else if self
            .active
            .values()
            .chain(self.in_flight.values())
            .any(|pending| pending.kind == PendingKind::UserInput)
        {
            Some(SessionState::AwaitingUserInput)
        } else {
            None
        }
    }

    fn clear_active(&mut self) {
        self.active.clear();
        self.in_flight.clear();
    }

    fn remember_resolved(&mut self, request_id: String, kind: PendingKind) {
        let key = (request_id, kind);
        if self.resolved.insert(key.clone()) {
            self.resolved_order.push_back(key);
        }
        while self.resolved_order.len() > self.capacity {
            if let Some(expired) = self.resolved_order.pop_front() {
                self.resolved.remove(&expired);
                self.delivery_uncertain.remove(&expired);
            }
        }
    }

    fn remember_delivery_uncertain(&mut self, request_id: String, kind: PendingKind) {
        let key = (request_id, kind);
        if self.delivery_uncertain.insert(key.clone()) {
            self.resolved_order.push_back(key);
        }
        while self.resolved_order.len() > self.capacity {
            if let Some(expired) = self.resolved_order.pop_front() {
                self.resolved.remove(&expired);
                self.delivery_uncertain.remove(&expired);
            }
        }
    }
}

impl BoundedBytes {
    fn push(&mut self, data: &[u8], limit: usize) {
        if limit == 0 {
            self.truncated |= !data.is_empty();
            return;
        }
        if data.len() >= limit {
            self.bytes.clear();
            self.bytes
                .extend(data[data.len().saturating_sub(limit)..].iter().copied());
            self.truncated = true;
            return;
        }
        self.bytes.extend(data.iter().copied());
        while self.bytes.len() > limit {
            self.bytes.pop_front();
            self.truncated = true;
        }
    }
}

impl RawProtocolCaptureState {
    fn new(run_id: RunId) -> Self {
        Self {
            run_id,
            bytes: Zeroizing::new(Vec::new()),
            observed_byte_count: 0,
            truncated: false,
            complete: false,
        }
    }

    fn push(&mut self, input: &[u8], limit: usize) {
        let input_length = u64::try_from(input.len()).unwrap_or(u64::MAX);
        let next_observed = self.observed_byte_count.saturating_add(input_length);
        if next_observed == u64::MAX && input_length != 0 {
            self.truncated = true;
        }
        self.observed_byte_count = next_observed;

        let remaining = limit.saturating_sub(self.bytes.len());
        let retained = remaining.min(input.len());
        self.bytes.extend_from_slice(&input[..retained]);
        self.truncated |= retained < input.len();
    }
}

#[derive(Debug)]
struct JsonLineDecoder {
    frame: Vec<u8>,
    maximum_frame_bytes: usize,
    failed: bool,
}

#[derive(Debug)]
struct DecodeBatch {
    frames: Vec<Value>,
    failure: Option<ProtocolFailure>,
}

impl JsonLineDecoder {
    fn new(maximum_frame_bytes: usize) -> Self {
        Self {
            frame: Vec::with_capacity(maximum_frame_bytes.min(READ_CHUNK_BYTES)),
            maximum_frame_bytes,
            failed: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> DecodeBatch {
        let mut frames = Vec::new();
        if self.failed {
            return DecodeBatch {
                frames,
                failure: Some(ProtocolFailure::new("decoder_already_failed", false)),
            };
        }
        for &byte in bytes {
            if byte == b'\n' {
                if self.frame.last() == Some(&b'\r') {
                    self.frame.pop();
                }
                if self.frame.is_empty() {
                    self.failed = true;
                    return DecodeBatch {
                        frames,
                        failure: Some(ProtocolFailure::new("empty_frame", false)),
                    };
                }
                if let Ok(value) = serde_json::from_slice(&self.frame) {
                    frames.push(value);
                } else {
                    self.failed = true;
                    self.frame.clear();
                    return DecodeBatch {
                        frames,
                        failure: Some(ProtocolFailure::new("malformed_json", false)),
                    };
                }
                self.frame.clear();
            } else if self.frame.len() >= self.maximum_frame_bytes {
                self.failed = true;
                self.frame.clear();
                return DecodeBatch {
                    frames,
                    failure: Some(ProtocolFailure::new("oversized_frame", false)),
                };
            } else {
                self.frame.push(byte);
            }
        }
        DecodeBatch {
            frames,
            failure: None,
        }
    }

    fn finish(self) -> Result<(), ProtocolFailure> {
        if self.failed {
            Err(ProtocolFailure::new("decoder_already_failed", false))
        } else if self.frame.is_empty() {
            Ok(())
        } else {
            Err(ProtocolFailure::new("truncated_frame", false))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProtocolFailure {
    category: &'static str,
    incompatible: bool,
}

impl ProtocolFailure {
    const fn new(category: &'static str, incompatible: bool) -> Self {
        Self {
            category,
            incompatible,
        }
    }
}

fn is_supported_event_kind(kind: &str) -> bool {
    matches!(
        kind,
        "init"
            | "message_delta"
            | "tool_start"
            | "tool_end"
            | "artifact"
            | "usage"
            | "result"
            | "permission_request"
            | "permission_result"
            | "user_input_request"
            | "user_input_result"
            | "ready"
            | "action_ack"
            | "message"
            | "delta"
            | "binding"
            | "resumed"
            | "child_started"
            | "signal_ignored"
    )
}

fn required_string<'a>(value: Option<&'a Value>, field: &str) -> Result<&'a str, ProtocolFailure> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| match field {
            "binding_id" => ProtocolFailure::new("invalid_binding", false),
            _ => ProtocolFailure::new("invalid_request_id", false),
        })
}

fn user_event(kind: &str, payload: Value) -> NormalizedEvent {
    NormalizedEvent::user(kind, payload)
}

fn validate_launch(
    executable: &Path,
    scenario: &str,
    cwd: &Path,
    binding: Option<&str>,
) -> Result<(), MaestroError> {
    if executable.as_os_str().is_empty() {
        return Err(invalid_request("the fake-agent executable path is empty"));
    }
    if cwd.as_os_str().is_empty() {
        return Err(invalid_request("the fake-agent working directory is empty"));
    }
    if !scenario.starts_with("structured/")
        || scenario.is_empty()
        || scenario.len() > MAX_LAUNCH_TEXT_BYTES
    {
        return Err(invalid_request(
            "the fake-session scenario must be a bounded structured scenario",
        ));
    }
    if let Some(binding) = binding {
        validate_identifier(binding, "vendor binding")?;
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<(), MaestroError> {
    if value.is_empty() || value.len() > MAX_LAUNCH_TEXT_BYTES || value.contains(['\n', '\r']) {
        return Err(invalid_request(format!("{label} is empty or too large")));
    }
    Ok(())
}

fn safe_process_error(error: &ProcessError, operation: &'static str) -> MaestroError {
    let mut result = MaestroError::new(
        ErrorCode::ProcessCrashed,
        "the fake CLI process operation failed",
    );
    result.details = Some(json!({
        "operation": operation,
        "category": match error {
            ProcessError::Capacity { .. } => "capacity",
            ProcessError::Io(_) => "io",
            ProcessError::Pty(_) => "pty",
            ProcessError::MissingStream(_) => "missing_stream",
            ProcessError::AlreadyExited => "already_exited",
            ProcessError::MissingProcessId => "missing_process_id",
            ProcessError::Termination(_) => "termination",
        },
    }));
    result
}

fn annotate_delivery_error(
    mut error: MaestroError,
    delivery: &'static str,
    retry_safe: bool,
) -> MaestroError {
    let mut details = match error.details.take() {
        Some(Value::Object(details)) => details,
        _ => serde_json::Map::new(),
    };
    details.insert("delivery".to_owned(), Value::String(delivery.to_owned()));
    details.insert("retry_safe".to_owned(), Value::Bool(retry_safe));
    error.details = Some(Value::Object(details));
    error
}

fn invalid_request(message: impl Into<String>) -> MaestroError {
    MaestroError::new(ErrorCode::InvalidRequest, message)
}

fn session_not_found(session_id: SessionId) -> MaestroError {
    let mut error = MaestroError::new(ErrorCode::SessionNotFound, "fake session does not exist");
    error.details = Some(json!({ "session_id": session_id }));
    error
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc, time::Duration};

    use maestro_domain::{ErrorCode, EventSource, NormalizedEvent, SessionId, SessionState};
    use maestro_process::ProcessSpawner;
    use serde_json::json;

    use super::{
        BoundedBytes, FakeSessionLimits, FakeSessionSupervisor, FakeSubscriptionError,
        JsonLineDecoder, PendingKind, PendingTracker, PermissionDecision, SessionRecord,
    };

    #[test]
    fn decoder_handles_fragmented_unicode_and_multiple_frames() {
        let bytes = b"{\"type\":\"message\",\"content\":\"Maestro \xe2\x9c\x93\"}\n{\"type\":\"result\"}\r\n";
        let mut decoder = JsonLineDecoder::new(1024);
        let mut values = Vec::new();
        for chunk in bytes.chunks(3) {
            let batch = decoder.push(chunk);
            assert!(batch.failure.is_none());
            values.extend(batch.frames);
        }
        decoder.finish().expect("complete frame stream");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["content"], "Maestro ✓");
        assert_eq!(values[1]["type"], "result");
    }

    #[test]
    fn decoder_preserves_valid_frames_before_malformed_or_oversized_data() {
        let mut decoder = JsonLineDecoder::new(16);
        let batch = decoder.push(b"{\"type\":\"ok\"}\nnot-json\n");
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(batch.failure.expect("malformed").category, "malformed_json");

        let mut decoder = JsonLineDecoder::new(4);
        let batch = decoder.push(b"12345");
        assert_eq!(
            batch.failure.expect("oversized").category,
            "oversized_frame"
        );
    }

    #[test]
    fn decoder_rejects_unterminated_partial_frame() {
        let mut decoder = JsonLineDecoder::new(1024);
        assert!(decoder.push(b"{\"type\":\"partial").failure.is_none());
        assert_eq!(
            decoder.finish().expect_err("truncated").category,
            "truncated_frame"
        );
    }

    #[test]
    fn stderr_retention_is_byte_bounded_and_marks_truncation() {
        let mut bytes = BoundedBytes::default();
        bytes.push(b"abc", 5);
        bytes.push(b"defg", 5);
        assert_eq!(bytes.bytes.iter().copied().collect::<Vec<_>>(), b"cdefg");
        assert!(bytes.truncated);
    }

    #[tokio::test]
    async fn pending_requests_are_scoped_typed_single_use_and_bounded() {
        let mut pending = PendingTracker::new(1);
        let future = tokio::time::Instant::now() + Duration::from_mins(1);
        pending
            .register("one".to_owned(), PendingKind::Permission, future)
            .expect("register");
        assert!(
            pending
                .register("two".to_owned(), PendingKind::Permission, future)
                .is_err()
        );
        assert!(
            pending
                .resolve_active("one", PendingKind::UserInput)
                .is_err()
        );
        pending
            .resolve_active("one", PendingKind::Permission)
            .expect("correct response");
        assert!(
            pending
                .resolve_active("one", PendingKind::Permission)
                .is_err()
        );
        assert!(pending.was_resolved("one", PendingKind::Permission));

        pending
            .register("retry".to_owned(), PendingKind::UserInput, future)
            .expect("retry request registers");
        pending
            .claim_active("retry", PendingKind::UserInput)
            .expect("first delivery claims the request");
        assert!(
            pending
                .claim_active("retry", PendingKind::UserInput)
                .is_err()
        );
        assert!(pending.restore_in_flight("retry", PendingKind::UserInput));
        pending
            .claim_active("retry", PendingKind::UserInput)
            .expect("definite failure permits retry");
        pending
            .resolve_in_flight("retry", PendingKind::UserInput)
            .expect("successful retry resolves the request");

        pending
            .register("uncertain".to_owned(), PendingKind::Permission, future)
            .expect("uncertain request registers");
        pending
            .claim_active("uncertain", PendingKind::Permission)
            .expect("uncertain request is claimed");
        assert!(!pending.mark_delivery_uncertain("uncertain", PendingKind::Permission));
        assert!(
            pending
                .claim_active("uncertain", PendingKind::Permission)
                .is_err()
        );
        assert!(pending.confirm_resolved("uncertain", PendingKind::Permission));
        assert!(pending.was_resolved("uncertain", PendingKind::Permission));
    }

    #[tokio::test]
    async fn explicit_binding_has_exactly_one_active_writer() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor = FakeSessionSupervisor::new(
            ProcessSpawner::new(2),
            FakeSessionLimits {
                request_timeout: Duration::from_secs(1),
                ..FakeSessionLimits::default()
            },
        )
        .expect("supervisor");
        let first = supervisor
            .start(
                &executable,
                "structured/stall",
                &cwd,
                Some("exclusive-binding".to_owned()),
            )
            .await
            .expect("first writer");
        let second = supervisor
            .start(
                &executable,
                "structured/stall",
                &cwd,
                Some("exclusive-binding".to_owned()),
            )
            .await;
        assert!(matches!(second, Err(error) if error.code == ErrorCode::InvalidRequest));
        assert_eq!(supervisor.session_count().await, 1);
        supervisor
            .stop(first.session_id, first.run_id)
            .await
            .expect("stop first writer");
        wait_for_state(&supervisor, first.session_id, SessionState::Stopped).await;
    }

    #[tokio::test]
    async fn stop_all_terminates_active_runs_without_cross_session_leakage() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor =
            FakeSessionSupervisor::new(ProcessSpawner::new(2), FakeSessionLimits::default())
                .expect("supervisor");
        let first = supervisor
            .start(&executable, "structured/stall", &cwd, None)
            .await
            .expect("first run starts");
        let second = supervisor
            .start(&executable, "structured/stall", &cwd, None)
            .await
            .expect("second run starts");

        assert_eq!(supervisor.stop_all().await, 2);
        wait_for_state(&supervisor, first.session_id, SessionState::Stopped).await;
        wait_for_state(&supervisor, second.session_id, SessionState::Stopped).await;
        assert_ne!(first.session_id, second.session_id);
    }

    #[tokio::test]
    async fn happy_fragmented_and_multi_read_have_ordered_normalized_events() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        for scenario in [
            "structured/happy",
            "structured/fragmented",
            "structured/multi-frame-read",
        ] {
            let supervisor =
                FakeSessionSupervisor::new(ProcessSpawner::new(1), FakeSessionLimits::default())
                    .expect("supervisor");
            let handle = supervisor
                .start(&executable, scenario, &cwd, None)
                .await
                .expect("start");
            wait_for_state(&supervisor, handle.session_id, SessionState::Completed).await;
            let mut subscription = supervisor
                .subscribe(handle.session_id, 0)
                .await
                .expect("subscription");
            let latest = supervisor
                .snapshot(handle.session_id)
                .await
                .expect("snapshot")
                .latest_sequence;
            let mut events = Vec::new();
            for _ in 0..latest {
                events.push(subscription.recv().await.expect("replay event"));
            }
            assert!(
                events
                    .windows(2)
                    .all(|pair| pair[1].sequence == pair[0].sequence + 1)
            );
            let cli_kinds = events
                .iter()
                .filter(|event| event.source == EventSource::Cli)
                .map(|event| event.event.kind.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                cli_kinds,
                [
                    "init",
                    "message_delta",
                    "message_delta",
                    "tool_start",
                    "tool_end",
                    "artifact",
                    "usage",
                    "result"
                ]
            );
        }
    }

    #[tokio::test]
    async fn raw_capture_is_default_off_and_exact_across_fragmented_and_multi_frame_writes() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor =
            FakeSessionSupervisor::new(ProcessSpawner::new(1), FakeSessionLimits::default())
                .expect("supervisor");
        let disabled = supervisor
            .start(&executable, "structured/happy", &cwd, None)
            .await
            .expect("default-off run starts");
        wait_for_state(&supervisor, disabled.session_id, SessionState::Completed).await;
        assert_eq!(
            supervisor
                .raw_capture(disabled.session_id, disabled.run_id)
                .await
                .expect("raw capture can be queried"),
            None
        );

        let mut previous = None;
        for scenario in ["structured/fragmented", "structured/multi-frame-read"] {
            let expected = tokio::process::Command::new(&executable)
                .args(["--scenario", scenario])
                .current_dir(&cwd)
                .output()
                .await
                .expect("fixture runs directly");
            assert!(expected.status.success());

            let handle = supervisor
                .start_with_raw_capture(&executable, scenario, &cwd, None, true)
                .await
                .expect("captured run starts");
            wait_for_state(&supervisor, handle.session_id, SessionState::Completed).await;
            let capture = supervisor
                .raw_capture(handle.session_id, handle.run_id)
                .await
                .expect("raw capture loads")
                .expect("raw capture was explicitly enabled");
            assert_eq!(capture.bytes, expected.stdout);
            assert_eq!(
                capture.observed_byte_count,
                u64::try_from(capture.bytes.len()).expect("fixture size fits")
            );
            assert!(!capture.truncated);
            assert!(capture.complete);
            if let Some(previous) = previous.as_ref() {
                assert_eq!(&capture.bytes, previous);
            }
            previous = Some(capture.into_bytes());
        }
    }

    #[tokio::test]
    async fn raw_capture_retains_only_the_bounded_prefix_and_reports_observed_bytes() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor = FakeSessionSupervisor::new(
            ProcessSpawner::new(1),
            FakeSessionLimits {
                maximum_raw_protocol_bytes: 64,
                ..FakeSessionLimits::default()
            },
        )
        .expect("supervisor");
        let handle = supervisor
            .start_with_volume_and_raw_capture(
                &executable,
                "structured/flood",
                &cwd,
                None,
                Some(8),
                true,
            )
            .await
            .expect("captured flood starts");
        wait_for_state(&supervisor, handle.session_id, SessionState::Completed).await;
        let capture = supervisor
            .raw_capture(handle.session_id, handle.run_id)
            .await
            .expect("capture loads")
            .expect("capture enabled");
        assert_eq!(capture.bytes.len(), 64);
        assert!(capture.observed_byte_count > 64);
        assert!(capture.truncated);
        assert!(capture.complete);
    }

    #[tokio::test]
    async fn permission_and_user_input_are_correlated_and_single_use() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor =
            FakeSessionSupervisor::new(ProcessSpawner::new(2), FakeSessionLimits::default())
                .expect("supervisor");
        let permission = supervisor
            .start(&executable, "structured/permission", &cwd, None)
            .await
            .expect("permission run");
        wait_for_state(
            &supervisor,
            permission.session_id,
            SessionState::AwaitingPermission,
        )
        .await;
        supervisor
            .respond_permission(
                permission.session_id,
                permission.run_id,
                "permission-0001",
                PermissionDecision::Allow,
            )
            .await
            .expect("permission response");
        assert!(
            supervisor
                .respond_permission(
                    permission.session_id,
                    permission.run_id,
                    "permission-0001",
                    PermissionDecision::Deny,
                )
                .await
                .is_err()
        );
        wait_for_state(&supervisor, permission.session_id, SessionState::Completed).await;

        let input = supervisor
            .start(&executable, "structured/user-input", &cwd, None)
            .await
            .expect("input run");
        wait_for_state(
            &supervisor,
            input.session_id,
            SessionState::AwaitingUserInput,
        )
        .await;
        supervisor
            .respond_user_input(input.session_id, input.run_id, "input-0001", json!("alpha"))
            .await
            .expect("input response");
        wait_for_state(&supervisor, input.session_id, SessionState::Completed).await;
    }

    #[tokio::test]
    async fn failed_user_input_delivery_can_be_retried_without_false_audit() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor = FakeSessionSupervisor::new(
            ProcessSpawner::new(1),
            FakeSessionLimits {
                maximum_input_bytes: 128,
                ..FakeSessionLimits::default()
            },
        )
        .expect("supervisor");
        let handle = supervisor
            .start(&executable, "structured/user-input", &cwd, None)
            .await
            .expect("input run");
        wait_for_state(
            &supervisor,
            handle.session_id,
            SessionState::AwaitingUserInput,
        )
        .await;
        let before_failure = supervisor
            .snapshot(handle.session_id)
            .await
            .expect("snapshot before failed delivery");

        let error = supervisor
            .respond_user_input(
                handle.session_id,
                handle.run_id,
                "input-0001",
                json!("x".repeat(1_024)),
            )
            .await
            .expect_err("oversized delivery is rejected before writing");
        assert_eq!(error.code, ErrorCode::InputTooLarge);
        assert_eq!(
            error.details.as_ref().expect("delivery details")["delivery"],
            "not_delivered"
        );
        assert_eq!(
            error.details.as_ref().expect("delivery details")["retry_safe"],
            true
        );
        let after_failure = supervisor
            .snapshot(handle.session_id)
            .await
            .expect("snapshot after failed delivery");
        assert_eq!(after_failure.state, SessionState::AwaitingUserInput);
        assert_eq!(
            after_failure.latest_sequence,
            before_failure.latest_sequence
        );

        supervisor
            .respond_user_input(
                handle.session_id,
                handle.run_id,
                "input-0001",
                json!("alpha"),
            )
            .await
            .expect("retry reaches the child");
        wait_for_state(&supervisor, handle.session_id, SessionState::Completed).await;

        let snapshot = supervisor
            .snapshot(handle.session_id)
            .await
            .expect("completed snapshot");
        let mut replay = supervisor
            .subscribe(handle.session_id, 0)
            .await
            .expect("subscription");
        let mut response_audits = 0;
        for _ in 0..snapshot.latest_sequence {
            response_audits += usize::from(
                replay.recv().await.expect("event").event.kind == "gui_user_input_response",
            );
        }
        assert_eq!(response_audits, 1);
    }

    #[tokio::test]
    async fn concurrent_duplicate_permission_submission_delivers_once() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor =
            FakeSessionSupervisor::new(ProcessSpawner::new(1), FakeSessionLimits::default())
                .expect("supervisor");
        let handle = supervisor
            .start(&executable, "structured/permission", &cwd, None)
            .await
            .expect("permission run");
        wait_for_state(
            &supervisor,
            handle.session_id,
            SessionState::AwaitingPermission,
        )
        .await;

        let (first, second) = tokio::join!(
            supervisor.respond_permission(
                handle.session_id,
                handle.run_id,
                "permission-0001",
                PermissionDecision::Allow,
            ),
            supervisor.respond_permission(
                handle.session_id,
                handle.run_id,
                "permission-0001",
                PermissionDecision::Deny,
            ),
        );
        assert_eq!(
            [first.is_ok(), second.is_ok()]
                .into_iter()
                .filter(|ok| *ok)
                .count(),
            1
        );
        wait_for_state(&supervisor, handle.session_id, SessionState::Completed).await;

        let snapshot = supervisor
            .snapshot(handle.session_id)
            .await
            .expect("completed snapshot");
        let mut replay = supervisor
            .subscribe(handle.session_id, 0)
            .await
            .expect("subscription");
        let mut response_audits = 0;
        for _ in 0..snapshot.latest_sequence {
            response_audits += usize::from(
                replay.recv().await.expect("event").event.kind == "gui_permission_response",
            );
        }
        assert_eq!(response_audits, 1);
    }

    #[tokio::test]
    async fn permission_expiry_cancels_the_child_and_cannot_be_reused() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor = FakeSessionSupervisor::new(
            ProcessSpawner::new(1),
            FakeSessionLimits {
                request_timeout: Duration::from_millis(25),
                ..FakeSessionLimits::default()
            },
        )
        .expect("supervisor");
        let handle = supervisor
            .start(&executable, "structured/permission", &cwd, None)
            .await
            .expect("permission run");
        wait_for_state(&supervisor, handle.session_id, SessionState::Completed).await;
        assert!(
            supervisor
                .respond_permission(
                    handle.session_id,
                    handle.run_id,
                    "permission-0001",
                    PermissionDecision::Allow,
                )
                .await
                .is_err()
        );
        let mut replay = supervisor
            .subscribe(handle.session_id, 0)
            .await
            .expect("subscription");
        let latest = supervisor
            .snapshot(handle.session_id)
            .await
            .expect("snapshot")
            .latest_sequence;
        let mut expired = false;
        for _ in 0..latest {
            expired |= replay.recv().await.expect("event").event.kind == "request_expired";
        }
        assert!(expired);
    }

    #[tokio::test]
    async fn resume_reuses_binding_and_continues_logical_sequence() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor =
            FakeSessionSupervisor::new(ProcessSpawner::new(1), FakeSessionLimits::default())
                .expect("supervisor");
        let first = supervisor
            .start(&executable, "structured/resume", &cwd, None)
            .await
            .expect("first run");
        wait_for_state(&supervisor, first.session_id, SessionState::Completed).await;
        let first_latest = supervisor
            .snapshot(first.session_id)
            .await
            .expect("first snapshot")
            .latest_sequence;
        let second = supervisor
            .resume(
                first.session_id,
                &executable,
                "structured/resume",
                &cwd,
                None,
            )
            .await
            .expect("resumed run");
        assert_ne!(first.run_id, second.run_id);
        wait_for_state(&supervisor, first.session_id, SessionState::Completed).await;
        let mut replay = supervisor
            .subscribe(first.session_id, first_latest)
            .await
            .expect("second-run replay");
        assert!(replay.recv().await.expect("run start").sequence > first_latest);
        assert_eq!(
            replay.recv().await.expect("resumed event").event.kind,
            "resumed"
        );
    }

    #[tokio::test]
    async fn gui_actions_are_correlated_and_display_streams_are_redacted() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor =
            FakeSessionSupervisor::new(ProcessSpawner::new(1), FakeSessionLimits::default())
                .expect("supervisor");
        let handle = supervisor
            .start_with_raw_capture(&executable, "structured/gui-actions", &cwd, None, true)
            .await
            .expect("GUI action run");
        wait_for_state(&supervisor, handle.session_id, SessionState::Ready).await;
        let action_id = supervisor
            .send_gui_action(
                handle.session_id,
                handle.run_id,
                "session.resume",
                json!({ "api_key": "fixture-secret-value" }),
            )
            .await
            .expect("action sent");
        assert!(!action_id.is_empty());
        wait_for_state(&supervisor, handle.session_id, SessionState::Completed).await;
        let mut replay = supervisor
            .subscribe(handle.session_id, 0)
            .await
            .expect("subscription");
        let latest = supervisor
            .snapshot(handle.session_id)
            .await
            .expect("snapshot")
            .latest_sequence;
        let mut serialized = String::new();
        for _ in 0..latest {
            serialized.push_str(
                &serde_json::to_string(&replay.recv().await.expect("event"))
                    .expect("event serializes"),
            );
        }
        assert!(!serialized.contains("fixture-secret-value"));
        let capture = supervisor
            .raw_capture(handle.session_id, handle.run_id)
            .await
            .expect("raw capture loads")
            .expect("raw capture enabled");
        assert!(
            capture
                .bytes
                .windows(b"fixture-secret-value".len())
                .any(|window| window == b"fixture-secret-value")
        );
        let debug = format!("{capture:?}");
        assert!(debug.contains("SENSITIVE RAW PROTOCOL BYTES"));
        assert!(!debug.contains("fixture-secret-value"));
    }

    #[tokio::test]
    async fn publish_redacts_before_replay_and_live_fanout() {
        let supervisor =
            FakeSessionSupervisor::new(ProcessSpawner::new(1), FakeSessionLimits::default())
                .expect("supervisor");
        let record = Arc::new(SessionRecord::new(SessionId::new(), 2));
        let mut live = record.sender.subscribe();
        let envelope = supervisor
            .inner
            .publish(
                &record,
                None,
                EventSource::Cli,
                NormalizedEvent::user("test", json!({ "api_key": "must-not-reach-display" })),
            )
            .await
            .expect("publish");
        let serialized = serde_json::to_string(&envelope).expect("event serializes");
        assert!(!serialized.contains("must-not-reach-display"));
        assert_eq!(
            live.recv().await.expect("live event").event.payload,
            envelope.event.payload
        );
        assert_eq!(
            record.events.lock().await.retained[0].event.payload,
            envelope.event.payload
        );
    }

    #[tokio::test]
    async fn nonzero_and_crash_preserve_prior_events_and_bounded_stderr() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor =
            FakeSessionSupervisor::new(ProcessSpawner::new(2), FakeSessionLimits::default())
                .expect("supervisor");
        let nonzero = supervisor
            .start(&executable, "structured/nonzero", &cwd, None)
            .await
            .expect("nonzero run");
        wait_for_state(&supervisor, nonzero.session_id, SessionState::Failed).await;
        let snapshot = supervisor
            .snapshot(nonzero.session_id)
            .await
            .expect("snapshot");
        assert!(
            String::from_utf8_lossy(&snapshot.stderr).contains("deterministic fixture failure")
        );
        assert!(snapshot.latest_sequence >= 3);

        let crash = supervisor
            .start(&executable, "structured/crash", &cwd, None)
            .await
            .expect("crash run");
        wait_for_state(&supervisor, crash.session_id, SessionState::Failed).await;
        let mut subscription = supervisor
            .subscribe(crash.session_id, 0)
            .await
            .expect("subscription");
        let latest = supervisor
            .snapshot(crash.session_id)
            .await
            .expect("snapshot")
            .latest_sequence;
        let mut kinds = Vec::new();
        for _ in 0..latest {
            kinds.push(subscription.recv().await.expect("event").event.kind);
        }
        assert!(kinds.contains(&"protocol_error".to_owned()));
        assert!(!kinds.iter().any(|kind| kind == "partial"));
    }

    #[tokio::test]
    async fn replay_and_broadcast_are_bounded_and_lag_is_explicit() {
        let Some((executable, cwd)) = fixture() else {
            return;
        };
        let supervisor = FakeSessionSupervisor::new(
            ProcessSpawner::new(1),
            FakeSessionLimits {
                replay_events: 4,
                broadcast_events: 2,
                ..FakeSessionLimits::default()
            },
        )
        .expect("supervisor");
        let handle = supervisor
            .start_with_volume(&executable, "structured/flood", &cwd, None, Some(64))
            .await
            .expect("flood run");
        let mut live = supervisor
            .subscribe(handle.session_id, 1)
            .await
            .expect("live subscription");
        wait_for_state(&supervisor, handle.session_id, SessionState::Completed).await;
        let replay = supervisor
            .subscribe(handle.session_id, 0)
            .await
            .expect("replay");
        assert!(replay.replay_gap().is_some());
        loop {
            match live.recv().await {
                Err(FakeSubscriptionError::Lagged { missed }) => {
                    assert!(missed > 0);
                    break;
                }
                Ok(_) => {}
                Err(error) => panic!("unexpected subscription error: {error}"),
            }
        }
    }

    fn fixture() -> Option<(PathBuf, PathBuf)> {
        let executable = std::env::var_os("MAESTRO_FAKE_AGENT").map(PathBuf::from)?;
        let cwd = std::env::current_dir().ok()?;
        Some((executable, cwd))
    }

    async fn wait_for_state(
        supervisor: &FakeSessionSupervisor,
        session_id: maestro_domain::SessionId,
        expected: SessionState,
    ) {
        for _ in 0..500 {
            if supervisor
                .snapshot(session_id)
                .await
                .expect("snapshot")
                .state
                == expected
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("session did not reach {expected:?}");
    }
}
