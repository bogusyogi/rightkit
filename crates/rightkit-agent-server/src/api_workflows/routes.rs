//! REST endpoints for the autonomous workflow control plane (WP-09 / ADR-0021).
//!
//! Every handler enforces the workflow feature gate and emits
//! [`WorkflowApiError`] on rejection. State-changing endpoints call the
//! harness (WP-03) and let it write through the journal writer; routes
//! never mutate `workflow_*` rows directly.
//!
//! Reads project durable workflow storage. Lifecycle writes are reduced and
//! committed through the command coordinator; SSE replays durable journal
//! rows with stable ids.

#![forbid(unsafe_code)]

use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
    Json, Router,
};
use futures_util::stream::{self, Stream};
use serde_json::{json, Value};
use std::convert::Infallible;

use rightkit_agent_engine_api_workflows::route_types::{
    CreateOptimizationRequest, CreateRequest, ImportRequest, ImportReviewRequest,
    McpTaskUpdateRequest, MonitoringRequest, PageQuery, PromoteOptimizationRequest, StartRequest,
    TriggerPatch, TriggerRequest,
};
pub(crate) use rightkit_agent_engine_api_workflows::{
    LegacyTriggerDelivery, LegacyWorkflowAdmission,
};
use rightkit_agent_engine_storage::{
    HmacAdmittedTriggerEnvelope, JournalAppend, ProjectionUpdate, TriggerRunAdmission,
    WorkflowCommandRow, WorkflowDefinitionWriter, WorkflowOccurrenceRow, WorkflowQueueRow,
    WorkflowRepository, WorkflowRunRow, WorkflowTriggerArtifactPut, WorkflowTriggerArtifactStore,
    WorkflowTriggerEventRow, WorkflowTriggerRawArtifactRow, WorkflowTriggerRawArtifactWrite,
    WorkflowTriggerWriter,
};
use rightkit_workflow::control_plane::{
    clock::{Clock, SystemClock},
    compiler::{compile, parse_authoring_json, parse_json, validate_source_authority},
    domain::CompilerOutcome,
    domain::{run_id, workflow_id, workflow_version_id, LifecyclePhase, LifecycleState, Revision},
    prelude::{DedupeStrategy, MisfirePolicy, OccurrencePolicy, OverlapPolicy},
    reducer::{
        ApprovalDecisionKind, Command, CommandKind, NodeCommand, NodeLifecycleState, NodePhase,
    },
    trigger::DstResolutionPolicy,
};

use crate::api::DaemonState;

use super::error::WorkflowApiError;
use super::openapi::openapi_v1_schema;
use super::state::WorkflowApiState;

struct LocalTriggerArtifactStore {
    root: PathBuf,
}

impl LocalTriggerArtifactStore {
    fn for_database(path: &str) -> rightkit_agent_engine_storage::Result<Self> {
        if path.trim().is_empty() {
            return Err(rightkit_agent_engine_storage::StorageError::Serde(
                "workflow trigger artifact store requires database path".into(),
            ));
        }
        let root = PathBuf::from(path)
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("workflow-trigger-artifacts");
        rightkit_fs::ensure_private_dir(&root)
            .map_err(|e| rightkit_agent_engine_storage::StorageError::Serde(e.to_string()))?;
        Ok(Self { root })
    }
}

impl WorkflowTriggerArtifactStore for LocalTriggerArtifactStore {
    fn put_content_addressed(
        &self,
        artifact: &WorkflowTriggerRawArtifactWrite,
    ) -> rightkit_agent_engine_storage::Result<WorkflowTriggerArtifactPut> {
        if rightkit_workflow::control_plane::domain::ContentHash::sha256(&artifact.bytes).as_str()
            != artifact.content_hash
        {
            return Err(rightkit_agent_engine_storage::StorageError::Serde(
                "trigger artifact hash mismatch".into(),
            ));
        }
        let name = artifact
            .content_hash
            .strip_prefix("sha256:")
            .ok_or_else(|| {
                rightkit_agent_engine_storage::StorageError::Serde(
                    "invalid trigger artifact hash".into(),
                )
            })?;
        let path = self.root.join(name);
        let created = !path.exists();
        if path.exists() {
            if std::fs::read(&path)
                .map_err(|e| rightkit_agent_engine_storage::StorageError::Serde(e.to_string()))?
                != artifact.bytes
            {
                return Err(rightkit_agent_engine_storage::StorageError::Serde(
                    "trigger artifact collision".into(),
                ));
            }
        } else {
            rightkit_fs::write_private_file(&path, &artifact.bytes)
                .map_err(|e| rightkit_agent_engine_storage::StorageError::Serde(e.to_string()))?;
        }
        Ok(WorkflowTriggerArtifactPut {
            row: WorkflowTriggerRawArtifactRow {
                id: artifact.id.clone(),
                content_hash: artifact.content_hash.clone(),
                source: artifact.source.clone(),
                provenance_json: artifact.provenance_json.clone(),
                trust_json: artifact.trust_json.clone(),
                storage_uri: format!("file://{}", path.display()),
                metadata_json: serde_json::json!({"mediaType":artifact.media_type,"redacted":true})
                    .to_string(),
                byte_length: artifact.bytes.len() as i64,
                created_at: artifact.created_at.clone(),
            },
            created,
        })
    }

    fn remove_content_addressed(
        &self,
        artifact: &WorkflowTriggerRawArtifactRow,
    ) -> rightkit_agent_engine_storage::Result<()> {
        let name = artifact
            .content_hash
            .strip_prefix("sha256:")
            .ok_or_else(|| {
                rightkit_agent_engine_storage::StorageError::Serde(
                    "invalid trigger artifact hash".into(),
                )
            })?;
        let expected = self.root.join(name);
        let uri = format!("file://{}", expected.display());
        if artifact.storage_uri != uri {
            return Err(rightkit_agent_engine_storage::StorageError::Serde(
                "trigger artifact cleanup path mismatch".into(),
            ));
        }
        if expected.exists() {
            std::fs::remove_file(expected).map_err(|error| {
                rightkit_agent_engine_storage::StorageError::Serde(error.to_string())
            })?;
        }
        Ok(())
    }
}

/// Handoff seam for legacy authenticated deliveries. The caller supplies only
/// post-admission facts; unified coordinator wiring owns normalized event,
/// occurrence, run, and root-queue atomicity behind this boundary.
pub(crate) async fn admit_legacy_trigger_delivery(
    state: &DaemonState,
    delivery: LegacyTriggerDelivery,
    shadow: bool,
) -> Result<LegacyWorkflowAdmission, WorkflowApiError> {
    if !state.workflows_feature.is_enabled() {
        return Ok(LegacyWorkflowAdmission::NotMapped);
    }
    let Some(wf) = state.workflow_api_state.as_ref() else {
        return Ok(LegacyWorkflowAdmission::NotMapped);
    };
    let (trigger, published) = {
        let storage = wf.storage.lock().await;
        let repo = WorkflowRepository::new(storage.conn());
        let Some(trigger) = repo
            .find_trigger(&delivery.trigger_id)
            .map_err(WorkflowApiError::from)?
        else {
            return Ok(LegacyWorkflowAdmission::NotMapped);
        };
        let published = match trigger.version_pin.as_deref() {
            Some(version_id) => repo
                .find_version(version_id)
                .map_err(WorkflowApiError::from)?
                .is_some_and(|version| {
                    version.workflow_id == trigger.workflow_id
                        && version.publication_state == "published"
                }),
            None => false,
        };
        (trigger, published)
    };
    if trigger.version_pin.is_none() {
        return Ok(LegacyWorkflowAdmission::CoordinatorUnavailable {
            reason: "mapped_trigger_has_no_version_pin".into(),
        });
    }
    if !published {
        return Ok(LegacyWorkflowAdmission::CoordinatorUnavailable {
            reason: "mapped_trigger_version_not_published".into(),
        });
    }
    if shadow {
        let reason = format!(
            "mapped trigger={} provider={} delivery={} event={} workspace={} principal={} trust={} hash={}",
            trigger.id,
            delivery.provider,
            delivery.delivery_id,
            delivery.event_type.as_deref().unwrap_or("unknown"),
            delivery.workspace_root,
            delivery.principal,
            delivery.trust_level,
            delivery.content_hash,
        );
        return Ok(LegacyWorkflowAdmission::Shadow { reason });
    }
    if !trigger.enabled {
        return Ok(LegacyWorkflowAdmission::CutOver);
    }
    admit_authenticated_delivery_via_coordinator(state, delivery).await
}

