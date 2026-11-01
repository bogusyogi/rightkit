//! Glue between `coderight-providers` and the engine's `LlmClient` trait.
//!
//! `ProviderLlmClient` wraps `OpenAiCompatClient` and implements `LlmClient`
//! so the `SessionEngine` can talk to a real OpenAI-compatible provider.
//! It also holds the tool registry so tool definitions are sent with each request.
//! Large/plugin-contributed schemas are deferred until `tool_search` scopes them,
//! keeping the stable tools block small while failing closed for unscoped tools.

use crate::api::lock_recover;
use crate::api::EngineConfig;
use crate::observation::{provider_turn_evidence_observation, HostObservationProvenanceV1};
use async_trait::async_trait;
use futures_util::StreamExt;
use rightkit_agent_engine_core::llm::{
    LlmClient, LlmRequestMetadata, LlmResponse, LlmResponseStream, LlmStreamEvent,
};
use rightkit_agent_engine_protocol::TranscriptMessage;
use rightkit_agent_tools::coding_tools::registry::{
    tool_defs_for_messages_checked, PromotedToolState, ToolCatalogGenerationTracker,
    ToolPromotionEvent,
};
use rightkit_agent_tools::coding_tools::ToolRegistry;
use rightkit_llm::agent_providers::client::{
    ProviderRequestIdentityV1, RequestAuthorityBindingV1, RequestAuthorityErrorV1,
    RequestAuthorityInputV1, RequestAuthorityResponseV1, RequestAuthorityStateV1,
    RequestAuthorityV1,
};
use rightkit_llm::agent_providers::registry::ModelRegistry;
use rightkit_llm::agent_providers::registry_meta::RegistryMeta;
use rightkit_llm::agent_providers::subscription_auth::ModelAuth;
use rightkit_llm::agent_providers::{
    strip_think_tags, transcript_to_wire, AnthropicCompatClient, OpenAiCompatClient, ProviderError,
    ProviderRequestMetadata,
};
use rightkit_llm::agent_providers::{
    CapacityError, CapacityProfileV1, PreparedModalityV1, PreparedProviderRequestV1,
    PreparedRequestGateV1,
};
use rightkit_llm::agent_providers::{ExtensionProviderRegistry, ExtensionRegistrationPolicy};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// CRA-10 producer (semantic-contracts.md "Model-aware capacity & final
/// insertion"): builds a [`PreparedProviderRequestV1`] over the exact `wire`
/// bytes this request will serialize to, and admits it against `profile`
/// (from `ModelCapability::capacity_profile`) before dispatch. This is the
/// `provider_client.rs` half of the `HostObservedValueV1` producer named in
/// this lane's `h9-h10-host-observation` interface-handoff row: it measures
/// under the mandatory `o200k_base/1` Estimated basis (never provider-
/// reported, never `bytes/4`) and returns typed `CapacityUnavailable` /
/// `FinalInsertionOverBudget` instead of a silent send.
///
/// `final_image_tokens` is the caller-supplied CRA-10 image accounting
/// (`o200k_base` measurement of the projection built by
/// `rightkit_agent_engine_core::image_eviction::PreparedImageHistory::final_image_projection`
/// plus [`IMAGE_TOKEN_COST`] once per surviving inline image, already
/// summed by the caller) — pass `0` for text-only requests.
///
/// Real provider adapters now own final-body admission and physical
/// submission observations. This helper remains available to existing
/// image-projection callers that already hold exact final request bytes.
fn admit_provider_request_capacity(
    wire: &[serde_json::Value],
    route_fingerprint: impl Into<String>,
    model_id: impl Into<String>,
    serializer_generation: u64,
    tool_catalog_generation: Option<u64>,
    profile: CapacityProfileV1,
    final_image_tokens: u64,
) -> Result<(PreparedProviderRequestV1, u64), CapacityError> {
    let bytes = serde_json::to_vec(wire).unwrap_or_default();
    let modality = if final_image_tokens > 0 {
        PreparedModalityV1::TextPlusInlineImages
    } else {
        PreparedModalityV1::TextOnly
    };
    let prepared = PreparedProviderRequestV1::new(
        bytes,
        "application/json",
        route_fingerprint,
        model_id,
        serializer_generation,
        tool_catalog_generation,
        modality,
    );
    let gate = PreparedRequestGateV1::new(profile);
    // Base-wire measurement plus the already-summed image projection tokens,
    // per `effective_limit - reserve - measured_base_wire_tokens` with the
    // 765-token image provision included in `measured_base_wire_tokens`.
    let base_measured = gate.admit(&prepared)?;
    let total_measured = base_measured.saturating_add(final_image_tokens);
    let budget = gate
        .profile
        .window_tokens
        .saturating_sub(gate.profile.reserve_tokens);
    if total_measured > budget {
        return Err(CapacityError::FinalInsertionOverBudget {
            measured_tokens: total_measured,
            effective_budget_tokens: budget,
        });
    }
    Ok((prepared, total_measured))
}

/// Context already pulled for one session's current task. Later model
/// iterations of the same task reuse it, so the prompt prefix stays
/// byte-stable and the Membrane CLI is called once per task, not once per
/// iteration.
#[derive(Clone)]
struct PulledTaskContext {
    task_digest: String,
    /// Fully formed `<membrane_context>` envelope; `None` when Membrane had
    /// nothing or context is off.
    envelope: Option<String>,
}

/// Upper bound on one Membrane CLI context call. A turn never waits longer.
const CONTEXT_PULL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct RetainedContextRevisionObservationV1 {
    pub session_id: String,
    pub task_id: String,
    pub representation_handle: serde_json::Value,
    pub source_ref: String,
    pub source_digest: String,
    pub serialized_digest: String,
    pub context_epoch: u64,
}

pub(crate) type RetainedContextRevisionCallback =
    Arc<dyn Fn(Option<RetainedContextRevisionObservationV1>) + Send + Sync>;

/// Host-owned synchronous authority. It pulls task context through Pulse's
/// Membrane CLI (`coderight-membrane-cli`) and injects it into the final user
/// message. Pulse absent or any CLI failure means no context for the request,
/// never a failed turn.
#[derive(Clone)]
pub(crate) struct HostRequestAuthority {
    state: crate::api::DaemonState,
    pulled: Arc<Mutex<BTreeMap<String, PulledTaskContext>>>,
    /// authorization id -> sha256 of the exact context envelope serialized
    /// into that authorized request; joins H9/H10 evidence to delivered bytes.
    delivered: Arc<Mutex<BTreeMap<String, String>>>,
    #[allow(dead_code)]
    retention_callback: Arc<Mutex<Option<RetainedContextRevisionCallback>>>,
}

#[allow(dead_code)]
pub(crate) struct CompactionDecisionProvider {
    authority: Arc<HostRequestAuthority>,
    capacity: rightkit_llm::agent_providers::client::CapacityProfileV1,
    task_id: String,
    repository_id: String,
}

const COMPACTION_FEDERATION_MAX_TTL_MS: u64 = 30_000;

fn validate_compaction_federation_decision(
    decision: &rightkit_agent_engine_protocol::CompactionFederationDecisionV1,
    expected_identity: &str,
    expected_fence: &str,
    expected_scope: &str,
    expected_restart_epoch: u64,
    request_started: std::time::Instant,
    now: std::time::Instant,
) -> Result<(), String> {
    if decision.schema != rightkit_agent_engine_protocol::COMPACTION_FEDERATION_SCHEMA_V1
        || decision.decision != "approved"
        || decision.identity != expected_identity
        || decision.fence != expected_fence
        || decision.scope != expected_scope
        || decision.selection_receipt.selection_hash.trim().is_empty()
        || decision.expires_after_elapsed_ms == 0
        || decision.expires_after_elapsed_ms > COMPACTION_FEDERATION_MAX_TTL_MS
        || decision.restart_epoch == 0
        || decision.restart_epoch != expected_restart_epoch
    {
        return Err("Membrane compaction federation decision failed canonical identity/fence/scope/receipt validation".into());
    }

    let selected = serde_json::Value::Array(
        decision
            .selection_receipt
            .selected_item_ids
            .iter()
            .cloned()
            .map(serde_json::Value::String)
            .collect(),
    );
    let bytes = serde_json::to_vec(&selected).map_err(|error| error.to_string())?;
    let computed = format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(&bytes))
    );
    if computed != decision.selection_receipt.selection_hash {
        return Err("Membrane compaction federation selection receipt mismatch".into());
    }

    let elapsed_ms = now
        .checked_duration_since(request_started)
        .ok_or_else(|| "Membrane compaction federation request clock moved backwards".to_string())?
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    if elapsed_ms > decision.expires_after_elapsed_ms {
        return Err("Membrane compaction federation decision expired before return".into());
    }
    Ok(())
}

impl CompactionDecisionProvider {
    pub(crate) fn new(
        authority: Arc<HostRequestAuthority>,
        capacity: rightkit_llm::agent_providers::client::CapacityProfileV1,
        task_id: impl Into<String>,
        repository_id: impl Into<String>,
    ) -> Self {
        Self {
            authority,
            capacity,
            task_id: task_id.into(),
            repository_id: repository_id.into(),
        }
    }
}

#[async_trait::async_trait]
impl rightkit_agent_engine_core::CompactionDecisionProvider for CompactionDecisionProvider {
    async fn issue(
        &self,
        session_id: &str,
        snapshot: &rightkit_agent_engine_core::CompactionSnapshotV1,
    ) -> Result<
        (
            rightkit_agent_engine_protocol::CompactionFederationDecisionV1,
            std::time::Instant,
        ),
        String,
    > {
        // The Membrane CLI has no compaction-federation command yet
        // (Orthic-Labs/pulse#129), so the host issues the decision locally:
        // an approved, empty-selection receipt bound to this operation, the
        // repository scope and the binding revision. The same validation
        // that guarded the Membrane-issued decision still runs.
        let request_started = std::time::Instant::now();
        let op_id = format!("compact-{}", rightkit_agent_engine_protocol::uuid_v7());
        let restart_epoch = snapshot
            .binding_revision
            .ok_or_else(|| "compaction binding revision unavailable".to_owned())?;
        let selection_hash = {
            let empty = serde_json::to_vec(&serde_json::Value::Array(Vec::new()))
                .map_err(|error| error.to_string())?;
            format!(
                "sha256:{}",
                rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(&empty))
            )
        };
        let decision: rightkit_agent_engine_protocol::CompactionFederationDecisionV1 =
            serde_json::from_value(serde_json::json!({
                "$schema": rightkit_agent_engine_protocol::COMPACTION_FEDERATION_SCHEMA_V1,
                "identity": op_id,
                "fence": format!("{session_id}:{op_id}"),
                "scope": self.repository_id,
                "decision": "approved",
                "selectionReceipt": {
                    "selectedItemIds": [],
                    "selectionHash": selection_hash
                },
                "issuedMonotonicElapsedMs": 0,
                "expiresAfterElapsedMs": COMPACTION_FEDERATION_MAX_TTL_MS,
                "restartEpoch": restart_epoch
            }))
            .map_err(|error| error.to_string())?;
        let expected_identity = op_id;
        let expected_fence = format!("{session_id}:{expected_identity}");
        validate_compaction_federation_decision(
            &decision,
            &expected_identity,
            &expected_fence,
            &self.repository_id,
            restart_epoch,
            request_started,
            std::time::Instant::now(),
        )?;
        Ok((decision, request_started))
    }
}

/// The shared verified CLI backend with the context timeout, else `Unavailable`.
fn context_backend() -> Box<dyn rightkit_memory::membrane::MembraneBackend> {
    match rightkit_memory::backend::MembraneHandle::shared().verified_cli() {
        Some(cli) => Box::new(cli.with_timeout(CONTEXT_PULL_TIMEOUT)),
        None => Box::new(rightkit_memory::membrane::Unavailable),
    }
}

impl HostRequestAuthority {
    pub(crate) fn new(state: crate::api::DaemonState) -> Self {
        Self {
            state,
            pulled: Arc::new(Mutex::new(BTreeMap::new())),
            delivered: Arc::new(Mutex::new(BTreeMap::new())),
            retention_callback: Arc::new(Mutex::new(None)),
        }
    }

    fn direct_context(&self) -> bool {
        self.state.uses_direct_context()
    }

    pub(crate) fn set_retention_callback(&self, callback: RetainedContextRevisionCallback) {
        *lock_recover(&self.retention_callback, "retained context callback") = Some(callback);
    }

    /// Kept for host wiring; the MCP bridge no longer takes part in context.
    pub(crate) fn set_session_mcp(&self, _mcp: Arc<rightkit_mcp::agent_mcp::McpManager>) {}

    /// Make the reason a request carries no Membrane context visible in the
    /// session instead of only logging it.
    fn record_context_off(&self, identity: &ProviderRequestIdentityV1, reason: &str) {
        tracing::warn!(
            session_id = %identity.session_id,
            reason,
            "context off for this request: no context injected"
        );
        if let Some(live) = self.state.get_session(&identity.session_id) {
            let conduit = live.conduit.clone();
            let seq = conduit.next_seq();
            let now = rightkit_agent_engine_protocol::time::now_iso();
            let mut fields = std::collections::HashMap::new();
            fields.insert("reason".to_string(), reason.to_string());
            conduit.publish(
                rightkit_agent_engine_protocol::ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: identity.session_id.clone(),
                    seq,
                    created_at: now.clone(),
                    observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                        id: format!("context-empty-{}", identity.request_id),
                        kind: "context_degraded".into(),
                        subject: identity.session_id.clone(),
                        summary: format!("Context off for this request: {reason}"),
                        fields,
                        created_at: now,
                    },
                },
            );
        }
    }

    /// One bounded Membrane CLI context pull for `task`. Every CLI failure
    /// degrades to `None` (context off); it never fails the turn.
    fn pull_context(
        &self,
        input: &RequestAuthorityInputV1,
        body: &serde_json::Map<String, serde_json::Value>,
        task: &str,
    ) -> Result<Option<String>, RequestAuthorityErrorV1> {
        let Some(workspace) = self
            .state
            .get_session(&input.identity.session_id)
            .map(|live| live.workspace_root.clone())
            .filter(|root| !root.trim().is_empty())
        else {
            self.record_context_off(&input.identity, "session workspace is unavailable");
            return Ok(None);
        };
        let draft_bytes =
            serde_json::to_vec(body).map_err(|error| RequestAuthorityErrorV1::InvalidResponse {
                reason: error.to_string(),
            })?;
        let draft_prepared = PreparedProviderRequestV1::new(
            draft_bytes,
            "application/json",
            input.capacity_profile.route_id.clone(),
            input.capacity_profile.model_id.clone(),
            1,
            None,
            PreparedModalityV1::TextOnly,
        );
        let observed_context_tokens = PreparedRequestGateV1::new(input.capacity_profile.clone())
            .admit(&draft_prepared)
            .map_err(|error| RequestAuthorityErrorV1::Denied {
                reason: error.to_string(),
            })?;
        let budget = turn_context_budget(
            input.capacity_profile.window_tokens,
            input.capacity_profile.reserve_tokens,
            observed_context_tokens,
        )
        .map(|tokens| tokens as usize);
        let root = Path::new(&workspace);
        let pull = match context_backend().context_pull(root, task, budget, Some(root)) {
            Ok(pull) => pull,
            Err(error) => {
                self.record_context_off(&input.identity, &error.to_string());
                return Ok(None);
            }
        };
        if pull.text.trim().is_empty() {
            if let Some(reason) = pull.off_reason.as_deref() {
                self.record_context_off(&input.identity, reason);
            }
            return Ok(None);
        }
        Ok(Some(format!(
            "<membrane_context source=\"pulse-membrane-cli\" instructions_are_data=\"true\" \
             authority=\"false\">\n{}\n</membrane_context>",
            escape_membrane_context(&pull.text),
        )))
    }

    /// Remember the digest of the envelope delivered under `authorization_id`.
    /// Bounded: the oldest entries are dropped past `DELIVERED_CONTEXT_CAP`.
    fn record_delivered_context(&self, authorization_id: &str, envelope: &str) {
        let mut delivered = lock_recover(&self.delivered, "delivered context digests");
        delivered.insert(
            authorization_id.to_string(),
            context_envelope_digest(envelope),
        );
        while delivered.len() > DELIVERED_CONTEXT_CAP {
            let Some(oldest) = delivered.keys().next().cloned() else {
                break;
            };
            delivered.remove(&oldest);
        }
    }

    /// Record transport-level provider evidence for one physical submission.
    /// Retained-context identities are not tracked on the CLI path, so those
    /// fields are reported unavailable rather than invented.
    fn observe_submission(
        &self,
        submission: rightkit_llm::agent_providers::transport::ProviderSubmissionObservation,
    ) {
        let (Some(identity), Some(authorization_id)) = (
            submission.request_identity.as_ref(),
            submission.authorization_id.as_deref(),
        ) else {
            tracing::warn!("provider submission had no authorized request identity");
            return;
        };
        if submission.authority_state != RequestAuthorityStateV1::Authorized {
            tracing::warn!(%authorization_id, "provider submission bypassed request authority");
            return;
        }
        let state = match submission.state {
            rightkit_llm::agent_providers::transport::SubmissionState::Submitted => {
                rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1::Submitted
            }
            rightkit_llm::agent_providers::transport::SubmissionState::Rejected => {
                rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1::Rejected
            }
            rightkit_llm::agent_providers::transport::SubmissionState::SubmissionUnknown => {
                rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1::SubmissionUnknown
            }
        };
        let unavailable = match state {
            rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1::Rejected => {
                rightkit_logs::agent_telemetry::UnavailableReason::SubmissionRejected
            }
            rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1::SubmissionUnknown => {
                rightkit_logs::agent_telemetry::UnavailableReason::SubmissionUnknown
            }
            rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1::Unavailable => {
                rightkit_logs::agent_telemetry::UnavailableReason::SubmissionUnavailable
            }
            rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1::Submitted => {
                rightkit_logs::agent_telemetry::UnavailableReason::InputNotProvided
            }
        };
        let tool_catalog_generation = submission.prepared.tool_catalog_generation.map_or_else(
            || {
                rightkit_logs::agent_telemetry::evidence::ProviderEvidenceValueV1::unavailable(
                    rightkit_logs::agent_telemetry::UnavailableReason::InputNotProvided,
                )
            },
            rightkit_logs::agent_telemetry::evidence::ProviderEvidenceValueV1::complete,
        );
        let delivered_digest = lock_recover(&self.delivered, "delivered context digests")
            .get(authorization_id)
            .cloned();
        let (loaded_context_identities, retained_representation_digest) =
            context_evidence_values(state, delivered_digest, unavailable);
        let evidence = rightkit_logs::agent_telemetry::evidence::ProviderTurnEvidenceV1 {
            schema_version: rightkit_logs::agent_telemetry::evidence::PROVIDER_TURN_EVIDENCE_SCHEMA
                .to_string(),
            session_id: identity.session_id.clone(),
            task_id: identity.task_id.clone(),
            request_id: identity.request_id.clone(),
            generation: identity.generation,
            authorization_id: authorization_id.to_string(),
            prepared_digest: format!("sha256:{}", submission.prepared.body_digest),
            prepared_content_type: submission.prepared.content_type.to_string(),
            prepared_route: submission.prepared.route_fingerprint.clone(),
            prepared_model: submission.prepared.model_id.clone(),
            serializer_generation: submission.prepared.serializer_generation,
            tool_catalog_generation,
            modality: match submission.prepared.modality {
                PreparedModalityV1::TextOnly => "text_only".to_string(),
                PreparedModalityV1::TextPlusInlineImages => "text_plus_inline_images".to_string(),
            },
            measured_tokens: submission.measured_tokens,
            physical_attempt: submission.attempt,
            submission_state: state,
            loaded_context_identities,
            retained_representation_digest,
            provenance: rightkit_logs::agent_telemetry::ProvenanceV1::from_evidence(
                "provider_submission_boundary",
                vec![authorization_id.to_string()],
            ),
        };
        if let Err(error) = rightkit_logs::agent_telemetry::evidence::record_provider_turn_evidence(
            evidence.clone(),
        ) {
            tracing::warn!(?error, %authorization_id, "provider turn evidence rejected");
            return;
        }
        let _ = provider_turn_evidence_observation(
            format!(
                "provider-submission-{authorization_id}-{}",
                submission.attempt
            ),
            current_unix_ms(),
            evidence,
            HostObservationProvenanceV1 {
                receipt_id: authorization_id.to_string(),
                source: "coderight.provider.submission".to_string(),
                receipt_digest: format!("sha256:{}", submission.prepared.body_digest),
            },
        );
    }
}

/// Most delivered-context digests kept for evidence joins.
const DELIVERED_CONTEXT_CAP: usize = 256;

/// Content-addressed identity of one delivered context envelope.
fn context_envelope_digest(envelope: &str) -> String {
    format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(envelope.as_bytes())
    )
}

/// H9/H10 values for one submission. Complete only when the attempt was
/// actually submitted and a context envelope was delivered in its request;
/// every other case stays typed-unavailable with `unavailable`.
fn context_evidence_values(
    state: rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1,
    delivered_digest: Option<String>,
    unavailable: rightkit_logs::agent_telemetry::UnavailableReason,
) -> (
    rightkit_logs::agent_telemetry::evidence::ProviderEvidenceValueV1<Vec<String>>,
    rightkit_logs::agent_telemetry::evidence::ProviderEvidenceValueV1<String>,
) {
    use rightkit_logs::agent_telemetry::evidence::{
        ProviderEvidenceValueV1 as Value, ProviderSubmissionStateV1,
    };
    match (state, delivered_digest) {
        (ProviderSubmissionStateV1::Submitted, Some(digest)) => (
            Value::complete(vec![format!("membrane-cli:{digest}")]),
            Value::complete(digest),
        ),
        _ => (
            Value::unavailable(unavailable),
            Value::unavailable(unavailable),
        ),
    }
}

/// Default share of the model window one turn's context packet may use.
const CONTEXT_BUDGET_WINDOW_DIVISOR: u64 = 10;
const CONTEXT_BUDGET_DEFAULT_CAP_TOKENS: u64 = 12_000;
const CONTEXT_BUDGET_FLOOR_TOKENS: u64 = 1_000;

