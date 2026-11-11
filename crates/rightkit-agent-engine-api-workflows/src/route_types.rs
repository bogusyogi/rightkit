//! Cycle-free workflow HTTP request/query DTOs.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct CreateRequest {
    pub name: String,
    #[serde(default)]
    pub definition: Value,
}

#[derive(Debug, Deserialize)]
pub struct StartRequest {
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Deserialize)]
pub struct TriggerRequest {
    pub kind: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub cron_expression: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub version_pin: Option<String>,
    #[serde(default = "default_overlap_policy")]
    pub overlap_policy: String,
    #[serde(default = "default_misfire_policy")]
    pub misfire_policy: String,
    #[serde(default = "default_concurrency_cap")]
    pub concurrency_cap: i64,
    #[serde(default = "default_dst_policy")]
    pub dst_policy: String,
}

#[derive(Debug, Deserialize)]
pub struct TriggerPatch {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub after_seq: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRequest<P = serde_json::Value> {
    pub raw_artifact_ref: String,
    /// The workflow package; hosts deserialize into their own package type.
    pub package: P,
}

#[derive(Debug, Deserialize)]
pub struct ImportReviewRequest {
    pub reviewer: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoteOptimizationRequest {
    pub benchmark_id: String,
    pub rollback_id: String,
    pub monitoring_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOptimizationRequest {
    pub workflow_id: String,
    pub base_version_id: String,
    pub candidate_definition: Value,
    pub change_kind: rightkit_agent_learning::WorkflowProposalKind,
    pub diff_summary: String,
    pub proposed_by: String,
    pub benchmark_id: String,
    pub evidence: rightkit_agent_learning::EvalEvidence,
}

#[derive(Debug, Deserialize)]
pub struct MonitoringRequest {
    pub monitoring_id: String,
    pub passed: bool,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct McpTaskUpdateRequest {
    pub action: String,
}

fn default_limit() -> i64 {
    200
}
fn default_overlap_policy() -> String {
    "queue".into()
}
fn default_misfire_policy() -> String {
    "skip".into()
}
fn default_concurrency_cap() -> i64 {
    1
}
fn default_dst_policy() -> String {
    "skip_gap".into()
}