/// Coordinator handoff. WP06's storage writer atomically commits the event
/// inbox row, occurrence decision, root run, command, and queue row.
async fn admit_authenticated_delivery_via_coordinator(
    state: &DaemonState,
    delivery: LegacyTriggerDelivery,
) -> Result<LegacyWorkflowAdmission, WorkflowApiError> {
    let wf = workflow_state(state)?;
    let clock = SystemClock::new();
    let now_ms = clock.now_unix_ms();
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let trigger = repo
        .find_trigger(&delivery.trigger_id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found("mapped workflow trigger"))?;
    let version_id = trigger
        .version_pin
        .clone()
        .ok_or_else(|| WorkflowApiError::conflict("mapped workflow trigger has no version pin"))?;
    let version = repo
        .find_version(&version_id)
        .map_err(WorkflowApiError::from)?
        .filter(|version| {
            version.workflow_id == trigger.workflow_id && version.publication_state == "published"
        })
        .ok_or_else(|| {
            WorkflowApiError::conflict("mapped workflow trigger version is not published")
        })?;
    let source = format!("webhook:{}", delivery.provider);
    let event_id =
        crate::workflow::triggers::derive_event_id(&source, &delivery.delivery_id, now_ms);
    let raw_payload = redact_trigger_payload(delivery.raw_payload.clone());
    let normalized_payload = redact_trigger_payload(delivery.payload.clone());
    let raw_bytes = serde_json::to_vec(&raw_payload)
        .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?;
    let content_hash = rightkit_workflow::control_plane::domain::ContentHash::sha256(&raw_bytes);
    let raw_artifact_id = format!(
        "wfa_{}",
        content_hash.as_str().trim_start_matches("sha256:")
    );
    let dedupe_key = crate::workflow::triggers::derive_dedupe_key(
        &source,
        &delivery.delivery_id,
        &content_hash,
        Some(&trigger.id),
    );
    let event = WorkflowTriggerEventRow {
        id: event_id.to_string(),
        trigger_id: Some(trigger.id.clone()),
        source,
        event_type: delivery
            .event_type
            .clone()
            .unwrap_or_else(|| "webhook".into()),
        external_id: Some(delivery.delivery_id.clone()),
        dedupe_key,
        content_hash: content_hash.to_string(),
        trust_json: serde_json::json!({
            "level": delivery.trust_level,
            "principal": delivery.principal,
            "workspace": delivery.workspace_root,
            "signatureHash": delivery.signature_hash,
        })
        .to_string(),
        data_json: serde_json::to_string(&normalized_payload)
            .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?,
        raw_artifact_id: Some(raw_artifact_id.clone()),
        occurred_at: delivery.received_at.clone(),
        received_at: delivery.received_at.clone(),
    };
    let event_for_policy = crate::workflow::triggers::NormalizedTriggerEvent {
        spec_version: crate::workflow::triggers::NormalizedTriggerEvent::SPEC_VERSION.into(),
        id: event_id,
        trigger_id: Some(rightkit_workflow::control_plane::domain::trigger_id(
            trigger.id.clone(),
        )),
        external_id: Some(delivery.delivery_id.clone()),
        source: event.source.clone(),
        type_: event.event_type.clone(),
        subject: None,
        time: event.occurred_at.clone(),
        received_at: event.received_at.clone(),
        data_content_type: "application/json".into(),
        data_schema: None,
        data: crate::workflow::triggers::NormalizedData::from_json(normalized_payload)
            .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?,
        trust: crate::workflow::triggers::TrustInfo {
            level: crate::workflow::triggers::TrustLevel::Authenticated,
            authenticated: true,
            origin: crate::workflow::triggers::TrustOrigin::WebhookDerived,
            principal: delivery.principal.clone(),
            admission_policy: "legacy-hmac-cutover".into(),
            signature_algorithm: Some("hmac-sha256".into()),
            signature_key_id: Some(format!("legacy-{}", delivery.provider)),
            actor_association: None,
            workspace_trust: Some(delivery.trust_level.clone()),
            findings: Vec::new(),
        },
        dedupe_key: event.dedupe_key.clone(),
        content_hash: content_hash.clone(),
        raw_artifact: None,
        correlation_id: None,
        causation_id: None,
        trace_parent: None,
        extensions: Default::default(),
    };
    let policy = occurrence_policy(&trigger)?;
    let decision = crate::workflow::triggers::OccurrenceAdmissionLayer::new(storage.conn())
        .evaluate(
            &crate::workflow::triggers::OccurrenceInputs {
                normalized_event: &event_for_policy,
                event_row: &event,
                trigger_row: &trigger,
                policy: &policy,
            },
            &clock,
        )
        .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?;
    let occurrence = decision.row().clone();
    let run_admission = matches!(
        decision,
        crate::workflow::triggers::OccurrenceDecision::Admit(_)
            | crate::workflow::triggers::OccurrenceDecision::Coalesce(_)
    )
    .then(|| trigger_run_admission(&trigger, &version, &occurrence, &event, now_ms));
    let raw_artifact = WorkflowTriggerRawArtifactWrite {
        id: raw_artifact_id,
        content_hash: content_hash.to_string(),
        source: event.source.clone(),
        provenance_json: serde_json::json!({"provider":delivery.provider,"deliveryId":delivery.delivery_id,"workspace":delivery.workspace_root}).to_string(),
        trust_json: serde_json::json!({"principal":delivery.principal,"level":delivery.trust_level,"signatureHash":delivery.signature_hash}).to_string(),
        media_type: "application/json".into(),
        bytes: raw_bytes,
        created_at: event.received_at.clone(),
    };
    let envelope = HmacAdmittedTriggerEnvelope {
        event,
        occurrence,
        raw_artifact,
        hmac_key_id: format!("legacy-{}", delivery.provider),
        signature_hash: delivery.signature_hash,
    };
    let artifact_store = LocalTriggerArtifactStore::for_database(&state.database_path)
        .map_err(WorkflowApiError::from)?;
    let writer = WorkflowTriggerWriter::new(storage.conn());
    let outcome = if let Some(parts) = run_admission.as_ref() {
        let journal = JournalAppend {
            run_id: &parts.run.id,
            event_kind: "run_started",
            node_id: None,
            payload_json: &parts.journal_payload,
            created_at: &parts.journal_created_at,
        };
        let projection = ProjectionUpdate {
            run_id: &parts.run.id,
            run_phase: Some("running"),
            ..Default::default()
        };
        let admission = TriggerRunAdmission {
            snapshot: &parts.snapshot,
            reducer_command: &parts.command,
            run: &parts.run,
            command: &parts.command_row,
            outcome_json: "{}",
            event: &journal,
            projection: &projection,
            root_queue: &parts.root_queue,
        };
        writer.admit_hmac_envelope(&artifact_store, &envelope, Some(&admission))
    } else {
        writer.admit_hmac_envelope(&artifact_store, &envelope, None)
    }
    .map_err(WorkflowApiError::from)?;
    if !outcome.duplicate {
        let _ = crate::workflow::continuations::match_event_waits(
            storage.conn(),
            &event_for_policy.type_,
            event_for_policy.correlation_id.as_deref().unwrap_or(""),
            &event_for_policy.data.as_value(),
            now_ms,
        );
    }
    Ok(LegacyWorkflowAdmission::CutOver)
}

fn redact_trigger_payload(value: Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let sensitive = [
                        "secret",
                        "token",
                        "password",
                        "authorization",
                        "auth",
                        "cookie",
                        "header",
                        "credential",
                        "url",
                        "api_key",
                        "apikey",
                    ]
                    .iter()
                    .any(|needle| key.to_ascii_lowercase().contains(needle));
                    (
                        key,
                        if sensitive {
                            Value::String("[redacted]".into())
                        } else {
                            redact_trigger_payload(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(redact_trigger_payload).collect())
        }
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            let prose_secret = [
                "bearer ",
                "authorization:",
                "password=",
                "token=",
                "api_key=",
                "cookie:",
                "set-cookie:",
                "http://",
                "https://",
            ]
            .iter()
            .any(|needle| lower.contains(needle));
            let opaque = text.len() >= 32
                && !text.chars().any(char::is_whitespace)
                && text
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.=+/".contains(c));
            if prose_secret || opaque {
                Value::String("[redacted]".into())
            } else {
                Value::String(text)
            }
        }
        other => other,
    }
}

pub(crate) fn occurrence_policy(
    trigger: &rightkit_agent_engine_storage::WorkflowTriggerRow,
) -> Result<OccurrencePolicy, WorkflowApiError> {
    let overlap_policy = match trigger.overlap_policy.as_str() {
        "allow" => OverlapPolicy::Allow,
        "queue" => OverlapPolicy::Queue,
        "forbid" => OverlapPolicy::Forbid,
        _ => return Err(WorkflowApiError::conflict("invalid stored overlap policy")),
    };
    let misfire_policy = match trigger.misfire_policy.as_str() {
        "skip" => MisfirePolicy::Skip,
        "coalesce" => MisfirePolicy::Coalesce,
        "bounded_catchup" => MisfirePolicy::BoundedCatchUp,
        _ => return Err(WorkflowApiError::conflict("invalid stored misfire policy")),
    };
    Ok(OccurrencePolicy {
        version_pin: trigger.version_pin.clone(),
        dedupe_strategy: DedupeStrategy::IdempotencyKey,
        misfire_policy,
        overlap_policy,
        max_concurrency: trigger.concurrency_cap.max(1) as u32,
    })
}

pub(crate) struct TriggerRunParts {
    pub(crate) snapshot: LifecycleState,
    pub(crate) command: Command,
    pub(crate) run: WorkflowRunRow,
    pub(crate) command_row: WorkflowCommandRow,
    pub(crate) journal_payload: String,
    pub(crate) journal_created_at: String,
    pub(crate) root_queue: WorkflowQueueRow,
}

