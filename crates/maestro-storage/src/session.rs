use std::{fmt, num::TryFromIntError};

use chrono::{DateTime, Utc};
use maestro_domain::{
    AgentKind, EventEnvelope, EventId, IntegrationMode, NormalizedEvent, ProjectId, RunId,
    SessionId, SessionState,
};
use maestro_redaction::redact_json;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroize;

use crate::Database;

/// Durable exit information for one daemon-owned process run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistedRunExit {
    Exited(i32),
    Signaled(i32),
    Unknown,
}

/// Durable replay metadata used when no in-memory supervisor owns a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistedSessionMetadata {
    pub project_id: ProjectId,
    pub agent_kind: AgentKind,
    pub integration_mode: IntegrationMode,
    pub state: SessionState,
    pub active_run_id: Option<RunId>,
    pub latest_sequence: u64,
}

/// One logical session entry restored for a project's session index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedSessionSummary {
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub agent_kind: AgentKind,
    pub integration_mode: IntegrationMode,
    pub state: SessionState,
    pub title: Option<String>,
    pub active_run_id: Option<RunId>,
    pub latest_sequence: u64,
    pub updated_at: DateTime<Utc>,
}

/// Exact CLI stdout bytes captured for one opted-in structured process run.
///
/// These bytes are intentionally not redacted. Callers must treat every value
/// as sensitive and expose it only inside an explicitly enabled raw inspector.
#[derive(Clone, PartialEq, Eq)]
pub struct PersistedRawProtocolCapture {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub bytes: Vec<u8>,
    pub observed_byte_count: u64,
    pub truncated: bool,
    pub completed: bool,
}

impl PersistedRawProtocolCapture {
    pub fn into_bytes(mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}

impl fmt::Debug for PersistedRawProtocolCapture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersistedRawProtocolCapture")
            .field("session_id", &self.session_id)
            .field("run_id", &self.run_id)
            .field("bytes", &"[SENSITIVE RAW PROTOCOL BYTES]")
            .field("captured_byte_count", &self.bytes.len())
            .field("observed_byte_count", &self.observed_byte_count)
            .field("truncated", &self.truncated)
            .field("completed", &self.completed)
            .finish()
    }
}

impl Drop for PersistedRawProtocolCapture {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

/// Narrow repository for logical sessions, process runs, and normalized events.
#[derive(Debug, Clone, Copy)]
pub struct SessionStore<'database> {
    connection: &'database Connection,
}

impl<'database> SessionStore<'database> {
    pub fn new(database: &'database Database) -> Self {
        Self {
            connection: database.connection(),
        }
    }

    /// Creates or refreshes a logical session without replacing its event history.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] when the project does not exist or the
    /// session cannot be persisted.
    pub fn upsert_session(
        &self,
        session_id: SessionId,
        project_id: ProjectId,
        agent_kind: AgentKind,
        integration_mode: IntegrationMode,
        state: SessionState,
        title: Option<&str>,
    ) -> Result<(), SessionStoreError> {
        let now = Utc::now().to_rfc3339();
        self.connection.execute(
            "INSERT INTO sessions (
                 id, project_id, agent_kind, integration_mode, state, title, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(id) DO UPDATE SET
                 state = excluded.state,
                 title = COALESCE(excluded.title, sessions.title),
                 updated_at = excluded.updated_at",
            params![
                session_id.to_string(),
                project_id.to_string(),
                enum_name(agent_kind)?,
                enum_name(integration_mode)?,
                enum_name(state)?,
                title,
                now,
            ],
        )?;
        Ok(())
    }

    /// Updates the durable logical-session state.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError::SessionNotFound`] for an unknown session,
    /// or another error when storage fails.
    pub fn update_session_state(
        &self,
        session_id: SessionId,
        state: SessionState,
    ) -> Result<(), SessionStoreError> {
        let changed = self.connection.execute(
            "UPDATE sessions SET state = ?2, updated_at = ?3 WHERE id = ?1",
            params![
                session_id.to_string(),
                enum_name(state)?,
                Utc::now().to_rfc3339()
            ],
        )?;
        if changed == 0 {
            return Err(SessionStoreError::SessionNotFound(session_id));
        }
        Ok(())
    }

