//! The one typed wire envelope for Right Suite sidecars and local services.
//!
//! Every app used to carry its own: ScrapeRight `EngineFrame` (hello/ready/
//! request/response/event/bye), HeardRight `EngineFrame` (`schema_name`,
//! `request_id`, `session_id`, `trace_id`, `payload`, `error`) wrapped again
//! by its speech-service `ServiceEnvelope`, CutRight/GenRight their own
//! protocol crates. They all reduce to this set of JSON-lines frames, which is
//! also what `rightkit-jobs` engines and `rightkit-daemon` hosts speak:
//!
//! ```text
//! {"type":"hello","engine":"x","version":"1","contract_version":1}
//! {"type":"ready"}
//! {"type":"request","id":7,"method":"transcribe","params":{...},"session_id":"s1","trace_id":"t"}
//! {"type":"event","id":7,"event":{...}}                 progress of request 7
//! {"type":"event","session_id":"s1","event":{...}}      unsolicited (no id)
//! {"type":"response","id":7,"ok":true,"result":{...}}
//! {"type":"response","id":7,"ok":false,"error":{"code":"x","message":"y","retryable":false}}
//! {"type":"bye","reason":"shutdown"}
//! ```
//!
//! `method` carries what HeardRight calls `schema_name`; `session_id` and
//! `trace_id` are optional correlation fields that survive the round trip.
//! Parsing is lenient about unknown fields so versions can add to a frame.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A typed error as carried in a failed `response`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireError {
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl WireError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
        }
    }
}

/// One frame of the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    /// First frame from the child/host. `engine`, `version`, `contract_version`
    /// are the identity fields every engine already sends; other fields the
    /// peer adds are ignored here (read the raw `Value` to see them).
    Hello {
        #[serde(default)]
        engine: String,
        #[serde(default)]
        version: String,
        #[serde(default)]
        contract_version: u32,
    },
    /// The peer finished its own warm-up (models loaded, snapshots applied).
    Ready,
    Request {
        id: u64,
        method: String,
        #[serde(default)]
        params: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace_id: Option<String>,
    },
    /// `id` is set for progress of a request, absent for an unsolicited event.
    Event {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        event: Value,
    },
    Response {
        id: u64,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<WireError>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// Orderly goodbye; the stream ends next.
    Bye {
        #[serde(default)]
        reason: String,
    },
}

impl Frame {
    pub fn request(id: u64, method: impl Into<String>, params: Value) -> Frame {
        Frame::Request {
            id,
            method: method.into(),
            params,
            session_id: None,
            trace_id: None,
        }
    }
    pub fn ok(id: u64, result: Value) -> Frame {
        Frame::Response {
            id,
            ok: true,
            result: Some(result),
            error: None,
            session_id: None,
        }
    }
    pub fn err(id: u64, error: WireError) -> Frame {
        Frame::Response {
            id,
            ok: false,
            result: None,
            error: Some(error),
            session_id: None,
        }
    }
    pub fn event(id: Option<u64>, event: Value) -> Frame {
        Frame::Event {
            id,
            session_id: None,
            event,
        }
    }
    /// The request id this frame answers or reports on, if any.
    pub fn id(&self) -> Option<u64> {
        match self {
            Frame::Request { id, .. } | Frame::Response { id, .. } => Some(*id),
            Frame::Event { id, .. } => *id,
            _ => None,
        }
    }
}