pub(crate) fn trigger_run_admission(
    trigger: &rightkit_agent_engine_storage::WorkflowTriggerRow,
    version: &rightkit_agent_engine_storage::WorkflowVersionRow,
    occurrence: &WorkflowOccurrenceRow,
    event: &WorkflowTriggerEventRow,
    now_ms: i64,
) -> TriggerRunParts {
    let run_id = format!("wfr_{}", unique_suffix());
    let now = crate::workflow::triggers::format_unix_ms_as_iso(now_ms);
    let input = event.data_json.clone();
    let root_node = serde_json::from_str::<Value>(&version.compiled_plan_json)
        .ok()
        .and_then(|value| {
            value
                .get("entry_node")
                .or_else(|| value.get("entryNode"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "root".into());
    let snapshot = LifecycleState {
        workflow_id: workflow_id(trigger.workflow_id.clone()),
        version_id: workflow_version_id(version.id.clone()),
        revision: Revision(0),
        phase: LifecyclePhase::Pending,
    };
    let command_id = format!("wfc_{}", unique_suffix());
    let command = Command {
        command_id: command_id.clone(),
        kind: "start".into(),
        run_id: rightkit_workflow::control_plane::domain::run_id(&run_id),
        expected_revision: Revision(0),
        payload_json: input.clone(),
    };
    let run = WorkflowRunRow {
        id: run_id.clone(),
        workflow_id: trigger.workflow_id.clone(),
        workflow_version_id: version.id.clone(),
        parent_run_id: None,
        lifecycle_phase: "pending".into(),
        lifecycle_revision: 0,
        initiated_by_kind: "trigger".into(),
        initiated_by_ref: Some(occurrence.id.clone()),
        trigger_payload_json: Some(input.clone()),
        input_json: input.clone(),
        started_at: now.clone(),
        terminal_at: None,
        terminal_failure_class: None,
        runs_root_durable: true,
    };
    let command_row = WorkflowCommandRow {
        run_id: run_id.clone(),
        command_id,
        expected_revision: 0,
        command_kind: "start".into(),
        payload_json: input.clone(),
        created_at: now.clone(),
    };
    let root_queue = WorkflowQueueRow {
        run_id,
        node_id: root_node,
        available_at: now,
        priority: 0,
        claimed_by: None,
        claim_fence: None,
    };
    TriggerRunParts {
        snapshot,
        command,
        run,
        command_row,
        journal_payload: event.data_json.clone(),
        journal_created_at: crate::workflow::triggers::format_unix_ms_as_iso(now_ms),
        root_queue,
    }
}

/// Build the v1 axum router.
///
/// Paths are aligned with the OpenAPI schema in [`crate::api_workflows::openapi`]:
/// the schema documents `/workflows`, `/workflows/{workflow_id}`, `/runs`,
/// `/runs/{run_id}`, `/runs/{run_id}/cancel`, and `/openapi.json`. The
/// per-handler feature gate (see [`ensure_enabled`]) is what 423s when
/// `features.workflows_enabled = false`; the daemon-side mount decides
/// whether to even merge these routes.
pub fn router() -> Router<DaemonState> {
    let v1 = Router::new()
        .route("/workflows", get(list_workflows).post(create_workflow))
        .route("/workflows/{id}", get(get_workflow))
        .route("/workflows/{id}/versions", get(list_versions))
        .route(
            "/workflows/{id}/versions/{version_id}/validate",
            post(validate_version),
        )
        .route(
            "/workflows/{id}/versions/{version_id}/publish",
            post(publish_version),
        )
        .route("/workflows/{id}/runs", post(start_run))
        .route(
            "/workflows/{id}/triggers",
            get(list_triggers).post(create_trigger),
        )
        .route(
            "/workflow-triggers/{id}",
            get(get_trigger).patch(set_trigger).delete(delete_trigger),
        )
        .route(
            "/workflow-triggers/{id}/occurrences",
            get(list_occurrences).post(admit_occurrence),
        )
        .route("/occurrences/{id}", get(get_occurrence))
        .route("/approvals/{id}", get(get_approval))
        .route("/effects/{id}", get(get_effect))
        .route("/attempts/{id}", get(get_attempt))
        .route("/runs", get(list_runs))
        .route("/runs/{id}", get(get_run))
        .route("/runs/{id}/journal", get(get_journal))
        .route("/runs/{id}/node-states", get(get_node_states))
        .route("/runs/{id}/attempts", get(list_attempts))
        .route("/runs/{id}/evidence", get(list_evidence))
        .route("/runs/{id}/effects", get(list_effects))
        .route("/runs/{id}/cancel", post(cancel_run))
        .route("/runs/{id}/pause", post(pause_run))
        .route("/runs/{id}/resume", post(resume_run))
        .route("/runs/{id}/approve", post(approve_run))
        .route("/runs/{id}/events", get(stream_events))
        // Inbox keeps desktop discovery stable while supervisor-owned approval
        // projection is populated. It deliberately reveals no effect payload.
        .route("/workflow-inbox", get(list_inbox))
        .route("/workflow-inbox/{id}/decision", post(decide_inbox_item))
        .route("/workflow-imports", get(list_imports).post(submit_import))
        .route("/workflow-imports/{id}/review", post(review_import))
        .route(
            "/workflow-optimizations",
            get(list_optimizations).post(create_optimization),
        )
        .route(
            "/workflow-optimizations/{id}/promote",
            post(promote_optimization),
        )
        .route(
            "/workflow-optimizations/{id}/monitor",
            post(monitor_optimization),
        )
        .route(
            "/workflow-optimizations/{id}/rollback",
            post(rollback_optimization),
        )
        .route(
            "/workflow-interop/mcp/tasks",
            get(list_mcp_tasks).post(create_mcp_task),
        )
        .route(
            "/workflow-interop/mcp/tasks/{id}",
            get(get_mcp_task)
                .patch(update_mcp_task)
                .delete(cancel_mcp_task),
        )
        .route("/openapi.json", get(openapi_handler));
    Router::new().nest("/v1", v1.clone()).merge(v1)
}

// ─── Feature gate ────────────────────────────────────────────────────────────

/// Extract the workflow per-router state from the daemon's shared state.
///
/// Returns `WorkflowApiError::feature_locked()` when the daemon did not build
/// one (gate on but storage unavailable — the router mount in `api_router.rs`
/// skips the merge in that case, so this branch is only reachable if a future
/// caller mounts the router directly without state, in which case the
/// per-handler guard is the safety net).
fn workflow_state(state: &DaemonState) -> Result<&Arc<WorkflowApiState>, WorkflowApiError> {
    state
        .workflow_api_state
        .as_ref()
        .ok_or_else(WorkflowApiError::feature_locked)
}

/// Per-handler feature gate. Reads the workflow gate from the resolved
/// `WorkflowApiState`; the daemon-side mount in `api_router.rs` decides
/// whether to merge the router at all, so this guard is the
/// defence-in-depth fallback for direct mounts and for the gate-on-but-no-state
/// edge case.
fn ensure_enabled(state: &WorkflowApiState) -> Result<(), WorkflowApiError> {
    if state.feature.is_enabled() {
        Ok(())
    } else {
        Err(WorkflowApiError::feature_locked())
    }
}

async fn submit_import(
    State(state): State<DaemonState>,
    Json(req): Json<
        ImportRequest<rightkit_workflow::control_plane::import_export::WorkflowPackage>,
    >,
) -> Result<(axum::http::StatusCode, Json<Value>), WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    if req.raw_artifact_ref.trim().is_empty() {
        return Err(WorkflowApiError::bad_request("rawArtifactRef is required"));
    }
    let now = now_string();
    let id = format!("wfimp_{}", unique_suffix());
    let allowed = rightkit_workflow::control_plane::domain::CapabilitySet::empty();
    let verification = rightkit_workflow::control_plane::import_export::verify_import(
        &req.package,
        &allowed,
        wf.import_verifier.as_ref(),
    );
    let (signature_status, reason) = match verification {
        Ok(()) => (
            "verified",
            "verified package retained disabled pending review".to_string(),
        ),
        Err(error) => ("rejected", error.to_string()),
    };
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    repo.insert_import_quarantine(
        &rightkit_agent_engine_storage::WorkflowImportQuarantineRow {
            id: id.clone(),
            raw_artifact_ref: req.raw_artifact_ref,
            content_hash: req.package.manifest.content_hash.clone(),
            signature_status: signature_status.into(),
            manifest_json: serde_json::to_string(&req.package.manifest)
                .map_err(|e| WorkflowApiError::bad_request(e.to_string()))?,
            state: "disabled".into(),
            reviewer: None,
            reason: reason.clone(),
            created_at: now,
            reviewed_at: None,
        },
    )
    .map_err(WorkflowApiError::from)?;
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(
            json!({"id":id,"state":"disabled","signatureStatus":signature_status,"reason":reason}),
        ),
    ))
}

async fn list_imports(State(state): State<DaemonState>) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let rows = WorkflowRepository::new(storage.conn())
        .list_import_quarantine(200)
        .map_err(WorkflowApiError::from)?;
    Ok(Json(json!({"items":rows})))
}
async fn review_import(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(req): Json<ImportReviewRequest>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    if req.reviewer.trim().is_empty() {
        return Err(WorkflowApiError::bad_request("reviewer is required"));
    }
    let storage = wf.storage.lock().await;
    if !WorkflowRepository::new(storage.conn())
        .review_import_quarantine(&id, &req.reviewer, &req.reason, &now_string())
        .map_err(WorkflowApiError::from)?
    {
        return Err(WorkflowApiError::not_found(format!("import `{id}`")));
    }
    Ok(Json(json!({"id":id,"state":"disabled","reviewed":true})))
}
async fn list_optimizations(
    State(state): State<DaemonState>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let rows = WorkflowRepository::new(storage.conn())
        .list_promotion_receipts(200)
        .map_err(WorkflowApiError::from)?;
    Ok(Json(json!({"items":rows})))
}

async fn create_optimization(
    State(state): State<DaemonState>,
    Json(req): Json<CreateOptimizationRequest>,
) -> Result<(axum::http::StatusCode, Json<Value>), WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    for (name, value) in [
        ("workflowId", req.workflow_id.as_str()),
        ("baseVersionId", req.base_version_id.as_str()),
        ("proposedBy", req.proposed_by.as_str()),
        ("benchmarkId", req.benchmark_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(WorkflowApiError::bad_request(format!("{name} is required")));
        }
    }
    let now = now_string();
    let candidate_json = serde_json::to_string(&req.candidate_definition)
        .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?;
    let source = parse_authoring_json(&candidate_json)
        .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?;
    if source.definition.id.stable().as_str() != req.workflow_id {
        return Err(WorkflowApiError::conflict(
            "candidate definition workflow id does not match workflowId",
        ));
    }
    validate_source_authority(&source)
        .map_err(|error| WorkflowApiError::conflict(error.to_string()))?;
    let (candidate_hash, candidate_plan) = match compile(&source.definition, &source.nodes, &now) {
        CompilerOutcome::Compiled {
            content_hash, plan, ..
        } => (
            content_hash.as_str().to_string(),
            serde_json::to_string(&plan)
                .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?,
        ),
        CompilerOutcome::Failed { diagnostics } => {
            return Err(WorkflowApiError::bad_request(
                serde_json::to_string(&diagnostics)
                    .unwrap_or_else(|_| "candidate validation failed".into()),
            ));
        }
    };
    let canonical_source = serde_json::to_string(&source)
        .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let base = repo
        .find_version(&req.base_version_id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found("base workflow version"))?;
    if base.workflow_id != req.workflow_id || base.publication_state != "published" {
        return Err(WorkflowApiError::conflict(
            "baseVersionId must be a published version of workflowId",
        ));
    }
    let proposal = rightkit_adapt::ProposalBuilder::new(req.workflow_id.clone())
        .proposed_by(req.proposed_by)
        .change_kind(req.change_kind)
        .diff_summary(req.diff_summary)
        .evidence(req.evidence)
        .created_at(now.clone())
        .base_version(req.base_version_id, base.content_hash)
        .candidate_payload(canonical_source, candidate_plan)
        .build()
        .map_err(WorkflowApiError::bad_request)?;
    let status = match proposal.gate_outcome {
        rightkit_adapt::GateOutcome::Proceed { .. } => "proceed",
        rightkit_adapt::GateOutcome::Block { .. } => "blocked",
        rightkit_adapt::GateOutcome::HoldForReview { .. } => "hold",
    };
    let proposal_json = serde_json::to_string(&proposal)
        .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?;
    repo.insert_learning_proposal(
        &proposal.id,
        &proposal.workflow_id,
        &proposal.proposed_by,
        &proposal_json,
        status,
        &proposal.created_at,
    )
    .map_err(WorkflowApiError::from)?;
    repo.insert_learning_eval(
        &req.benchmark_id,
        &proposal.id,
        proposal.evidence.pass_rate_pct,
        proposal.evidence.regression_count as i64,
        proposal.evidence.total_runs as i64,
        &serde_json::to_string(&proposal.evidence).unwrap_or_default(),
        &proposal.created_at,
    )
    .map_err(WorkflowApiError::from)?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(
            json!({"id":proposal.id,"workflowId":proposal.workflow_id,"status":status,"contentHash":candidate_hash,"benchmarkId":req.benchmark_id}),
        ),
    ))
}