/// Explicit bounded per-turn context budget in tokens, or `None` when the
/// operator selected host-fit mode (`RIGHTKIT_CONTEXT_BUDGET_MODE=host_fit`).
/// `RIGHTKIT_CONTEXT_BUDGET_TOKENS` overrides the default (a tenth of the
/// window, capped). Never exceeds the remaining window room.
pub(crate) fn turn_context_budget(window: u64, reserve: u64, observed: u64) -> Option<u64> {
    let host_fit = std::env::var("RIGHTKIT_CONTEXT_BUDGET_MODE")
        .map(|mode| matches!(mode.trim(), "host_fit" | "host-fit"))
        .unwrap_or(false);
    if host_fit {
        return None;
    }
    let override_tokens = std::env::var("RIGHTKIT_CONTEXT_BUDGET_TOKENS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|tokens| *tokens > 0);
    Some(compute_context_budget(
        window,
        reserve,
        observed,
        override_tokens,
    ))
}

pub(crate) fn compute_context_budget(
    window: u64,
    reserve: u64,
    observed: u64,
    override_tokens: Option<u64>,
) -> u64 {
    let configured = override_tokens.unwrap_or_else(|| {
        (window / CONTEXT_BUDGET_WINDOW_DIVISOR).clamp(
            CONTEXT_BUDGET_FLOOR_TOKENS,
            CONTEXT_BUDGET_DEFAULT_CAP_TOKENS,
        )
    });
    let remaining = window.saturating_sub(reserve).saturating_sub(observed);
    configured.min(remaining).max(1)
}

impl RequestAuthorityV1 for HostRequestAuthority {
    fn authorize(
        &self,
        input: RequestAuthorityInputV1,
    ) -> Result<RequestAuthorityResponseV1, RequestAuthorityErrorV1> {
        if self.direct_context() {
            return Err(RequestAuthorityErrorV1::Unavailable {
                reason: "Membrane request authorization is disabled in direct context mode"
                    .to_string(),
            });
        }
        let Some(mut body) = input.draft_body.as_object().cloned() else {
            return Err(RequestAuthorityErrorV1::InvalidResponse {
                reason: "provider request draft must be an object".to_string(),
            });
        };
        if body.get("requestCompaction").is_some() {
            return Err(RequestAuthorityErrorV1::Unavailable {
                reason: "compaction federation is not available via Membrane CLI (Orthic-Labs/pulse#129)"
                    .to_string(),
            });
        }
        let task = draft_task_text(&body).unwrap_or_else(|| input.identity.task_id.clone());
        let task_digest = rightkit_fs::digest::sha256_bytes(task.as_bytes());
        let cached = lock_recover(&self.pulled, "pulled task context")
            .get(&input.identity.session_id)
            .filter(|pulled| pulled.task_digest == task_digest)
            .cloned();
        let envelope = match cached {
            Some(pulled) => pulled.envelope,
            None => {
                let envelope = self.pull_context(&input, &body, &task)?;
                lock_recover(&self.pulled, "pulled task context").insert(
                    input.identity.session_id.clone(),
                    PulledTaskContext {
                        task_digest,
                        envelope: envelope.clone(),
                    },
                );
                envelope
            }
        };
        let authorization_id = format!(
            "membrane-cli:{}:{}:{}",
            input.identity.session_id, input.identity.request_id, input.identity.generation
        );
        if let Some(envelope) = envelope.as_deref() {
            append_context_to_task_message(&mut body, envelope)?;
            self.record_delivered_context(&authorization_id, envelope);
        }
        Ok(RequestAuthorityResponseV1 {
            authorization_id,
            final_body: serde_json::Value::Object(body),
        })
    }
}

/// Append the context envelope to the latest user message that carries text
/// (the task instruction), keeping it inside the provider's existing
/// conversation slot. A draft with no such message carries no context.
fn append_context_to_task_message(
    body: &mut serde_json::Map<String, serde_json::Value>,
    envelope: &str,
) -> Result<(), RequestAuthorityErrorV1> {
    let (slot, messages) = provider_conversation_mut(body).ok_or_else(|| {
        RequestAuthorityErrorV1::InvalidResponse {
            reason: "provider draft carried no messages for context delivery".to_string(),
        }
    })?;
    let has_text = |message: &serde_json::Value| match message.get("content") {
        Some(serde_json::Value::String(text)) => !text.trim().is_empty(),
        Some(serde_json::Value::Array(parts)) => parts.iter().any(|part| {
            part.get("text")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| !text.trim().is_empty())
        }),
        _ => false,
    };
    if let Some(message) = messages
        .iter_mut()
        .rev()
        .find(|message| is_user_message(message) && has_text(message))
    {
        append_envelope_text(message, &format!("\n\n{envelope}"), slot == "input");
    }
    Ok(())
}

/// Append envelope text to one message's content: absent content becomes the
/// text itself, strings grow in place, arrays gain a text part, and any other
/// content shape wraps into an array so the original part survives.
fn append_envelope_text(message: &mut serde_json::Value, text: &str, responses_input: bool) {
    if message.get("content").is_none() {
        if let Some(object) = message.as_object_mut() {
            object.insert("content".to_string(), text.to_owned().into());
        }
        return;
    }
    match message.get_mut("content") {
        Some(serde_json::Value::String(content)) => content.push_str(text),
        Some(serde_json::Value::Array(parts)) => parts.push(serde_json::json!({
            "type": if responses_input { "input_text" } else { "text" },
            "text": text
        })),
        Some(content) => {
            let original = std::mem::replace(content, serde_json::Value::Null);
            *content = serde_json::Value::Array(vec![
                original,
                serde_json::json!({"type": if responses_input { "input_text" } else { "text" }, "text": text}),
            ]);
        }
        None => unreachable!("content presence was checked"),
    }
}

fn is_user_message(message: &serde_json::Value) -> bool {
    message.get("role").and_then(serde_json::Value::as_str) == Some("user")
        && !message
            .get("content")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|content| content.starts_with("<membrane_context_update "))
}

// Native Codex Responses drafts carry their conversation in `input`; other
// provider drafts use chat-completions `messages`. Context remains in the
// provider's existing conversation slot so transport serialization preserves it.
fn body_slot(body: &serde_json::Map<String, serde_json::Value>) -> Option<&'static str> {
    if body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .is_some()
    {
        Some("messages")
    } else if body
        .get("input")
        .and_then(serde_json::Value::as_array)
        .is_some()
    {
        Some("input")
    } else {
        None
    }
}

fn provider_conversation_mut(
    body: &mut serde_json::Map<String, serde_json::Value>,
) -> Option<(&'static str, &mut Vec<serde_json::Value>)> {
    let slot = body_slot(body)?;
    Some((slot, body.get_mut(slot)?.as_array_mut()?))
}

/// Block text is untrusted durable data: envelope-structural characters are
/// escaped so context bytes can never forge a closing tag or attribute.
fn escape_membrane_context(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The task this host is actually asking about: the latest user instruction
/// already inside the draft. Planner-facing identity only — never
/// synthesized when absent (callers fall back to the bound task identity).
fn draft_task_text(body: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    body.get(body_slot(body)?)?
        .as_array()?
        .iter()
        .rev()
        .filter(|message| message.get("role").and_then(serde_json::Value::as_str) == Some("user"))
        .find_map(|message| match message.get("content") {
            Some(serde_json::Value::String(text)) => Some(text.clone()),
            Some(serde_json::Value::Array(parts)) => {
                let text = parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty()).then_some(text)
            }
            _ => None,
        })
        .filter(|text| !text.trim().is_empty())
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRoute {
    pub api_key: String,
    pub model_id: String,
    pub base_url: String,
    pub effort: Option<String>,
    pub thinking_budget_tokens: Option<u32>,
    pub disable_thinking: bool,
    pub fallback_models: Vec<String>,
    pub approved_private: bool,
}

impl ProviderRoute {
    /// Adapt a resolved PL-001 credential into the route consumed by the
    /// dynamic provider client. OAuth flows expose bearer credentials through
    /// `ModelAuth::bearer_header`; api-key flows use `ModelAuth::api_key`.
    /// No other caller-supplied header is collapsed into a credential here.
    pub fn from_model_auth(
        model_id: impl Into<String>,
        default_base_url: impl Into<String>,
        auth: ModelAuth,
    ) -> Result<Self, String> {
        let ModelAuth {
            api_key,
            headers,
            base_url,
        } = auth;
        let api_key = match api_key {
            Some(api_key) if !api_key.trim().is_empty() => api_key,
            _ => headers
                .as_ref()
                .and_then(|headers| {
                    headers
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                        .and_then(|(_, value)| value.strip_prefix("Bearer "))
                })
                .filter(|token| !token.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    "resolved provider auth must contain an API key or Bearer authorization"
                        .to_string()
                })?,
        };
        let base_url = base_url.unwrap_or_else(|| default_base_url.into());
        if base_url.trim().is_empty() {
            return Err("resolved provider auth has no provider base URL".to_string());
        }
        Ok(Self {
            api_key,
            model_id: model_id.into(),
            base_url,
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models: Vec::new(),
            approved_private: false,
        })
    }
}

pub type SharedProviderRoute = Arc<Mutex<ProviderRoute>>;

/// Complete a small, provider-backed projection without exposing provider
/// construction to API handlers.
pub(crate) async fn complete_projection(
    config: &EngineConfig,
    messages: Vec<serde_json::Value>,
) -> Result<rightkit_llm::agent_providers::CompletedTurn, ProviderError> {
    OpenAiCompatClient::new(&config.api_key, &config.model_id, &config.base_url)
        .complete(messages)
        .await
}

/// Project enabled plugin provider contracts into existing provider registries.
/// Default registration policy preserves the extension's untrusted state: a
/// manifest cannot acquire core subscription trust merely by being discovered.
pub(crate) fn register_discovered_provider_extensions(
    plugin_dir: &Path,
    providers: &mut ExtensionProviderRegistry,
    routes: &mut ModelRegistry,
    metadata: &mut RegistryMeta,
) -> Result<usize, String> {
    let documents = crate::api_plugins::provider_extension_documents(plugin_dir)?;
    let count = documents.len();
    for document in documents {
        providers
            .register_json(
                &document,
                routes,
                metadata,
                ExtensionRegistrationPolicy::default(),
            )
            .map_err(|error| error.to_string())?;
    }
    Ok(count)
}

/// Bounded daemon-only transport for orchestration's narrow assist tasks.
///
/// This intentionally shares the session's checked provider route rather than
/// mobile runtime plumbing: mobile Foundation Models remains its own Swift
/// execution path. Assist calls send no tools, disable reasoning, apply a
/// fixed output ceiling, & return a typed result to orchestration callers.
pub struct DaemonSmallModelClient {
    route: SharedProviderRoute,
}

impl DaemonSmallModelClient {
    pub fn new(route: SharedProviderRoute) -> Self {
        Self { route }
    }
}

#[async_trait]
impl rightkit_agent_orchestration::coding::small_model_utility::SmallModelClient
    for DaemonSmallModelClient
{
    async fn run(
        &self,
        task: rightkit_agent_orchestration::coding::small_model_utility::SmallModelTask,
        input: &rightkit_agent_orchestration::coding::small_model_utility::SmallModelInput,
        budget: &rightkit_agent_orchestration::coding::small_model_utility::SmallModelBudget,
    ) -> Result<
        rightkit_agent_orchestration::coding::small_model_utility::SmallModelOutput,
        rightkit_agent_orchestration::coding::small_model_utility::SmallModelError,
    > {
        use rightkit_agent_orchestration::coding::small_model_utility::{
            parse_small_model_output, SmallModelError, DEFAULT_CALL_TOKEN_ESTIMATE,
        };

        if !budget.can_afford(DEFAULT_CALL_TOKEN_ESTIMATE) {
            return Err(SmallModelError::BudgetExceeded);
        }

        let route = lock_recover(&self.route, "small-model provider route").clone();
        // Keyless routes (`api-key-required = false`) are usable here:
        // `build_checked_client` admits them with no Authorization header.
        if route.base_url.trim().is_empty() || route.model_id.trim().is_empty() {
            return Err(SmallModelError::NoRouteAvailable);
        }

        let client = build_checked_client(&route)
            .await
            .map_err(|error| SmallModelError::ProviderError(error.to_string()))?
            .with_reasoning_disabled();
        let instruction = small_model_instruction(&task);
        let turn = client
            .complete_with_tools_metadata(
                vec![
                    serde_json::json!({"role": "system", "content": instruction}),
                    serde_json::json!({"role": "user", "content": input.content.as_str()}),
                ],
                Vec::new(),
                &ProviderRequestMetadata {
                    max_output_tokens: Some(DEFAULT_CALL_TOKEN_ESTIMATE),
                    ..ProviderRequestMetadata::default()
                },
            )
            .await
            .map_err(|error| SmallModelError::ProviderError(error.to_string()))?;

        if !turn.tool_calls.is_empty() {
            return Err(SmallModelError::ProviderError(
                "small-model assist call returned an unexpected tool call".to_string(),
            ));
        }
        let (text, _) = sanitize_provider_content(turn.text, turn.reasoning, false)
            .map_err(SmallModelError::ProviderError)?;
        Ok(parse_small_model_output(task, text))
    }
}

fn small_model_instruction(
    task: &rightkit_agent_orchestration::coding::small_model_utility::SmallModelTask,
) -> String {
    use rightkit_agent_orchestration::coding::small_model_utility::SmallModelTask;

    match task {
        SmallModelTask::Recap => {
            "Summarize supplied session content in concise plain language. Do not call tools or claim work not present in input.".to_string()
        }
        SmallModelTask::ProgressAudit => {
            "Audit supplied goal progress only. First line must be ON_TRACK or BLOCKED. Follow with concise summary, then each concrete blocker on its own '- ' line. Do not call tools or perform work.".to_string()
        }
        SmallModelTask::DiffNarration => {
            "Explain supplied unified-diff hunk in concise plain language. Do not call tools, propose unrelated changes, or claim verification.".to_string()
        }
        SmallModelTask::SessionFollowUp => {
            // Same instruction the mobile runtime uses for this task
            // (crates/mobile_runtime/src/small_model.rs), so both hosts drive
            // the shared enum with one contract.
            "Return exactly one concise, user-controlled next instruction based only on supplied recap and pinned project knowledge. Do not call tools, execute work, or invent facts.".to_string()
        }
        SmallModelTask::Classify { labels } => format!(
            "Return exactly one label from this list: {}. Do not call tools.",
            labels.join(", ")
        ),
    }
}

/// Reasoning settings for Anthropic-protocol clients, taken from the same
/// session fields as `ProviderRoute::{effort, thinking_budget_tokens,
/// disable_thinking}`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AnthropicReasoning {
    pub effort: Option<String>,
    pub thinking_budget_tokens: Option<u32>,
    pub disable_thinking: bool,
}

impl From<&ProviderRoute> for AnthropicReasoning {
    fn from(route: &ProviderRoute) -> Self {
        Self {
            effort: route.effort.clone(),
            thinking_budget_tokens: route.thinking_budget_tokens,
            disable_thinking: route.disable_thinking,
        }
    }
}

enum ProviderClientInner {
    Static(OpenAiCompatClient),
    Anthropic(AnthropicCompatClient),
    Direct {
        route: Arc<Mutex<crate::inference_runtime::ResolvedInferenceRoute>>,
        updates: tokio::sync::Mutex<
            tokio::sync::mpsc::Receiver<crate::inference_runtime::ResolvedInferenceRoute>,
        >,
        scheduler: crate::inference_runtime::InferenceScheduler,
        active_requests: Arc<AtomicUsize>,
    },
    Dynamic {
        route: SharedProviderRoute,
        /// Optional route-update admission. `None` preserves the legacy
        /// constructors' behavior; `Some` (installed via
        /// [`ProviderLlmClient::with_route_admission`]) synchronizes request
        /// dispatch against the session's `route_update_gate` /
        /// `inference_active_requests` so a PATCH route update cannot commit
        /// between this client's route capture and request completion.
        admission: Option<DynamicRouteAdmission>,
    },
}

/// Route-update admission shared with `LiveSession`: the updater holds
/// `gate` from its busy check through durable commit, so a request that
/// registers `active_requests` while briefly holding `gate` is always
/// observed by the updater's `inference_active_requests > 0` refusal.
#[derive(Clone)]
pub(crate) struct DynamicRouteAdmission {
    gate: Arc<tokio::sync::Mutex<()>>,
    active_requests: Arc<AtomicUsize>,
}

/// Acquire the route-update gate just long enough to mark one Dynamic
/// request in-flight, then release it. The returned guard keeps the counter
/// non-zero until dropped — hold it across the complete call or the full
/// lifetime of the returned stream (including error/drop/cancel), never the
/// async gate itself across network or stream work.
async fn acquire_dynamic_route_admission(
    admission: &Option<DynamicRouteAdmission>,
) -> Option<ActiveInferenceGuard> {
    let admission = admission.as_ref()?;
    let _gate = admission.gate.lock().await;
    Some(ActiveInferenceGuard::new(&admission.active_requests))
}

pub struct ProviderLlmClient {
    inner: ProviderClientInner,
    tools: Mutex<Arc<ToolRegistry>>,
    /// Installed once by daemon session composition. Each logical model turn
    /// derives its own binding from engine-owned request metadata.
    host_request_authority: Option<Arc<HostRequestAuthority>>,
    /// Session effort / thinking budget / disable flag applied to Direct
    /// Anthropic-protocol requests (the OpenAI path reads the same fields
    /// from `ProviderRoute`).
    anthropic_reasoning: Option<AnthropicReasoning>,
    compaction_policy_tx:
        Option<tokio::sync::watch::Sender<rightkit_agent_engine_core::CompactionModelPolicy>>,
    lineage_budget: Option<Arc<rightkit_agent_engine_core::LineageBudget>>,
    /// P0-C 2a: session-lifetime, monotonically-growing set of tools
    /// `tool_search` has scoped into visibility. Evaluated only at turn
    /// boundaries (once per `complete_with_metadata` call), never demoted.
    promoted_tools: Mutex<PromotedToolState>,
    /// P0-C 2c/2d: tracks the combined (system-prefix + tool-schema) cache
    /// prefix hash across turns so a change can be attributed to a reason
    /// and reported as a prefix-generation bump in telemetry.
    cache_prefix: Mutex<CachePrefixTracker>,
    /// P0-C 2d: total promotion events recorded this session, for telemetry.
    promotion_events_total: AtomicU64,
    /// HCTX-003: request-boundary generation identity for the owned tool
    /// schema vector. The tracker advances only after a future request
    /// observes a changed catalog; an in-flight request keeps its owned Vec.
    tool_catalog_tracker: Mutex<ToolCatalogGenerationTracker>,
    /// Last metadata snapshot passed to a real provider request.
    ///
    /// HCTX-003: convenience diagnostic ONLY. It is a single last-write-wins
    /// slot, so when requests overlap it names whichever request wrote last.
    /// Closure evidence must use `request_lineage_for`/`lineage_ledger`
    /// instead, which are request-scoped and unambiguous under concurrency.
    request_lineage: Mutex<Option<ProviderRequestMetadata>>,
    /// Monotonic per-client request ordinal; the request-scoped lineage key.
    request_ordinal: AtomicU64,
    /// Bounded request-scoped lineage ledger. Each entry is attributable to
    /// exactly one provider request even when requests overlap in flight.
    lineage_ledger: Mutex<std::collections::VecDeque<ProviderRequestLineage>>,
}

/// Lineage of ONE provider request, attributable without ambiguity even when
/// requests overlap. Unlike the `request_lineage` diagnostic slot, an entry
/// here is never overwritten by a later request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequestLineage {
    /// Request-scoped identity, unique per client.
    pub request_ordinal: u64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub tool_catalog_generation: u64,
    pub tool_catalog_digest: String,
    pub provider_boundary_generation: Option<u64>,
    pub context_fingerprint: Option<String>,
}

/// How many request lineages to retain. Bounded so a long session cannot grow
/// this without limit; large enough to cover any realistic overlap window.
const LINEAGE_LEDGER_CAPACITY: usize = 256;

impl ProviderLlmClient {
    /// Inject canonical host federation at client construction. Provider
    /// adapters receive request-local bindings only in Membrane mode, after
    /// metadata supplies exact session/task/request/generation identity.
    pub(crate) fn with_host_request_authority(
        mut self,
        authority: Arc<HostRequestAuthority>,
    ) -> Self {
        self.host_request_authority = Some(authority);
        self
    }

    /// Apply session effort / thinking settings to Anthropic-protocol requests.
    pub(crate) fn with_anthropic_reasoning(mut self, reasoning: AnthropicReasoning) -> Self {
        self.anthropic_reasoning = Some(reasoning);
        self
    }

    /// Install route-update admission on a `Dynamic` client. `gate` is the
    /// session's `route_update_gate` (held by the route updater across its
    /// busy check and durable commit) and `active_requests` is its
    /// `inference_active_requests` counter: each request acquires the gate,
    /// registers an [`ActiveInferenceGuard`], and releases the gate
    /// immediately, so a concurrent update either commits before the request
    /// registers or observes the non-zero counter and refuses. The guard is
    /// held across the whole complete call or the full lifetime of the
    /// returned stream. No-op for non-Dynamic inners, which already carry
    /// their own `active_requests` plumbing.
    pub(crate) fn with_route_admission(
        mut self,
        gate: Arc<tokio::sync::Mutex<()>>,
        active_requests: Arc<AtomicUsize>,
    ) -> Self {
        if let ProviderClientInner::Dynamic { admission, .. } = &mut self.inner {
            *admission = Some(DynamicRouteAdmission {
                gate,
                active_requests,
            });
        }
        self
    }

    fn request_authority_binding(
        &self,
        metadata: &LlmRequestMetadata,
    ) -> Result<Option<RequestAuthorityBindingV1>, String> {
        let Some(authority) = self.host_request_authority.as_ref() else {
            return Ok(None);
        };
        if authority.direct_context() {
            return Ok(None);
        }
        // Identity-less requests (the compaction summarizer) carry no task to
        // pull Membrane context for; they go out unbound instead of failing.
        let Some(identity) = metadata.request_identity.as_ref() else {
            return Ok(None);
        };
        if identity.session_id.trim().is_empty()
            || identity.task_id.trim().is_empty()
            || identity.request_id.trim().is_empty()
        {
            return Err(
                ProviderError::Authority(RequestAuthorityErrorV1::Unavailable {
                    reason: "provider request metadata identity is incomplete".to_string(),
                })
                .to_string(),
            );
        }
        Ok(Some(RequestAuthorityBindingV1::new(
            ProviderRequestIdentityV1 {
                session_id: identity.session_id.clone(),
                task_id: identity.task_id.clone(),
                request_id: identity.request_id.clone(),
                generation: identity.generation,
            },
            authority.clone(),
        )))
    }