    /// Records the start of a daemon-owned process run.
    ///
    /// Invocation data is redacted again at this persistence boundary.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] when the session does not exist, invocation
    /// JSON cannot be serialized, or the run cannot be persisted.
    pub fn start_run(
        &self,
        run_id: RunId,
        session_id: SessionId,
        pid: u32,
        invocation: &Value,
        channel: &str,
    ) -> Result<(), SessionStoreError> {
        let invocation_json = serde_json::to_string(&redact_json(invocation))?;
        self.connection.execute(
            "INSERT INTO process_runs (
                 id, session_id, pid, invocation_json, channel, state, started_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6)",
            params![
                run_id.to_string(),
                session_id.to_string(),
                i64::from(pid),
                invocation_json,
                channel,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// Finalizes a process run with stable state and exit metadata.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError::RunNotFound`] for an unknown run, or
    /// another error when storage fails.
    pub fn finish_run(
        &self,
        run_id: RunId,
        state: &str,
        exit: PersistedRunExit,
        recovery: Option<&Value>,
    ) -> Result<(), SessionStoreError> {
        let (exit_code, recovery_exit) = match exit {
            PersistedRunExit::Exited(code) => (Some(code), None),
            PersistedRunExit::Signaled(signal) => (
                None,
                Some(serde_json::json!({
                    "signal": signal,
                })),
            ),
            PersistedRunExit::Unknown => (
                None,
                Some(serde_json::json!({
                    "cause": "unknown",
                })),
            ),
        };
        let merged_recovery = match (recovery, recovery_exit) {
            (Some(value), Some(exit)) => Some(serde_json::json!({
                "details": redact_json(value),
                "exit": exit,
            })),
            (Some(value), None) => Some(redact_json(value)),
            (None, Some(exit)) => Some(exit),
            (None, None) => None,
        };
        let recovery_json = merged_recovery
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let changed = self.connection.execute(
            "UPDATE process_runs
             SET state = ?2, exited_at = ?3, exit_code = ?4, recovery_json = ?5
             WHERE id = ?1",
            params![
                run_id.to_string(),
                state,
                Utc::now().to_rfc3339(),
                exit_code,
                recovery_json,
            ],
        )?;
        if changed == 0 {
            return Err(SessionStoreError::RunNotFound(run_id));
        }
        Ok(())
    }

    /// Appends one normalized event using the session sequence as the durable
    /// idempotency boundary.
    ///
    /// The payload is redacted again immediately before serialization.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] when identifiers do not fit `SQLite`'s
    /// integer representation, serialization fails, or the event cannot be
    /// inserted.
    pub fn append_event(&self, envelope: &EventEnvelope) -> Result<(), SessionStoreError> {
        let payload = redact_json(&envelope.event.payload);
        self.connection.execute(
            "INSERT INTO events (
                 id, session_id, run_id, sequence, timestamp, source, kind, visibility,
                 vendor_event_id, payload_json, raw_segment_reference
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                envelope.event_id.to_string(),
                envelope.session_id.to_string(),
                envelope.run_id.map(|run_id| run_id.to_string()),
                i64::try_from(envelope.sequence)?,
                envelope.timestamp.to_rfc3339(),
                enum_name(envelope.source)?,
                envelope.event.kind,
                enum_name(envelope.event.visibility)?,
                envelope.event.vendor_event_id,
                serde_json::to_string(&payload)?,
                envelope.event.raw_segment_reference,
            ],
        )?;
        Ok(())
    }

    /// Creates or replaces the bounded exact-byte capture for one process run.
    ///
    /// The containing database is SQLCipher-encrypted. Payload bytes bypass
    /// redaction by design and are never interpolated into logs or errors.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] when the byte counts are inconsistent,
    /// identifiers exceed `SQLite` limits, or the session/run cannot be stored.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_raw_protocol_capture(
        &self,
        session_id: SessionId,
        run_id: RunId,
        bytes: &[u8],
        observed_byte_count: u64,
        truncated: bool,
        completed: bool,
        storage_path: &str,
    ) -> Result<(), SessionStoreError> {
        if observed_byte_count < u64::try_from(bytes.len())? {
            return Err(SessionStoreError::InvalidRawCapture);
        }
        let now = Utc::now().to_rfc3339();
        let ended_at = completed.then(|| now.clone());
        self.connection.execute(
            "INSERT INTO raw_segments (
                 id, session_id, started_at, ended_at, byte_count, storage_path,
                 run_id, content, observed_byte_count, truncated, completed
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?1, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 ended_at = excluded.ended_at,
                 byte_count = excluded.byte_count,
                 content = excluded.content,
                 observed_byte_count = excluded.observed_byte_count,
                 truncated = excluded.truncated,
                 completed = excluded.completed",
            params![
                run_id.to_string(),
                session_id.to_string(),
                now,
                ended_at,
                i64::try_from(bytes.len())?,
                storage_path,
                bytes,
                i64::try_from(observed_byte_count)?,
                truncated,
                completed,
            ],
        )?;
        Ok(())
    }