async fn promote_optimization(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PromoteOptimizationRequest>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("Bearer "))
        .ok_or_else(|| WorkflowApiError::bad_request("authenticated actor is required"))?;
    let actor_hash =
        rightkit_workflow::control_plane::domain::ContentHash::sha256(authorization.as_bytes());
    let actor = format!("local:{}", &actor_hash.as_str()[7..19]);
    if [
        req.benchmark_id.as_str(),
        req.rollback_id.as_str(),
        req.monitoring_id.as_str(),
    ]
    .iter()
    .any(|v| v.trim().is_empty())
    {
        return Err(WorkflowApiError::bad_request(
            "approver, benchmarkId, rollbackId, and monitoringId are required",
        ));
    }
    let now = now_string();
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let (workflow_id, proposal_status, proposal_json) = repo
        .find_learning_proposal(&id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!("learning proposal `{id}`")))?;
    let proposal: rightkit_adapt::WorkflowProposal = serde_json::from_str(&proposal_json)
        .map_err(|error| WorkflowApiError::conflict(format!("invalid proposal: {error}")))?;
    if proposal.id != id
        || proposal.workflow_id != workflow_id
        || proposal_status != "proceed"
        || !matches!(
            &proposal.gate_outcome,
            rightkit_adapt::GateOutcome::Proceed { .. }
        )
        || !matches!(
            rightkit_adapt::EvalGate::new().evaluate(&proposal.evidence),
            rightkit_adapt::GateOutcome::Proceed { .. }
        )
        || proposal.promotion.is_some()
    {
        return Err(WorkflowApiError::conflict(
            "proposal is not an unpromoted, evaluation-cleared workflow proposal",
        ));
    }
    let base = repo
        .find_version(&proposal.base_version_id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::conflict("proposal base version no longer exists"))?;
    if base.workflow_id != workflow_id
        || base.publication_state != "published"
        || base.content_hash != proposal.base_content_hash
    {
        return Err(WorkflowApiError::conflict(
            "proposal base version changed or is no longer published",
        ));
    }
    let candidate = parse_authoring_json(&proposal.candidate_definition_json).map_err(|error| {
        WorkflowApiError::conflict(format!("invalid candidate source: {error}"))
    })?;
    validate_source_authority(&candidate)
        .map_err(|error| WorkflowApiError::conflict(error.to_string()))?;
    let (candidate_hash, candidate_plan) =
        match compile(&candidate.definition, &candidate.nodes, &now) {
            CompilerOutcome::Compiled {
                content_hash, plan, ..
            } => (content_hash.as_str().to_string(), plan),
            CompilerOutcome::Failed { diagnostics } => {
                return Err(WorkflowApiError::conflict(
                    serde_json::to_string(&diagnostics)
                        .unwrap_or_else(|_| "candidate no longer compiles".into()),
                ));
            }
        };
    let stored_plan: Value = serde_json::from_str(&proposal.candidate_compiled_plan_json)
        .map_err(|error| WorkflowApiError::conflict(format!("invalid candidate plan: {error}")))?;
    if stored_plan
        != serde_json::to_value(&candidate_plan)
            .map_err(|error| WorkflowApiError::conflict(error.to_string()))?
    {
        return Err(WorkflowApiError::conflict(
            "candidate compiled plan does not match immutable proposal",
        ));
    }
    let activation_id = format!("wfv_{}", unique_suffix());
    let (pass_rate, regressions, total_runs, evidence_json) = repo
        .find_learning_eval(&req.benchmark_id, &id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found("proposal evaluation run"))?;
    serde_json::from_str::<Value>(&evidence_json).map_err(|error| {
        WorkflowApiError::conflict(format!("invalid evaluation evidence: {error}"))
    })?;
    if regressions != 0
        || total_runs <= 0
        || (pass_rate - proposal.evidence.pass_rate_pct).abs() > f64::EPSILON
        || total_runs as u32 != proposal.evidence.total_runs
    {
        return Err(WorkflowApiError::conflict(
            "evaluation receipt does not match proposal evidence",
        ));
    }
    proposal
        .reviewed_promotion(rightkit_adapt::PromotionRecord {
            approver: actor.clone(),
            benchmark_id: req.benchmark_id.clone(),
            activation_id: activation_id.clone(),
            rollback_id: req.rollback_id.clone(),
            monitoring_id: req.monitoring_id.clone(),
        })
        .map_err(WorkflowApiError::conflict)?;
    // Promotion materializes candidate as a fresh immutable published version;
    // callers cannot point at an arbitrary pre-published activation.
    if proposal.candidate_definition_json.trim().is_empty()
        || proposal.candidate_compiled_plan_json.trim().is_empty()
    {
        return Err(WorkflowApiError::conflict(
            "proposal has no immutable candidate workflow payload",
        ));
    }
    let next_number = repo
        .list_versions(&workflow_id)
        .map_err(WorkflowApiError::from)?
        .iter()
        .map(|v| v.version_number)
        .max()
        .unwrap_or(0)
        + 1;
    let rollback = repo
        .find_version(&req.rollback_id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found("rollback workflow version"))?;
    if rollback.workflow_id != workflow_id || rollback.publication_state != "published" {
        return Err(WorkflowApiError::conflict(
            "rollback must be a published version of proposal workflow",
        ));
    }
    WorkflowDefinitionWriter::new(storage.conn()).insert_version(&rightkit_agent_engine_storage::WorkflowVersionRow {
        id: activation_id.clone(), workflow_id: workflow_id.clone(),
        version_number: next_number,
        content_hash: candidate_hash,
        definition_json: proposal.candidate_definition_json.clone(),
        compiled_plan_json: proposal.candidate_compiled_plan_json.clone(),
        publication_state: "published".into(), created_at: now.clone(),
        published_at: Some(now.clone()), created_by: proposal.proposed_by.clone(),
        notes: serde_json::json!({"proposalId": id, "baseVersionId": proposal.base_version_id, "benchmarkId": req.benchmark_id}).to_string(),
    }).map_err(WorkflowApiError::from)?;
    let active = repo
        .find_version(&activation_id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found("activation workflow version"))?;
    if active.workflow_id != workflow_id
        || rollback.workflow_id != workflow_id
        || active.publication_state != "published"
        || rollback.publication_state != "published"
        || active.id == rollback.id
    {
        return Err(WorkflowApiError::conflict(
            "activation and rollback must be distinct immutable published versions of proposal workflow",
        ));
    }
    repo.insert_promotion_receipt(
        &rightkit_agent_engine_storage::WorkflowPromotionReceiptRow {
            proposal_id: id.clone(),
            approver: actor,
            benchmark_id: req.benchmark_id,
            activation_id,
            rollback_id: req.rollback_id,
            monitoring_id: req.monitoring_id,
            state: "active".into(),
            created_at: now.clone(),
            updated_at: now,
        },
    )
    .map_err(WorkflowApiError::from)?;
    Ok(Json(json!({"id":id,"state":"active"})))
}
async fn monitor_optimization(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(req): Json<MonitoringRequest>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let now = now_string();
    let storage = wf.storage.lock().await;
    WorkflowRepository::new(storage.conn())
        .record_monitoring(
            &rightkit_agent_engine_storage::WorkflowMonitoringReceiptRow {
                id: format!("wfmon_{}", unique_suffix()),
                proposal_id: id.clone(),
                monitoring_id: req.monitoring_id,
                passed: req.passed,
                reason: req.reason,
                created_at: now,
            },
        )
        .map_err(WorkflowApiError::from)?;
    Ok(Json(
        json!({"id":id,"state":if req.passed {"active"} else {"rolled_back"}}),
    ))
}
async fn rollback_optimization(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    if !WorkflowRepository::new(storage.conn())
        .rollback_promotion(&id, &now_string())
        .map_err(WorkflowApiError::from)?
    {
        return Err(WorkflowApiError::not_found(format!("optimization `{id}`")));
    }
    Ok(Json(json!({"id":id,"state":"rolled_back"})))
}

// ─── Workflows ───────────────────────────────────────────────────────────────

async fn list_workflows(State(state): State<DaemonState>) -> Result<Json<Value>, WorkflowApiError> {
    let wf_state = workflow_state(&state)?;
    ensure_enabled(wf_state)?;
    let storage = wf_state.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let rows: Vec<Value> = repo.list_definitions(200).map_err(WorkflowApiError::from)?.into_iter().map(|def| json!({"id":def.id,"name":def.name,"sourceKind":def.source_kind,"createdAt":def.created_at,"updatedAt":def.updated_at})).collect();
    Ok(Json(json!({
        "items": rows,
        "nextCursor": Value::Null,
    })))
}

