use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{EventId, RunId, SessionId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    Cli,
    Gui,
    Daemon,
    Hook,
    Pty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventVisibility {
    User,
    Debug,
    Sensitive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedEvent {
    pub kind: String,
    pub visibility: EventVisibility,
    pub payload: Value,
    pub vendor_event_id: Option<String>,
    pub raw_segment_reference: Option<String>,
}

impl NormalizedEvent {
    pub fn user(kind: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: kind.into(),
            visibility: EventVisibility::User,
            payload,
            vendor_event_id: None,
            raw_segment_reference: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: EventId,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub source: EventSource,
    pub event: NormalizedEvent,
}

impl EventEnvelope {
    pub fn new(
        session_id: SessionId,
        run_id: Option<RunId>,
        sequence: u64,
        source: EventSource,
        event: NormalizedEvent,
    ) -> Self {
        Self {
            event_id: EventId::new(),
            session_id,
            run_id,
            sequence,
            timestamp: Utc::now(),
            source,
            event,
        }
    }
}