    fn submission_observer(
        &self,
    ) -> Option<rightkit_llm::agent_providers::transport::SubmissionObserver> {
        self.host_request_authority
            .as_ref()
            .filter(|authority| !authority.direct_context())
            .map(|authority| {
                let authority = authority.clone();
                Arc::new(move |submission| authority.observe_submission(submission))
                    as rightkit_llm::agent_providers::transport::SubmissionObserver
            })
    }

    fn configure_openai_client(
        &self,
        client: OpenAiCompatClient,
        metadata: &LlmRequestMetadata,
    ) -> Result<OpenAiCompatClient, String> {
        let mut client = client;
        if let Some(binding) = self.request_authority_binding(metadata)? {
            client = client.with_request_authority(binding);
        } else if self
            .host_request_authority
            .as_ref()
            .is_some_and(|authority| authority.direct_context())
        {
            client = client.with_direct_dispatch();
        }
        if let Some(observer) = self.submission_observer() {
            client = client.with_submission_observer(observer);
        }
        Ok(client)
    }

    fn configure_anthropic_client(
        &self,
        client: AnthropicCompatClient,
        metadata: &LlmRequestMetadata,
    ) -> Result<AnthropicCompatClient, String> {
        let mut client = client;
        if let Some(binding) = self.request_authority_binding(metadata)? {
            client = client.with_request_authority(binding);
        } else if self
            .host_request_authority
            .as_ref()
            .is_some_and(|authority| authority.direct_context())
        {
            client = client.with_direct_dispatch();
        }
        if let Some(observer) = self.submission_observer() {
            client = client.with_submission_observer(observer);
        }
        if let Some(reasoning) = &self.anthropic_reasoning {
            if let Some(effort) = &reasoning.effort {
                client = client.with_effort(effort.clone());
            }
            if let Some(tokens) = reasoning.thinking_budget_tokens {
                client = client.with_thinking_budget_tokens(tokens);
            }
            if reasoning.disable_thinking {
                client = client.with_thinking_disabled();
            }
        }
        Ok(client)
    }
    /// Provider/model attribution for prompt-overhead telemetry
    /// (master-change 5.8). Static boot-time routes carry no live model id
    /// through this seam; dynamic/Direct routes do.
    fn telemetry_attribution(&self) -> (Option<String>, Option<String>) {
        match &self.inner {
            ProviderClientInner::Static(_) | ProviderClientInner::Anthropic(_) => (None, None),
            ProviderClientInner::Direct { route, .. } => {
                let route = lock_recover(route, "inference route");
                (
                    Some(route.selection.connection_id.clone()),
                    Some(route.selection.model_id.clone()),
                )
            }
            ProviderClientInner::Dynamic { route, .. } => {
                let route = lock_recover(route, "provider route");
                (None, Some(route.model_id.clone()))
            }
        }
    }

    /// Record the fixed per-request prompt overhead (master-change 5.8):
    /// base system prompt bytes + serialized tool prelude bytes, attributed
    /// to the live route. The token figure is an explicit conservative
    /// estimate, never a billing number.
    fn record_prompt_overhead_metric(
        &self,
        messages: &[TranscriptMessage],
        tool_defs: &[serde_json::Value],
    ) {
        let system_prompt_bytes: usize = messages
            .iter()
            .filter_map(|message| match message {
                TranscriptMessage::System { content, .. } => Some(content.len()),
                _ => None,
            })
            .sum();
        let tool_prelude_bytes = serde_json::to_string(tool_defs)
            .map(|encoded| encoded.len())
            .unwrap_or_default();
        let (provider, model) = self.telemetry_attribution();
        rightkit_logs::agent_telemetry::record_prompt_overhead(
            rightkit_logs::agent_telemetry::PromptOverheadEvent {
                provider,
                model,
                system_prompt_bytes,
                tool_prelude_bytes,
            },
        );
    }