    /// Loads the exact-byte capture for one session/run pair.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] when stored identifiers or byte counts are
    /// corrupt, or when the encrypted database cannot be queried.
    pub fn raw_protocol_capture(
        &self,
        session_id: SessionId,
        run_id: RunId,
    ) -> Result<Option<PersistedRawProtocolCapture>, SessionStoreError> {
        let stored = self
            .connection
            .query_row(
                "SELECT session_id, run_id, content, observed_byte_count, truncated, completed
                 FROM raw_segments WHERE session_id = ?1 AND run_id = ?2",
                params![session_id.to_string(), run_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, bool>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?;
        stored
            .map(
                |(stored_session_id, stored_run_id, bytes, observed, truncated, completed)| {
                    let observed_byte_count = u64::try_from(observed)?;
                    if observed_byte_count < u64::try_from(bytes.len())? {
                        return Err(SessionStoreError::InvalidRawCapture);
                    }
                    Ok(PersistedRawProtocolCapture {
                        session_id: SessionId::from_uuid(uuid::Uuid::parse_str(
                            &stored_session_id,
                        )?),
                        run_id: RunId::from_uuid(uuid::Uuid::parse_str(&stored_run_id)?),
                        bytes,
                        observed_byte_count,
                        truncated,
                        completed,
                    })
                },
            )
            .transpose()
    }

    /// Loads a bounded replay window after the supplied sequence cursor.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError::InvalidLimit`] outside `1..=4096`, or
    /// another error when stored event data is corrupt or unavailable.
    pub fn load_events(
        &self,
        session_id: SessionId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<EventEnvelope>, SessionStoreError> {
        if limit == 0 || limit > 4_096 {
            return Err(SessionStoreError::InvalidLimit);
        }
        if self.session_state(session_id)?.is_none() {
            return Err(SessionStoreError::SessionNotFound(session_id));
        }
        let mut statement = self.connection.prepare(
            "SELECT id, run_id, sequence, timestamp, source, kind, visibility,
                    vendor_event_id, payload_json, raw_segment_reference
             FROM events
             WHERE session_id = ?1 AND sequence > ?2
             ORDER BY sequence ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                session_id.to_string(),
                i64::try_from(after_sequence)?,
                i64::try_from(limit)?,
            ],
            |row| {
                Ok(StoredEvent {
                    event_id: row.get(0)?,
                    run_id: row.get(1)?,
                    sequence: row.get(2)?,
                    timestamp: row.get(3)?,
                    source: row.get(4)?,
                    kind: row.get(5)?,
                    visibility: row.get(6)?,
                    vendor_event_id: row.get(7)?,
                    payload_json: row.get(8)?,
                    raw_segment_reference: row.get(9)?,
                })
            },
        )?;
        rows.map(|row| stored_event(row?, session_id)).collect()
    }

    /// Returns the currently persisted state for one logical session.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] when the state cannot be queried or parsed.
    pub fn session_state(
        &self,
        session_id: SessionId,
    ) -> Result<Option<SessionState>, SessionStoreError> {
        let state = self
            .connection
            .query_row(
                "SELECT state FROM sessions WHERE id = ?1",
                [session_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        state.map(|value| enum_from_name(&value)).transpose()
    }

    /// Returns the state and replay boundary for a logical session.
    ///
    /// This query is the recovery source of truth after daemon restart and for
    /// PTY-backed sessions that do not have a structured in-memory supervisor.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError`] when persisted identifiers, enums, or
    /// sequence values are invalid, or when the database cannot be queried.
    pub fn session_metadata(
        &self,
        session_id: SessionId,
    ) -> Result<Option<PersistedSessionMetadata>, SessionStoreError> {
        let row = self
            .connection
            .query_row(
                "SELECT s.project_id,
                        s.agent_kind,
                        s.integration_mode,
                        s.state,
                        COALESCE((
                            SELECT MAX(e.sequence) FROM events e WHERE e.session_id = s.id
                        ), 0),
                        (
                            SELECT r.id FROM process_runs r
                            WHERE r.session_id = s.id AND r.state = 'running'
                            ORDER BY r.started_at DESC LIMIT 1
                        )
                 FROM sessions s WHERE s.id = ?1",
                [session_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(project_id, agent_kind, integration_mode, state, latest_sequence, active_run_id)| {
                Ok(PersistedSessionMetadata {
                    project_id: uuid::Uuid::parse_str(&project_id).map(ProjectId::from_uuid)?,
                    agent_kind: enum_from_name(&agent_kind)?,
                    integration_mode: enum_from_name(&integration_mode)?,
                    state: enum_from_name(&state)?,
                    active_run_id: active_run_id
                        .map(|value| uuid::Uuid::parse_str(&value).map(RunId::from_uuid))
                        .transpose()?,
                    latest_sequence: u64::try_from(latest_sequence)?,
                })
            },
        )
        .transpose()
    }

    /// Returns a bounded, most-recent-first session index for one project.
    ///
    /// # Errors
    ///
    /// Returns [`SessionStoreError::InvalidLimit`] outside `1..=256`, or an
    /// error when persisted identifiers, enums, timestamps, or sequences are
    /// invalid.
    pub fn list_sessions(
        &self,
        project_id: ProjectId,
        limit: usize,
    ) -> Result<Vec<PersistedSessionSummary>, SessionStoreError> {
        if limit == 0 || limit > 256 {
            return Err(SessionStoreError::InvalidLimit);
        }
        let mut statement = self.connection.prepare(
            "SELECT s.id, s.project_id, s.agent_kind, s.integration_mode, s.state, s.title,
                    (
                        SELECT r.id FROM process_runs r
                        WHERE r.session_id = s.id AND r.state = 'running'
                        ORDER BY r.started_at DESC LIMIT 1
                    ),
                    COALESCE((
                        SELECT MAX(e.sequence) FROM events e WHERE e.session_id = s.id
                    ), 0),
                    s.updated_at
             FROM sessions s
             WHERE s.project_id = ?1
             ORDER BY s.updated_at DESC, s.id ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![project_id.to_string(), i64::try_from(limit)?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (
                session_id,
                stored_project_id,
                agent_kind,
                integration_mode,
                state,
                title,
                active_run_id,
                latest_sequence,
                updated_at,
            ) = row?;
            Ok(PersistedSessionSummary {
                session_id: SessionId::from_uuid(uuid::Uuid::parse_str(&session_id)?),
                project_id: ProjectId::from_uuid(uuid::Uuid::parse_str(&stored_project_id)?),
                agent_kind: enum_from_name(&agent_kind)?,
                integration_mode: enum_from_name(&integration_mode)?,
                state: enum_from_name(&state)?,
                title,
                active_run_id: active_run_id
                    .map(|value| uuid::Uuid::parse_str(&value).map(RunId::from_uuid))
                    .transpose()?,
                latest_sequence: u64::try_from(latest_sequence)?,
                updated_at: DateTime::parse_from_rfc3339(&updated_at)?.with_timezone(&Utc),
            })
        })
        .collect()
    }
}

#[derive(Debug)]
struct StoredEvent {
    event_id: String,
    run_id: Option<String>,
    sequence: i64,
    timestamp: String,
    source: String,
    kind: String,
    visibility: String,
    vendor_event_id: Option<String>,
    payload_json: String,
    raw_segment_reference: Option<String>,
}

fn stored_event(
    event: StoredEvent,
    session_id: SessionId,
) -> Result<EventEnvelope, SessionStoreError> {
    Ok(EventEnvelope {
        event_id: EventId::from_uuid(uuid::Uuid::parse_str(&event.event_id)?),
        session_id,
        run_id: event
            .run_id
            .map(|value| uuid::Uuid::parse_str(&value).map(RunId::from_uuid))
            .transpose()?,
        sequence: u64::try_from(event.sequence)?,
        timestamp: DateTime::parse_from_rfc3339(&event.timestamp)?.with_timezone(&Utc),
        source: enum_from_name(&event.source)?,
        event: NormalizedEvent {
            kind: event.kind,
            visibility: enum_from_name(&event.visibility)?,
            payload: serde_json::from_str(&event.payload_json)?,
            vendor_event_id: event.vendor_event_id,
            raw_segment_reference: event.raw_segment_reference,
        },
    })
}

fn enum_name<T: Serialize>(value: T) -> Result<String, SessionStoreError> {
    serde_json::to_value(value)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or(SessionStoreError::InvalidEnum)
}

fn enum_from_name<T: DeserializeOwned>(value: &str) -> Result<T, SessionStoreError> {
    serde_json::from_value(Value::String(value.to_owned())).map_err(SessionStoreError::from)
}

#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error("session storage limit is invalid")]
    InvalidLimit,
    #[error("stored enum value is invalid")]
    InvalidEnum,
    #[error("stored raw protocol capture metadata is inconsistent")]
    InvalidRawCapture,
    #[error("logical session {0} does not exist")]
    SessionNotFound(SessionId),
    #[error("process run {0} does not exist")]
    RunNotFound(RunId),
    #[error("stored identifier is invalid: {0}")]
    Identifier(#[from] uuid::Error),
    #[error("stored timestamp is invalid: {0}")]
    Timestamp(#[from] chrono::ParseError),
    #[error("stored integer is outside its supported range: {0}")]
    Integer(#[from] TryFromIntError),
    #[error("stored JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use maestro_domain::{EventSource, IntegrationMode};
    use tempfile::tempdir;

    use super::*;
    use crate::DatabaseKey;

    #[test]
    fn session_run_and_redacted_event_round_trip() {
        let key = DatabaseKey::generate();
        let mut database = Database::open_in_memory(&key).expect("database opens");
        let project_id = ProjectId::new();
        database
            .upsert_project(
                &project_id.to_string(),
                "Example",
                &["/tmp/example".to_owned()],
            )
            .expect("project persists");
        let store = SessionStore::new(&database);
        let session_id = SessionId::new();
        let run_id = RunId::new();
        store
            .upsert_session(
                session_id,
                project_id,
                AgentKind::Fake,
                IntegrationMode::Structured,
                SessionState::Running,
                Some("Fixture"),
            )
            .expect("session persists");
        store
            .start_run(
                run_id,
                session_id,
                42,
                &serde_json::json!({ "token": "sk-test-secret-1234567890" }),
                "structured",
            )
            .expect("run persists");

        let event = EventEnvelope::new(
            session_id,
            Some(run_id),
            1,
            EventSource::Cli,
            NormalizedEvent::user(
                "message",
                serde_json::json!({ "api_key": "sk-test-secret-1234567890" }),
            ),
        );
        store.append_event(&event).expect("event persists");

        let loaded = store.load_events(session_id, 0, 10).expect("events load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].event.payload["api_key"], "[REDACTED]");
        assert_eq!(
            store.session_state(session_id).expect("state loads"),
            Some(SessionState::Running)
        );
        assert_eq!(
            store.session_metadata(session_id).expect("metadata loads"),
            Some(PersistedSessionMetadata {
                project_id,
                agent_kind: AgentKind::Fake,
                integration_mode: IntegrationMode::Structured,
                state: SessionState::Running,
                active_run_id: Some(run_id),
                latest_sequence: 1,
            })
        );
        let index = store
            .list_sessions(project_id, 10)
            .expect("session index loads");
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].session_id, session_id);
        assert_eq!(index[0].active_run_id, Some(run_id));
        assert_eq!(index[0].latest_sequence, 1);
        store
            .finish_run(run_id, "completed", PersistedRunExit::Exited(0), None)
            .expect("run completes");
        assert_eq!(
            store
                .session_metadata(session_id)
                .expect("completed metadata loads")
                .expect("session remains")
                .active_run_id,
            None
        );
    }

    #[test]
    fn replay_is_cursor_ordered_and_bounded() {
        let key = DatabaseKey::generate();
        let mut database = Database::open_in_memory(&key).expect("database opens");
        let project_id = ProjectId::new();
        database
            .upsert_project(
                &project_id.to_string(),
                "Example",
                &["/tmp/example".to_owned()],
            )
            .expect("project persists");
        let store = SessionStore::new(&database);
        let session_id = SessionId::new();
        store
            .upsert_session(
                session_id,
                project_id,
                AgentKind::Fake,
                IntegrationMode::Structured,
                SessionState::Running,
                None,
            )
            .expect("session persists");
        for sequence in 1..=4 {
            store
                .append_event(&EventEnvelope::new(
                    session_id,
                    None,
                    sequence,
                    EventSource::Daemon,
                    NormalizedEvent::user("test", serde_json::json!({ "sequence": sequence })),
                ))
                .expect("event persists");
        }

        let loaded = store
            .load_events(session_id, 1, 2)
            .expect("bounded replay loads");
        assert_eq!(
            loaded
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(matches!(
            store.load_events(session_id, 0, 0),
            Err(SessionStoreError::InvalidLimit)
        ));
    }

    #[test]
    fn opted_in_raw_protocol_capture_is_exact_bounded_metadata_and_encrypted() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("maestro.db");
        let key = DatabaseKey::generate();
        let mut database = Database::open(&path, &key).expect("database opens");
        let project_id = ProjectId::new();
        database
            .upsert_project(
                &project_id.to_string(),
                "Raw capture fixture",
                &["/tmp/raw-capture".to_owned()],
            )
            .expect("project persists");
        let store = SessionStore::new(&database);
        let session_id = SessionId::new();
        let run_id = RunId::new();
        store
            .upsert_session(
                session_id,
                project_id,
                AgentKind::Fake,
                IntegrationMode::Structured,
                SessionState::Running,
                None,
            )
            .expect("session persists");
        store
            .start_run(run_id, session_id, 7, &serde_json::json!({}), "structured")
            .expect("run persists");

        assert_eq!(
            store
                .raw_protocol_capture(session_id, run_id)
                .expect("absence can be queried"),
            None,
            "raw capture must remain absent until explicitly enabled"
        );
        assert!(matches!(
            store.upsert_raw_protocol_capture(
                session_id,
                run_id,
                b"too-long",
                2,
                true,
                false,
                "raw-segments-v1/test",
            ),
            Err(SessionStoreError::InvalidRawCapture)
        ));

        let exact_bytes = b"{\"type\":\"message\",\"token\":\"raw-fixture-secret\"}\n\
{\"type\":\"result\"}\r\n";
        store
            .upsert_raw_protocol_capture(
                session_id,
                run_id,
                exact_bytes,
                u64::try_from(exact_bytes.len() + 11).expect("fixture size fits"),
                true,
                true,
                "raw-segments-v1/test",
            )
            .expect("raw capture persists");
        let loaded = store
            .raw_protocol_capture(session_id, run_id)
            .expect("raw capture loads")
            .expect("raw capture exists");
        assert_eq!(loaded.session_id, session_id);
        assert_eq!(loaded.run_id, run_id);
        assert_eq!(loaded.bytes, exact_bytes);
        assert_eq!(
            loaded.observed_byte_count,
            u64::try_from(exact_bytes.len() + 11).expect("fixture size fits")
        );
        assert!(loaded.truncated);
        assert!(loaded.completed);

        database
            .connection()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .expect("capture checkpoints");
        drop(database);
        let encrypted_bytes = fs::read(path).expect("database bytes load");
        assert!(
            !encrypted_bytes
                .windows(b"raw-fixture-secret".len())
                .any(|window| window == b"raw-fixture-secret"),
            "raw protocol content must not appear in plaintext at rest"
        );
    }
}
