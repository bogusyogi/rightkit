use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post, put},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::path::Path as FsPath;

use crate::api::{lock_recover, now_iso, now_millis, require_license_feature, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;
use crate::api_stores::default_instincts_store_path;

pub fn runtime_routes() -> Router<DaemonState> {
    Router::new()
        .route("/runtime/learning", get(get_learning))
        .route(
            "/runtime/skillopt/runs",
            get(list_skillopt_runs).post(create_skillopt_run),
        )
        .route("/runtime/skillopt/runs/{id}", get(get_skillopt_run))
        .route(
            "/runtime/skillopt/runs/{id}/resume",
            post(resume_skillopt_run),
        )
        .route(
            "/runtime/skillopt/runs/{id}/execute",
            post(execute_skillopt_epoch),
        )
        .route(
            "/runtime/skillopt/runs/{id}/propose-candidate",
            post(propose_skillopt_candidate),
        )
        .route(
            "/runtime/skillopt/runs/{id}/cancel",
            post(cancel_skillopt_run),
        )
        .route(
            "/runtime/skillopt/runs/{id}/history",
            get(list_skillopt_run_history),
        )
        .route(
            "/runtime/skillopt/runs/{id}/rejected-edits",
            get(list_skillopt_rejected_edits),
        )
        .route(
            "/skills/training-runs",
            get(list_skillopt_runs).post(create_skillopt_run),
        )
        .route("/skills/training-runs/{id}", get(get_skillopt_run))
        .route(
            "/skills/training-runs/{id}/resume",
            post(resume_skillopt_run),
        )
        .route(
            "/skills/training-runs/{id}/execute",
            post(execute_skillopt_epoch),
        )
        .route(
            "/skills/training-runs/{id}/propose-candidate",
            post(propose_skillopt_candidate),
        )
        .route(
            "/skills/training-runs/{id}/cancel",
            post(cancel_skillopt_run),
        )
        .route(
            "/skills/training-runs/{id}/history",
            get(list_skillopt_run_history),
        )
        .route(
            "/skills/training-runs/{id}/rejected-edits",
            get(list_skillopt_rejected_edits),
        )
        .route(
            "/runtime/learning/{id}/approve",
            post(approve_learning_proposal),
        )
        .route(
            "/runtime/learning/{id}/reject",
            post(reject_learning_proposal),
        )
        .route("/runtime/metrics", get(get_metrics))
        .route("/runtime/resource-metrics", get(get_resource_metrics))
        .route("/runtime/process-topology", get(get_process_topology))
        .route(
            "/qualification/matrix/run",
            post(post_qualification_matrix_run),
        )
        .route("/runtime/context/status", get(get_context_status))
        .route("/runtime/context/adapt/status", get(get_adapt_status))
        .route(
            "/runtime/context/adapt/review-taste",
            post(post_adapt_review_taste),
        )
        .route(
            "/runtime/context/adapt/adjudicate-taste",
            post(post_adapt_adjudicate_taste),
        )
        .route(
            "/runtime/context/adapt/apply-insights",
            post(post_adapt_apply_insights),
        )
        .route("/runtime/context/adapt/insights", post(post_adapt_insights))
        .route(
            "/runtime/context/adapt/apply-taste",
            post(post_adapt_apply_taste),
        )
        .route("/runtime/context/adapt/recall", post(post_adapt_recall))
        .route("/runtime/taste", get(get_taste))
        .route("/runtime/proposals", get(get_proposals))
        .route("/runtime/rollouts", get(get_rollouts))
        .route("/runtime/finetuning", get(get_finetuning))
        .route("/runtime/finetuning/export", get(export_finetuning))
        .route("/runtime/harness-radar", get(get_harness_radar))
        .route(
            "/runtime/harness-radar/proposals/{id}/promote",
            post(promote_harness_radar_proposal),
        )
        .route(
            "/runtime/harness-radar/schedule",
            post(schedule_harness_radar_refresh),
        )
        .route("/runtime/audit", get(get_audit))
        .route("/runtime/skills/synthesize", post(synthesize_skill_handler))
        .route("/runtime/middleware", get(get_runtime_middleware))
        .route(
            "/runtime/middleware/{id}/enabled",
            put(set_runtime_middleware_enabled),
        )
        .route("/runtime/spans", get(get_runtime_spans))
        .route("/runtime/spans/{id}", get(get_runtime_span))
        .route("/runtime/observations", get(get_runtime_observations))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinetuneExportQuery {
    pub min_gds: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesizeSkillBody {
    pub task_description: String,
    #[serde(default)]
    pub template: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSkillOptRunBody {
    pub name: String,
    pub target: rightkit_agent_learning::SkillOptimizationTarget,
    #[serde(default)]
    pub rejected_edits: Vec<rightkit_agent_learning::RejectedEdit>,
    #[serde(default)]
    pub gates: Vec<rightkit_agent_learning::BenchmarkGate>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSkillOptRunBody {
    #[serde(default)]
    pub checkpoint: Option<serde_json::Value>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelSkillOptRunBody {
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetMiddlewareEnabledBody {
    pub enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSessionQuery {
    pub session_id: Option<String>,
}