    /// Emit one C-2 observation for every schema that was actually placed in
    /// this provider request.  Counts come from the visible transcript rather
    /// than guessed session totals; pending or externalized results stay
    /// explicitly unavailable.
    fn record_tool_schema_observations(
        &self,
        messages: &[TranscriptMessage],
        tool_defs: &[serde_json::Value],
        promoted_names: &std::collections::HashSet<String>,
    ) {
        let visible_turn_ids: Vec<String> = messages
            .iter()
            .filter(|message| transcript_to_wire(message).is_some())
            .map(|message| message.id().to_string())
            .collect();
        let transcript_counts = transcript_tool_counts(messages);
        for definition in tool_defs {
            let Some(name) = definition["function"]["name"].as_str() else {
                continue;
            };
            let Ok(schema_bytes) = serde_json::to_vec(definition) else {
                continue;
            };
            let counts =
                transcript_counts
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| TranscriptToolCounts {
                        // No call for this schema is a complete zero observation
                        // over the visible transcript window.
                        results_complete: true,
                        bytes_complete: true,
                        ..Default::default()
                    });
            let results_complete = counts.results_complete;
            let bytes_complete = counts.bytes_complete;
            let pending_reason = rightkit_logs::agent_telemetry::UnavailableReason::PendingResult;
            let (success_count, failure_count) = if results_complete {
                (
                    rightkit_logs::agent_telemetry::CountObservationV1::observed(counts.successes),
                    rightkit_logs::agent_telemetry::CountObservationV1::observed(counts.failures),
                )
            } else {
                (
                    rightkit_logs::agent_telemetry::CountObservationV1::unavailable(pending_reason),
                    rightkit_logs::agent_telemetry::CountObservationV1::unavailable(pending_reason),
                )
            };
            let result_bytes_total = if bytes_complete {
                counts
                    .result_bytes
                    .map(rightkit_logs::agent_telemetry::CountObservationV1::observed)
                    .unwrap_or_else(|| {
                        rightkit_logs::agent_telemetry::CountObservationV1::unavailable(
                            rightkit_logs::agent_telemetry::UnavailableReason::NotInstrumented,
                        )
                    })
            } else {
                rightkit_logs::agent_telemetry::CountObservationV1::unavailable(pending_reason)
            };
            rightkit_logs::agent_telemetry::record_tool_schema_observation(
                rightkit_logs::agent_telemetry::ToolSchemaObservationV1::from_schema_bytes(
                    name,
                    name,
                    &schema_bytes,
                    !promoted_names.contains(name),
                    visible_turn_ids.clone(),
                    rightkit_logs::agent_telemetry::CountObservationV1::observed(
                        counts.invocations,
                    ),
                    success_count,
                    failure_count,
                    result_bytes_total,
                    Vec::new(),
                    rightkit_logs::agent_telemetry::ProvenanceV1::from_evidence(
                        "provider_request_transcript",
                        visible_turn_ids.clone(),
                    ),
                ),
            );
        }
    }

    /// P0-C 2d: record the extended cache/promotion telemetry for one
    /// request — core (always-on) vs. promoted (deferred, scoped) schema
    /// bytes, the promoted count, cumulative promotion events this session,
    /// and the cache-prefix generation with an attributable reason on
    /// change. Kept as a separate sink from `PromptOverheadEvent` (see
    /// `rightkit_logs::agent_telemetry::prompt_overhead::CachePrefixEvent` for why).
    #[allow(clippy::too_many_arguments)]
    fn record_cache_prefix_metric(
        &self,
        tool_defs: &[serde_json::Value],
        promoted_names: &std::collections::HashSet<String>,
        promotion_events_this_turn: &[ToolPromotionEvent],
        prefix_observation: &PrefixObservation,
    ) {
        let core_bytes: usize = tool_defs
            .iter()
            .filter(|def| {
                def["function"]["name"]
                    .as_str()
                    .map(|name| !promoted_names.contains(name))
                    .unwrap_or(true)
            })
            .filter_map(|def| serde_json::to_vec(def).ok().map(|v| v.len()))
            .sum();
        let promoted_bytes: usize = tool_defs
            .iter()
            .filter(|def| {
                def["function"]["name"]
                    .as_str()
                    .map(|name| promoted_names.contains(name))
                    .unwrap_or(false)
            })
            .filter_map(|def| serde_json::to_vec(def).ok().map(|v| v.len()))
            .sum();
        if !promotion_events_this_turn.is_empty() {
            self.promotion_events_total
                .fetch_add(promotion_events_this_turn.len() as u64, Ordering::AcqRel);
        }
        let (provider, model) = self.telemetry_attribution();
        rightkit_logs::agent_telemetry::prompt_overhead::record_cache_prefix_event(
            rightkit_logs::agent_telemetry::prompt_overhead::CachePrefixEvent {
                provider,
                model,
                core_bytes,
                promoted_bytes,
                promoted_count: promoted_names.len(),
                promotion_events_this_session: self.promotion_events_total.load(Ordering::Acquire),
                prefix_generation: prefix_observation.generation,
                prefix_changed: prefix_observation.changed,
                prefix_change_reason: prefix_observation.reason.clone(),
            },
        );
    }

    /// CR-34: the turn-boundary bookkeeping shared by `complete_with_metadata`
    /// and `stream_with_metadata` — accumulate newly-scoped tool promotions,
    /// build the canonical tool schema block, fold it into the stable prefix
    /// key, and record prompt-overhead / cache-prefix telemetry. Before this
    /// lane, streaming skipped all of it (it only accumulated promotions);
    /// now both callers go through the identical path.
    fn prepare_turn_metadata(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> Result<
        (
            Vec<serde_json::Value>,
            ProviderRequestMetadata,
            ProviderRequestLineage,
        ),
        String,
    > {
        // P0-C 2a: turn boundary — accumulate any newly tool_search-scoped
        // tools into the session's monotonic promoted set. Never demotes.
        let promotion_events_this_turn = {
            let mut promoted = lock_recover(&self.promoted_tools, "promoted tool state");
            promoted.accumulate(messages)
        };
        let promoted_snapshot = {
            let promoted = lock_recover(&self.promoted_tools, "promoted tool state");
            promoted.promoted_names().clone()
        };

        // P0-C 2b/2c: canonically-ordered, ceiling-checked tool schema block.
        let tool_registry = lock_recover(&self.tools, "tool registry").clone();
        let tool_defs = {
            let promoted = lock_recover(&self.promoted_tools, "promoted tool state");
            tool_defs_for_messages_checked(tool_registry.as_ref(), &promoted)
        }
        .map_err(|error| error.to_string())?;
        let catalog_snapshot = {
            let mut tracker = lock_recover(&self.tool_catalog_tracker, "tool catalog tracker");
            tracker
                .observe(&tool_defs)
                .map_err(|error| error.to_string())?
        };
        self.record_tool_schema_observations(messages, &tool_defs, &promoted_snapshot);
        self.record_prompt_overhead_metric(messages, &tool_defs);

        // P0-C 2c: fold tool_defs into this request's stable_prefix_key
        // (telemetry attribution + OpenAI's official prompt_cache_key hint —
        // see `combined_stable_prefix_key`'s doc comment for exactly what
        // this does and does not cover on the wire).
        let combined_prefix_key =
            combined_stable_prefix_key(metadata.stable_prefix_key.as_deref(), &tool_defs);
        let prefix_observation = if let Some(combined) = combined_prefix_key.as_deref() {
            let mut tracker = lock_recover(&self.cache_prefix, "cache prefix tracker");
            Some(tracker.observe(
                combined,
                metadata.stable_prefix_key.as_deref(),
                &promotion_events_this_turn,
            ))
        } else {
            None
        };
        if let Some(observation) = &prefix_observation {
            self.record_cache_prefix_metric(
                &tool_defs,
                &promoted_snapshot,
                &promotion_events_this_turn,
                observation,
            );
        }

        let catalog_snapshot_generation = catalog_snapshot.generation;
        let catalog_snapshot_digest = catalog_snapshot.digest_hex.clone();
        let request_metadata = ProviderRequestMetadata {
            stable_prefix_key: combined_prefix_key.or_else(|| metadata.stable_prefix_key.clone()),
            max_output_tokens: metadata.max_output_tokens,
            provider_boundary_generation: metadata.provider_boundary_generation,
            context_fingerprint: metadata.context_fingerprint.clone(),
            tool_catalog_generation: Some(catalog_snapshot.generation),
            tool_catalog_digest: Some(catalog_snapshot.digest_hex),
            retry_notifier: metadata.retry_notifier.clone(),
        };
        *lock_recover(&self.request_lineage, "provider request lineage") =
            Some(request_metadata.clone());

        // HCTX-003: record request-scoped lineage. This is the attributable
        // record; the slot above stays only as a convenience diagnostic.
        let (provider, model) = self.telemetry_attribution();
        let lineage = ProviderRequestLineage {
            request_ordinal: self.request_ordinal.fetch_add(1, Ordering::Relaxed),
            provider,
            model,
            tool_catalog_generation: catalog_snapshot_generation,
            tool_catalog_digest: catalog_snapshot_digest,
            provider_boundary_generation: request_metadata.provider_boundary_generation,
            context_fingerprint: request_metadata.context_fingerprint.clone(),
        };
        {
            let mut ledger = lock_recover(&self.lineage_ledger, "provider lineage ledger");
            if ledger.len() == LINEAGE_LEDGER_CAPACITY {
                ledger.pop_front();
            }
            ledger.push_back(lineage.clone());
        }
        Ok((tool_defs, request_metadata, lineage))
    }

    /// Convenience diagnostic. Ambiguous when requests overlap - do not use it
    /// as closure evidence; use `request_lineage_for` instead.
    pub(crate) fn request_lineage_snapshot(&self) -> Option<ProviderRequestMetadata> {
        lock_recover(&self.request_lineage, "provider request lineage").clone()
    }

    /// Request-scoped lineage for one specific provider request.
    pub(crate) fn request_lineage_for(&self, ordinal: u64) -> Option<ProviderRequestLineage> {
        lock_recover(&self.lineage_ledger, "provider lineage ledger")
            .iter()
            .find(|entry| entry.request_ordinal == ordinal)
            .cloned()
    }

    /// All retained request-scoped lineages, oldest first.
    pub(crate) fn lineage_ledger(&self) -> Vec<ProviderRequestLineage> {
        lock_recover(&self.lineage_ledger, "provider lineage ledger")
            .iter()
            .cloned()
            .collect()
    }

    pub fn new(inner: OpenAiCompatClient, tools: Arc<ToolRegistry>) -> Self {
        Self {
            inner: ProviderClientInner::Static(inner),
            tools: Mutex::new(tools),
            host_request_authority: None,
            anthropic_reasoning: None,
            compaction_policy_tx: None,
            lineage_budget: None,
            promoted_tools: Mutex::new(PromotedToolState::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    /// Direct Anthropic-compatible routes use their own wire protocol while
    /// returning the same engine-facing completed turn as OpenAI-compatible routes.
    pub fn new_anthropic(inner: AnthropicCompatClient, tools: Arc<ToolRegistry>) -> Self {
        Self {
            inner: ProviderClientInner::Anthropic(inner),
            tools: Mutex::new(tools),
            host_request_authority: None,
            anthropic_reasoning: None,
            compaction_policy_tx: None,
            lineage_budget: None,
            promoted_tools: Mutex::new(PromotedToolState::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    pub fn new_direct(
        route: crate::inference_runtime::ResolvedInferenceRoute,
        scheduler: crate::inference_runtime::InferenceScheduler,
        tools: Arc<ToolRegistry>,
        updates: tokio::sync::mpsc::Receiver<crate::inference_runtime::ResolvedInferenceRoute>,
        active_requests: Arc<AtomicUsize>,
    ) -> Result<Self, String> {
        if !matches!(
            route.selection.authority,
            crate::inference_runtime::InferenceAuthority::Direct
        ) || route.direct_connection.is_none()
        {
            return Err("direct route is not qualified".into());
        }
        Ok(Self {
            inner: ProviderClientInner::Direct {
                route: Arc::new(Mutex::new(route)),
                updates: tokio::sync::Mutex::new(updates),
                scheduler,
                active_requests,
            },
            tools: Mutex::new(tools),
            host_request_authority: None,
            anthropic_reasoning: None,
            compaction_policy_tx: None,
            lineage_budget: None,
            promoted_tools: Mutex::new(PromotedToolState::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
        })
    }

    pub fn new_dynamic(route: SharedProviderRoute, tools: Arc<ToolRegistry>) -> Self {
        Self {
            inner: ProviderClientInner::Dynamic {
                route,
                admission: None,
            },
            tools: Mutex::new(tools),
            host_request_authority: None,
            anthropic_reasoning: None,
            compaction_policy_tx: None,
            lineage_budget: None,
            promoted_tools: Mutex::new(PromotedToolState::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    #[cfg(test)]
    pub fn new_dynamic_with_compaction_policy(
        route: SharedProviderRoute,
        tools: Arc<ToolRegistry>,
        compaction_policy_tx: tokio::sync::watch::Sender<
            rightkit_agent_engine_core::CompactionModelPolicy,
        >,
    ) -> Self {
        Self {
            inner: ProviderClientInner::Dynamic {
                route,
                admission: None,
            },
            tools: Mutex::new(tools),
            host_request_authority: None,
            anthropic_reasoning: None,
            compaction_policy_tx: Some(compaction_policy_tx),
            lineage_budget: None,
            promoted_tools: Mutex::new(PromotedToolState::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    pub fn new_dynamic_with_compaction_policy_and_lineage_budget(
        route: SharedProviderRoute,
        tools: Arc<ToolRegistry>,
        compaction_policy_tx: tokio::sync::watch::Sender<
            rightkit_agent_engine_core::CompactionModelPolicy,
        >,
        lineage_budget: Arc<rightkit_agent_engine_core::LineageBudget>,
    ) -> Self {
        Self {
            inner: ProviderClientInner::Dynamic {
                route,
                admission: None,
            },
            tools: Mutex::new(tools),
            host_request_authority: None,
            anthropic_reasoning: None,
            compaction_policy_tx: Some(compaction_policy_tx),
            lineage_budget: Some(lineage_budget),
            promoted_tools: Mutex::new(PromotedToolState::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    fn publish_active_compaction_policy(&self, model_id: &str) {
        if let Some(compaction_policy_tx) = &self.compaction_policy_tx {
            let _ = compaction_policy_tx.send(compaction_policy_for_model(model_id));
        }
    }
}

struct ActiveInferenceGuard(Arc<AtomicUsize>);

impl ActiveInferenceGuard {
    fn new(counter: &Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter.clone())
    }
}

impl Drop for ActiveInferenceGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// P0-C 2c: fold the canonically-ordered tool-schema bytes into the
/// engine-supplied stable prefix key (built from pinned system messages +
/// day, `pinned_system_prefix_as_stable`), and use the combined key as
/// `ProviderRequestMetadata.stable_prefix_key` for this request.
///
/// Scope, stated precisely after reconciling against
/// repos/cli/fx/src/core/gateway/gateway_json.zig (2c's reference — see
/// `findCacheBreakpoint` / `shouldCacheMessage`): fx's real cache-control
/// breakpoints apply to the `messages` array only — `tools_json` is written
/// verbatim, unmarked, never hashed into anything. This function does NOT
/// touch that mechanism, and does NOT touch `rightkit_agent_engine_core::cache::
/// CacheTracker` / `StableCacheKey` (`engine_loop.rs`) either — those still
/// observe only pinned system messages + day, exactly as before this lane.
/// What this combined key actually reaches: (a) telemetry attribution here
/// and in `CachePrefixTracker`, and (b) OpenAI's official `prompt_cache_key`
/// field (`client.rs`'s `is_official_openai_api()` gate) — a routing hint on
/// OpenAI's first-party endpoint, not a substitute for byte-identical-prefix
/// caching. For OpenRouter/Anthropic-style providers, the actual mechanism
/// that keeps the wire-level `tools` block cache-stable across turns is 2b's
/// canonical ordering plus 2a's monotonic promotion (deterministic,
/// append-only serialization) — not this key. `None` in ⇒ `None` out: no
/// pinned system prefix means no stable identity to key on yet.
fn combined_stable_prefix_key(
    base: Option<&str>,
    tool_defs: &[serde_json::Value],
) -> Option<String> {
    let base = base?;
    let tool_bytes = serde_json::to_vec(tool_defs).unwrap_or_default();
    let tool_hash = rightkit_agent_engine_core::cache::fnv1a(&String::from_utf8_lossy(&tool_bytes));
    Some(format!("{base}:tools:{tool_hash:016x}"))
}

#[derive(Clone, Default)]
struct TranscriptToolCounts {
    invocations: u64,
    successes: u64,
    failures: u64,
    result_bytes: Option<u64>,
    results_complete: bool,
    bytes_complete: bool,
}

/// Derive only facts present in transcript rows. A tool result without a
/// retrievable `OutputRef` is not silently treated as a zero-byte result.
fn transcript_tool_counts(
    messages: &[TranscriptMessage],
) -> BTreeMap<String, TranscriptToolCounts> {
    let mut calls = BTreeMap::<String, String>::new();
    for message in messages {
        if let TranscriptMessage::Assistant { tool_calls, .. } = message {
            for call in tool_calls {
                calls
                    .entry(call.id.clone())
                    .or_insert_with(|| call.name.clone());
            }
        }
    }

    let mut counts = BTreeMap::<String, TranscriptToolCounts>::new();
    for name in calls.values() {
        let row = counts.entry(name.clone()).or_default();
        row.invocations = row.invocations.saturating_add(1);
    }
    let mut completed = std::collections::HashSet::new();
    let mut bytes_by_tool = BTreeMap::<String, u64>::new();
    let mut bytes_complete = BTreeMap::<String, bool>::new();
    for message in messages {
        let TranscriptMessage::Tool {
            tool_call_id,
            is_error,
            output,
            ..
        } = message
        else {
            continue;
        };
        let Some(tool_name) = calls.get(tool_call_id) else {
            continue;
        };
        if !completed.insert(tool_call_id.clone()) {
            continue;
        }
        let row = counts.entry(tool_name.clone()).or_default();
        if *is_error {
            row.failures = row.failures.saturating_add(1);
        } else {
            row.successes = row.successes.saturating_add(1);
        }
        let Some(output) = output else {
            bytes_complete.insert(tool_name.clone(), false);
            continue;
        };
        let Some(size) = u64::try_from(output.size_bytes).ok() else {
            bytes_complete.insert(tool_name.clone(), false);
            continue;
        };
        let Some(total) = bytes_by_tool
            .entry(tool_name.clone())
            .or_insert(0)
            .checked_add(size)
        else {
            bytes_complete.insert(tool_name.clone(), false);
            continue;
        };
        bytes_by_tool.insert(tool_name.clone(), total);
    }

    for (name, row) in &mut counts {
        let completed_count = row.successes.saturating_add(row.failures);
        row.results_complete = completed_count == row.invocations;
        row.bytes_complete =
            row.results_complete && bytes_complete.get(name).copied().unwrap_or(true);
        if row.bytes_complete {
            row.result_bytes = Some(bytes_by_tool.get(name).copied().unwrap_or(0));
        }
    }
    counts
}

/// P0-C 2c/2d: one observation of the combined cache-prefix hash.
#[derive(Debug, Clone, Default)]
struct PrefixObservation {
    generation: u64,
    changed: bool,
    reason: Option<String>,
}

/// P0-C 2c/2d: tracks the combined stable-prefix hash across turns so a
/// change (a cache break, from the provider's perspective) can be attributed
/// to a reason and reported as a monotonically-increasing generation number.
/// One tracker per `ProviderLlmClient` — i.e. per session's live route.
#[derive(Debug, Default)]
struct CachePrefixTracker {
    last_hash: Option<u64>,
    last_base_key: Option<String>,
    generation: u64,
}

impl CachePrefixTracker {
    /// Observe one turn's combined key. `promotion_events_this_turn` — when
    /// non-empty — is treated as the authoritative reason for any change
    /// (tool-schema churn dominates over incidental system-prompt drift in
    /// the same turn, since it's the specific thing this lane makes
    /// visible). Otherwise a changed `base_key` (day rollover or a pinned
    /// system message edit) is attributed to that; any other change is
    /// reported as unattributed drift rather than silently dropped.
    fn observe(
        &mut self,
        combined_key: &str,
        base_key: Option<&str>,
        promotion_events_this_turn: &[ToolPromotionEvent],
    ) -> PrefixObservation {
        let hash = rightkit_agent_engine_core::cache::fnv1a(combined_key);
        let changed = self.last_hash != Some(hash);
        if changed {
            self.generation += 1;
        }
        let reason = if !changed {
            None
        } else if !promotion_events_this_turn.is_empty() {
            let names = promotion_events_this_turn
                .iter()
                .map(|event| event.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(",");
            Some(format!("tool-promotion:{names}"))
        } else if self.last_base_key.as_deref() != base_key {
            Some("system-prefix-changed".to_string())
        } else {
            Some("unattributed-prefix-drift".to_string())
        };
        self.last_hash = Some(hash);
        self.last_base_key = base_key.map(str::to_string);
        PrefixObservation {
            generation: self.generation,
            changed,
            reason,
        }
    }
}

async fn current_inference_route(
    current: &Arc<Mutex<crate::inference_runtime::ResolvedInferenceRoute>>,
    updates: &tokio::sync::Mutex<
        tokio::sync::mpsc::Receiver<crate::inference_runtime::ResolvedInferenceRoute>,
    >,
) -> crate::inference_runtime::ResolvedInferenceRoute {
    let mut latest = None;
    {
        let mut receiver = updates.lock().await;
        while let Ok(route) = receiver.try_recv() {
            latest = Some(route);
        }
    }
    let mut current = lock_recover(current, "session.inference_route");
    if let Some(route) = latest {
        *current = route;
    }
    current.clone()
}

pub(crate) fn compaction_policy_for_model(
    model_id: &str,
) -> rightkit_agent_engine_core::CompactionModelPolicy {
    rightkit_llm::agent_providers::compaction_capability(model_id).map_or_else(
        || rightkit_agent_engine_core::CompactionModelPolicy::unknown(model_id),
        |capability| {
            rightkit_agent_engine_core::CompactionModelPolicy::known(
                model_id,
                capability.context_window_tokens,
                capability.compaction_trigger_percent,
                capability.compaction_reserve_tokens,
                capability.compaction_max_output_tokens,
            )
        },
    )
}

fn capacity_profile_for_inference_route(
    route: &crate::inference_runtime::ResolvedInferenceRoute,
) -> Result<CapacityProfileV1, ProviderError> {
    let capability =
        rightkit_llm::agent_providers::compaction_capability(&route.selection.model_id)
            .ok_or_else(|| {
                ProviderError::Capacity(CapacityError::CapacityUnavailable {
                    route_id: route.fingerprint.clone(),
                    reason: format!(
                        "loaded model catalog has no capacity profile for {}",
                        route.selection.model_id
                    ),
                })
            })?;
    Ok(capability.capacity_profile(
        route.fingerprint.clone(),
        Some(
            rightkit_llm::agent_providers::model_metadata::catalog_status()
                .payload_sha256
                .clone(),
        ),
    ))
}

fn build_client(route: &ProviderRoute) -> OpenAiCompatClient {
    let mut client = OpenAiCompatClient::new(&route.api_key, &route.model_id, &route.base_url);
    if let Some(effort) = &route.effort {
        client = client.with_effort(effort.clone());
    }
    if let Some(tokens) = route.thinking_budget_tokens {
        client = client.with_reasoning_budget_tokens(tokens);
    }
    if route.disable_thinking {
        client = client.with_reasoning_disabled();
    }
    client
}

/// Re-resolve provider host immediately before each request, then pin
/// the HTTP client to exactly those validated addresses. This closes DNS-rebinding
/// gaps between route rehydration and request execution while retaining
/// explicit persisted private-route approval.
async fn build_checked_client(route: &ProviderRoute) -> Result<OpenAiCompatClient, ProviderError> {
    // Native provider auth is resolved only at request construction. This
    // keeps OAuth refresh inside broker's per-provider lock and leaves
    // arbitrary/custom routes on their caller-supplied key path.
    let native_auth = rightkit_llm::agent_providers::find_provider_by_base_url(&route.base_url)
        .filter(|provider| matches!(provider.id, "openai" | "commandcode"))
        .map(|provider| provider.id);
    let mut effective_base_url = route.base_url.clone();
    let mut effective_api_key = route.api_key.clone();
    let mut auth_headers = BTreeMap::new();
    if let Some(provider) = native_auth {
        let resolved_auth = crate::api_provider_auth::resolve_provider_model_auth(provider)
            .await
            .map_err(ProviderError::Provider)?;
        if resolved_auth.is_none() && effective_api_key.trim().is_empty() {
            return Err(ProviderError::Provider(format!(
                "{provider} is signed out; run `coderight provider login {provider}`"
            )));
        }
        if let Some(auth) = resolved_auth {
            if let Some(api_key) = auth.api_key.filter(|value| !value.trim().is_empty()) {
                effective_api_key = api_key;
            }
            if let Some(base_url) = auth.base_url.filter(|value| !value.trim().is_empty()) {
                effective_base_url = base_url;
            }
            if let Some(headers) = auth.headers {
                auth_headers.extend(headers);
            }
        }
    }
    // An empty key here can only come from a route that declared
    // `api-key-required = false`: `engine_config_for_route` refuses
    // key-required routes with no resolved credential before this layer.
    // Keyless routes are intentionally unauthenticated — send no
    // Authorization header rather than failing closed on a credential the
    // route never asked for.
    let auth_header = (!effective_api_key.trim().is_empty()).then(|| {
        (
            "Authorization".to_string(),
            format!("Bearer {effective_api_key}"),
        )
    });

    let url = url::Url::parse(&effective_base_url)
        .map_err(|error| ProviderError::Provider(format!("invalid provider URL: {error}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| ProviderError::Provider("provider URL has no host".into()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ProviderError::Provider("provider URL has no port".into()))?;
    let addresses =
        rightkit_agent_policy::resolve_provider_endpoint(host, port, route.approved_private)
            .await
            .map_err(|error| {
                ProviderError::Provider(format!("provider egress rejected: {error}"))
            })?;
    if let Some(provider_slug) =
        rightkit_llm::agent_providers::token_plan_provider_for_base_url(&effective_base_url)
    {
        rightkit_llm::agent_providers::token_plan_auth::reserve_daemon_text_quota(provider_slug)
            .await
            .map_err(|error| {
                ProviderError::Provider(format!("token-plan route unavailable: {error}"))
            })?;
    }

    let mut public_headers = BTreeMap::new();
    public_headers.insert(
        "HTTP-Referer".to_string(),
        "https://github.com/adrdsouza/coderight".to_string(),
    );
    public_headers.insert("X-Title".to_string(), "RightKit".to_string());
    public_headers.extend(auth_headers);
    let mut client = OpenAiCompatClient::new_direct(
        route.model_id.clone(),
        effective_base_url,
        auth_header,
        public_headers,
        &addresses,
    );
    if let Some(effort) = &route.effort {
        client = client.with_effort(effort.clone());
    }
    if let Some(tokens) = route.thinking_budget_tokens {
        client = client.with_reasoning_budget_tokens(tokens);
    }
    if route.disable_thinking {
        client = client.with_reasoning_disabled();
    }
    Ok(client)
}

fn image_provider_profile(
    base_url: &str,
) -> rightkit_agent_engine_core::image_eviction::ImageProviderProfile {
    use rightkit_agent_engine_core::image_eviction::ImageProviderProfile;
    let host = url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
    match host.as_deref() {
        Some("api.openai.com") => ImageProviderProfile::OpenAi,
        Some("openrouter.ai") => ImageProviderProfile::OpenRouter,
        Some("api.minimax.io") => ImageProviderProfile::MiniMax,
        Some("api.x.ai") => ImageProviderProfile::Xai,
        Some(_) => ImageProviderProfile::Custom,
        None => ImageProviderProfile::Unknown,
    }
}

fn configured_image_budget_policy(
    base_url: &str,
) -> rightkit_agent_engine_core::image_eviction::ImageBudgetPolicy {
    let profile = image_provider_profile(base_url);
    let suffix = profile.as_str().to_ascii_uppercase();
    let configured = |kind: &str| {
        std::env::var(format!(
            "RIGHTKIT_IMAGE_REQUEST_{kind}_WATER_BYTES_{suffix}"
        ))
        .ok()
        .or_else(|| std::env::var(format!("RIGHTKIT_IMAGE_REQUEST_{kind}_WATER_BYTES")).ok())
        .and_then(|value| value.parse::<usize>().ok())
    };
    let base = rightkit_llm::agent_providers::find_provider_by_base_url(base_url).map_or_else(
        || rightkit_agent_engine_core::image_eviction::ImageBudgetPolicy::for_profile(profile),
        |provider| {
            rightkit_agent_engine_core::image_eviction::ImageBudgetPolicy::from_provider_metadata(
                profile,
                provider.image_request_high_water_bytes,
                provider.image_request_low_water_bytes,
            )
        },
    );
    base.with_overrides(configured("HIGH"), configured("LOW"))
}

// RIGHTKIT-SERIOUS: ordered failover stays local until async P-1 lands; rightkit-llm FailoverChain is blocking and cannot wrap these async clients (report P-1).
// `attempt_dynamic_route`, `dynamic_stream_with_fallback`, `fallback_attempt_routes`,
// `provider_error_allows_model_fallback` and `stream_error_allows_fallback` are the live
// ordered-fallback chain. The classifier maps onto rightkit-llm `FailoverOn::Custom`
// (failover.rs:95-100) once the async chain is ported.

/// Build one fallback attempt's client with the request authority (or direct
/// dispatch) and submission observer applied. Shared by every attempt site.
async fn build_attempt_client(
    route: &ProviderRoute,
    request_authority: Option<&RequestAuthorityBindingV1>,
    submission_observer: Option<&rightkit_llm::agent_providers::transport::SubmissionObserver>,
) -> Result<OpenAiCompatClient, ProviderError> {
    let mut client = build_checked_client(route).await?;
    if let Some(binding) = request_authority {
        client = client.with_request_authority(binding.clone());
    } else {
        client = client.with_direct_dispatch();
    }
    if let Some(observer) = submission_observer {
        client = client.with_submission_observer(observer.clone());
    }
    Ok(client)
}

/// CR-09/CR-11: the single route-selection-plus-budget attempt loop used by
/// BOTH `complete_with_metadata` and `stream_with_metadata` for a `Dynamic`
/// route. Reserves the lineage budget once (if the route is metered), then
/// tries each fallback-ordered route in turn via `op`, classifying failures
/// with `provider_error_allows_model_fallback` exactly as the completed path
/// always has. The caller gets back the successful value, the route that
/// produced it (for compaction-policy publication and cost pricing), and the
/// still-open reservation — reconciling it is the caller's job, because only
/// the caller knows when the real cost is known: immediately for a completed
/// turn, only at stream-terminus for a streamed one (see `StreamBudgetGuard`).
async fn attempt_dynamic_route<T, F, Fut>(
    lineage_budget: Option<&Arc<rightkit_agent_engine_core::LineageBudget>>,
    route: &SharedProviderRoute,
    request_authority: Option<RequestAuthorityBindingV1>,
    submission_observer: Option<rightkit_llm::agent_providers::transport::SubmissionObserver>,
    mut op: F,
) -> Result<
    (
        T,
        ProviderRoute,
        Option<rightkit_agent_engine_core::LineageReservation>,
    ),
    String,
>
where
    F: FnMut(OpenAiCompatClient) -> Fut,
    Fut: std::future::Future<Output = Result<T, ProviderError>>,
{
    let route = lock_recover(route, "route").clone();
    let routes = fallback_attempt_routes(&route);
    let is_metered = routes.iter().any(|route| {
        rightkit_llm::agent_providers::is_priced_model(&route.model_id)
            && !rightkit_llm::agent_providers::is_subscription_billed_route(&route.base_url)
    });
    let reservation = if is_metered {
        lineage_budget
            .map(|budget| budget.reserve_remaining())
            .transpose()?
    } else {
        None
    };
    let mut prior_fallback_error: Option<String> = None;
    for (index, attempt_route) in routes.iter().enumerate() {
        let result = match build_attempt_client(
            attempt_route,
            request_authority.as_ref(),
            submission_observer.as_ref(),
        )
        .await
        {
            Ok(client) => op(client).await,
            Err(error) => return Err(error.to_string()),
        };
        match result {
            Ok(value) => return Ok((value, attempt_route.clone(), reservation)),
            Err(error) => {
                let error_text = error.to_string();
                let has_next = index + 1 < routes.len();
                if has_next && provider_error_allows_model_fallback(&error) {
                    prior_fallback_error =
                        Some(format!("{} failed: {error_text}", attempt_route.model_id));
                    continue;
                }
                if let Some(prior) = prior_fallback_error {
                    return Err(format!(
                        "{prior}; {} failed: {error_text}",
                        attempt_route.model_id
                    ));
                }
                return Err(error_text);
            }
        }
    }
    Err("provider completed without an attempt".to_string())
}

/// Establish a dynamic provider stream, then keep eligible fallback routes
/// available until the stream has emitted content or a tool call. A provider
/// can fail after headers have arrived; retrying such a stream after content
/// or a tool effect would duplicate the assistant turn, so only a stream with
/// no visible/side-effectful event may advance to its next route.
async fn dynamic_stream_with_fallback(
    lineage_budget: Option<&Arc<rightkit_agent_engine_core::LineageBudget>>,
    route: &SharedProviderRoute,
    wire: Vec<serde_json::Value>,
    tool_defs: Vec<serde_json::Value>,
    request_metadata: ProviderRequestMetadata,
    request_authority: Option<RequestAuthorityBindingV1>,
    submission_observer: Option<rightkit_llm::agent_providers::transport::SubmissionObserver>,
) -> Result<rightkit_llm::agent_providers::ProviderResponseStream, String> {
    let route = lock_recover(route, "route").clone();
    let routes = fallback_attempt_routes(&route);
    let is_metered = routes.iter().any(|route| {
        rightkit_llm::agent_providers::is_priced_model(&route.model_id)
            && !rightkit_llm::agent_providers::is_subscription_billed_route(&route.base_url)
    });
    let reservation = if is_metered {
        lineage_budget
            .map(|budget| budget.reserve_remaining())
            .transpose()?
    } else {
        None
    };

    let mut prior_fallback_error = None;
    let mut first_stream = None;
    let mut first_route = None;
    let mut next_index = 0usize;
    for (index, attempt_route) in routes.iter().enumerate() {
        let result = match build_attempt_client(
            attempt_route,
            request_authority.as_ref(),
            submission_observer.as_ref(),
        )
        .await
        {
            Ok(client) => {
                client
                    .stream_with_tools_metadata(wire.clone(), tool_defs.clone(), &request_metadata)
                    .await
            }
            Err(error) => return Err(error.to_string()),
        };
        match result {
            Ok(stream) => {
                first_stream = Some(stream);
                first_route = Some(attempt_route.clone());
                next_index = index + 1;
                break;
            }
            Err(error) => {
                if stream_error_allows_fallback(false, index + 1 < routes.len(), &error) {
                    prior_fallback_error = Some(error.to_string());
                    continue;
                }
                return Err(prior_fallback_error.map_or_else(
                    || error.to_string(),
                    |prior| format!("{prior}; {} failed: {error}", attempt_route.model_id),
                ));
            }
        }
    }

    let Some(first_stream) = first_stream else {
        return Err("provider completed without an attempt".to_string());
    };
    let first_route = first_route.expect("stream route exists when stream exists");
    let remaining_routes = routes[next_index..].to_vec();
    let output = async_stream::stream! {
        let mut reservation = reservation;
        let mut active_route = first_route;
        let mut raw_stream = Some(first_stream);
        let mut routes = remaining_routes;

        'attempts: loop {
            let mut stream = if let Some(stream) = raw_stream.take() {
                stream
            } else {
                let Some(next_route) = routes.first().cloned() else {
                    yield Err(ProviderError::Network("provider stream ended before a fallback route was available".into()));
                    return;
                };
                routes.remove(0);
                active_route = next_route.clone();
                let result = match build_attempt_client(
                    &next_route,
                    request_authority.as_ref(),
                    submission_observer.as_ref(),
                )
                .await
                {
                    Ok(client) => {
                        client
                            .stream_with_tools_metadata(
                                wire.clone(),
                                tool_defs.clone(),
                                &request_metadata,
                            )
                            .await
                    }
                    Err(error) => Err(error),
                };
                match result {
                    Ok(stream) => stream,
                    Err(error) => {
                        if stream_error_allows_fallback(false, !routes.is_empty(), &error) {
                            continue 'attempts;
                        }
                        yield Err(error);
                        return;
                    }
                }
            };

            let mut emitted = false;
            let mut pending_usage = None;
            while let Some(item) = stream.next().await {
                match item {
                    Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        cache_read_tokens,
                        cache_creation_tokens,
                    }) => {
                        if emitted {
                            if let Some(reservation) = reservation.take() {
                                reservation.reconcile_microusd(priced_turn_cost_microusd(
                                    &active_route,
                                    prompt_tokens,
                                    completion_tokens,
                                    cache_read_tokens,
                                    cache_creation_tokens,
                                ));
                            }
                            yield Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Usage {
                                prompt_tokens,
                                completion_tokens,
                                cache_read_tokens,
                                cache_creation_tokens,
                            });
                        } else {
                            pending_usage = Some((
                                prompt_tokens,
                                completion_tokens,
                                cache_read_tokens,
                                cache_creation_tokens,
                            ));
                        }
                    }
                    Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Finished(reason)) => {
                        if let Some((prompt_tokens, completion_tokens, cache_read_tokens, cache_creation_tokens)) = pending_usage.take() {
                            if let Some(reservation) = reservation.take() {
                                reservation.reconcile_microusd(priced_turn_cost_microusd(
                                    &active_route,
                                    prompt_tokens,
                                    completion_tokens,
                                    cache_read_tokens,
                                    cache_creation_tokens,
                                ));
                            }
                            yield Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Usage {
                                prompt_tokens,
                                completion_tokens,
                                cache_read_tokens,
                                cache_creation_tokens,
                            });
                        }
                        yield Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Finished(reason));
                        return;
                    }
                    Ok(event) => {
                        if matches!(
                            &event,
                            rightkit_llm::agent_providers::ProviderStreamEvent::Text(_)
                                | rightkit_llm::agent_providers::ProviderStreamEvent::Reasoning(_)
                                | rightkit_llm::agent_providers::ProviderStreamEvent::ToolCall(_)
                        ) {
                            emitted = true;
                            if let Some((prompt_tokens, completion_tokens, cache_read_tokens, cache_creation_tokens)) = pending_usage.take() {
                                if let Some(reservation) = reservation.take() {
                                    reservation.reconcile_microusd(priced_turn_cost_microusd(
                                        &active_route,
                                        prompt_tokens,
                                        completion_tokens,
                                        cache_read_tokens,
                                        cache_creation_tokens,
                                    ));
                                }
                                yield Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Usage {
                                    prompt_tokens,
                                    completion_tokens,
                                    cache_read_tokens,
                                    cache_creation_tokens,
                                });
                            }
                        }
                        yield Ok(event);
                    }
                    Err(error) => {
                        if stream_error_allows_fallback(emitted, !routes.is_empty(), &error) {
                            raw_stream = None;
                            continue 'attempts;
                        }
                        yield Err(error);
                        return;
                    }
                }
            }

            let error = ProviderError::Sse("provider stream ended before completion".into());
            if stream_error_allows_fallback(emitted, !routes.is_empty(), &error) {
                raw_stream = None;
                continue 'attempts;
            }
            yield Err(error);
            return;
        }
    };
    Ok(Box::pin(output))
}

/// CR-09: compute the actual spend for a completed/streamed turn's usage
/// figures against its billed route, in microUSD, for reservation
/// reconciliation. Zero for a non-priced or subscription-billed route.
fn priced_turn_cost_microusd(
    route: &ProviderRoute,
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
) -> u64 {
    if !rightkit_llm::agent_providers::is_priced_model(&route.model_id)
        || rightkit_llm::agent_providers::is_subscription_billed_route(&route.base_url)
    {
        return 0;
    }
    let (prompt_price, completion_price) =
        rightkit_llm::agent_providers::price_per_1k(&route.model_id);
    let cost = (prompt_tokens as f64 / 1000.0) * prompt_price
        + (completion_tokens as f64 / 1000.0) * completion_price
        + (cache_read_tokens as f64 / 1000.0) * prompt_price * 0.1
        + (cache_creation_tokens as f64 / 1000.0) * prompt_price * 1.25;
    rightkit_agent_engine_core::usd_to_microusd(cost).unwrap_or(0)
}

/// CR-09/CR-11: reconciles a streamed turn's lineage reservation exactly
/// once — on the `Usage` event, where actual token counts (and therefore
/// cost) become known, mirroring `complete_with_metadata`'s reconciliation.
/// If the stream is dropped before ever reaching `Usage` (aborted mid-turn,
/// or errored before usage arrived), `reservation` is still `Some` and its
/// own `Drop` releases the full held amount back to the budget — so an
/// aborted stream never leaves a dangling reservation (invariant 4).
struct StreamBudgetGuard {
    reservation: Option<rightkit_agent_engine_core::LineageReservation>,
    route: ProviderRoute,
}

impl StreamBudgetGuard {
    fn reconcile_usage(
        &mut self,
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
    ) {
        if let Some(reservation) = self.reservation.take() {
            let actual_microusd = priced_turn_cost_microusd(
                &self.route,
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_creation_tokens,
            );
            reservation.reconcile_microusd(actual_microusd);
        }
    }
}

fn fallback_attempt_routes(route: &ProviderRoute) -> Vec<ProviderRoute> {
    let mut routes = vec![ProviderRoute {
        api_key: route.api_key.clone(),
        model_id: route.model_id.clone(),
        base_url: route.base_url.clone(),
        effort: route.effort.clone(),
        thinking_budget_tokens: route.thinking_budget_tokens,
        disable_thinking: route.disable_thinking,
        fallback_models: Vec::new(),
        approved_private: route.approved_private,
    }];
    for model in fallback_models_for_primary(&route.fallback_models, &route.model_id) {
        routes.push(ProviderRoute {
            api_key: route.api_key.clone(),
            model_id: model,
            base_url: route.base_url.clone(),
            effort: route.effort.clone(),
            thinking_budget_tokens: route.thinking_budget_tokens,
            disable_thinking: route.disable_thinking,
            fallback_models: Vec::new(),
            approved_private: route.approved_private,
        });
    }
    routes
}

fn fallback_models_for_primary(models: &[String], primary: &str) -> Vec<String> {
    let mut out = Vec::new();
    for model in models {
        let trimmed = model.trim();
        if trimmed.is_empty() || trimmed == primary || out.iter().any(|m| m == trimmed) {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

/// A second route is a second paid request for the same turn, so it is allowed only when the first provably did no
/// work: the request never left (connect failure, pre-send capacity refusal) or the provider gave an explicit
/// refusal (HTTP 400, 401 or 403, or the typed `Auth`). Everything else may have been delivered and billed, or is
/// not a definite refusal, and gets no fallback: 408, 425, 429 (typed `RateLimited` too), any 5xx, a post-send
/// `Network` error, a cut stream (`Sse`), `DeliveryUncertain`, and an in-body `Provider` error.
fn provider_error_allows_model_fallback(error: &ProviderError) -> bool {
    match error {
        ProviderError::Http { status, .. } => matches!(*status, 400 | 401 | 403),
        ProviderError::NotDelivered(_) | ProviderError::Auth => true,
        // The request reached the provider before these, or the refusal is not definite: never replay it.
        ProviderError::RateLimited { .. }
        | ProviderError::Provider(_)
        | ProviderError::Network(_)
        | ProviderError::Sse(_)
        | ProviderError::DeliveryUncertain { .. } => false,
        ProviderError::Capacity(CapacityError::CapacityUnavailable { .. })
        | ProviderError::Capacity(CapacityError::FinalInsertionOverBudget { .. }) => true,
        ProviderError::Capacity(CapacityError::ContinuationReauthorizationRequired) => false,
        ProviderError::Authority(_) => false,
        ProviderError::IncompleteOutput { .. } => false,
    }
}

fn stream_error_allows_fallback(
    emitted_visible_or_side_effect: bool,
    has_next_route: bool,
    error: &ProviderError,
) -> bool {
    !emitted_visible_or_side_effect && has_next_route && provider_error_allows_model_fallback(error)
}

fn sanitize_provider_content(
    text: String,
    reasoning: Option<String>,
    has_tool_calls: bool,
) -> Result<(String, Option<String>), String> {
    let (text, inline_reasoning) = strip_think_tags(&text);
    let reasoning = match (
        reasoning.filter(|value| !value.trim().is_empty()),
        inline_reasoning,
    ) {
        (Some(existing), Some(inline)) if existing.trim() != inline.trim() => {
            Some(format!("{}\n\n{}", existing.trim(), inline.trim()))
        }
        (Some(existing), _) => Some(existing.trim().to_string()),
        (None, Some(inline)) => Some(inline),
        (None, None) => None,
    };

    if text.trim().is_empty() && !has_tool_calls {
        return Err(
            "provider returned no visible content or tool calls after reasoning was separated"
                .to_string(),
        );
    }

    Ok((text, reasoning))
}

const OPEN_THINK_TAG: &str = "<think>";
const CLOSE_THINK_TAG: &str = "</think>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamContentMode {
    Visible,
    Thinking,
}

/// Incremental counterpart to [`strip_think_tags`]. Only a possible split tag
/// is held between provider chunks; confirmed visible text is returned at
/// once, while thought content is captured separately. The parser is ASCII
/// case-insensitive for the provider markers and removes orphan `</think>`
/// closers just like the completed path.
struct IncrementalContentNormalizer {
    mode: StreamContentMode,
    candidate: String,
    thought: String,
    inline_reasoning: String,
    provider_reasoning: String,
    leading_or_trailing_whitespace: String,
    visible_started: bool,
    has_tool_calls: bool,
}

impl Default for IncrementalContentNormalizer {
    fn default() -> Self {
        Self {
            mode: StreamContentMode::Visible,
            candidate: String::new(),
            thought: String::new(),
            inline_reasoning: String::new(),
            provider_reasoning: String::new(),
            leading_or_trailing_whitespace: String::new(),
            visible_started: false,
            has_tool_calls: false,
        }
    }
}

impl IncrementalContentNormalizer {
    fn expected_tags(&self) -> &'static [&'static str] {
        match self.mode {
            StreamContentMode::Visible => &[OPEN_THINK_TAG, CLOSE_THINK_TAG],
            StreamContentMode::Thinking => &[CLOSE_THINK_TAG],
        }
    }

    fn candidate_is_prefix(&self) -> bool {
        let candidate = self.candidate.to_ascii_lowercase();
        self.expected_tags()
            .iter()
            .any(|tag| tag.starts_with(&candidate))
    }

    fn append_visible(&mut self, text: &str, output: &mut String) {
        for character in text.chars() {
            if !self.visible_started {
                if character.is_whitespace() {
                    continue;
                }
                self.visible_started = true;
            }
            if character.is_whitespace() {
                self.leading_or_trailing_whitespace.push(character);
            } else {
                if !self.leading_or_trailing_whitespace.is_empty() {
                    output.push_str(&self.leading_or_trailing_whitespace);
                    self.leading_or_trailing_whitespace.clear();
                }
                output.push(character);
            }
        }
    }

    fn append_mode_text(&mut self, text: &str, output: &mut String) {
        match self.mode {
            StreamContentMode::Visible => self.append_visible(text, output),
            StreamContentMode::Thinking => self.thought.push_str(text),
        }
    }

    fn close_thought(&mut self) {
        self.inline_reasoning.push_str(self.thought.trim());
        self.thought.clear();
    }

    fn resolve_candidate(&mut self, output: &mut String) {
        loop {
            let lower = self.candidate.to_ascii_lowercase();
            if self.mode == StreamContentMode::Visible && lower == OPEN_THINK_TAG {
                self.candidate.clear();
                self.mode = StreamContentMode::Thinking;
                self.thought.clear();
                return;
            }
            if self.mode == StreamContentMode::Visible && lower == CLOSE_THINK_TAG {
                // Provider occasionally prefixes visible content with an
                // orphan closer. The completed path removes every one.
                self.candidate.clear();
                return;
            }
            if self.mode == StreamContentMode::Thinking && lower == CLOSE_THINK_TAG {
                self.candidate.clear();
                self.close_thought();
                self.mode = StreamContentMode::Visible;
                return;
            }
            if self.candidate_is_prefix() {
                return;
            }

            // Candidate no longer matches a marker. Preserve only its longest
            // suffix that could begin a marker, then emit the confirmed
            // prefix in the current mode. Tag candidates are <= 8 bytes, so
            // this delimiter tail stays bounded even with arbitrary chunks.
            let chars: Vec<char> = self.candidate.chars().collect();
            let mut keep = 0usize;
            for suffix_len in (1..=chars.len()).rev() {
                let suffix: String = chars[chars.len() - suffix_len..].iter().collect();
                let suffix = suffix.to_ascii_lowercase();
                if self
                    .expected_tags()
                    .iter()
                    .any(|tag| tag.starts_with(&suffix))
                {
                    keep = suffix_len;
                    break;
                }
            }
            let flush_len = chars.len().saturating_sub(keep);
            let flush: String = chars[..flush_len].iter().collect();
            self.candidate = chars[flush_len..].iter().collect();
            self.append_mode_text(&flush, output);
            if self.candidate.is_empty() {
                return;
            }
        }
    }

    fn push_text(&mut self, text: &str) -> String {
        let mut output = String::new();
        for character in text.chars() {
            if self.candidate.is_empty() {
                if character == '<' {
                    self.candidate.push(character);
                } else {
                    self.append_mode_text(&character.to_string(), &mut output);
                }
            } else {
                self.candidate.push(character);
                self.resolve_candidate(&mut output);
            }
        }
        output
    }

    fn push_provider_reasoning(&mut self, text: &str) {
        self.provider_reasoning.push_str(text);
    }

    /// Inline `<think>` reasoning alone (provider reasoning excluded).
    fn inline_reasoning_only(&self) -> Option<String> {
        let inline = self.inline_reasoning.trim();
        let provider = self.provider_reasoning.trim();
        if inline.is_empty() || inline == provider {
            None
        } else if provider.is_empty() {
            Some(inline.to_string())
        } else {
            // Continues the provider text already streamed, so the engine's
            // concatenation equals the completed path's combined reasoning.
            Some(format!("\n\n{inline}"))
        }
    }

    fn note_tool_call(&mut self) {
        self.has_tool_calls = true;
    }

    #[allow(clippy::result_large_err)]
    fn finish(&mut self) -> Result<(String, Option<String>), ProviderError> {
        let mut output = String::new();
        if !self.candidate.is_empty() {
            let candidate = std::mem::take(&mut self.candidate);
            self.append_mode_text(&candidate, &mut output);
        }
        if self.mode == StreamContentMode::Thinking {
            self.close_thought();
        }
        // A completed response trims the visible text as a whole. Leading
        // whitespace is discarded as it arrives; this tail is discarded only
        // now, after the provider has declared the turn finished.
        self.leading_or_trailing_whitespace.clear();
        if !self.visible_started && !self.has_tool_calls {
            return Err(ProviderError::Provider(
                "provider returned no visible content or tool calls after reasoning was separated"
                    .into(),
            ));
        }
        let provider = self.provider_reasoning.trim();
        let inline = self.inline_reasoning.trim();
        let reasoning = match (provider.is_empty(), inline.is_empty()) {
            (false, false) if provider != inline => Some(format!("{provider}\n\n{inline}")),
            (false, _) => Some(provider.to_string()),
            (true, false) => Some(inline.to_string()),
            (true, true) => None,
        };
        Ok((output, reasoning))
    }
}

/// Apply the same terminal content policy to every streaming provider path as
/// the completed path: inline `<think>` blocks are removed from visible text,
/// inline reasoning is combined with provider reasoning, and reasoning-only
/// turns are rejected unless a tool call exists. Visible text remains live;
/// only a possible marker tail and reasoning normalization are deferred.
fn normalize_provider_stream(
    inner: rightkit_llm::agent_providers::ProviderResponseStream,
) -> rightkit_llm::agent_providers::ProviderResponseStream {
    Box::pin(async_stream::try_stream! {
        futures_util::pin_mut!(inner);
        let mut normalizer = IncrementalContentNormalizer::default();
        let mut streamed_provider_reasoning = false;
        while let Some(item) = inner.next().await {
            match item? {
                rightkit_llm::agent_providers::ProviderStreamEvent::Text(chunk) => {
                    let visible = normalizer.push_text(&chunk);
                    if !visible.is_empty() {
                        yield rightkit_llm::agent_providers::ProviderStreamEvent::Text(visible);
                    }
                }
                rightkit_llm::agent_providers::ProviderStreamEvent::Reasoning(chunk) => {
                    // Provider reasoning is forwarded as it arrives so it
                    // precedes the answer it led to; only inline `<think>`
                    // reasoning has to wait for the end of the stream.
                    normalizer.push_provider_reasoning(&chunk);
                    streamed_provider_reasoning = true;
                    yield rightkit_llm::agent_providers::ProviderStreamEvent::Reasoning(chunk);
                }
                rightkit_llm::agent_providers::ProviderStreamEvent::ToolCall(call) => {
                    normalizer.note_tool_call();
                    yield rightkit_llm::agent_providers::ProviderStreamEvent::ToolCall(call);
                }
                rightkit_llm::agent_providers::ProviderStreamEvent::Usage {
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                } => {
                    yield rightkit_llm::agent_providers::ProviderStreamEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        cache_read_tokens,
                        cache_creation_tokens,
                    };
                }
                rightkit_llm::agent_providers::ProviderStreamEvent::UsageDetail { reasoning_tokens } => {
                    yield rightkit_llm::agent_providers::ProviderStreamEvent::UsageDetail { reasoning_tokens };
                }
                rightkit_llm::agent_providers::ProviderStreamEvent::Finished(reason) => {
                    let (text, reasoning) = normalizer.finish()?;
                    if !text.is_empty() {
                        yield rightkit_llm::agent_providers::ProviderStreamEvent::Text(text);
                    }
                    // Provider reasoning already went out live; the remainder
                    // is the inline `<think>` text it did not carry.
                    let reasoning = if streamed_provider_reasoning {
                        normalizer.inline_reasoning_only()
                } else {
                        reasoning
                    };
                    if let Some(reasoning) = reasoning {
                        yield rightkit_llm::agent_providers::ProviderStreamEvent::Reasoning(reasoning);
                    }
                    yield rightkit_llm::agent_providers::ProviderStreamEvent::Finished(reason);
                    return;
                }
            }
        }
    })
}

#[async_trait]
impl LlmClient for ProviderLlmClient {
    async fn complete(&self, messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        self.complete_with_metadata(messages, &LlmRequestMetadata::default())
            .await
    }

    async fn complete_with_metadata(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponse, String> {
        let wire: Vec<serde_json::Value> = messages.iter().filter_map(transcript_to_wire).collect();
        let (tool_defs, request_metadata, _lineage) =
            self.prepare_turn_metadata(messages, metadata)?;
        let request_authority = self.request_authority_binding(metadata)?;
        let submission_observer = self.submission_observer();

        let (turn, billed_route, reservation) = match &self.inner {
            ProviderClientInner::Static(client) => (
                self.configure_openai_client(client.clone(), metadata)?
                    .complete_with_tools_metadata(wire, tool_defs, &request_metadata)
                    .await
                    .map_err(|e| e.to_string())?,
                None,
                None,
            ),
            ProviderClientInner::Anthropic(client) => (
                self.configure_anthropic_client(client.clone(), metadata)?
                    .complete_with_tools(wire, tool_defs, request_metadata.max_output_tokens)
                    .await
                    .map_err(|e| e.to_string())?,
                None,
                None,
            ),
            ProviderClientInner::Direct {
                route,
                updates,
                scheduler,
                active_requests,
            } => {
                let _active = ActiveInferenceGuard::new(active_requests);
                let route = current_inference_route(route, updates).await;
                let _permit = scheduler
                    .acquire(
                        &route.selection.connection_id,
                        route.selection.route_policy_id.as_deref(),
                    )
                    .await
                    .map_err(str::to_string)?;
                let direct = route
                    .direct_connection
                    .as_ref()
                    .ok_or_else(|| "direct connection state unavailable".to_string())?;
                let addresses = crate::inference_runtime::resolved_direct_addresses(direct)
                    .await
                    .map_err(|error| error.code().to_string())?;
                let auth_header = route.auth_header_name.as_ref().and_then(|name| {
                    route.auth_secret.as_ref().map(|secret| {
                        let value = if name.eq_ignore_ascii_case("authorization") {
                            format!("Bearer {secret}")
                        } else {
                            secret.clone()
                        };
                        (name.clone(), value)
                    })
                });
                let completed = match route.selection.protocol {
                    crate::inference_runtime::InferenceProtocol::OpenaiCompatible => {
                        self.configure_openai_client(
                            OpenAiCompatClient::new_direct(
                                route.selection.model_id.clone(),
                                route.endpoint.clone(),
                                auth_header,
                                route.public_headers.clone(),
                                &addresses,
                            ),
                            metadata,
                        )?
                        .complete_with_tools_metadata(wire, tool_defs, &request_metadata)
                        .await
                    }
                    crate::inference_runtime::InferenceProtocol::AnthropicCompatible => {
                        self.configure_anthropic_client(
                            AnthropicCompatClient::new_direct(
                                route.selection.model_id.clone(),
                                route.endpoint.clone(),
                                auth_header,
                                route.public_headers.clone(),
                                &addresses,
                            ),
                            metadata,
                        )?
                        .complete_with_tools(wire, tool_defs, request_metadata.max_output_tokens)
                        .await
                    }
                }
                .map_err(|error| error.to_string())?;
                (completed, None, None)
            }
            ProviderClientInner::Dynamic { route, admission } => {
                // Register under the route-update gate BEFORE the route is
                // captured inside `attempt_dynamic_route`: the updater holds
                // the same gate across its busy check and durable commit, so
                // a PATCH can never interleave between capture and dispatch.
                // The gate is released immediately; `_active` alone is held
                // across the request.
                let _active = acquire_dynamic_route_admission(admission).await;
                let (turn, attempt_route, reservation) = attempt_dynamic_route(
                    self.lineage_budget.as_ref(),
                    route,
                    request_authority,
                    submission_observer,
                    |client| {
                        let wire = wire.clone();
                        let tool_defs = tool_defs.clone();
                        let request_metadata = &request_metadata;
                        async move {
                            client
                                .complete_with_tools_metadata(wire, tool_defs, request_metadata)
                                .await
                        }
                    },
                )
                .await?;
                self.publish_active_compaction_policy(&attempt_route.model_id);
                (turn, Some(attempt_route), reservation)
            }
        };

        if let Some(reservation) = reservation {
            let actual_microusd = billed_route
                .as_ref()
                .map(|route| {
                    priced_turn_cost_microusd(
                        route,
                        turn.prompt_tokens,
                        turn.completion_tokens,
                        turn.cache_read_tokens,
                        turn.cache_creation_tokens,
                    )
                })
                .unwrap_or(0);
            reservation.reconcile_microusd(actual_microusd);
        }

        let (text, reasoning) =
            sanitize_provider_content(turn.text, turn.reasoning, !turn.tool_calls.is_empty())?;

        Ok(LlmResponse {
            text,
            reasoning,
            tool_calls: turn.tool_calls,
            finish_reason: turn.finish_reason,
            prompt_tokens: turn.prompt_tokens,
            completion_tokens: turn.completion_tokens,
            cache_read_tokens: turn.cache_read_tokens,
            cache_creation_tokens: turn.cache_creation_tokens,
        })
    }

    async fn stream_with_metadata(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponseStream, String> {
        let wire: Vec<serde_json::Value> = messages.iter().filter_map(transcript_to_wire).collect();
        let (tool_defs, request_metadata, _lineage) =
            self.prepare_turn_metadata(messages, metadata)?;
        let request_authority = self.request_authority_binding(metadata)?;
        let submission_observer = self.submission_observer();

        let provider_stream = match &self.inner {
            ProviderClientInner::Static(client) => self
                .configure_openai_client(client.clone(), metadata)?
                .stream_with_tools_metadata(wire, tool_defs, &request_metadata)
                .await
                .map_err(|error| error.to_string())?,
            ProviderClientInner::Anthropic(client) => self
                .configure_anthropic_client(client.clone(), metadata)?
                .stream_with_tools(wire, tool_defs, request_metadata.max_output_tokens)
                .await
                .map_err(|error| error.to_string())?,
            ProviderClientInner::Dynamic { route, admission } => {
                // CR-09/CR-11: use the stream-specific counterpart to the
                // completed route loop. It performs the same reservation and
                // fallback selection, then keeps later routes available for a
                // pre-emission mid-stream failure.
                //
                // Route-update admission is acquired BEFORE the route is
                // captured (below and again inside
                // `dynamic_stream_with_fallback`), released immediately, and
                // the active-request guard is moved into the returned stream
                // wrapper so the counter stays non-zero for the stream's full
                // lifetime — error, drop, or cancel included — exactly like
                // the Direct arm below.
                let active = acquire_dynamic_route_admission(admission).await;
                let initial_model_id = lock_recover(route, "route").model_id.clone();
                self.publish_active_compaction_policy(&initial_model_id);
                // The stream-specific state machine keeps fallback routes
                // available after headers arrive, but only before any text,
                // reasoning, or tool call has been emitted. It also owns the
                // lineage reservation so a failed pre-emission attempt cannot
                // reconcile usage against the wrong model.
                let stream = dynamic_stream_with_fallback(
                    self.lineage_budget.as_ref(),
                    route,
                    wire,
                    tool_defs,
                    request_metadata,
                    request_authority,
                    submission_observer,
                )
                .await?;
                match active {
                    Some(active) => {
                        let wrapped: rightkit_llm::agent_providers::ProviderResponseStream =
                            Box::pin(async_stream::try_stream! {
                                let _active = active;
                                let mut stream = stream;
                                while let Some(event) = stream.next().await {
                                    yield event?;
                                }
                            });
                        wrapped
                    }
                    None => stream,
                }
            }
            ProviderClientInner::Direct {
                route,
                updates,
                scheduler,
                active_requests,
            } => {
                let active = ActiveInferenceGuard::new(active_requests);
                let route = current_inference_route(route, updates).await;
                let permit = scheduler
                    .acquire(
                        &route.selection.connection_id,
                        route.selection.route_policy_id.as_deref(),
                    )
                    .await
                    .map_err(str::to_string)?;
                let direct = route
                    .direct_connection
                    .as_ref()
                    .ok_or_else(|| "direct connection state unavailable".to_string())?;
                let addresses = crate::inference_runtime::resolved_direct_addresses(direct)
                    .await
                    .map_err(|error| error.code().to_string())?;
                let auth_header = route.auth_header_name.as_ref().and_then(|name| {
                    route.auth_secret.as_ref().map(|secret| {
                        let value = if name.eq_ignore_ascii_case("authorization") {
                            format!("Bearer {secret}")
                        } else {
                            secret.clone()
                        };
                        (name.clone(), value)
                    })
                });
                let inner = match route.selection.protocol {
                    crate::inference_runtime::InferenceProtocol::OpenaiCompatible => self
                        .configure_openai_client(
                            OpenAiCompatClient::new_direct(
                                route.selection.model_id.clone(),
                                route.endpoint.clone(),
                                auth_header,
                                route.public_headers.clone(),
                                &addresses,
                            ),
                            metadata,
                        )?
                        .stream_with_tools_metadata(wire, tool_defs, &request_metadata)
                        .await
                        .map_err(|error| error.to_string())?,
                    crate::inference_runtime::InferenceProtocol::AnthropicCompatible => self
                        .configure_anthropic_client(
                            AnthropicCompatClient::new_direct(
                                route.selection.model_id.clone(),
                                route.endpoint.clone(),
                                auth_header,
                                route.public_headers.clone(),
                                &addresses,
                            ),
                            metadata,
                        )?
                        .stream_with_tools(wire, tool_defs, request_metadata.max_output_tokens)
                        .await
                        .map_err(|error| error.to_string())?,
                };
                Box::pin(async_stream::try_stream! {
                    let _active = active;
                    let _permit = permit;
                    let mut inner = inner;
                    while let Some(event) = inner.next().await {
                        yield event?;
                    }
                })
            }
        };

        let provider_stream = normalize_provider_stream(provider_stream);
        Ok(Box::pin(provider_stream.map(|event| {
            event
                .map(|event| match event {
                    rightkit_llm::agent_providers::ProviderStreamEvent::Text(text) => {
                        LlmStreamEvent::Text(text)
                    }
                    rightkit_llm::agent_providers::ProviderStreamEvent::Reasoning(text) => {
                        LlmStreamEvent::Reasoning(text)
                    }
                    rightkit_llm::agent_providers::ProviderStreamEvent::ToolCall(call) => {
                        LlmStreamEvent::ToolCall(call)
                    }
                    rightkit_llm::agent_providers::ProviderStreamEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        cache_read_tokens,
                        cache_creation_tokens,
                    } => LlmStreamEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        cache_read_tokens,
                        cache_creation_tokens,
                    },
                    rightkit_llm::agent_providers::ProviderStreamEvent::UsageDetail {
                        reasoning_tokens,
                    } => LlmStreamEvent::UsageDetail { reasoning_tokens },
                    rightkit_llm::agent_providers::ProviderStreamEvent::Finished(reason) => {
                        LlmStreamEvent::Finished(reason)
                    }
                })
                .map_err(|error| error.to_string())
        })))
    }

    /// CR-33: this is the SAME accumulated `PromotedToolState` this client
    /// serializes onto the wire (`self.promoted_tools`, mutated only at real
    /// turn boundaries in `complete_with_metadata`/`stream_with_metadata`) —
    /// never a fresh one. The engine's CacheTracker digest is built from
    /// whatever this returns, so a wire-visible promotion always produces
    /// the matching digest transition.
    fn replace_tool_registry(&self, tools: Arc<ToolRegistry>) {
        *lock_recover(&self.tools, "tool registry") = tools;
    }

    fn promoted_tool_state_snapshot(
        &self,
    ) -> rightkit_agent_tools::coding_tools::registry::PromotedToolState {
        lock_recover(&self.promoted_tools, "promoted tool state").clone()
    }

    /// Live route identity for prompt-cache break attribution: a PATCHed model
    /// or route changes this between requests, which is an expected break.
    /// Fixed-model clients (`Static`, `Anthropic`) cannot switch.
    fn active_route_label(&self) -> Option<String> {
        match &self.inner {
            ProviderClientInner::Dynamic { route, .. } => {
                Some(lock_recover(route, "route").model_id.clone())
            }
            ProviderClientInner::Direct { route, .. } => Some(
                lock_recover(route, "session.inference_route")
                    .fingerprint
                    .clone(),
            ),
            ProviderClientInner::Static(_) | ProviderClientInner::Anthropic(_) => None,
        }
    }

    fn image_budget_policy(&self) -> rightkit_agent_engine_core::image_eviction::ImageBudgetPolicy {
        match &self.inner {
            ProviderClientInner::Static(client) => {
                configured_image_budget_policy(client.base_url())
            }
            ProviderClientInner::Anthropic(client) => {
                configured_image_budget_policy(client.base_url())
            }
            ProviderClientInner::Direct { route, .. } => configured_image_budget_policy(
                &lock_recover(route, "session.inference_route").endpoint,
            ),
            ProviderClientInner::Dynamic { route, .. } => {
                let route = lock_recover(route, "route");
                configured_image_budget_policy(&route.base_url)
            }
        }
    }

    fn image_request_accounting(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> rightkit_agent_engine_core::image_eviction::ImageRequestAccounting {
        use rightkit_agent_engine_core::image_eviction::{
            ImageAccountingBasis, ImageRequestAccounting,
        };
        let wire = messages.iter().filter_map(transcript_to_wire).collect();
        // Accounting-only path (image-eviction budget search), not a real
        // outbound turn: read the current promoted state without mutating
        // it — promotion is evaluated only at true turn boundaries, inside
        // `complete_with_metadata`, never here (this may be called many
        // times per candidate during eviction search).
        let tool_registry = lock_recover(&self.tools, "tool registry").clone();
        let tool_defs = {
            let promoted = lock_recover(&self.promoted_tools, "promoted tool state");
            tool_defs_for_messages_checked(tool_registry.as_ref(), &promoted)
        }
        .unwrap_or_default();
        let request_metadata = ProviderRequestMetadata {
            stable_prefix_key: metadata.stable_prefix_key.clone(),
            max_output_tokens: metadata.max_output_tokens,
            ..ProviderRequestMetadata::default()
        };
        let bytes = match &self.inner {
            ProviderClientInner::Static(client) => {
                client.request_body_bytes(wire, tool_defs, &request_metadata)
            }
            ProviderClientInner::Anthropic(_) => {
                serde_json::to_vec(&serde_json::json!({"messages": wire, "tools": tool_defs}))
                    .map(|body| body.len())
            }
            ProviderClientInner::Direct { route, .. } => {
                let route = lock_recover(route, "session.inference_route");
                if matches!(
                    route.selection.protocol,
                    crate::inference_runtime::InferenceProtocol::OpenaiCompatible
                ) {
                    OpenAiCompatClient::new(
                        "",
                        route.selection.model_id.clone(),
                        route.endpoint.clone(),
                    )
                    .request_body_bytes(wire, tool_defs, &request_metadata)
                } else {
                    serde_json::to_vec(&serde_json::json!({"messages": wire, "tools": tool_defs}))
                        .map(|body| body.len())
                }
            }
            ProviderClientInner::Dynamic { route, .. } => {
                let route = lock_recover(route, "route").clone();
                build_client(&route).request_body_bytes(wire, tool_defs, &request_metadata)
            }
        }
        .unwrap_or(usize::MAX);
        ImageRequestAccounting {
            body_bytes: bytes,
            basis: ImageAccountingBasis::OpenAiChatCompletionsJsonV1,
        }
    }
}

// P0-C: `tool_defs_for_messages` and `scoped_deferred_tool_names` used to be
// private fns here (in the binary crate) while the core-tool allowlist they
// filtered against lived in `coderight-tools` — a split with no
// cross-cutting tests (see the Known Trap in
// `docs/canon/daemon-master-canon-atoms.md` Lane 2). Both the policy and the
// allowlist now live together in `rightkit_agent_tools::coding_tools::registry`
// (`lazy_loading.rs`); this crate only threads a persistent
// `PromotedToolState` through it at real turn boundaries (see
// `complete_with_metadata` / `image_request_accounting` above). The
// single-shot, stateless shape below exists only so the tests further down
// — written against the pre-move two-argument call — keep compiling
// unchanged; it is byte-for-byte what `rightkit_agent_tools::coding_tools::registry::
// tool_defs_for_full_history` does.
#[cfg(test)]
fn tool_defs_for_messages(
    tools: &ToolRegistry,
    messages: &[TranscriptMessage],
) -> Vec<serde_json::Value> {
    rightkit_agent_tools::coding_tools::registry::tool_defs_for_full_history(tools, messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_protocol::ToolCall;
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    #[test]
    fn delivered_context_digest_has_the_exact_sha256_text() {
        assert_eq!(
            context_envelope_digest("hello"),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn delivered_context_digest_is_content_addressed_over_exact_envelope() {
        let a = context_envelope_digest("<membrane_context>a</membrane_context>");
        assert!(a.starts_with("sha256:") && a.len() == "sha256:".len() + 64);
        assert_eq!(
            a,
            context_envelope_digest("<membrane_context>a</membrane_context>")
        );
        assert_ne!(
            a,
            context_envelope_digest("<membrane_context>b</membrane_context>")
        );
    }

    #[test]
    fn context_evidence_is_complete_only_for_submitted_delivered_context() {
        use rightkit_logs::agent_telemetry::evidence::ProviderSubmissionStateV1 as S;
        use rightkit_logs::agent_telemetry::{ObservationCoverage, UnavailableReason};
        let digest = Some("sha256:abc".to_string());
        let (ids, retained) = context_evidence_values(
            S::Submitted,
            digest.clone(),
            UnavailableReason::InputNotProvided,
        );
        assert_eq!(ids.value, Some(vec!["membrane-cli:sha256:abc".to_string()]));
        assert_eq!(retained.value.as_deref(), Some("sha256:abc"));
        for (state, reason) in [
            (S::Rejected, UnavailableReason::SubmissionRejected),
            (S::SubmissionUnknown, UnavailableReason::SubmissionUnknown),
            (S::Unavailable, UnavailableReason::SubmissionUnavailable),
        ] {
            let (ids, retained) = context_evidence_values(state, digest.clone(), reason);
            assert_eq!(ids.coverage, ObservationCoverage::Unavailable);
            assert_eq!(retained.coverage, ObservationCoverage::Unavailable);
            assert_eq!(retained.unavailable_reason, Some(reason));
        }
        // No delivered envelope: nothing is invented even when submitted.
        let (ids, retained) =
            context_evidence_values(S::Submitted, None, UnavailableReason::InputNotProvided);
        assert_eq!(ids.coverage, ObservationCoverage::Unavailable);
        assert_eq!(retained.coverage, ObservationCoverage::Unavailable);
    }

    #[test]
    fn delivered_envelope_digest_matches_bytes_in_serialized_request() {
        let mut body = serde_json::json!({"messages": [{"role": "user", "content": "fix it"}]})
            .as_object()
            .cloned()
            .unwrap();
        let envelope =
            "<membrane_context source=\"pulse-membrane-cli\">x &lt; y</membrane_context>";
        append_context_to_task_message(&mut body, envelope).unwrap();
        let serialized = serde_json::to_string(&serde_json::Value::Object(body)).unwrap();
        let content = serde_json::from_str::<serde_json::Value>(&serialized).unwrap()["messages"]
            [0]["content"]
            .as_str()
            .unwrap()
            .to_string();
        let delivered = content.strip_prefix("fix it\n\n").unwrap();
        assert_eq!(
            context_envelope_digest(delivered),
            context_envelope_digest(envelope)
        );
    }

    fn canonical_empty_selection_decision(
    ) -> rightkit_agent_engine_protocol::CompactionFederationDecisionV1 {
        let selected = serde_json::Value::Array(Vec::new());
        let selection_hash = format!(
            "sha256:{}",
            rightkit_fs::digest::sha256_bytes(&serde_json::to_vec(&selected).unwrap())
        );
        serde_json::from_value(serde_json::json!({
            "$schema": "compaction-federation-v1",
            "identity": "compact-7",
            "fence": "session-1:compact-7",
            "scope": "repo-1",
            "decision": "approved",
            "selectionReceipt": {
                "selectedItemIds": [],
                "selectionHash": selection_hash
            },
            "issuedMonotonicElapsedMs": 100,
            "expiresAfterElapsedMs": 30000,
            "restartEpoch": 42
        }))
        .unwrap()
    }

    fn validate_fixture(
        decision: &rightkit_agent_engine_protocol::CompactionFederationDecisionV1,
        start: Instant,
        now: Instant,
    ) -> Result<(), String> {
        validate_compaction_federation_decision(
            decision,
            "compact-7",
            "session-1:compact-7",
            "repo-1",
            42,
            start,
            now,
        )
    }

    #[test]
    fn canonical_fixture_allows_empty_selection_issuer_permits() {
        let decision = canonical_empty_selection_decision();
        let start = Instant::now();
        assert!(validate_fixture(&decision, start, start + Duration::from_millis(1)).is_ok());
    }

    #[test]
    fn altered_fence_scope_hash_or_epoch_is_rejected() {
        for alter in ["fence", "scope", "hash", "epoch"] {
            let mut decision = canonical_empty_selection_decision();
            match alter {
                "fence" => decision.fence.push_str("-altered"),
                "scope" => decision.scope.push_str("-altered"),
                "hash" => decision.selection_receipt.selection_hash = "sha256:altered".into(),
                "epoch" => decision.restart_epoch = 41,
                _ => unreachable!(),
            }
            let start = Instant::now();
            assert!(
                validate_fixture(&decision, start, start + Duration::from_millis(1)).is_err(),
                "{alter}"
            );
        }
    }

    #[test]
    fn excessive_ttl_is_rejected() {
        let mut decision = canonical_empty_selection_decision();
        decision.expires_after_elapsed_ms = COMPACTION_FEDERATION_MAX_TTL_MS + 1;
        let start = Instant::now();
        assert!(validate_fixture(&decision, start, start).is_err());
    }

    #[test]
    fn expired_request_elapsed_is_rejected_before_return() {
        let decision = canonical_empty_selection_decision();
        let start = Instant::now();
        assert!(validate_fixture(
            &decision,
            start,
            start + Duration::from_millis(COMPACTION_FEDERATION_MAX_TTL_MS + 1),
        )
        .is_err());
    }

    #[test]
    fn independent_requests_with_equal_generation_get_distinct_operation_ids() {
        let first = format!("compact-{}", rightkit_agent_engine_protocol::uuid_v7());
        let second = format!("compact-{}", rightkit_agent_engine_protocol::uuid_v7());
        assert_ne!(first, second);
        assert!(first.starts_with("compact-"));
        assert!(second.starts_with("compact-"));
    }

    #[test]
    fn context_budget_is_bounded_fraction_of_window_never_all_remaining_room() {
        // Default: a tenth of the window, capped.
        assert_eq!(compute_context_budget(100_000, 4_000, 10_000, None), 10_000);
        assert_eq!(
            compute_context_budget(1_000_000, 4_000, 10_000, None),
            12_000
        );
        // Floor for tiny windows, but never above the remaining room.
        assert_eq!(compute_context_budget(4_000, 500, 100, None), 1_000);
        assert_eq!(compute_context_budget(4_000, 500, 3_300, None), 200);
        // Explicit override is honored yet still bounded by remaining room.
        assert_eq!(compute_context_budget(100_000, 0, 0, Some(3_000)), 3_000);
        assert_eq!(compute_context_budget(10_000, 0, 9_500, Some(3_000)), 500);
        // Exhausted window never yields zero.
        assert_eq!(compute_context_budget(10_000, 0, 10_000, None), 1);
    }

    struct FakeTool {
        name: &'static str,
        defer_schema: bool,
    }

    #[async_trait::async_trait]
    impl rightkit_agent_tools::coding_tools::Tool for FakeTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "fake test tool"
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        fn defer_schema_by_default(&self) -> bool {
            self.defer_schema
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &rightkit_agent_tools::coding_tools::ToolContext,
        ) -> rightkit_agent_tools::coding_tools::ToolResult {
            rightkit_agent_tools::coding_tools::ToolResult::ok("ok")
        }
    }

    fn tool_def_names(defs: &[serde_json::Value]) -> Vec<String> {
        defs.iter()
            .filter_map(|def| {
                def.get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(|name| name.as_str())
                    .map(|name| name.to_string())
            })
            .collect()
    }

    #[test]
    fn transcript_counts_keep_result_bytes_gap_separate_from_verdict_counts() {
        let messages = vec![
            TranscriptMessage::Assistant {
                id: "assistant-1".into(),
                created_at: "t".into(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    arguments_json: "{}".into(),
                }],
                pinned: false,
            },
            TranscriptMessage::Tool {
                id: "tool-1".into(),
                created_at: "t".into(),
                tool_call_id: "call-1".into(),
                summary: "ok".into(),
                output: None,
                trust: None,
                is_error: false,
                pinned: false,
                process_id: None,
            },
        ];
        let counts = transcript_tool_counts(&messages)
            .get("read_file")
            .cloned()
            .expect("tool count");
        assert_eq!(counts.invocations, 1);
        assert_eq!(counts.successes, 1);
        assert!(counts.results_complete);
        assert!(!counts.bytes_complete);
        assert_eq!(counts.result_bytes, None);
    }

    #[test]
    fn fallback_attempts_skip_blank_duplicate_and_primary_models() {
        let route = ProviderRoute {
            api_key: "key".into(),
            model_id: "primary".into(),
            base_url: "http://127.0.0.1:9".into(),
            effort: Some("high".into()),
            thinking_budget_tokens: Some(2048),
            disable_thinking: false,
            fallback_models: vec![
                "".into(),
                "primary".into(),
                "fallback-a".into(),
                "fallback-a".into(),
                " fallback-b ".into(),
            ],
            approved_private: false,
        };

        let attempts = fallback_attempt_routes(&route);
        let models: Vec<_> = attempts.iter().map(|r| r.model_id.as_str()).collect();
        assert_eq!(models, vec!["primary", "fallback-a", "fallback-b"]);
        assert!(attempts.iter().all(|r| r.fallback_models.is_empty()));
        assert!(attempts.iter().all(|r| r.effort.as_deref() == Some("high")));
        assert!(attempts
            .iter()
            .all(|r| r.thinking_budget_tokens == Some(2048)));
        assert!(attempts.iter().all(|r| !r.disable_thinking));
    }

    #[test]
    fn provider_switch_changes_image_budget_profile_without_restart() {
        use rightkit_agent_engine_core::image_eviction::ImageProviderProfile;
        let route = Arc::new(Mutex::new(ProviderRoute {
            api_key: "test".into(),
            model_id: "model".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models: Vec::new(),
            approved_private: false,
        }));
        let client = ProviderLlmClient::new_dynamic(route.clone(), Arc::new(ToolRegistry::new()));
        let first = client.image_budget_policy();
        assert_eq!(first.provider_profile, ImageProviderProfile::OpenRouter);
        assert_eq!(first.trigger_bytes, 24 * 1024 * 1024);

        lock_recover(&route, "route").base_url = "https://api.minimax.io/v1".into();
        let second = client.image_budget_policy();
        assert_eq!(second.provider_profile, ImageProviderProfile::MiniMax);
        assert_eq!(second.trigger_bytes, 16 * 1024 * 1024);
        assert!(second.reclaim_target_bytes < second.trigger_bytes);
    }

    #[test]
    fn exact_image_accounting_includes_provider_request_envelope() {
        use rightkit_agent_engine_core::image_eviction::ImageAccountingBasis;
        let route = Arc::new(Mutex::new(ProviderRoute {
            api_key: "test".into(),
            model_id: "vision-model".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models: Vec::new(),
            approved_private: false,
        }));
        let client = ProviderLlmClient::new_dynamic(route, Arc::new(ToolRegistry::new()));
        let messages = vec![TranscriptMessage::User {
            id: "u1".into(),
            created_at: "2026-07-17T00:00:00Z".into(),
            content: serde_json::json!([{
                "type": "image_url",
                "image_url": {"url": "data:image/png;base64,AAAA"}
            }])
            .to_string(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }];
        let accounting = client.image_request_accounting(&messages, &LlmRequestMetadata::default());

        assert_eq!(
            accounting.basis,
            ImageAccountingBasis::OpenAiChatCompletionsJsonV1
        );
        assert!(accounting.body_bytes > 100);
    }

    #[test]
    fn fallback_eligibility_only_covers_provider_unavailability() {
        // 429 is not a delivery proof: no fallback. 5xx and 408/425 may have been processed too.
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Http {
                status: 429,
                body: String::new()
            }
        ));
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Http {
                status: 529,
                body: String::new()
            }
        ));
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Http {
                status: 503,
                body: "overloaded, temporarily unavailable".into()
            }
        ));
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Http {
                status: 408,
                body: String::new()
            }
        ));
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Http {
                status: 425,
                body: String::new()
            }
        ));
        for status in [400u16, 401, 403] {
            assert!(
                provider_error_allows_model_fallback(&ProviderError::Http {
                    status,
                    body: String::new()
                }),
                "{status} is an explicit refusal"
            );
        }
        for status in [404u16, 408, 409, 422, 425, 429, 500, 502, 503, 504, 529] {
            assert!(
                !provider_error_allows_model_fallback(&ProviderError::Http {
                    status,
                    body: "overloaded, temporarily unavailable".into()
                }),
                "{status} gets no fallback"
            );
        }
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::RateLimited {
                retry_after_secs: 1
            }
        ));
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Provider("model temporarily unavailable".into())
        ));
        assert!(provider_error_allows_model_fallback(&ProviderError::Auth));
        // A post-send transport error may have been delivered; a connect failure provably was not.
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Network("connection reset".into())
        ));
        assert!(provider_error_allows_model_fallback(
            &ProviderError::NotDelivered("connection refused".into())
        ));
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::DeliveryUncertain {
                reason: "stream ended".into(),
                delivered_output: false,
            }
        ));
        assert!(!provider_error_allows_model_fallback(&ProviderError::Sse(
            "bad chunk".into()
        )));
        assert!(provider_error_allows_model_fallback(
            &ProviderError::Capacity(CapacityError::CapacityUnavailable {
                route_id: "route-a".into(),
                reason: "capacity profile absent".into(),
            })
        ));
        assert!(provider_error_allows_model_fallback(
            &ProviderError::Capacity(CapacityError::FinalInsertionOverBudget {
                measured_tokens: 101,
                effective_budget_tokens: 100,
            })
        ));
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Capacity(CapacityError::ContinuationReauthorizationRequired)
        ));
        assert!(!provider_error_allows_model_fallback(
            &ProviderError::Authority(
                rightkit_llm::agent_providers::client::RequestAuthorityErrorV1::Denied {
                    reason: "policy".into(),
                }
            )
        ));
    }

    #[test]
    fn successful_active_route_publishes_its_real_compaction_policy() {
        let route = Arc::new(Mutex::new(ProviderRoute {
            api_key: "test".into(),
            model_id: "gpt-5-mini".into(),
            base_url: "https://api.openai.com/v1".into(),
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models: vec![],
            approved_private: false,
        }));
        let (tx, rx) = tokio::sync::watch::channel(
            rightkit_agent_engine_core::CompactionModelPolicy::unknown("initial"),
        );
        let client = ProviderLlmClient::new_dynamic_with_compaction_policy(
            route,
            Arc::new(ToolRegistry::new()),
            tx,
        );

        client.publish_active_compaction_policy("openai/gpt-5.1");

        let policy = rx.borrow().clone();
        assert!(policy.capacity_known);
        assert_eq!(policy.context_window_tokens, Some(400_000));
        assert_eq!(policy.summary_max_output_tokens, Some(32_768));
    }

    #[test]
    fn unknown_route_publishes_visible_unknown_capacity() {
        let policy = compaction_policy_for_model("acme/mystery");
        assert!(!policy.capacity_known);
        assert_eq!(policy.context_window_tokens, None);
        assert_eq!(policy.auto_trigger_tokens(), None);
    }

    #[test]
    fn provider_boundary_removes_orphan_think_closer() {
        let (text, reasoning) = sanitize_provider_content(
            "</think>Visible answer".into(),
            Some("internal reasoning".into()),
            false,
        )
        .expect("visible answer remains valid");

        assert_eq!(text, "Visible answer");
        assert_eq!(reasoning.as_deref(), Some("internal reasoning"));
    }

    #[test]
    fn provider_boundary_rejects_reasoning_only_completion() {
        let error =
            sanitize_provider_content("</think>".into(), Some("internal reasoning".into()), false)
                .expect_err("reasoning-only completion must not silently finish a run");

        assert!(error.contains("no visible content or tool calls"));
    }

    #[tokio::test]
    async fn streamed_content_matches_completed_sanitization_across_chunk_boundaries() {
        let raw: rightkit_llm::agent_providers::ProviderResponseStream =
            Box::pin(futures_util::stream::iter([
                Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Text(
                    "<think>inline".into(),
                )),
                Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Text(
                    " reasoning</think>Visible".into(),
                )),
                Ok(
                    rightkit_llm::agent_providers::ProviderStreamEvent::Reasoning(
                        "provider reasoning".into(),
                    ),
                ),
                Ok(
                    rightkit_llm::agent_providers::ProviderStreamEvent::Finished(Some(
                        "stop".into(),
                    )),
                ),
            ]));
        let mut stream = normalize_provider_stream(raw);
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("synthetic stream should normalize"));
        }

        assert!(matches!(
            events.first(),
            Some(rightkit_llm::agent_providers::ProviderStreamEvent::Text(text)) if text == "Visible"
        ));
        let reasoning: String = events
            .iter()
            .filter_map(|event| match event {
                rightkit_llm::agent_providers::ProviderStreamEvent::Reasoning(text) => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(reasoning, "provider reasoning\n\ninline reasoning");
        assert!(matches!(
            events.last(),
            Some(rightkit_llm::agent_providers::ProviderStreamEvent::Finished(Some(reason))) if reason == "stop"
        ));
    }

    #[tokio::test]
    async fn streamed_visible_text_is_emitted_before_finished() {
        let raw: rightkit_llm::agent_providers::ProviderResponseStream =
            Box::pin(futures_util::stream::iter([
                Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Text(
                    "live".into(),
                )),
                Ok(
                    rightkit_llm::agent_providers::ProviderStreamEvent::Finished(Some(
                        "stop".into(),
                    )),
                ),
            ]));
        let mut stream = normalize_provider_stream(raw);
        assert!(matches!(
            stream.next().await,
            Some(Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Text(text))) if text == "live"
        ));
        assert!(matches!(
            stream.next().await,
            Some(Ok(rightkit_llm::agent_providers::ProviderStreamEvent::Finished(Some(reason))))
                if reason == "stop"
        ));
    }

    #[test]
    fn incremental_normalizer_handles_case_insensitive_split_tags_and_orphans() {
        let mut normalizer = IncrementalContentNormalizer::default();
        assert_eq!(normalizer.push_text("</TH"), "");
        assert_eq!(normalizer.push_text("INK>visible <ThInK>hidden"), "visible");
        assert_eq!(normalizer.push_text("</tH"), "");
        assert_eq!(normalizer.push_text("InK> after"), "  after");
        let (tail, reasoning) = normalizer.finish().expect("visible content is complete");
        assert_eq!(tail, "");
        assert_eq!(reasoning.as_deref(), Some("hidden"));
    }

    #[test]
    fn stream_fallback_stops_after_visible_or_side_effectful_emission() {
        assert!(stream_error_allows_fallback(
            false,
            true,
            &ProviderError::NotDelivered("connection refused".into())
        ));
        assert!(stream_error_allows_fallback(
            false,
            true,
            &ProviderError::Http {
                status: 403,
                body: String::new()
            }
        ));
        assert!(!stream_error_allows_fallback(
            false,
            true,
            &ProviderError::Network("connection reset".into())
        ));
        assert!(!stream_error_allows_fallback(
            false,
            true,
            &ProviderError::Sse("truncated frame".into())
        ));
        assert!(!stream_error_allows_fallback(
            true,
            true,
            &ProviderError::NotDelivered("connection refused".into())
        ));
        assert!(stream_error_allows_fallback(
            false,
            true,
            &ProviderError::Auth
        ));
    }

    #[test]
    fn provider_request_captures_immutable_tool_catalog_generation() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FakeTool {
            name: "tool_search",
            defer_schema: false,
        }));
        registry.register(Box::new(FakeTool {
            name: "plugin_alpha",
            defer_schema: true,
        }));
        let route = Arc::new(Mutex::new(ProviderRoute {
            api_key: "test".into(),
            model_id: "model".into(),
            base_url: "http://127.0.0.1:9".into(),
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models: Vec::new(),
            approved_private: false,
        }));
        let client = ProviderLlmClient::new_dynamic(route, Arc::new(registry));
        let (first_defs, first_meta, first_scoped) = client
            .prepare_turn_metadata(&[], &LlmRequestMetadata::default())
            .unwrap();
        assert_eq!(first_meta.tool_catalog_generation, Some(1));
        assert_eq!(tool_def_names(&first_defs), vec!["tool_search"]);
        let first_digest = first_meta.tool_catalog_digest.clone().unwrap();

        let messages = vec![
            TranscriptMessage::Assistant {
                id: "a".into(),
                created_at: "t".into(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "search-1".into(),
                    name: "tool_search".into(),
                    arguments_json: "{}".into(),
                }],
                pinned: false,
            },
            TranscriptMessage::Tool {
                id: "r".into(),
                created_at: "t".into(),
                tool_call_id: "search-1".into(),
                summary: serde_json::json!([{ "name": "plugin_alpha" }]).to_string(),
                output: None,
                trust: None,
                is_error: false,
                pinned: false,
                process_id: None,
            },
        ];
        let (second_defs, second_meta, second_scoped) = client
            .prepare_turn_metadata(&messages, &LlmRequestMetadata::default())
            .unwrap();
        assert_eq!(second_meta.tool_catalog_generation, Some(2));
        assert_ne!(
            second_meta.tool_catalog_digest.as_deref(),
            Some(first_digest.as_str())
        );
        assert_eq!(
            tool_def_names(&second_defs),
            vec!["plugin_alpha", "tool_search"]
        );
        // The first request owns its schema vector: observing a future
        // generation cannot mutate an already-captured in-flight contract.
        assert_eq!(tool_def_names(&first_defs), vec!["tool_search"]);

        // HCTX-003: lineage is request-scoped, so each request stays
        // attributable even though a later request has since run. The
        // global `request_lineage` slot now names only the SECOND request,
        // which is exactly why closure evidence must not depend on it.
        assert_ne!(first_scoped.request_ordinal, second_scoped.request_ordinal);
        assert_eq!(first_scoped.tool_catalog_generation, 1);
        assert_eq!(second_scoped.tool_catalog_generation, 2);
        assert_eq!(first_scoped.tool_catalog_digest, first_digest);
        assert_ne!(
            first_scoped.tool_catalog_digest,
            second_scoped.tool_catalog_digest
        );

        // Re-reading by request identity returns each request's own lineage,
        // not last-write-wins.
        assert_eq!(
            client.request_lineage_for(first_scoped.request_ordinal),
            Some(first_scoped.clone())
        );
        assert_eq!(
            client.request_lineage_for(second_scoped.request_ordinal),
            Some(second_scoped.clone())
        );
        let ledger = client.lineage_ledger();
        assert_eq!(ledger.len(), 2, "every request must be retained");
        assert_eq!(ledger[0], first_scoped);
        assert_eq!(ledger[1], second_scoped);

        // The convenience slot is last-write-wins and therefore ambiguous.
        assert_eq!(
            client
                .request_lineage_snapshot()
                .and_then(|meta| meta.tool_catalog_generation),
            Some(2),
            "diagnostic slot reflects only the most recent request"
        );
    }

    async fn capture_openai_request(
        mut stream: tokio::net::TcpStream,
        captures: Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>,
        release: Option<Arc<tokio::sync::Notify>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "request closed before headers");
            request.extend_from_slice(&chunk[..n]);
            assert!(
                request.len() <= 1024 * 1024,
                "fixture request exceeds 1 MiB"
            );
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length: ")
                    .or_else(|| line.strip_prefix("Content-Length: "))
            })
            .unwrap()
            .parse::<usize>()
            .unwrap();
        assert!(content_length <= 1024 * 1024, "fixture body exceeds 1 MiB");
        while request.len() < header_end + content_length {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0, "request closed before body");
            request.extend_from_slice(&chunk[..n]);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&request[header_end..header_end + content_length]).unwrap();
        captures.lock().await.push(body);

        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        if let Some(release) = release {
            release.notified().await;
        }
        let payload = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        stream
            .write_all(format!("{:X}\r\n{}\r\n0\r\n\r\n", payload.len(), payload).as_bytes())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "HCTX-003 final qualification: run the live streamed registry-generation driver"]
    async fn qualification_hctx003_live_stream_registry_generation_is_request_scoped() {
        use futures_util::StreamExt;
        use tokio::net::TcpListener;

        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base_url = format!("http://{}", listener.local_addr().unwrap());
            let captures = Arc::new(tokio::sync::Mutex::new(Vec::new()));
            let release_first = Arc::new(tokio::sync::Notify::new());
            let server_captures = captures.clone();
            let server_release = release_first.clone();
            let server = tokio::spawn(async move {
                let mut handlers = Vec::new();
                for request_index in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    let captures = server_captures.clone();
                    let release = (request_index == 0).then(|| server_release.clone());
                    handlers.push(tokio::spawn(capture_openai_request(
                        stream, captures, release,
                    )));
                }
                for handler in handlers {
                    handler.await.unwrap();
                }
            });

            let mut first_registry = ToolRegistry::new();
            first_registry.register(Box::new(FakeTool {
                name: "plugin_alpha",
                defer_schema: false,
            }));
            let client = ProviderLlmClient::new(
                OpenAiCompatClient::new("test-key", "test-model", &base_url),
                Arc::new(first_registry),
            );

            let mut first_stream = client
                .stream_with_metadata(&[], &LlmRequestMetadata::default())
                .await
                .expect("first streamed request starts");
            let first_lineage = client
                .lineage_ledger()
                .first()
                .cloned()
                .expect("first lineage");
            let first_tools_before = {
                let captured = captures.lock().await;
                serde_json::to_vec(&captured[0]["tools"]).expect("first tool schemas serialize")
            };

            let mut second_registry = ToolRegistry::new();
            second_registry.register(Box::new(FakeTool {
                name: "plugin_alpha",
                defer_schema: false,
            }));
            second_registry.register(Box::new(FakeTool {
                name: "plugin_beta",
                defer_schema: false,
            }));
            client.replace_tool_registry(Arc::new(second_registry));

            let mut second_stream = client
                .stream_with_metadata(&[], &LlmRequestMetadata::default())
                .await
                .expect("second streamed request starts");
            let second_lineage = client
                .lineage_ledger()
                .get(1)
                .cloned()
                .expect("second lineage");

            release_first.notify_one();
            for stream in [&mut second_stream, &mut first_stream] {
                let mut text = String::new();
                let mut finished = false;
                while let Some(event) = stream.next().await {
                    match event.expect("provider stream must succeed") {
                        LlmStreamEvent::Text(delta) => text.push_str(&delta),
                        LlmStreamEvent::Finished(reason) => {
                            assert_eq!(reason.as_deref(), Some("stop"));
                            finished = true;
                        }
                        _ => {}
                    }
                }
                assert_eq!(text, "ok", "each request must deliver its streamed text");
                assert!(finished, "each request must finish successfully");
            }
            server.await.unwrap();

            let captured = captures.lock().await;
            assert_eq!(captured.len(), 2);
            assert_eq!(
                tool_def_names(captured[0]["tools"].as_array().unwrap()),
                vec!["plugin_alpha"]
            );
            assert_eq!(
                tool_def_names(captured[1]["tools"].as_array().unwrap()),
                vec!["plugin_alpha", "plugin_beta"]
            );
            assert_eq!(
                serde_json::to_vec(&captured[0]["tools"]).unwrap(),
                first_tools_before,
                "mutating the future registry cannot alter the already-sent schema bytes"
            );
            assert_eq!(first_lineage.tool_catalog_generation, 1);
            assert_eq!(second_lineage.tool_catalog_generation, 2);
            assert_ne!(
                first_lineage.request_ordinal,
                second_lineage.request_ordinal
            );
            assert_eq!(
                client.request_lineage_for(first_lineage.request_ordinal),
                Some(first_lineage.clone()),
                "first request retains its own identity after a future request starts"
            );
            assert_eq!(
                client.request_lineage_for(second_lineage.request_ordinal),
                Some(second_lineage.clone())
            );
            assert_eq!(
                client.lineage_ledger(),
                vec![first_lineage.clone(), second_lineage.clone()]
            );
            assert_ne!(
                first_lineage.tool_catalog_digest,
                second_lineage.tool_catalog_digest
            );
        })
        .await
        .expect("live catalog qualification must finish within 30 seconds");
    }

    #[test]
    fn deferred_tool_schemas_are_not_bound_without_scope() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FakeTool {
            name: "tool_search",
            defer_schema: false,
        }));
        registry.register(Box::new(FakeTool {
            name: "plugin_alpha",
            defer_schema: true,
        }));

        let defs = tool_defs_for_messages(&registry, &[]);

        assert_eq!(tool_def_names(&defs), vec!["tool_search"]);
    }

    #[test]
    fn tool_search_result_scopes_deferred_tool_schema() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FakeTool {
            name: "tool_search",
            defer_schema: false,
        }));
        registry.register(Box::new(FakeTool {
            name: "plugin_alpha",
            defer_schema: true,
        }));
        registry.register(Box::new(FakeTool {
            name: "plugin_beta",
            defer_schema: true,
        }));
        let messages = vec![
            TranscriptMessage::Assistant {
                id: "ast".into(),
                created_at: "t".into(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "search-1".into(),
                    name: "tool_search".into(),
                    arguments_json: serde_json::json!({"query": "alpha"}).to_string(),
                }],
                pinned: false,
            },
            TranscriptMessage::Tool {
                id: "tool".into(),
                created_at: "t".into(),
                tool_call_id: "search-1".into(),
                summary: serde_json::json!([
                    {"name": "plugin_alpha", "description": "Alpha plugin"}
                ])
                .to_string(),
                output: None,
                trust: None,
                is_error: false,
                process_id: None,
                pinned: false,
            },
        ];

        let defs = tool_defs_for_messages(&registry, &messages);

        // P0-C 2b: canonical name order, not registration order — "plugin_alpha"
        // sorts before "tool_search". Lead-authorized update (2b's canonical
        // ordering supersedes registration order; DoD #3, byte-identical
        // serialization across turns, cannot hold without it). Which tools are
        // scoped is unchanged; only the incidental order is updated.
        assert_eq!(tool_def_names(&defs), vec!["plugin_alpha", "tool_search"]);
    }

    // ── Deferred/lazy tool loading (par §#157) ──────────────────────────────

    #[test]
    fn lazy_loading_off_ships_full_prelude_regression_guard() {
        let registry = ToolRegistry::build_default();
        assert!(!registry.lazy_loading_enabled());

        let defs = tool_defs_for_messages(&registry, &[]);

        // Every non-deferred (defer_schema_by_default == false) native tool
        // must still be in the prelude when lazy mode is off, matching the
        // pre-lane behavior exactly.
        let expected: HashSet<String> = registry
            .all()
            .iter()
            .filter(|tool| !tool.defer_schema_by_default())
            .map(|tool| tool.name().to_string())
            .collect();
        assert_eq!(
            tool_def_names(&defs).into_iter().collect::<HashSet<_>>(),
            expected
        );
    }

    #[test]
    fn canonical_tool_prelude_never_duplicates_function_declarations() {
        // Adapted-to-Rust from Gemini CLI local-executor.test.ts
        // `should produce NO duplicate function declarations when tools are
        // DeclarativeTool instances`: RightKit projects one canonical
        // registry into provider JSON rather than building an isolated TS
        // ToolRegistry per executor.
        let registry = ToolRegistry::build_default();
        let names = tool_def_names(&tool_defs_for_messages(&registry, &[]));
        let unique: HashSet<&str> = names.iter().map(String::as_str).collect();

        assert_eq!(unique.len(), names.len(), "tool schemas must be unique");
    }

    #[test]
    fn lazy_loading_on_shrinks_prelude_below_full_registry_count() {
        let full = ToolRegistry::build_default();
        let full_count = full.all().len();

        let lazy = ToolRegistry::build_default().with_lazy_loading(true);
        assert!(lazy.lazy_loading_enabled());

        let defs = tool_defs_for_messages(&lazy, &[]);

        // The whole point of the lane: prelude tool count < full registry
        // count once lazy mode is on and nothing has been scoped yet.
        assert!(
            defs.len() < full_count,
            "expected lazy prelude ({}) to be smaller than full registry ({full_count})",
            defs.len()
        );
        // tool_search itself must always be present — it's the only way to
        // discover everything else.
        assert!(tool_def_names(&defs).contains(&"tool_search".to_string()));
    }

    #[test]
    fn lazy_loading_on_keeps_core_tools_in_prelude() {
        let lazy = ToolRegistry::build_default().with_lazy_loading(true);
        let defs = tool_defs_for_messages(&lazy, &[]);
        let names: HashSet<String> = tool_def_names(&defs).into_iter().collect();

        for core in [
            "read_file",
            "write_file",
            "edit_file",
            "shell",
            "search",
            "tool_search",
        ] {
            assert!(
                names.contains(core),
                "{core} should stay in the lazy prelude"
            );
        }
    }

    #[test]
    fn lazy_loading_defers_non_core_native_tool_until_scoped_by_tool_search() {
        let lazy = ToolRegistry::build_default().with_lazy_loading(true);

        // github is a real native tool, not a plugin — defer_schema_by_default()
        // is false for it, so before this lane it always shipped its schema.
        let before = tool_defs_for_messages(&lazy, &[]);
        assert!(!tool_def_names(&before).contains(&"github".to_string()));

        let messages = vec![
            TranscriptMessage::Assistant {
                id: "ast".into(),
                created_at: "t".into(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: vec![ToolCall {
                    id: "search-1".into(),
                    name: "tool_search".into(),
                    arguments_json: serde_json::json!({"query": "github"}).to_string(),
                }],
                pinned: false,
            },
            TranscriptMessage::Tool {
                id: "tool".into(),
                created_at: "t".into(),
                tool_call_id: "search-1".into(),
                summary: serde_json::json!([
                    {"name": "github", "description": "Interact with GitHub."}
                ])
                .to_string(),
                output: None,
                trust: None,
                is_error: false,
                process_id: None,
                pinned: false,
            },
        ];
        let after = tool_defs_for_messages(&lazy, &messages);
        assert!(tool_def_names(&after).contains(&"github".to_string()));
    }

    #[tokio::test]
    async fn deferred_tool_still_executes_when_dispatched_by_name() {
        // A tool being withheld from the prelude must not stop it from being
        // dispatchable by name once the model calls it — deferral is a
        // prompt-serialization concern only, never a capability change.
        let lazy = ToolRegistry::build_default().with_lazy_loading(true);
        let defs = tool_defs_for_messages(&lazy, &[]);
        assert!(!tool_def_names(&defs).contains(&"github".to_string()));

        let tool = lazy.get("github").expect("github must remain registered");
        let dir = std::env::temp_dir();
        let ctx = rightkit_agent_tools::coding_tools::ToolContext::new(
            "s1",
            &dir,
            rightkit_agent_policy::PermissionMode::Bypass,
            rightkit_agent_policy::WorkspaceTrust::Trusted,
        );
        // Missing required args -> expect a well-formed error, not a panic
        // or "tool not found" — proves dispatch actually reached the tool.
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_error);
        assert!(!result.content.to_lowercase().contains("not found"));
    }

    #[test]
    fn core_toolset_classification_matches_registry_prelude_membership() {
        let registry = ToolRegistry::new().with_lazy_loading(true);
        for name in rightkit_agent_tools::coding_tools::registry::CORE_TOOL_NAMES {
            assert!(registry.is_prelude_tool(name));
        }
        assert!(!registry.is_prelude_tool("github"));
        assert!(!registry.is_prelude_tool("web_search"));
    }

    // ── P0-C 2c/2d: cache-prefix linkage + telemetry ────────────────────
    //
    // `complete_with_metadata` computes and observes the combined cache
    // prefix key *before* it dispatches the network call, so these tests
    // exercise it fully even though the Dynamic route below is
    // unreachable — the network attempt fails, but the prefix/promotion
    // observation already happened and was recorded through
    // `rightkit_logs::agent_telemetry::prompt_overhead`. Each test uses a unique
    // `model_id` tag so it can filter the process-global telemetry sink
    // down to just its own rows, safe under parallel test execution.

    fn unreachable_dynamic_client(model_tag: &str, tools: Arc<ToolRegistry>) -> ProviderLlmClient {
        let route = Arc::new(Mutex::new(ProviderRoute {
            api_key: "test".into(),
            model_id: model_tag.into(),
            base_url: "http://127.0.0.1:9".into(),
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models: Vec::new(),
            approved_private: false,
        }));
        ProviderLlmClient::new_dynamic(route, tools)
    }

    fn cache_prefix_events_for(
        model_tag: &str,
    ) -> Vec<rightkit_logs::agent_telemetry::prompt_overhead::CachePrefixEvent> {
        rightkit_logs::agent_telemetry::prompt_overhead::cache_prefix_events()
            .into_iter()
            .filter(|event| event.model.as_deref() == Some(model_tag))
            .collect()
    }

    fn pinned_system_messages() -> Vec<TranscriptMessage> {
        vec![TranscriptMessage::System {
            id: "sys".into(),
            created_at: "t".into(),
            content: "stable identity".into(),
            pinned: true,
        }]
    }

    #[tokio::test]
    async fn cache_prefix_stable_across_a_no_promotion_turn() {
        let model_tag = "p0c-prefix-stable";
        let client = unreachable_dynamic_client(model_tag, Arc::new(ToolRegistry::new()));
        let metadata = LlmRequestMetadata {
            stable_prefix_key: Some("stable-key-v1".into()),
            max_output_tokens: None,
            ..LlmRequestMetadata::default()
        };
        let messages = pinned_system_messages();

        // Two turns, identical messages and base stable_prefix_key, no
        // tool_search anywhere — the combined prefix must not change.
        let _ = client.complete_with_metadata(&messages, &metadata).await;
        let _ = client.complete_with_metadata(&messages, &metadata).await;

        let events = cache_prefix_events_for(model_tag);
        assert_eq!(events.len(), 2);
        assert!(
            events[0].prefix_changed,
            "first observation is always a change (Fresh)"
        );
        assert_eq!(events[0].prefix_generation, 1);
        assert!(
            !events[1].prefix_changed,
            "identical stable prefix + tool set on turn 2 must not change the prefix"
        );
        assert_eq!(events[1].prefix_generation, 1);
        assert!(events[1].prefix_change_reason.is_none());
    }

    #[tokio::test]
    async fn cache_prefix_changes_with_attributable_reason_on_promotion_turn() {
        let model_tag = "p0c-prefix-promotion";
        // A lazy-loading registry with a real deferred, non-core tool
        // (`github`) so a tool_search result can promote it.
        let registry = Arc::new(ToolRegistry::build_default().with_lazy_loading(true));
        let client = unreachable_dynamic_client(model_tag, registry);
        let metadata = LlmRequestMetadata {
            stable_prefix_key: Some("stable-key-v1".into()),
            max_output_tokens: None,
            ..LlmRequestMetadata::default()
        };

        // Turn 1: no promotion.
        let turn1 = pinned_system_messages();
        let _ = client.complete_with_metadata(&turn1, &metadata).await;

        // Turn 2: same base stable_prefix_key, but tool_search has now
        // scoped `github` into visibility — the tool_defs block changes,
        // so the combined prefix must change too, with an attributable
        // "tool-promotion:..." reason.
        let mut turn2 = pinned_system_messages();
        turn2.push(TranscriptMessage::Assistant {
            id: "ast".into(),
            created_at: "t".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "search-1".into(),
                name: "tool_search".into(),
                arguments_json: serde_json::json!({"query": "github"}).to_string(),
            }],
            pinned: false,
        });
        turn2.push(TranscriptMessage::Tool {
            id: "tool".into(),
            created_at: "t".into(),
            tool_call_id: "search-1".into(),
            summary:
                serde_json::json!([{"name": "github", "description": "Interact with GitHub."}])
                    .to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        });
        let _ = client.complete_with_metadata(&turn2, &metadata).await;

        let events = cache_prefix_events_for(model_tag);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].prefix_generation, 1);
        assert!(events[1].prefix_changed);
        assert_eq!(events[1].prefix_generation, 2);
        let reason = events[1]
            .prefix_change_reason
            .as_deref()
            .expect("a changed prefix must carry a reason");
        assert!(
            reason.starts_with("tool-promotion:"),
            "expected an attributable tool-promotion reason, got {reason:?}"
        );
        assert!(reason.contains("github"));
        assert_eq!(events[1].promoted_count, 1);
        assert_eq!(events[1].promotion_events_this_session, 1);
    }

    #[tokio::test]
    async fn promoted_tool_stays_promoted_for_the_session_lifetime() {
        let model_tag = "p0c-promotion-monotonic";
        let registry = Arc::new(ToolRegistry::build_default().with_lazy_loading(true));
        let client = unreachable_dynamic_client(model_tag, registry);
        let metadata = LlmRequestMetadata {
            stable_prefix_key: Some("stable-key-v1".into()),
            max_output_tokens: None,
            ..LlmRequestMetadata::default()
        };

        let mut history = pinned_system_messages();
        history.push(TranscriptMessage::Assistant {
            id: "ast".into(),
            created_at: "t".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "search-1".into(),
                name: "tool_search".into(),
                arguments_json: serde_json::json!({"query": "github"}).to_string(),
            }],
            pinned: false,
        });
        history.push(TranscriptMessage::Tool {
            id: "tool".into(),
            created_at: "t".into(),
            tool_call_id: "search-1".into(),
            summary:
                serde_json::json!([{"name": "github", "description": "Interact with GitHub."}])
                    .to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        });
        let _ = client.complete_with_metadata(&history, &metadata).await;
        {
            let promoted = lock_recover(&client.promoted_tools, "promoted tool state");
            assert!(promoted.promoted_names().contains("github"));
        }

        // A later turn whose message window no longer carries the
        // tool_search call/result (e.g. compacted away) must still keep
        // `github` promoted — promotion is session-lifetime, not
        // recomputed from the live window each turn.
        let later_turn = pinned_system_messages();
        let _ = client.complete_with_metadata(&later_turn, &metadata).await;
        {
            let promoted = lock_recover(&client.promoted_tools, "promoted tool state");
            assert!(
                promoted.promoted_names().contains("github"),
                "promotion must survive across turns even once the tool_search evidence scrolls out of the window"
            );
        }
    }

    /// CR-33: `LlmClient::promoted_tool_state_snapshot()` — the seam
    /// `engine_loop.rs` uses to build the CacheTracker digest — must return
    /// the SAME accumulated promoted set this client puts on the wire, not
    /// a fresh/empty one. Regressing this would silently desynchronize the
    /// wire prefix from the digest that is supposed to explain its change.
    #[tokio::test]
    async fn promoted_tool_state_snapshot_matches_the_accumulated_wire_state() {
        let model_tag = "cr33-promotion-snapshot";
        let registry = Arc::new(ToolRegistry::build_default().with_lazy_loading(true));
        let client = unreachable_dynamic_client(model_tag, registry);
        let metadata = LlmRequestMetadata {
            stable_prefix_key: Some("stable-key-v1".into()),
            max_output_tokens: None,
            ..LlmRequestMetadata::default()
        };

        // Before any turn: both the wire-accumulated state and the snapshot
        // seam agree there is nothing promoted yet.
        assert!(
            rightkit_agent_engine_core::llm::LlmClient::promoted_tool_state_snapshot(&client)
                .promoted_names()
                .is_empty()
        );

        let mut history = pinned_system_messages();
        history.push(TranscriptMessage::Assistant {
            id: "ast".into(),
            created_at: "t".into(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: vec![ToolCall {
                id: "search-1".into(),
                name: "tool_search".into(),
                arguments_json: serde_json::json!({"query": "github"}).to_string(),
            }],
            pinned: false,
        });
        history.push(TranscriptMessage::Tool {
            id: "tool".into(),
            created_at: "t".into(),
            tool_call_id: "search-1".into(),
            summary:
                serde_json::json!([{"name": "github", "description": "Interact with GitHub."}])
                    .to_string(),
            output: None,
            trust: None,
            is_error: false,
            process_id: None,
            pinned: false,
        });
        let _ = client.complete_with_metadata(&history, &metadata).await;

        let snapshot =
            rightkit_agent_engine_core::llm::LlmClient::promoted_tool_state_snapshot(&client);
        let wire_accumulated = {
            let promoted = lock_recover(&client.promoted_tools, "promoted tool state");
            promoted.promoted_names().clone()
        };
        assert_eq!(
            snapshot.promoted_names(),
            &wire_accumulated,
            "the CacheTracker-facing snapshot must equal the exact set serialized onto the wire"
        );
        assert!(snapshot.promoted_names().contains("github"));
    }

    // ── R2 (CR-09/CR-11/CR-12/CR-34): streaming budget/route parity ────────

    fn priced_dynamic_route(model_id: &str, fallback_models: Vec<String>) -> ProviderRoute {
        ProviderRoute {
            api_key: "test-key".into(),
            model_id: model_id.into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models,
            approved_private: false,
        }
    }

    /// DoD #1: a completed turn and a streamed turn, given the same usage
    /// figures on the same billed route, must move a lineage budget by
    /// exactly the same amount. `complete_with_metadata` reconciles inline
    /// (via `priced_turn_cost_microusd`); `stream_with_metadata` reconciles
    /// through `StreamBudgetGuard::reconcile_usage` on the `Usage` event.
    /// Both must land on the identical remaining balance.
    #[test]
    fn streamed_and_completed_turns_produce_the_same_budget_ledger_effect() {
        let route = priced_dynamic_route("openai/gpt-5.1", vec![]);
        assert!(rightkit_llm::agent_providers::is_priced_model(
            &route.model_id
        ));

        let completed_budget = rightkit_agent_engine_core::LineageBudget::from_usd(Some(1.0), 0.0);
        let completed_reservation = completed_budget.reserve_remaining().unwrap();
        let completed_cost = priced_turn_cost_microusd(&route, 1_000, 500, 0, 0);
        completed_reservation.reconcile_microusd(completed_cost);

        let streamed_budget = rightkit_agent_engine_core::LineageBudget::from_usd(Some(1.0), 0.0);
        let streamed_reservation = streamed_budget.reserve_remaining().unwrap();
        let mut guard = StreamBudgetGuard {
            reservation: Some(streamed_reservation),
            route: route.clone(),
        };
        guard.reconcile_usage(1_000, 500, 0, 0);

        assert_eq!(
            completed_budget.remaining_microusd(),
            streamed_budget.remaining_microusd(),
            "a streamed and a completed turn with identical usage must leave the same remaining balance"
        );
        assert_eq!(
            completed_budget.spent_microusd(),
            streamed_budget.spent_microusd()
        );
    }

    /// DoD #2: `attempt_dynamic_route` — the single route+budget attempt
    /// loop shared by `complete_with_metadata` and `stream_with_metadata` —
    /// must fall back to the second route when the first fails with an
    /// error `provider_error_allows_model_fallback` classifies as transient.
    #[tokio::test]
    async fn forced_first_route_failure_attempts_the_second_route() {
        let route = priced_dynamic_route("primary-model", vec!["fallback-model".into()]);
        let shared_route = Arc::new(Mutex::new(route));
        let calls = std::sync::atomic::AtomicUsize::new(0);

        let (value, attempt_route, _reservation) = attempt_dynamic_route(
            None,
            &shared_route,
            None,
            None,
            |_client: OpenAiCompatClient| {
                let attempt_index = calls.fetch_add(1, Ordering::AcqRel);
                async move {
                    if attempt_index == 0 {
                        Err(ProviderError::NotDelivered("connection refused".into()))
                    } else {
                        Ok(attempt_index)
                    }
                }
            },
        )
        .await
        .expect("second route must succeed after the first is classified as retryable");

        assert_eq!(
            value, 1,
            "the second attempt must be the one that succeeded"
        );
        assert_eq!(attempt_route.model_id, "fallback-model");
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }

    /// M17: every uncertain failure is exactly one paid request. The attempt loop is given a second
    /// route and must never run `op` for it; a provably undelivered or explicitly refused request
    /// does run both.
    #[tokio::test]
    async fn an_uncertain_failure_is_exactly_one_paid_request() {
        let uncertain = [
            ProviderError::Http {
                status: 408,
                body: String::new(),
            },
            ProviderError::Http {
                status: 425,
                body: String::new(),
            },
            ProviderError::Http {
                status: 429,
                body: String::new(),
            },
            ProviderError::Http {
                status: 500,
                body: "overloaded".into(),
            },
            ProviderError::Http {
                status: 503,
                body: "temporarily unavailable".into(),
            },
            ProviderError::Http {
                status: 529,
                body: String::new(),
            },
            ProviderError::RateLimited {
                retry_after_secs: 1,
            },
            ProviderError::Network("connection reset after send".into()),
            ProviderError::Sse("stream cut".into()),
            ProviderError::DeliveryUncertain {
                reason: "stream ended".into(),
                delivered_output: false,
            },
            ProviderError::Provider("model unavailable".into()),
        ];
        for error in uncertain {
            let shared_route = Arc::new(Mutex::new(priced_dynamic_route(
                "primary-model",
                vec!["fallback-model".into()],
            )));
            let calls = AtomicUsize::new(0);
            let text = error.to_string();
            let result = attempt_dynamic_route(
                None,
                &shared_route,
                None,
                None,
                |_client: OpenAiCompatClient| {
                    calls.fetch_add(1, Ordering::AcqRel);
                    let error = match &error {
                        ProviderError::Http { status, body } => ProviderError::Http {
                            status: *status,
                            body: body.clone(),
                        },
                        ProviderError::RateLimited { retry_after_secs } => {
                            ProviderError::RateLimited {
                                retry_after_secs: *retry_after_secs,
                            }
                        }
                        ProviderError::Network(t) => ProviderError::Network(t.clone()),
                        ProviderError::Sse(t) => ProviderError::Sse(t.clone()),
                        ProviderError::Provider(t) => ProviderError::Provider(t.clone()),
                        _ => ProviderError::DeliveryUncertain {
                            reason: "stream ended".into(),
                            delivered_output: false,
                        },
                    };
                    async move { Err::<usize, _>(error) }
                },
            )
            .await;
            assert!(result.is_err(), "{text}");
            assert_eq!(
                calls.load(Ordering::Acquire),
                1,
                "{text}: exactly one paid request, no fallback"
            );
        }
        // Provably not delivered, or explicitly refused: the next route is tried.
        for status in [400u16, 401, 403] {
            let shared_route = Arc::new(Mutex::new(priced_dynamic_route(
                "primary-model",
                vec!["fallback-model".into()],
            )));
            let calls = AtomicUsize::new(0);
            let _ = attempt_dynamic_route(
                None,
                &shared_route,
                None,
                None,
                |_client: OpenAiCompatClient| {
                    calls.fetch_add(1, Ordering::AcqRel);
                    async move {
                        Err::<usize, _>(ProviderError::Http {
                            status,
                            body: String::new(),
                        })
                    }
                },
            )
            .await;
            assert_eq!(calls.load(Ordering::Acquire), 2, "{status}");
        }
    }

    /// DoD #3: an aborted mid-stream turn — one that never reaches a `Usage`
    /// event, so `StreamBudgetGuard::reconcile_usage` is never called — must
    /// not leave the reservation dangling. Dropping the guard while it still
    /// holds a reservation must release the full held amount back to the
    /// budget (via `LineageReservation`'s own `Drop`), never zero and never
    /// double-released.
    #[test]
    fn aborted_mid_stream_turn_leaves_no_dangling_reservation() {
        let route = priced_dynamic_route("openai/gpt-5.1", vec![]);
        let budget = rightkit_agent_engine_core::LineageBudget::from_usd(Some(1.0), 0.0);
        assert_eq!(budget.remaining_microusd(), Some(1_000_000));

        let reservation = budget.reserve_remaining().unwrap();
        assert_eq!(
            budget.remaining_microusd(),
            Some(0),
            "reserving takes the whole remaining balance"
        );
        let guard = StreamBudgetGuard {
            reservation: Some(reservation),
            route,
        };

        // The stream is aborted before any `Usage` event arrives — no call
        // to `reconcile_usage` ever happens. Dropping the guard here stands
        // in for the consumer dropping the stream mid-turn.
        drop(guard);

        assert_eq!(
            budget.remaining_microusd(),
            Some(1_000_000),
            "an aborted stream must release its full reservation, not leak it"
        );
        assert_eq!(budget.spent_microusd(), 0);
    }

    // ── Dynamic route-update admission ─────────────────────────────────
    //
    // The legacy `Dynamic` inner previously had no `inference_active_requests`
    // guard (unlike `Direct`), so idle compaction could dispatch
    // a provider request while `live_turn_handle` was inactive and race a
    // PATCH route update. `with_route_admission` installs the session's
    // `route_update_gate` + `inference_active_requests` on the client: every
    // request registers under the gate before route capture, then keeps the
    // counter non-zero for the whole complete call / returned-stream
    // lifetime. These tests drive a real `ProviderLlmClient` against local
    // fake listeners only — no external network.

    fn admitted_dynamic_client(
        base_url: &str,
    ) -> (
        ProviderLlmClient,
        Arc<tokio::sync::Mutex<()>>,
        Arc<AtomicUsize>,
    ) {
        let route = Arc::new(Mutex::new(ProviderRoute {
            api_key: "test".into(),
            // Loaded capacity metadata is required before transport sends.
            // An invented model fails before reaching the hanging listener.
            model_id: "moonshotai/kimi-k2.6".into(),
            base_url: base_url.into(),
            effort: None,
            thinking_budget_tokens: None,
            disable_thinking: false,
            fallback_models: Vec::new(),
            approved_private: true,
        }));
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let active = Arc::new(AtomicUsize::new(0));
        let client = ProviderLlmClient::new_dynamic(route, Arc::new(ToolRegistry::new()))
            .with_route_admission(gate.clone(), active.clone());
        (client, gate, active)
    }

    fn admission_test_messages() -> Vec<TranscriptMessage> {
        vec![TranscriptMessage::User {
            id: "u1".into(),
            created_at: "t".into(),
            content: "hi".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }]
    }

    /// Accepts connections and holds them open forever, never answering —
    /// keeps the provider request deterministically in-flight.
    async fn hanging_provider() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local fake");
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        (format!("http://{addr}"), task)
    }

    /// Answers with SSE response headers then holds the connection open with
    /// no chunks — `stream_with_metadata` returns an open stream that emits
    /// nothing until dropped.
    async fn open_ended_sse_provider() -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local fake");
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((mut socket, _)) = listener.accept().await {
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
                    )
                    .await;
                held.push(socket);
            }
        });
        (format!("http://{addr}"), task)
    }

    async fn wait_for_active(active: &AtomicUsize, expected: usize, context: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while active.load(Ordering::Acquire) != expected {
            assert!(
                Instant::now() < deadline,
                "inference_active_requests never reached {expected}: {context}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The admission helper itself: `None` preserves legacy behavior (no
    /// gate, no counter); `Some` marks the counter while the returned guard
    /// lives and releases the async gate immediately.
    #[tokio::test]
    async fn route_admission_guard_tracks_counter_for_its_lifetime() {
        assert!(acquire_dynamic_route_admission(&None).await.is_none());

        let gate = Arc::new(tokio::sync::Mutex::new(()));
        let active = Arc::new(AtomicUsize::new(0));
        let admission = Some(DynamicRouteAdmission {
            gate: gate.clone(),
            active_requests: active.clone(),
        });
        let guard = acquire_dynamic_route_admission(&admission).await;
        assert!(guard.is_some());
        assert_eq!(active.load(Ordering::Acquire), 1);
        assert!(
            gate.try_lock().is_ok(),
            "the async gate must be released immediately after registration"
        );
        drop(guard);
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    /// A request that fails must still decrement the counter — the guard is
    /// scoped to the attempt, not to success.
    #[tokio::test]
    async fn failed_dynamic_request_releases_active_request_guard() {
        let (client, _gate, active) = admitted_dynamic_client("http://127.0.0.1:9");
        let result = client
            .complete_with_metadata(&admission_test_messages(), &LlmRequestMetadata::default())
            .await;
        assert!(result.is_err(), "unreachable provider must fail");
        assert_eq!(
            active.load(Ordering::Acquire),
            0,
            "a failed request must not leave the active counter elevated"
        );
    }

    /// The decisive race: while the updater holds `route_update_gate`
    /// (busy check through durable commit), a Dynamic request must not even
    /// capture the route — it waits on the gate. Once the gate releases, the
    /// request registers, and from then on the updater's
    /// `inference_active_requests > 0` check refuses any new update.
    #[tokio::test]
    async fn held_route_update_gate_blocks_dynamic_route_capture() {
        let (base_url, _server) = hanging_provider().await;
        let (client, gate, active) = admitted_dynamic_client(&base_url);

        let update_guard = gate.lock().await;
        let request = tokio::spawn({
            let messages = admission_test_messages();
            async move { client.complete(&messages).await }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            active.load(Ordering::Acquire),
            0,
            "request must wait on the route-update gate before route capture"
        );

        drop(update_guard);
        wait_for_active(&active, 1, "request registers once gate releases").await;

        // Updater's view while the request is in-flight: the gate is free
        // (the request must not hold it across the network call) but the
        // counter refuses the update.
        assert!(
            gate.try_lock().is_ok(),
            "gate must be free during the request"
        );
        assert!(
            active.load(Ordering::Acquire) > 0,
            "in-flight request must be visible to the updater's busy check"
        );

        request.abort();
        wait_for_active(&active, 0, "aborted request releases the counter").await;
    }

    /// The stream path holds the active-request guard inside the returned
    /// stream wrapper: dropping the stream — before any event, matching the
    /// cancel path — must release the counter.
    #[tokio::test]
    async fn dropped_dynamic_stream_releases_active_request_guard() {
        let (base_url, _server) = open_ended_sse_provider().await;
        let (client, gate, active) = admitted_dynamic_client(&base_url);

        let stream = client
            .stream_with_metadata(&admission_test_messages(), &LlmRequestMetadata::default())
            .await
            .expect("SSE headers should establish the stream");
        wait_for_active(&active, 1, "stream establishment marks the request active").await;
        assert!(
            gate.try_lock().is_ok(),
            "gate must be free once the stream is live"
        );

        drop(stream);
        wait_for_active(&active, 0, "dropping the stream releases the counter").await;
    }

    /// A stream-establishment failure (unreachable provider) must also
    /// release the counter — the guard's Drop runs on every exit path.
    #[tokio::test]
    async fn failed_dynamic_stream_releases_active_request_guard() {
        let (client, _gate, active) = admitted_dynamic_client("http://127.0.0.1:9");
        let result = client
            .stream_with_metadata(&admission_test_messages(), &LlmRequestMetadata::default())
            .await;
        assert!(result.is_err(), "unreachable provider must fail");
        assert_eq!(active.load(Ordering::Acquire), 0);
    }
}