async fn create_workflow(
    State(state): State<DaemonState>,
    Json(req): Json<CreateRequest>,
) -> Result<(axum::http::StatusCode, Json<Value>), WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    if req.name.trim().is_empty() {
        return Err(WorkflowApiError::bad_request("name is required"));
    }
    let now = now_string();
    let id = format!("wfd_{}", unique_suffix());
    let definition_source = serde_json::to_string(&req.definition)
        .map_err(|e| WorkflowApiError::bad_request(e.to_string()))?;
    let mut source = parse_authoring_json(&definition_source)
        .map_err(|e| WorkflowApiError::bad_request(e.to_string()))?;
    source.definition.id = workflow_id(id.clone());
    source.definition.name = req.name.clone();
    let definition =
        serde_json::to_string(&source).map_err(|e| WorkflowApiError::bad_request(e.to_string()))?;
    let (vid, hash, compiled_plan) = match compile(&source.definition, &source.nodes, &now) {
        CompilerOutcome::Compiled {
            version_id,
            content_hash,
            plan,
        } => (
            version_id.stable().as_str().to_string(),
            content_hash.as_str().to_string(),
            serde_json::to_string(&plan)
                .map_err(|e| WorkflowApiError::bad_request(e.to_string()))?,
        ),
        CompilerOutcome::Failed { diagnostics } => {
            return Err(WorkflowApiError::bad_request(
                serde_json::to_string(&diagnostics)
                    .unwrap_or_else(|_| "workflow validation failed".into()),
            ));
        }
    };
    let storage_handle = wf.storage.clone();
    let storage = storage_handle.lock().await;
    let writer = rightkit_agent_engine_storage::WorkflowDefinitionWriter::new(storage.conn());
    writer
        .upsert_definition(&rightkit_agent_engine_storage::WorkflowDefinitionRow {
            id: id.clone(),
            name: req.name,
            source_kind: "manual".into(),
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .map_err(WorkflowApiError::from)?;
    writer
        .insert_version(&rightkit_agent_engine_storage::WorkflowVersionRow {
            id: vid.clone(),
            workflow_id: id.clone(),
            version_number: 1,
            content_hash: hash.clone(),
            definition_json: definition,
            compiled_plan_json: compiled_plan,
            publication_state: "draft".into(),
            created_at: now.clone(),
            published_at: None,
            created_by: "api".into(),
            notes: json!({"validated":true,"validatedAt":now}).to_string(),
        })
        .map_err(WorkflowApiError::from)?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(
            json!({"id":id,"name":"created","versionId":vid,"contentHash":hash,"publicationState":"draft","createdAt":now}),
        ),
    ))
}

async fn get_workflow(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf_state = workflow_state(&state)?;
    ensure_enabled(wf_state)?;
    let storage = wf_state.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let row = repo.find_definition(&id).map_err(WorkflowApiError::from)?;
    match row {
        Some(def) => Ok(Json(json!({
            "id": def.id,
            "name": def.name,
            "sourceKind": def.source_kind,
            "createdAt": def.created_at,
            "updatedAt": def.updated_at,
        }))),
        None => Err(WorkflowApiError::not_found(format!("workflow `{id}`"))),
    }
}

async fn list_versions(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage_handle = wf.storage.clone();
    let storage = storage_handle.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_definition(&id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("workflow `{id}`")));
    }
    let items=repo.list_versions(&id).map_err(WorkflowApiError::from)?.into_iter().map(|v| json!({"id":v.id,"workflowId":v.workflow_id,"versionNumber":v.version_number,"contentHash":v.content_hash,"definitionJson":v.definition_json,"compiledPlanJson":v.compiled_plan_json,"publicationState":v.publication_state,"createdAt":v.created_at,"publishedAt":v.published_at,"createdBy":v.created_by,"notes":v.notes})).collect::<Vec<_>>();
    Ok(Json(Value::Array(items)))
}

async fn start_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(req): Json<StartRequest>,
) -> Result<(axum::http::StatusCode, Json<Value>), WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let version = if let Some(v) = req.version_id {
        repo.find_version(&v).map_err(WorkflowApiError::from)?
    } else {
        repo.list_versions(&id)
            .map_err(WorkflowApiError::from)?
            .into_iter()
            .find(|v| v.publication_state == "published")
    }
    .ok_or_else(|| WorkflowApiError::not_found("published workflow version"))?;
    if version.workflow_id != id {
        return Err(WorkflowApiError::not_found(
            "workflow version for requested workflow",
        ));
    }
    if version.publication_state != "published" {
        return Err(WorkflowApiError::conflict(
            "workflow version is not published",
        ));
    }
    let run_id = format!("wfr_{}", unique_suffix());
    let now = now_string();
    let input = serde_json::to_string(&req.input)
        .map_err(|e| WorkflowApiError::bad_request(e.to_string()))?;
    let writer = rightkit_agent_engine_storage::WorkflowJournalWriter::new(storage.conn());
    let event = rightkit_agent_engine_storage::JournalAppend {
        run_id: &run_id,
        event_kind: "run_started",
        node_id: None,
        payload_json: &input,
        created_at: &now,
    };
    let projection = rightkit_agent_engine_storage::ProjectionUpdate {
        run_id: &run_id,
        run_phase: Some("running"),
        ..Default::default()
    };
    let command_id = format!("wfc_{}", unique_suffix());
    let command = rightkit_agent_engine_storage::WorkflowCommandRow {
        run_id: run_id.clone(),
        command_id: command_id.clone(),
        expected_revision: 0,
        command_kind: "start".into(),
        payload_json: input.clone(),
        created_at: now.clone(),
    };
    let reducer_command = Command {
        command_id,
        kind: "start".into(),
        run_id: rightkit_workflow::control_plane::domain::run_id(&run_id),
        expected_revision: Revision(0),
        payload_json: input.clone(),
    };
    let snapshot = LifecycleState {
        workflow_id: workflow_id(id.clone()),
        version_id: workflow_version_id(version.id.clone()),
        revision: Revision(0),
        phase: LifecyclePhase::Pending,
    };
    let root_node = serde_json::from_str::<Value>(&version.compiled_plan_json)
        .ok()
        .and_then(|value| {
            value
                .get("entry_node")
                .or_else(|| value.get("entryNode"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "root".into());
    let run = rightkit_agent_engine_storage::WorkflowRunRow {
        id: run_id.clone(),
        workflow_id: id.clone(),
        workflow_version_id: version.id.clone(),
        parent_run_id: None,
        lifecycle_phase: "pending".into(),
        lifecycle_revision: 0,
        initiated_by_kind: "manual".into(),
        initiated_by_ref: None,
        trigger_payload_json: None,
        input_json: input.clone(),
        started_at: now.clone(),
        terminal_at: None,
        terminal_failure_class: None,
        runs_root_durable: true,
    };
    let root_queue = rightkit_agent_engine_storage::WorkflowQueueRow {
        run_id: run_id.clone(),
        node_id: root_node,
        available_at: now.clone(),
        priority: 0,
        claimed_by: None,
        claim_fence: None,
    };
    writer
        .admit_run_from_snapshot(
            &snapshot,
            &reducer_command,
            &run,
            &command,
            "{}",
            &event,
            &projection,
            &root_queue,
        )
        .map_err(WorkflowApiError::from)?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(
            json!({"id":run_id,"workflowId":id,"workflowVersionId":version.id,"lifecyclePhase":"running","lifecycleRevision":1,"startedAt":now}),
        ),
    ))
}

async fn validate_version(
    State(state): State<DaemonState>,
    Path((id, version_id)): Path<(String, String)>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let version = repo
        .find_version(&version_id)
        .map_err(WorkflowApiError::from)?
        .filter(|row| row.workflow_id == id)
        .ok_or_else(|| WorkflowApiError::not_found(format!("workflow version `{version_id}`")))?;
    let source = parse_json(&version.definition_json)
        .map_err(|e| WorkflowApiError::bad_request(e.to_string()))?;
    match compile(&source.definition, &source.nodes, &now_string()) {
        CompilerOutcome::Compiled {
            content_hash, plan, ..
        } => Ok(Json(
            json!({"workflowId":id,"versionId":version_id,"valid":true,"contentHash":content_hash.as_str(),"compiledPlan":plan,"diagnostics":[]}),
        )),
        CompilerOutcome::Failed { diagnostics } => Ok(Json(
            json!({"workflowId":id,"versionId":version_id,"valid":false,"diagnostics":diagnostics}),
        )),
    }
}

async fn publish_version(
    State(state): State<DaemonState>,
    Path((id, version_id)): Path<(String, String)>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let now = now_string();
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let version = repo
        .find_version(&version_id)
        .map_err(WorkflowApiError::from)?
        .filter(|row| row.workflow_id == id)
        .ok_or_else(|| {
            WorkflowApiError::not_found(format!("draft workflow version `{version_id}`"))
        })?;
    if version.publication_state != "draft" {
        return Err(WorkflowApiError::conflict(
            "only draft workflow versions can be published",
        ));
    }
    let source = parse_json(&version.definition_json)
        .map_err(|e| WorkflowApiError::bad_request(e.to_string()))?;
    validate_source_authority(&source).map_err(|e| WorkflowApiError::conflict(e.to_string()))?;
    match compile(&source.definition, &source.nodes, &now) {
        CompilerOutcome::Compiled { content_hash, .. }
            if content_hash.as_str() == version.content_hash => {}
        CompilerOutcome::Compiled { .. } => {
            return Err(WorkflowApiError::conflict(
                "validation receipt content hash is stale",
            ));
        }
        CompilerOutcome::Failed { diagnostics } => {
            return Err(WorkflowApiError::bad_request(
                serde_json::to_string(&diagnostics)
                    .unwrap_or_else(|_| "workflow validation failed".into()),
            ));
        }
    }
    if !serde_json::from_str::<Value>(&version.notes)
        .ok()
        .and_then(|value| value.get("validated").and_then(Value::as_bool))
        .unwrap_or(false)
    {
        return Err(WorkflowApiError::conflict(
            "workflow version has no validation receipt",
        ));
    }
    let writer = rightkit_agent_engine_storage::WorkflowDefinitionWriter::new(storage.conn());
    if !writer
        .publish_version(&id, &version_id, &now)
        .map_err(WorkflowApiError::from)?
    {
        return Err(WorkflowApiError::not_found(format!(
            "draft workflow version `{version_id}`"
        )));
    }
    Ok(Json(
        json!({"workflowId": id, "versionId": version_id, "publicationState": "published", "publishedAt": now}),
    ))
}

// ─── Runs ────────────────────────────────────────────────────────────────────

