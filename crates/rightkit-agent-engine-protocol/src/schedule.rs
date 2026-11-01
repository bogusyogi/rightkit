//! Canonical schedule-run types — the single definition in the workspace.
//!
//! Hoisted from the byte-near-identical duplicates that lived in
//! `host app/src/scheduler.rs` (write side) and `host app/src/api_schedules.rs`
//! (read side). The wire JSON is unchanged: all serde attributes moved with the
//! struct. `ScheduleRunEvent` here is the SUPERSET — it includes the
//! `schema_version`/`started_at`/`finished_at` evidence fields hermes plan #2
//! added to the write side, so the read side now exposes them too.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub fn default_job_mode() -> String {
    "agent".to_string()
}

pub fn default_schema_version() -> u32 {
    1
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRunState {
    #[serde(default)]
    pub last_claimed_minute: HashMap<String, i64>,
    #[serde(default)]
    pub history: Vec<ScheduleRunEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRunEvent {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub schedule_id: String,
    pub epoch_minute: i64,
    pub status: String,
    pub session_id: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default = "default_job_mode")]
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_execution_backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_workspace_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_limits: Option<ScheduleRunResourceLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivery_kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_recipe_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivery_errors: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRunResourceLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_mb: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full legacy record as the pre-hoist scheduler.rs write path emits it
    /// (camelCase, includes the hermes evidence fields). Must round-trip.
    const LEGACY_FULL: &str = r#"{
        "schemaVersion": 1,
        "scheduleId": "sched-abc123",
        "epochMinute": 28521600,
        "status": "completed",
        "sessionId": "sess-xyz789",
        "message": "run finished ok",
        "startedAt": "2026-06-21T10:00:00Z",
        "finishedAt": "2026-06-21T10:02:00Z",
        "mode": "agent",
        "toolset": "default",
        "executionBackend": "local",
        "resolvedExecutionBackend": "local",
        "executionWorkspaceId": "ws-1",
        "executionWorkspacePath": "/home/user/ws",
        "resourceLimits": { "cpu": 2, "memoryMb": 4096, "diskMb": 8192 },
        "workdir": "/home/user/ws/project",
        "skills": ["read_file", "write_file"],
        "deliveryKinds": ["webhook"],
        "sourceRecipeId": "recipe-001",
        "exitCode": 0,
        "artifactPath": "/tmp/out.tar.gz",
        "deliveryStatus": "delivered",
        "deliveryErrors": []
    }"#;

    /// Oldest-format record — written before hermes added schema_version /
    /// started_at / finished_at. Those three must fall back to their defaults.
    const PRE_HERMES_MINIMAL: &str = r#"{
        "scheduleId": "sched-min",
        "epochMinute": 1,
        "status": "running",
        "sessionId": "sess-min",
        "message": ""
    }"#;

    #[test]
    fn schedule_run_event_legacy_full_round_trips() {
        let ev: ScheduleRunEvent =
            serde_json::from_str(LEGACY_FULL).expect("full legacy record must deserialize");
        assert_eq!(ev.schema_version, 1);
        assert_eq!(ev.schedule_id, "sched-abc123");
        assert_eq!(ev.epoch_minute, 28521600);
        assert_eq!(ev.status, "completed");
        assert_eq!(ev.session_id, "sess-xyz789");
        assert_eq!(ev.started_at, "2026-06-21T10:00:00Z");
        assert_eq!(ev.finished_at.as_deref(), Some("2026-06-21T10:02:00Z"));
        assert_eq!(ev.mode, "agent");
        let limits = ev.resource_limits.as_ref().expect("resourceLimits present");
        assert_eq!(limits.cpu, Some(2));
        assert_eq!(limits.memory_mb, Some(4096));
        assert_eq!(ev.skills, vec!["read_file", "write_file"]);
    }

    #[test]
    fn schedule_run_event_pre_hermes_record_uses_defaults() {
        let ev: ScheduleRunEvent =
            serde_json::from_str(PRE_HERMES_MINIMAL).expect("pre-hermes record must deserialize");
        // The three evidence fields hermes added must default cleanly on old JSON.
        assert_eq!(ev.schema_version, 1);
        assert_eq!(ev.started_at, "");
        assert_eq!(ev.finished_at, None);
        // Pre-existing optional defaults still hold.
        assert_eq!(ev.mode, "agent");
        assert!(ev.toolset.is_none());
        assert!(ev.resource_limits.is_none());
        assert!(ev.skills.is_empty());
    }
}
