use crate::toolbox::{Journal, ToolEvent};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Appends one JSON object per event to a file (content-free; never uploaded).
pub struct JsonlJournal {
    path: PathBuf,
    lock: Mutex<()>,
}

impl JsonlJournal {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }
}

impl Journal for JsonlJournal {
    fn record(&self, event: &ToolEvent) {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{}", event.to_value());
        }
    }
}

/// One completed call in the existing v1 CLI journal shape. Supply only command metadata:
/// these fields are not scrubbed and must never contain a prompt, path, arguments or output.
pub struct CompletedCall<'a> {
    pub tool: &'a str,
    pub effect: &'a str,
    pub approval_state: &'a str,
    pub caller: &'a str,
    pub request_id: Option<&'a str>,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub host_route: &'a str,
}

/// A size-rotated completed-call JSONL journal at the host's existing state-root path.
/// The host must pass trusted command metadata, never tool input, output or paths.
pub struct CompletedCallJournal {
    store: rightkit_logs::JsonlStore,
    schema_version: &'static str,
}

impl CompletedCallJournal {
    /// Writes `<state_root>/cli-journal.jsonl`, retaining four suffix-numbered predecessors.
    pub fn new(state_root: impl AsRef<std::path::Path>, schema_version: &'static str) -> Self {
        let path = state_root.as_ref().join("cli-journal.jsonl");
        let store = rightkit_logs::JsonlStore::new(path, 1_048_576)
            .with_generations(4)
            .with_required_cross_process_lock();
        Self {
            store,
            schema_version,
        }
    }

    /// Writes the v1 shape. Caller labels are trimmed, stripped of control characters and limited
    /// to 64 scalars; request ids retain the first 256 scalars, matching the existing journal.
    /// A failed lock or write is returned; a CLI can ignore it without changing its exit status.
    pub fn append(&self, call: &CompletedCall<'_>) -> std::io::Result<()> {
        // These are command-table identifiers, never free-form tool arguments.
        if call.tool.is_empty()
            || call.tool.len() > 256
            || !call.tool.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b' ' | b'-' | b'_' | b'.')
            })
            || !matches!(call.effect, "read" | "write" | "destroy" | "external" | "spend")
            || !matches!(call.approval_state, "none" | "granted" | "dry_run" | "in_app")
            || !matches!(call.host_route, "local" | "inline" | "service" | "service_projection")
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid call metadata",
            ));
        }
        let caller: String = call
            .caller
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        let caller = if caller.is_empty() { "unknown" } else { &caller };
        let request_id = call.request_id.map(|id| id.chars().take(256).collect::<String>());
        let line = serde_json::json!({
            "schema_version": self.schema_version,
            "tool": call.tool,
            "effect": call.effect,
            "approval_state": call.approval_state,
            "caller": caller,
            "request_id": request_id,
            "exit_code": call.exit_code,
            "duration_ms": call.duration_ms,
            "host_route": call.host_route,
        });
        self.store.append_value(&line)
    }
}

#[cfg(test)]
mod tests {
    use super::{CompletedCall, CompletedCallJournal};
    use serde_json::json;
    use std::fs;

    #[test]
    fn completed_call_requires_the_lock_and_preserves_v1_fields() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cli-journal.jsonl");
        let lock = root.path().join("cli-journal.jsonl.lock");
        let journal = CompletedCallJournal::new(root.path(), "example.cli.journal.v1");
        let call = CompletedCall {
            tool: "status",
            effect: "read",
            approval_state: "none",
            caller: " pulse\u{0007} ",
            request_id: None,
            exit_code: 0,
            duration_ms: 7,
            host_route: "local",
        };

        fs::create_dir(&lock).unwrap();
        assert!(journal.append(&call).is_err());
        assert!(!path.exists());
        fs::remove_dir(&lock).unwrap();

        journal.append(&call).unwrap();
        let text = fs::read_to_string(path).unwrap();
        assert_eq!(text.lines().count(), 1);
        let record: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            record,
            json!({
                "schema_version": "example.cli.journal.v1",
                "tool": "status",
                "effect": "read",
                "approval_state": "none",
                "caller": "pulse",
                "request_id": null,
                "exit_code": 0,
                "duration_ms": 7,
                "host_route": "local",
            })
        );
    }
}