async fn list_runs(State(state): State<DaemonState>) -> Result<Json<Value>, WorkflowApiError> {
    let wf_state = workflow_state(&state)?;
    ensure_enabled(wf_state)?;
    let storage = wf_state.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let rows = repo.list_runs(200).map_err(WorkflowApiError::from)?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "workflowId": r.workflow_id,
                "workflowVersionId": r.workflow_version_id,
                "lifecyclePhase": r.lifecycle_phase,
                "lifecycleRevision": r.lifecycle_revision,
                "startedAt": r.started_at,
                "terminalAt": r.terminal_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "items": items,
        "nextCursor": Value::Null,
    })))
}

fn project_mcp_task(
    repo: &WorkflowRepository<'_>,
    run: &WorkflowRunRow,
) -> Result<Value, WorkflowApiError> {
    let started_at_unix_ms =
        crate::workflow::triggers::event::parse_iso_as_unix_ms(&run.started_at).unwrap_or_default();
    let last_updated_at_unix_ms = repo
        .list_journal(&run.id, 0, 1000)
        .map_err(WorkflowApiError::from)?
        .last()
        .and_then(|row| crate::workflow::triggers::event::parse_iso_as_unix_ms(&row.created_at))
        .unwrap_or(started_at_unix_ms);
    let terminal_at_unix_ms = run
        .terminal_at
        .as_deref()
        .and_then(crate::workflow::triggers::event::parse_iso_as_unix_ms);
    let name = repo
        .find_definition(&run.workflow_id)
        .map_err(WorkflowApiError::from)?
        .map(|definition| definition.name)
        .unwrap_or_else(|| "Workflow".into());
    let projection = rightkit_workflow::control_plane::interop::project_run_to_mcp(
        &rightkit_workflow::control_plane::interop::WorkflowRunRow {
            id: run.id.clone(),
            workflow_id: run.workflow_id.clone(),
            workflow_version_id: run.workflow_version_id.clone(),
            lifecycle_phase: run.lifecycle_phase.clone(),
            lifecycle_revision: run.lifecycle_revision,
            started_at_unix_ms,
            last_updated_at_unix_ms,
            terminal_at_unix_ms,
            title: format!("{name} · {}", run.id),
        },
    );
    Ok(json!({
        "taskId": projection.task_id,
        "status": projection.status.as_str(),
        "runId": projection.run_id,
        "title": projection.title,
        "createdAtUnixMs": projection.created_at_unix_ms,
        "lastUpdatedAtUnixMs": projection.last_updated_at_unix_ms,
    }))
}

async fn list_mcp_tasks(State(state): State<DaemonState>) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let items = repo
        .list_runs(200)
        .map_err(WorkflowApiError::from)?
        .iter()
        .map(|run| project_mcp_task(&repo, run))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({"items":items,"nextCursor":Value::Null})))
}

async fn get_mcp_task(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let run = repo
        .find_run(&id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!("MCP task `{id}`")))?;
    Ok(Json(project_mcp_task(&repo, &run)?))
}

/// Admit an MCP task through the same published-version admission path as
/// the first-party API. MCP never writes runs directly or bypasses reducer
/// policy, approvals, or effects.
async fn create_mcp_task(
    State(state): State<DaemonState>,
    Json(payload): Json<Value>,
) -> Result<(axum::http::StatusCode, Json<Value>), WorkflowApiError> {
    rightkit_workflow::control_plane::interop::validate_external_task_create_request(&payload)
        .map_err(|e| WorkflowApiError::bad_request(e.to_string()))?;
    let workflow_id = payload
        .get("workflowId")
        .or_else(|| payload.get("workflow_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| WorkflowApiError::bad_request("workflowId is required"))?
        .to_owned();
    let version_id = payload
        .get("versionId")
        .or_else(|| payload.get("version_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let input = payload.get("input").cloned().unwrap_or(Value::Null);
    start_run(
        State(state),
        Path(workflow_id),
        Json(StartRequest { version_id, input }),
    )
    .await
}

/// Update is intentionally a closed lifecycle command set. All mutations
/// flow through `mutate_run`, which journals and reduces the transition.
async fn update_mcp_task(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(req): Json<McpTaskUpdateRequest>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let (event, phase) = match req.action.as_str() {
        "cancel" => ("run_cancelled", "cancelled"),
        "pause" => ("run_paused", "suspended"),
        "resume" => ("run_resumed", "running"),
        _ => {
            return Err(WorkflowApiError::bad_request(
                "action must be one of cancel, pause, or resume",
            ));
        }
    };
    mutate_run(wf, &id, event, phase).await
}

async fn cancel_mcp_task(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    mutate_run(wf, &id, "run_cancelled", "cancelled").await
}

async fn get_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf_state = workflow_state(&state)?;
    ensure_enabled(wf_state)?;
    let storage = wf_state.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let row = repo.find_run(&id).map_err(WorkflowApiError::from)?;
    match row {
        Some(r) => Ok(Json(json!({
            "id": r.id,
            "workflowId": r.workflow_id,
            "workflowVersionId": r.workflow_version_id,
            "lifecyclePhase": r.lifecycle_phase,
            "lifecycleRevision": r.lifecycle_revision,
            "startedAt": r.started_at,
            "terminalAt": r.terminal_at,
            "terminalFailureClass": r.terminal_failure_class,
        }))),
        None => Err(WorkflowApiError::not_found(format!("run `{id}`"))),
    }
}

async fn cancel_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf_state = workflow_state(&state)?;
    ensure_enabled(wf_state)?;
    mutate_run(wf_state, &id, "run_cancelled", "cancelled").await
}

async fn pause_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    mutate_run(wf, &id, "run_paused", "suspended").await
}
async fn resume_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    mutate_run(wf, &id, "run_resumed", "running").await
}
async fn approve_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let _ = payload;
    mutate_run(wf, &id, "approval_decided", "running").await
}

async fn mutate_run(
    wf: &WorkflowApiState,
    id: &str,
    event_kind: &str,
    phase: &str,
) -> Result<Json<Value>, WorkflowApiError> {
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let run = repo
        .find_run(id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!("run `{id}`")))?;
    let now = now_string();
    let writer = rightkit_agent_engine_storage::WorkflowJournalWriter::new(storage.conn());
    let event = rightkit_agent_engine_storage::JournalAppend {
        run_id: id,
        event_kind,
        node_id: None,
        payload_json: "{}",
        created_at: &now,
    };
    let projection = rightkit_agent_engine_storage::ProjectionUpdate {
        run_id: id,
        run_phase: Some(phase),
        ..Default::default()
    };
    let reducer_kind = match event_kind {
        "run_cancelled" => "cancel",
        "run_paused" => "pause",
        "run_resumed" => "resume",
        _ => {
            return Err(WorkflowApiError::bad_request(
                "unsupported lifecycle command",
            ));
        }
    };
    let phase = match run.lifecycle_phase.as_str() {
        "pending" => LifecyclePhase::Pending,
        "running" => LifecyclePhase::Running,
        "suspended" => LifecyclePhase::Suspended,
        "cancelled" => LifecyclePhase::Cancelled,
        "succeeded" => LifecyclePhase::Succeeded,
        "failed" => LifecyclePhase::Failed,
        _ => LifecyclePhase::Degraded,
    };
    let command_id = format!("wfc_{}", unique_suffix());
    let command = rightkit_agent_engine_storage::WorkflowCommandRow {
        run_id: id.to_string(),
        command_id: command_id.clone(),
        expected_revision: run.lifecycle_revision,
        command_kind: reducer_kind.into(),
        payload_json: "{}".into(),
        created_at: now.clone(),
    };
    let reducer_command = Command {
        command_id,
        kind: reducer_kind.into(),
        run_id: run_id(id),
        expected_revision: Revision(run.lifecycle_revision as u64),
        payload_json: "{}".into(),
    };
    let snapshot = LifecycleState {
        workflow_id: workflow_id(run.workflow_id.clone()),
        version_id: workflow_version_id(run.workflow_version_id.clone()),
        revision: Revision(run.lifecycle_revision as u64),
        phase,
    };
    writer
        .apply_command_from_snapshot(
            &snapshot,
            &reducer_command,
            &command,
            "{}",
            &event,
            &projection,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .map_err(WorkflowApiError::from)?;
    Ok(Json(
        json!({"id":id,"workflowId":run.workflow_id,"workflowVersionId":run.workflow_version_id,"lifecyclePhase":phase,"lifecycleRevision":run.lifecycle_revision+1}),
    ))
}

fn unique_suffix() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_else(|_| "0".into())
}
fn now_string() -> String {
    rightkit_agent_engine_protocol::now_iso()
}
fn chrono_like_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

/// Journal rows retain original evidence. SSE is browser-visible, so redact
/// credential-shaped keys before replaying an event to a connected client.
fn redact_event_payload(raw: &str) -> String {
    fn redact(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    if [
                        "token",
                        "secret",
                        "password",
                        "authorization",
                        "auth",
                        "cookie",
                        "header",
                        "credential",
                        "url",
                        "api_key",
                        "apikey",
                    ]
                    .iter()
                    .any(|needle| key.to_ascii_lowercase().contains(needle))
                    {
                        *value = Value::String("[redacted]".into());
                    } else {
                        redact(value);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    redact(item);
                }
            }
            Value::String(text) => {
                let lower = text.to_ascii_lowercase();
                let prose_secret = [
                    "bearer ",
                    "authorization:",
                    "password=",
                    "token=",
                    "api_key=",
                    "cookie:",
                    "set-cookie:",
                    "http://",
                    "https://",
                ]
                .iter()
                .any(|needle| lower.contains(needle));
                let opaque = text.len() >= 32
                    && !text.chars().any(char::is_whitespace)
                    && text
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.=+/".contains(c));
                if prose_secret || opaque {
                    *text = "[redacted]".into();
                }
            }
            _ => {}
        }
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(mut value) => {
            redact(&mut value);
            value.to_string()
        }
        Err(_) => json!({"redacted": true, "kind": "non_json_journal_payload"}).to_string(),
    }
}

