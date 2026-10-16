//! Structured lifecycle events: one JSON object per line in `events.jsonl`,
//! also retained in memory. Events never carry credentials, signatures,
//! tokens or request bodies.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde_json::{json, Map, Value};

pub type Event = Value;

pub struct EventLog {
    file: Mutex<Option<File>>,
    memory: Mutex<Vec<Event>>,
}

/// daemon-api v1 `lifecycleEvents` name for an event, where one exists.
pub fn contract_name(event: &str) -> Option<&'static str> {
    Some(match event {
        "LISTENING" => "daemon_started",
        "SHUTDOWN" => "daemon_stopped",
        "ENROLLED" => "device_enrolled",
        "DEVICE_REVOKED" => "device_revoked",
        "UPLOAD_STARTED" => "upload_started",
        "UPLOAD_COMPLETE" => "upload_completed",
        "JOB_UPDATED" => "job_updated",
        _ => return None,
    })
}

impl EventLog {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(Some(f)),
            memory: Mutex::new(Vec::new()),
        })
    }

    pub fn memory_only() -> Self {
        Self {
            file: Mutex::new(None),
            memory: Mutex::new(Vec::new()),
        }
    }

    pub fn emit(&self, now: u64, event: &str, fields: Value) {
        let mut obj = Map::new();
        obj.insert("ts".into(), json!(now));
        obj.insert("event".into(), json!(event));
        if let Some(name) = contract_name(event) {
            obj.insert("contractEvent".into(), json!(name));
        } else if event == "AUTH_REJECTED"
            && fields.get("class").and_then(Value::as_str) == Some("signed_webhook")
        {
            obj.insert("contractEvent".into(), json!("webhook_rejected"));
        }
        if let Value::Object(m) = fields {
            for (k, v) in m {
                obj.insert(k, v);
            }
        }
        let value = Value::Object(obj);
        if let Ok(mut guard) = self.file.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{value}");
                let _ = f.flush();
            }
        }
        if let Ok(mut m) = self.memory.lock() {
            if m.len() >= 10_000 {
                m.remove(0);
            }
            m.push(value);
        }
    }

    pub fn snapshot(&self) -> Vec<Event> {
        self.memory.lock().map(|m| m.clone()).unwrap_or_default()
    }

    /// Events held in memory: at most 10,000, while the file keeps every event.
    pub fn retained(&self) -> usize {
        self.memory.lock().map(|m| m.len()).unwrap_or(0)
    }
}
