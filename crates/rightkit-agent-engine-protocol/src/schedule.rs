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