async fn get_journal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_run(&id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("run `{id}`")));
    }
    let items = repo
        .list_journal(&id, q.after_seq, q.limit.clamp(1, 1000))
        .map_err(WorkflowApiError::from)?;
    let next_cursor = items.last().map(|item| item.seq);
    let items = items.into_iter().map(|item| json!({
        "runId": item.run_id, "seq": item.seq, "revision": item.revision,
        "eventKind": item.event_kind, "nodeId": item.node_id,
        "payloadJson": redact_event_payload(&item.payload_json), "createdAt": item.created_at,
    })).collect::<Vec<_>>();
    Ok(Json(json!({"items":items,"nextCursor":next_cursor})))
}
async fn get_node_states(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_run(&id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("run `{id}`")));
    }
    Ok(Json(
        json!({"items":repo.list_node_states(&id).map_err(WorkflowApiError::from)?}),
    ))
}

async fn list_triggers(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_definition(&id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("workflow `{id}`")));
    }
    Ok(Json(
        json!({"items": repo.list_triggers(&id).map_err(WorkflowApiError::from)?}),
    ))
}

async fn create_trigger(
    State(state): State<DaemonState>,
    Path(workflow_id): Path<String>,
    Json(req): Json<TriggerRequest>,
) -> Result<(axum::http::StatusCode, Json<Value>), WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    if ![
        "manual",
        "one_time",
        "cron",
        "webhook",
        "github",
        "internal_event",
    ]
    .contains(&req.kind.as_str())
    {
        return Err(WorkflowApiError::bad_request("unknown trigger kind"));
    }
    if req.kind == "cron" && req.cron_expression.as_deref().is_none_or(str::is_empty) {
        return Err(WorkflowApiError::bad_request(
            "cron trigger requires cron_expression",
        ));
    }
    if req.kind != "cron" && req.cron_expression.is_some() {
        return Err(WorkflowApiError::bad_request(
            "cron_expression is only valid for cron triggers",
        ));
    }
    let now = now_string();
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_definition(&workflow_id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!(
            "workflow `{workflow_id}`"
        )));
    }
    let version_id = req.version_pin.as_deref().ok_or_else(|| {
        WorkflowApiError::bad_request("version_pin is required and must name a published version")
    })?;
    let version = repo
        .find_version(version_id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!("workflow version `{version_id}`")))?;
    if version.workflow_id != workflow_id || version.publication_state != "published" {
        return Err(WorkflowApiError::conflict(
            "version_pin must belong to this workflow and be published",
        ));
    }
    if !["allow", "queue", "forbid"].contains(&req.overlap_policy.as_str()) {
        return Err(WorkflowApiError::bad_request("unknown overlap_policy"));
    }
    if !["skip", "coalesce", "bounded_catchup"].contains(&req.misfire_policy.as_str()) {
        return Err(WorkflowApiError::bad_request("unknown misfire_policy"));
    }
    if req.concurrency_cap < 1 || req.concurrency_cap > 1024 {
        return Err(WorkflowApiError::bad_request(
            "concurrency_cap must be 1..=1024",
        ));
    }
    let dst_policy = match req.dst_policy.as_str() {
        "skip_gap" => DstResolutionPolicy::SkipGap,
        "earlier" => DstResolutionPolicy::Earlier,
        "later" => DstResolutionPolicy::Later,
        _ => return Err(WorkflowApiError::bad_request("unknown dst_policy")),
    };
    let next_fire_at_unix_ms = if req.kind == "cron" {
        let timezone = req.timezone.as_deref().ok_or_else(|| {
            WorkflowApiError::bad_request("cron trigger requires an IANA timezone")
        })?;
        let now_ms =
            rightkit_workflow::control_plane::clock::Clock::now_unix_ms(&SystemClock::new());
        crate::workflow::triggers::occurrence::resolve_cron_due(
            req.cron_expression
                .as_deref()
                .expect("validated cron expression"),
            timezone,
            now_ms,
            dst_policy,
        )
        .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?
    } else {
        if req.timezone.is_some() {
            return Err(WorkflowApiError::bad_request(
                "timezone is only valid for cron triggers",
            ));
        }
        None
    };
    let row = rightkit_agent_engine_storage::WorkflowTriggerRow {
        id: format!("wft_{}", unique_suffix()),
        workflow_id,
        kind: req.kind,
        enabled: req.enabled,
        cron_expression: req.cron_expression,
        timezone: req.timezone,
        version_pin: Some(version.id),
        dedupe_key: None,
        overlap_policy: req.overlap_policy,
        misfire_policy: req.misfire_policy,
        concurrency_cap: req.concurrency_cap,
        next_fire_at_unix_ms,
        revision: 1,
        created_at: now.clone(),
        updated_at: now,
    };
    repo.insert_trigger(&row).map_err(WorkflowApiError::from)?;
    Ok((axum::http::StatusCode::CREATED, Json(json!(row))))
}
async fn get_trigger(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    Ok(Json(json!(repo
        .find_trigger(&id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!(
            "trigger `{id}`"
        )))?)))
}
async fn set_trigger(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(req): Json<TriggerPatch>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let now = now_string();
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if !repo
        .set_trigger_enabled(&id, req.enabled, &now)
        .map_err(WorkflowApiError::from)?
    {
        return Err(WorkflowApiError::not_found(format!("trigger `{id}`")));
    }
    Ok(Json(json!({"id":id,"enabled":req.enabled,"updatedAt":now})))
}
async fn delete_trigger(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if !repo.delete_trigger(&id).map_err(WorkflowApiError::from)? {
        return Err(WorkflowApiError::not_found(format!("trigger `{id}`")));
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}
async fn list_occurrences(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_trigger(&id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("trigger `{id}`")));
    }
    Ok(Json(
        json!({"items":repo.list_occurrences(&id).map_err(WorkflowApiError::from)?}),
    ))
}
async fn admit_occurrence(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<(axum::http::StatusCode, Json<Value>), WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let trigger = repo
        .find_trigger(&id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!("trigger `{id}`")))?;
    if !trigger.enabled {
        return Err(WorkflowApiError::conflict("trigger is disabled"));
    }
    let version = if let Some(version_id) = trigger.version_pin.as_deref() {
        repo.find_version(version_id)
            .map_err(WorkflowApiError::from)?
    } else {
        repo.list_versions(&trigger.workflow_id)
            .map_err(WorkflowApiError::from)?
            .into_iter()
            .find(|version| version.publication_state == "published")
    }
    .filter(|version| {
        version.workflow_id == trigger.workflow_id && version.publication_state == "published"
    })
    .ok_or_else(|| WorkflowApiError::conflict("trigger has no published workflow version"))?;
    let clock = SystemClock::new();
    let now_ms = clock.now_unix_ms();
    let now = crate::workflow::triggers::format_unix_ms_as_iso(now_ms);
    let delivery_id = format!("manual-{}", unique_suffix());
    let source = "coderight:manual".to_string();
    let content_hash =
        rightkit_workflow::control_plane::domain::ContentHash::sha256(b"{\"manual\":true}");
    let event_id = crate::workflow::triggers::derive_event_id(&source, &delivery_id, now_ms);
    let dedupe_key = crate::workflow::triggers::derive_dedupe_key(
        &source,
        &delivery_id,
        &content_hash,
        Some(&trigger.id),
    );
    let event = WorkflowTriggerEventRow {
        id: event_id.to_string(),
        trigger_id: Some(trigger.id.clone()),
        source: source.clone(),
        event_type: "manual".into(),
        external_id: Some(delivery_id.clone()),
        dedupe_key: dedupe_key.clone(),
        content_hash: content_hash.to_string(),
        trust_json: json!({"level":"trusted","authenticated":true,"principal":"local-user","admissionPolicy":"manual-control-plane"}).to_string(),
        data_json: "{\"manual\":true}".into(),
        raw_artifact_id: None,
        occurred_at: now.clone(),
        received_at: now.clone(),
    };
    let normalized = crate::workflow::triggers::NormalizedTriggerEvent {
        spec_version: crate::workflow::triggers::NormalizedTriggerEvent::SPEC_VERSION.into(),
        id: event_id,
        trigger_id: Some(rightkit_workflow::control_plane::domain::trigger_id(
            trigger.id.clone(),
        )),
        external_id: Some(delivery_id),
        source,
        type_: "manual".into(),
        subject: None,
        time: now.clone(),
        received_at: now,
        data_content_type: "application/json".into(),
        data_schema: None,
        data: crate::workflow::triggers::NormalizedData::from_json(json!({"manual":true}))
            .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?,
        trust: crate::workflow::triggers::TrustInfo {
            level: crate::workflow::triggers::TrustLevel::Trusted,
            authenticated: true,
            origin: crate::workflow::triggers::TrustOrigin::SessionOrigin,
            principal: "local-user".into(),
            admission_policy: "manual-control-plane".into(),
            signature_algorithm: None,
            signature_key_id: None,
            actor_association: None,
            workspace_trust: None,
            findings: Vec::new(),
        },
        dedupe_key,
        content_hash,
        raw_artifact: None,
        correlation_id: None,
        causation_id: None,
        trace_parent: None,
        extensions: Default::default(),
    };
    let policy = occurrence_policy(&trigger)?;
    let decision = crate::workflow::triggers::OccurrenceAdmissionLayer::new(storage.conn())
        .evaluate(
            &crate::workflow::triggers::OccurrenceInputs {
                normalized_event: &normalized,
                event_row: &event,
                trigger_row: &trigger,
                policy: &policy,
            },
            &clock,
        )
        .map_err(|error| WorkflowApiError::bad_request(error.to_string()))?;
    let mut occurrence = decision.row().clone();
    if occurrence.selected_version_id.is_none() {
        occurrence.selected_version_id = Some(version.id.clone());
    }
    let run_parts = matches!(
        decision,
        crate::workflow::triggers::OccurrenceDecision::Admit(_)
            | crate::workflow::triggers::OccurrenceDecision::Coalesce(_)
    )
    .then(|| trigger_run_admission(&trigger, &version, &occurrence, &event, now_ms));
    let writer = WorkflowTriggerWriter::new(storage.conn());
    let outcome = if let Some(parts) = run_parts.as_ref() {
        let journal = JournalAppend {
            run_id: &parts.run.id,
            event_kind: "run_started",
            node_id: None,
            payload_json: &parts.journal_payload,
            created_at: &parts.journal_created_at,
        };
        let projection = ProjectionUpdate {
            run_id: &parts.run.id,
            run_phase: Some("running"),
            ..Default::default()
        };
        writer.admit_internal_event(
            &event,
            &occurrence,
            Some(&TriggerRunAdmission {
                snapshot: &parts.snapshot,
                reducer_command: &parts.command,
                run: &parts.run,
                command: &parts.command_row,
                outcome_json: "{}",
                event: &journal,
                projection: &projection,
                root_queue: &parts.root_queue,
            }),
        )
    } else {
        writer.admit_internal_event(&event, &occurrence, None)
    }
    .map_err(WorkflowApiError::from)?;
    if !outcome.duplicate {
        let _ = crate::workflow::continuations::match_event_waits(
            storage.conn(),
            &normalized.type_,
            normalized.correlation_id.as_deref().unwrap_or(""),
            &normalized.data.as_value(),
            now_ms,
        );
    }
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({"occurrence":occurrence,"runId":outcome.run_id,"duplicate":outcome.duplicate})),
    ))
}

