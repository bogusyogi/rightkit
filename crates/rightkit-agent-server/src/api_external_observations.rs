//! PL-009 authenticated external-observation intake and corrective delivery.

use crate::api::DaemonState;
use crate::daemon::external_observations::{
    ExternalObservationInput, ObservationError, ReactionPolicy,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::post,
    Router,
};
use rightkit_agent_engine_protocol::TaskNode;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObservationBody {
    observation: ExternalObservationInput,
    #[serde(default)]
    approval_granted: bool,
}

pub fn routes() -> Router<DaemonState> {
    Router::new().route("/sessions/{session_id}/external-observations", post(intake))
}

async fn intake(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ObservationBody>,
) -> Result<Json<Value>, StatusCode> {
    // FM-151 (PL-009 seam): this route sits behind the daemon-wide
    // `require_token` bearer-token gate (api_router.rs) — it is not itself a
    // bypassed/unauthenticated seam. `enforce_trusted_source_authority` is a
    // second, additive check: it rejects an explicitly-declared untrusted
    // caller (`x-coderight-source-trust`) with a typed `StatusCode::FORBIDDEN`
    // rather than a silent drop or panic. The primary FM-151 external-trigger
    // signature/identity mechanism lives at `deliver_trigger` in
    // api_triggers.rs; this intake path is the corrective/observation seam
    // PL-009 layers on top of it, not a replacement for it.
    crate::api_permissions::enforce_trusted_source_authority(&headers)?;
    let session = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let workspace_trusted = matches!(
        session.workspace_trust,
        rightkit_agent_policy::WorkspaceTrust::Trusted
    );
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    // Await before taking synchronous locks; serialize fact + reaction
    // commits with the daemon's existing durable storage authority.
    let mut storage = storage.lock().await;
    let (receipt, work, committed_snapshot) = {
        let mut loop_ = state
            .external_observations
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut candidate = loop_.clone();
        let receipt = candidate
            .observe(body.observation)
            .map_err(observation_status)?;
        let snapshot = candidate.snapshot().map_err(observation_status)?;
        // A denied reaction still leaves its observation durably recorded.
        storage
            .save_observation_snapshot(&snapshot)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        *loop_ = candidate.clone();
        let lease = candidate.current_lease();
        let owner = session
            .role
            .clone()
            .unwrap_or_else(|| "conductor".to_string());
        let mut live_tasks = session
            .tasks
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let mut tasks: Vec<TaskNode> = live_tasks.clone();
        // A restored observation ledger may already contain emitted markers.
        // Restore their canonical tasks before replay can suppress delivery.
        for persisted in storage
            .get_tasks(&session_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        {
            if !tasks.iter().any(|task| task.id == persisted.id) {
                tasks.push(persisted);
            }
        }
        let work = candidate
            .deliver_pending(
                &mut tasks,
                lease,
                ReactionPolicy {
                    workspace_trusted,
                    policy_allows: true,
                    approval_granted: body.approval_granted,
                },
                &|_: &_| Some(owner.clone()),
                rightkit_agent_engine_protocol::time::now_iso(),
            )
            .map_err(observation_status)?;
        let committed_snapshot = candidate.snapshot().map_err(observation_status)?;
        storage
            .save_observation_reaction(&session_id, &tasks, &committed_snapshot)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        *live_tasks = tasks;
        *loop_ = candidate;
        (receipt, work, committed_snapshot)
    };
    Ok(Json(
        json!({"receipt": receipt.observation_id, "snapshot": committed_snapshot, "work": work}),
    ))
}

fn observation_status(error: ObservationError) -> StatusCode {
    match error {
        ObservationError::InvalidInput(_) | ObservationError::UntrustedInput => {
            StatusCode::BAD_REQUEST
        }
        ObservationError::WorkspaceUntrusted
        | ObservationError::PolicyDenied
        | ObservationError::ApprovalRequired => StatusCode::FORBIDDEN,
        ObservationError::StaleController { .. } => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
