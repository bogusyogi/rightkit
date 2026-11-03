use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use rightkit_agent_engine_protocol::{ProtocolEvent, TaskState, TranscriptMessage};
use rightkit_evidence::{
    analyze_spec_with_constitution_decisions, build_trace_with_evidence,
    spec_has_placeholder_heavy_content, AcceptanceScenario, AnalysisStatus, ConstitutionDecision,
    ConstitutionDecisionVerdict, ConstitutionSource, FeatureSpec, FeatureTask, Requirement,
    SpecEvidence, SpecStatus, SuccessCriterion, TaskStatus, UserStory,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};

use crate::api::{lock_recover, now_iso, now_millis, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;

pub(crate) fn spec_routes() -> Router<DaemonState> {
    Router::new()
        .route("/specs", get(list_specs_handler).post(create_spec_handler))
        .route(
            "/specs/{id}",
            get(get_spec_handler).patch(patch_spec_handler),
        )
        .route("/specs/{id}/clarify", post(clarify_spec_handler))
        .route("/specs/{id}/plan", post(plan_spec_handler))
        .route("/specs/{id}/tasks", post(add_spec_tasks_handler))
        .route(
            "/specs/{id}/constitution/decisions",
            get(list_constitution_decisions_handler).post(record_constitution_decision_handler),
        )
        .route("/specs/{id}/checklist", post(checklist_spec_handler))
        .route("/specs/{id}/analyze", post(analyze_spec_handler))
        .route("/specs/{id}/implement", post(implement_spec_handler))
        .route("/specs/{id}/trace", get(get_spec_trace_handler))
}

pub(crate) fn load_spec_store(path: &FsPath) -> rightkit_evidence::SpecStore {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub(crate) fn write_spec_store(
    path: &FsPath,
    store: &rightkit_evidence::SpecStore,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create spec store directory: {err}"))?;
    }
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|err| format!("failed to serialize spec store: {err}"))?;
    std::fs::write(path, bytes).map_err(|err| format!("failed to write spec store: {err}"))?;
    Ok(())
}

fn persist_spec_store(
    state: &DaemonState,
    store: &rightkit_evidence::SpecStore,
) -> Result<(), SpecError> {
    let Some(path) = state.spec_store_path.as_deref() else {
        return Ok(());
    };
    write_spec_store(path, store).map_err(SpecError::internal_error)
}

pub(crate) fn build_basic_spec(
    workspace_root: &str,
    title: Option<&str>,
    description: &str,
    branch: Option<&str>,
) -> Result<FeatureSpec, String> {
    let description = description.trim();
    if description.is_empty() {
        return Err("description cannot be empty".to_string());
    }
    let now = now_iso();
    let resolved_title = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| first_sentence(description));
    let spec = FeatureSpec {
        id: format!("spec-{}", now_millis()),
        workspace_root: workspace_root.to_string(),
        branch: branch.map(str::to_string),
        title: resolved_title,
        status: SpecStatus::Draft,
        source_prompt_hash: rightkit_fs::digest::sha256_bytes(description.as_bytes()),
        user_stories: vec![UserStory {
            id: "US1".to_string(),
            priority: 1,
            title: first_sentence(description),
            narrative: description.to_string(),
            independent_test: format!("Verify that {description}"),
            acceptance_scenarios: Vec::new(),
        }],
        functional_requirements: vec![Requirement {
            id: "FR-001".to_string(),
            text: description.to_string(),
            source_story_id: Some("US1".to_string()),
            needs_clarification: false,
        }],
        success_criteria: vec![SuccessCriterion {
            id: "SC-001".to_string(),
            text: "A focused verification run passes and is attached as evidence.".to_string(),
            buildable: true,
        }],
        assumptions: Vec::new(),
        edge_cases: Vec::new(),
        constitution_sources: load_constitution_sources(workspace_root),
        created_at: now.clone(),
        updated_at: now,
    };
    if spec_has_placeholder_heavy_content(&spec) {
        return Err(
            "spec contains too much placeholder content to govern implementation".to_string(),
        );
    }
    Ok(spec)
}

include!("api_specs/types.rs");

include!("api_specs/openapi_components.rs");

include!("api_specs/schema_helpers.rs");

include!("api_specs/handlers_core.rs");

include!("api_specs/handlers_review.rs");

include!("api_specs/planning.rs");

include!("api_specs/evidence_live.rs");

include!("api_specs/evidence_tool_calls.rs");

include!("api_specs/evidence_tool_approvals.rs");

include!("api_specs/evidence_protocol.rs");

include!("api_specs/utilities.rs");

include!("api_specs/error.rs");

include!("api_specs/tests.rs");