pub(crate) async fn admit_manual_occurrence(
    state: DaemonState,
    trigger_id: String,
) -> Result<Value, WorkflowApiError> {
    let (_, Json(value)) = admit_occurrence(State(state), Path(trigger_id)).await?;
    Ok(value)
}
async fn get_occurrence(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    Ok(Json(json!(repo
        .find_occurrence(&id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!(
            "occurrence `{id}`"
        )))?)))
}

async fn get_approval(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    Ok(Json(json!(repo
        .find_approval(&id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!(
            "approval `{id}`"
        )))?)))
}
async fn get_effect(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    Ok(Json(json!(repo
        .find_effect(&id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!(
            "effect `{id}`"
        )))?)))
}
async fn get_attempt(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    Ok(Json(json!(repo
        .find_attempt(&id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!(
            "attempt `{id}`"
        )))?)))
}

async fn list_attempts(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_run(&id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("run `{id}`")));
    }
    Ok(Json(
        json!({"items": repo.list_attempts(&id).map_err(WorkflowApiError::from)?}),
    ))
}
async fn list_evidence(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_run(&id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("run `{id}`")));
    }
    Ok(Json(
        json!({"items": repo.list_evidence(&id).map_err(WorkflowApiError::from)?}),
    ))
}
async fn list_effects(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    if repo
        .find_run(&id)
        .map_err(WorkflowApiError::from)?
        .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("run `{id}`")));
    }
    Ok(Json(
        json!({"items": repo.list_effects(&id).map_err(WorkflowApiError::from)?}),
    ))
}

async fn list_inbox(State(state): State<DaemonState>) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let items = repo.list_open_attention(200).map_err(WorkflowApiError::from)?
        .into_iter().map(|item| json!({
            "id": item.id, "kind": item.kind, "state": item.state,
            "severity": item.severity, "title": item.title, "summary": item.summary,
            "runId": item.run_id, "nodeId": item.node_id, "effectId": item.effect_id,
            "approvalId": item.approval_id, "deepLink": serde_json::from_str::<Value>(&item.deep_link_json).unwrap_or(Value::Null),
            "createdAt": item.created_at, "updatedAt": item.updated_at,
        })).collect::<Vec<_>>();
    Ok(Json(json!({"items": items, "nextCursor": Value::Null})))
}

async fn decide_inbox_item(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let decision = payload.get("decision").and_then(Value::as_str);
    if !matches!(decision, Some("approved" | "denied")) {
        return Err(WorkflowApiError::bad_request(
            "decision must be approved or denied",
        ));
    }
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("Bearer "))
        .ok_or_else(|| WorkflowApiError::bad_request("authenticated actor is required"))?;
    let actor_hash =
        rightkit_workflow::control_plane::domain::ContentHash::sha256(authorization.as_bytes());
    let actor = format!("local:{}", &actor_hash.as_str()[7..19]);
    let storage = wf.storage.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let attention = repo
        .list_open_attention(200)
        .map_err(WorkflowApiError::from)?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| WorkflowApiError::not_found(format!("inbox item `{id}`")))?;
    let approval_id = attention.approval_id.ok_or_else(|| {
        WorkflowApiError::bad_request("inbox item does not require an approval decision")
    })?;
    let decision_state = if decision == Some("approved") {
        "approved"
    } else {
        "rejected"
    };
    let approval = repo
        .find_approval(&approval_id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::not_found(format!("approval `{approval_id}`")))?;
    let node = repo
        .find_node_state(&approval.run_id, &approval.node_id)
        .map_err(WorkflowApiError::from)?
        .ok_or_else(|| WorkflowApiError::conflict("approval node state is missing"))?;
    let generation = repo
        .list_attempts(&approval.run_id)
        .map_err(WorkflowApiError::from)?
        .into_iter()
        .filter(|attempt| attempt.node_id == approval.node_id)
        .max_by_key(|attempt| attempt.attempt)
        .map(|attempt| attempt.lease_fence)
        .unwrap_or(node.attempt);
    let phase = match node.phase.as_str() {
        "ready" => NodePhase::Ready,
        "claimed" => NodePhase::Claimed,
        "running" => NodePhase::Running,
        "waiting" => NodePhase::Waiting,
        "succeeded" => NodePhase::Succeeded,
        "skipped" => NodePhase::Skipped,
        "failed" => NodePhase::Failed,
        _ => NodePhase::Cancelled,
    };
    let now = now_string();
    let now_ms = chrono_like_now_ms();
    let command_id = format!("wdc_{}", unique_suffix());
    let command = rightkit_agent_engine_storage::WorkflowCommandRow {
        run_id: approval.run_id.clone(),
        command_id: command_id.clone(),
        expected_revision: node.last_reducer_revision,
        command_kind: "approval_decide".into(),
        payload_json: serde_json::json!({"approvalId": approval_id, "decision": decision_state})
            .to_string(),
        created_at: now.clone(),
    };
    let node_command = NodeCommand {
        command_id: command_id.clone(),
        kind: CommandKind::ApprovalDecide,
        node_id: approval.node_id.clone(),
        expected_revision: Revision(node.last_reducer_revision as u64),
        generation: generation as u64,
        approval_decision: Some(if decision_state == "approved" {
            ApprovalDecisionKind::Approved
        } else {
            ApprovalDecisionKind::Rejected
        }),
        effect_observation: None,
    };
    let snapshot = NodeLifecycleState {
        node_id: approval.node_id.clone(),
        revision: Revision(node.last_reducer_revision as u64),
        generation: generation as u64,
        phase,
    };
    let event = rightkit_agent_engine_storage::JournalAppend {
        run_id: &approval.run_id,
        event_kind: "approval_decided",
        node_id: Some(&approval.node_id),
        payload_json: &command.payload_json,
        created_at: &now,
    };
    let projection = rightkit_agent_engine_storage::ProjectionUpdate {
        run_id: &approval.run_id,
        node: Some((
            &approval.node_id,
            if decision_state == "approved" {
                "succeeded"
            } else {
                "failed"
            },
        )),
        node_attempt: Some(node.attempt),
        ..Default::default()
    };
    let binding = rightkit_agent_engine_storage::WorkflowApprovalDecision {
        approval_id: &approval_id,
        decision_id: &command_id,
        actor: &actor,
        role: &approval.role,
        state: decision_state,
        now_unix_ms: now_ms,
        version_id: &approval.version_id,
        inputs_hash: &approval.inputs_hash,
        outputs_hash: &approval.outputs_hash,
        artifact_refs_json: &approval.artifact_refs_json,
        capabilities_json: &approval.capabilities_json,
        effect_request_hash: &approval.effect_request_hash,
    };
    rightkit_agent_engine_storage::WorkflowJournalWriter::new(storage.conn())
        .apply_approval_decision(
            &snapshot,
            &node_command,
            &command,
            &event,
            &projection,
            &binding,
            None,
        )
        .map_err(WorkflowApiError::from)?;
    Ok(Json(
        json!({"id": id, "approvalId": approval_id, "decision": decision, "state": decision_state, "decidedAt": now}),
    ))
}
async fn stream_events(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, WorkflowApiError> {
    let wf = workflow_state(&state)?;
    ensure_enabled(wf)?;
    let storage_handle = wf.storage.clone();
    let storage = storage_handle.lock().await;
    let repo = WorkflowRepository::new(storage.conn());
    let after_seq = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let events = repo
        .list_journal(&id, after_seq, 1000)
        .map_err(WorkflowApiError::from)?;
    if events.is_empty()
        && repo
            .find_run(&id)
            .map_err(WorkflowApiError::from)?
            .is_none()
    {
        return Err(WorkflowApiError::not_found(format!("run {id}")));
    }
    drop(storage);
    let initial = events.into_iter().map(sse_event).collect::<VecDeque<_>>();
    let stream = stream::unfold(
        (
            storage_handle,
            id,
            after_seq,
            initial,
            tokio::time::interval(Duration::from_secs(1)),
        ),
        |(storage, run_id, mut cursor, mut pending, mut poll)| async move {
            loop {
                if let Some(event) = pending.pop_front() {
                    return Some((Ok(event), (storage, run_id, cursor, pending, poll)));
                }
                poll.tick().await;
                let guard = storage.lock().await;
                let repo = WorkflowRepository::new(guard.conn());
                let rows = repo.list_journal(&run_id, cursor, 1000).unwrap_or_default();
                drop(guard);
                if let Some(last) = rows.last() {
                    cursor = last.seq;
                }
                pending.extend(rows.into_iter().map(sse_event));
            }
        },
    );
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    ))
}

fn sse_event(row: rightkit_agent_engine_storage::WorkflowJournalRow) -> Event {
    Event::default().id(row.seq.to_string()).event(row.event_kind.clone()).data(json!({"runId": row.run_id, "seq": row.seq, "revision": row.revision, "eventKind": row.event_kind, "payload": serde_json::from_str::<Value>(&redact_event_payload(&row.payload_json)).unwrap_or(Value::Null)}).to_string())
}

// ─── OpenAPI ─────────────────────────────────────────────────────────────────

async fn openapi_handler() -> Json<Value> {
    Json(openapi_v1_schema())
}
