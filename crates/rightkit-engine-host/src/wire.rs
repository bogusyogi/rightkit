//! The engine wire types shared by a host shell and its engine sidecar.
//!
//! Requests travel as `rightkit-job-queue` contract frames (`{type:"request", id, method, params}`).
//! A named command keeps its own name as the method (`settings.get`, `job.status`, ...); requests
//! that belong to the engine itself carry [`ENGINE_METHOD_PREFIX`] so no command name can collide
//! with them. [`INIT_METHOD`] is always the first request a supervisor sends to an engine process.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Prefix of the engine's own (non-command) methods.
pub const ENGINE_METHOD_PREFIX: &str = "engine.";
/// Wire method that hands the engine its init payload; replayed first to every engine process.
pub const INIT_METHOD: &str = "engine.init";
/// Wire method of the engine health request.
pub const HEALTH_METHOD: &str = "engine.health";

/// A typed engine error: a stable `code`, a human `message` and optional structured `details`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineError {
    /// Stable, snake_case error code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Structured data for the UI. Omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl EngineError {
    /// An error with no details.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    /// Attach structured details.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    /// The `{"error": {code, message, details?}}` envelope the CLI prints.
    pub fn envelope(&self) -> Value {
        let mut body = json!({ "code": self.code, "message": self.message });
        if let Some(details) = &self.details {
            body["details"] = details.clone();
        }
        json!({ "error": body })
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EngineError {}

impl From<EngineError> for rightkit_job_queue::JobError {
    fn from(error: EngineError) -> Self {
        let out = rightkit_job_queue::JobError::new(error.code, error.message);
        match error.details {
            Some(details) => out.with_details(details),
            None => out,
        }
    }
}

/// A request on the engine wire.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineRequest {
    /// One of the engine's own methods (`engine.<method>`), with its parameters.
    Engine {
        /// The method name without [`ENGINE_METHOD_PREFIX`].
        method: String,
        /// The parameters.
        params: Value,
    },
    /// A named command shared with the CLI.
    Command {
        /// The command name (`job.status`).
        name: String,
        /// The command input.
        input: Value,
    },
    /// End the session.
    Shutdown,
}

impl EngineRequest {
    /// The contract form `(method, params)` of this request.
    pub fn to_wire(&self) -> (String, Value) {
        match self {
            EngineRequest::Engine { method, params } => {
                (format!("{ENGINE_METHOD_PREFIX}{method}"), params.clone())
            }
            EngineRequest::Command { name, input } => (name.clone(), input.clone()),
            EngineRequest::Shutdown => ("shutdown".to_string(), Value::Null),
        }
    }

    /// Inverse of [`EngineRequest::to_wire`].
    pub fn from_wire(method: &str, params: Value) -> Self {
        match method.strip_prefix(ENGINE_METHOD_PREFIX) {
            Some(rest) => EngineRequest::Engine {
                method: rest.to_string(),
                params,
            },
            None if method == "shutdown" => EngineRequest::Shutdown,
            None => EngineRequest::Command {
                name: method.to_string(),
                input: params,
            },
        }
    }
}

/// An unsolicited engine event, forwarded by the shell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EngineEvent {
    /// A log line, optionally tied to a job.
    Log {
        /// The job the line belongs to.
        job_id: Option<String>,
        /// `info`, `warn`, `error`, ...
        level: String,
        /// The text.
        message: String,
    },
    /// An artifact was indexed.
    ArtifactIndexed {
        /// Its path.
        path: String,
    },
    /// A named event with a JSON payload, forwarded under `topic` unchanged.
    Topic {
        /// The topic (`<area>://<kind>`).
        topic: String,
        /// The payload.
        payload: Value,
    },
}

impl EngineEvent {
    /// The unsolicited frame `{type:"event", event:<this event>}` (no request id).
    pub fn to_frame(&self) -> Value {
        json!({ "type": "event", "event": serde_json::to_value(self).unwrap_or(Value::Null) })
    }

    /// The event inside an unsolicited frame; `None` for a frame of another shape.
    pub fn from_frame(frame: &Value) -> Option<Self> {
        serde_json::from_value(frame.get("event")?.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        for request in [
            EngineRequest::Engine {
                method: "health".into(),
                params: json!({}),
            },
            EngineRequest::Command {
                name: "job.status".into(),
                input: json!({"job_id": "j1"}),
            },
            EngineRequest::Shutdown,
        ] {
            let (method, params) = request.to_wire();
            assert_eq!(EngineRequest::from_wire(&method, params), request);
        }
        assert_eq!(
            EngineRequest::Engine {
                method: "init".into(),
                params: Value::Null
            }
            .to_wire()
            .0,
            INIT_METHOD
        );
    }

    #[test]
    fn topic_event_frame_round_trips() {
        let event = EngineEvent::Topic {
            topic: "job://progress".into(),
            payload: json!({"id": "j1"}),
        };
        let frame = event.to_frame();
        assert!(frame.get("id").is_none());
        assert_eq!(EngineEvent::from_frame(&frame), Some(event));
    }

    #[test]
    fn error_envelope_carries_details() {
        let e = EngineError::new("bad_input", "nope").with_details(json!({"k": 1}));
        assert_eq!(e.envelope()["error"]["details"]["k"], 1);
    }
}
