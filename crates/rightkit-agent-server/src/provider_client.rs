//! Glue between `{app}-providers` and the engine's `LlmClient` trait.
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
use rightkit_agent_coding_tools::registry::{
    tool_defs_for_messages_checked, PromotedToolState, ToolCatalogGenerationTracker,
    ToolPromotionEvent,
};
use rightkit_agent_coding_tools::ToolRegistry;
use rightkit_agent_engine_core::llm::{
    LlmClient, LlmRequestMetadata, LlmResponse, LlmResponseStream, LlmStreamEvent,
};
use rightkit_agent_engine_protocol::TranscriptMessage;
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
    CapacityError, PreparedModalityV1, PreparedProviderRequestV1, PreparedRequestGateV1,
};
use rightkit_llm::agent_providers::{ExtensionProviderRegistry, ExtensionRegistrationPolicy};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Context already pulled for one session's current task. Later model
/// iterations of the same task reuse it, so the prompt prefix stays
/// byte-stable and the Membrane CLI is called once per task, not once per
/// iteration.
#[derive(Clone)]
struct PulledTaskContext {
    /// sha256 over session, task digest, workspace root, model, window and reserve: a cached
    /// entry is reused only when every part is equal.
    key: String,
    /// Fully formed `<membrane_context>` envelope; `None` when Membrane had
    /// nothing or context is off.
    envelope: Option<String>,
}

mod workspace_memory;

/// Upper bound on one Membrane CLI context call. A turn never waits longer.
const CONTEXT_PULL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Host-owned synchronous authority. It pulls task context through Pulse's
/// Membrane CLI (`{app}-membrane-cli`) and injects it into the final user
/// message. Pulse absent or any CLI failure means no context for the request,
/// never a failed turn.
#[derive(Clone)]
pub(crate) struct HostRequestAuthority {
    state: crate::api::DaemonState,
    pulled: Arc<Mutex<BTreeMap<String, PulledTaskContext>>>,
    /// authorization id -> sha256 of the exact context envelope serialized
    /// into that authorized request; joins H9/H10 evidence to delivered bytes.
    delivered: Arc<Mutex<BTreeMap<String, String>>>,
}

pub(crate) struct CompactionDecisionProvider {
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
    pub(crate) fn new(repository_id: impl Into<String>) -> Self {
        Self {
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
fn context_backend() -> Box<dyn rightkit_memory_client::membrane::MembraneBackend> {
    match rightkit_memory_client::backend::MembraneHandle::shared().verified_cli() {
        Some(cli) => Box::new(cli.with_timeout(CONTEXT_PULL_TIMEOUT)),
        None => Box::new(rightkit_memory_client::membrane::Unavailable),
    }
}

impl HostRequestAuthority {
    pub(crate) fn new(state: crate::api::DaemonState) -> Self {
        Self {
            state,
            pulled: Arc::new(Mutex::new(BTreeMap::new())),
            delivered: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn direct_context(&self) -> bool {
        self.state.uses_direct_context()
    }

    /// Kept for host wiring; the MCP bridge no longer takes part in context.
    pub(crate) fn set_session_mcp(&self, _mcp: Arc<rightkit_mcp::agent_mcp::McpManager>) {}

    /// Make the reason a request carries no Membrane context visible in the
    /// session instead of only logging it.
    fn record_context_off(&self, identity: &ProviderRequestIdentityV1, reason: &str) {
        self.record_context_observation(
            identity,
            "context_degraded",
            &format!("Context off for this request: {reason}"),
            reason,
            None,
        );
    }

    /// Publish one context observation. `membrane` is what Membrane itself said about the pull
    /// (its retrieval `reason` and `detail`); they ride in the fields as `membrane_reason` and
    /// `membrane_detail` (the detail as JSON text) so a reader sees why Membrane chose what it did.
    fn record_context_observation(
        &self,
        identity: &ProviderRequestIdentityV1,
        kind: &str,
        summary: &str,
        reason: &str,
        membrane: Option<(&Option<String>, &serde_json::Value)>,
    ) {
        if kind == "context_degraded" {
            tracing::warn!(
                session_id = %identity.session_id,
                reason,
                "context off for this request: no context injected"
            );
        }
        if let Some(live) = self.state.get_session(&identity.session_id) {
            let conduit = live.conduit.clone();
            let seq = conduit.next_seq();
            let now = rightkit_agent_engine_protocol::time::now_iso();
            let mut fields = std::collections::HashMap::new();
            fields.insert("reason".to_string(), reason.to_string());
            if let Some((membrane_reason, membrane_detail)) = membrane {
                if let Some(membrane_reason) = membrane_reason.as_deref() {
                    fields.insert("membrane_reason".to_string(), membrane_reason.to_string());
                }
                if !membrane_detail.is_null() {
                    fields.insert("membrane_detail".to_string(), membrane_detail.to_string());
                }
            }
            conduit.publish(
                rightkit_agent_engine_protocol::ProtocolEvent::OrchestrationObservationRecorded {
                    session_id: identity.session_id.clone(),
                    seq,
                    created_at: now.clone(),
                    observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                        id: if kind == "context_degraded" {
                            format!("context-empty-{}", identity.request_id)
                        } else {
                            format!("{}-{}", kind.replace('_', "-"), identity.request_id)
                        },
                        kind: kind.into(),
                        subject: identity.session_id.clone(),
                        summary: summary.to_string(),
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
        if budget == Some(0) {
            // Rule 11: no room left in the window, so nothing is pulled (never `--budget 1`).
            self.record_context_off(
                &input.identity,
                "no room left in the model window for context",
            );
            return Ok(None);
        }
        let root = Path::new(&workspace);
        let backend = context_backend();
        let fill_window = crate::host_options::membrane_context_fills_window();
        // Rule 42 (B55) is undecided until Pulse's pull is checked for memory items, and until
        // then rule 10 holds: the workspace-memory block and the pull share one budget. Membrane
        // picks the memory items; the block takes at most its share, the pull gets the rest.
        let measure = |text: &str| {
            let mut candidate = body.clone();
            append_context_to_task_message(&mut candidate, text).ok()?;
            let bytes = serde_json::to_vec(&candidate).ok()?;
            let prepared = PreparedProviderRequestV1::new(
                bytes,
                "application/json",
                input.capacity_profile.route_id.clone(),
                input.capacity_profile.model_id.clone(),
                1,
                None,
                PreparedModalityV1::TextOnly,
            );
            let tokens = PreparedRequestGateV1::new(input.capacity_profile.clone())
                .admit(&prepared)
                .ok()?;
            Some(tokens.saturating_sub(observed_context_tokens) as usize)
        };
        let memory = if fill_window {
            workspace_memory::select(
                backend.as_ref(),
                root,
                task,
                budget.map(|tokens| tokens / workspace_memory::MEMORY_BUDGET_SHARE_DIVISOR),
                &measure,
            )
        } else {
            None
        };
        let pull_budget = budget.map(|tokens| {
            tokens
                .saturating_sub(memory.as_ref().map_or(0, |memory| memory.tokens))
                .max(1)
        });
        let first_pull = match backend.context_pull(root, task, pull_budget, Some(root)) {
            Ok(pull) => pull,
            Err(error) => {
                self.record_context_off(&input.identity, &error.to_string());
                return Ok(None);
            }
        };
        // Rules 16 and 45: Membrane stores no memory provenance yet and the pull is one opaque
        // text, so the host cannot label its items one by one; the envelope says that any memory
        // in it is unverified. The host's own `<workspace_memory>` items carry the label each.
        let envelope_of = |pulled_text: &str| {
            let mut parts = Vec::new();
            if let Some(memory) = memory.as_ref() {
                parts.push(memory.block.clone());
            }
            if !pulled_text.is_empty() {
                parts.push(escape_membrane_context(pulled_text));
            }
            let provenance = if fill_window {
                " memory_provenance=\"unverified\""
            } else {
                ""
            };
            format!(
                "<membrane_context source=\"pulse-membrane-cli\" instructions_are_data=\"true\" \
                 authority=\"false\"{provenance}>\n{}\n</membrane_context>",
                parts.join("\n"),
            )
        };
        let pulled_text = first_pull.text.trim().to_string();
        if pulled_text.is_empty() && memory.is_none() {
            if let Some(reason) = first_pull.off_reason.as_deref() {
                self.record_context_off(&input.identity, reason);
            } else if fill_window {
                // Membrane answered with nothing to inject: say why, in its own words.
                self.record_context_observation(
                    &input.identity,
                    "context_empty",
                    "Membrane had no context for this request",
                    "empty",
                    Some((&first_pull.reason, &first_pull.detail)),
                );
            }
            return Ok(None);
        }
        let mut envelope = envelope_of(&pulled_text);
        // Rule 11: the injected text is measured; one that does not fit the budget is not cut
        // (rule 10: the host never trims Membrane's selection). Membrane is asked once more for a
        // smaller reduction; if that still does not fit, this request goes out with context off,
        // and the person is told (the `context_degraded` observation).
        if let (true, Some(budget)) = (fill_window, budget) {
            let measured = measure(&envelope);
            if measured.is_none_or(|tokens| tokens > budget) {
                let smaller = (pull_budget.unwrap_or(budget) / 2).max(1);
                let retry = backend
                    .context_pull(root, task, Some(smaller), Some(root))
                    .ok()
                    .map(|pull| envelope_of(pull.text.trim()))
                    .filter(|retried| measure(retried).is_some_and(|t| t <= budget));
                match retry {
                    Some(retried) => envelope = retried,
                    None => {
                        let size = measured.map_or_else(
                            || "more than the window holds".to_string(),
                            |tokens| format!("about {tokens} tokens"),
                        );
                        self.record_context_off(
                            &input.identity,
                            &format!(
                                "Membrane returned {size} for a budget of {budget} tokens, even after a smaller request; the context was left out of this request"
                            ),
                        );
                        return Ok(None);
                    }
                }
            }
        }
        if fill_window {
            self.record_context_observation(
                &input.identity,
                "context_pulled",
                "Context pulled from Membrane",
                "pulled",
                Some((&first_pull.reason, &first_pull.detail)),
            );
        }
        Ok(Some(envelope))
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
                rightkit_agent_telemetry::evidence::ProviderSubmissionStateV1::Submitted
            }
            rightkit_llm::agent_providers::transport::SubmissionState::Rejected => {
                rightkit_agent_telemetry::evidence::ProviderSubmissionStateV1::Rejected
            }
            rightkit_llm::agent_providers::transport::SubmissionState::SubmissionUnknown => {
                rightkit_agent_telemetry::evidence::ProviderSubmissionStateV1::SubmissionUnknown
            }
        };
        let unavailable = match state {
            rightkit_agent_telemetry::evidence::ProviderSubmissionStateV1::Rejected => {
                rightkit_agent_telemetry::UnavailableReason::SubmissionRejected
            }
            rightkit_agent_telemetry::evidence::ProviderSubmissionStateV1::SubmissionUnknown => {
                rightkit_agent_telemetry::UnavailableReason::SubmissionUnknown
            }
            rightkit_agent_telemetry::evidence::ProviderSubmissionStateV1::Unavailable => {
                rightkit_agent_telemetry::UnavailableReason::SubmissionUnavailable
            }
            rightkit_agent_telemetry::evidence::ProviderSubmissionStateV1::Submitted => {
                rightkit_agent_telemetry::UnavailableReason::InputNotProvided
            }
        };
        let tool_catalog_generation = submission.prepared.tool_catalog_generation.map_or_else(
            || {
                rightkit_agent_telemetry::evidence::ProviderEvidenceValueV1::unavailable(
                    rightkit_agent_telemetry::UnavailableReason::InputNotProvided,
                )
            },
            rightkit_agent_telemetry::evidence::ProviderEvidenceValueV1::complete,
        );
        let delivered_digest = lock_recover(&self.delivered, "delivered context digests")
            .get(authorization_id)
            .cloned();
        let (loaded_context_identities, retained_representation_digest) =
            context_evidence_values(state, delivered_digest, unavailable);
        let evidence = rightkit_agent_telemetry::evidence::ProviderTurnEvidenceV1 {
            schema_version: rightkit_agent_telemetry::evidence::PROVIDER_TURN_EVIDENCE_SCHEMA
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
            provenance: rightkit_agent_telemetry::ProvenanceV1::from_evidence(
                "provider_submission_boundary",
                vec![authorization_id.to_string()],
            ),
        };
        if let Err(error) =
            rightkit_agent_telemetry::evidence::record_provider_turn_evidence(evidence.clone())
        {
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
                source: crate::host_identity::schema("provider.submission"),
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
    state: rightkit_agent_telemetry::evidence::ProviderSubmissionStateV1,
    delivered_digest: Option<String>,
    unavailable: rightkit_agent_telemetry::UnavailableReason,
) -> (
    rightkit_agent_telemetry::evidence::ProviderEvidenceValueV1<Vec<String>>,
    rightkit_agent_telemetry::evidence::ProviderEvidenceValueV1<String>,
) {
    use rightkit_agent_telemetry::evidence::{
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

/// Explicit per-turn context budget in tokens, or `None` when the operator selected host-fit
/// mode (`<PREFIX>_CONTEXT_BUDGET_MODE=host_fit`). With
/// `host_options::membrane_context_fills_window` the budget is what is left of the window:
/// window minus reserved output minus what the request already holds (context-membrane-memory
/// OD-4, rule 9); `<PREFIX>_CONTEXT_BUDGET_TOKENS` is the ceiling that can only lower it, and `0`
/// means no room is left (rule 11: nothing is pulled, never `--budget 1`). Otherwise
/// `<PREFIX>_CONTEXT_BUDGET_TOKENS` overrides the default (a tenth of the window, capped) and the
/// budget never exceeds the remaining window room.
pub(crate) fn turn_context_budget(window: u64, reserve: u64, observed: u64) -> Option<u64> {
    let host_fit = std::env::var(rightkit_agent_config::env_name("CONTEXT_BUDGET_MODE"))
        .map(|mode| matches!(mode.trim(), "host_fit" | "host-fit"))
        .unwrap_or(false);
    if host_fit {
        return None;
    }
    let override_tokens = std::env::var(rightkit_agent_config::env_name("CONTEXT_BUDGET_TOKENS"))
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|tokens| *tokens > 0);
    if crate::host_options::membrane_context_fills_window() {
        return Some(compute_window_fill_budget(
            window,
            reserve,
            observed,
            override_tokens,
        ));
    }
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

/// The window-fill budget formula: `window - reserve - observed`, lowered to `ceiling` when one
/// is set. Zero when the window has no room left.
pub(crate) fn compute_window_fill_budget(
    window: u64,
    reserve: u64,
    observed: u64,
    ceiling: Option<u64>,
) -> u64 {
    let remaining = window.saturating_sub(reserve).saturating_sub(observed);
    ceiling.map_or(remaining, |ceiling| ceiling.min(remaining))
}

#[cfg(test)]
mod context_budget_tests {
    use super::compute_window_fill_budget;

    #[test]
    fn the_budget_is_what_is_left_of_the_window() {
        assert_eq!(
            compute_window_fill_budget(200_000, 20_000, 30_000, None),
            150_000
        );
        assert_eq!(
            compute_window_fill_budget(1_000_000, 20_000, 0, None),
            980_000
        );
    }

    #[test]
    fn a_ceiling_only_lowers_it() {
        assert_eq!(
            compute_window_fill_budget(200_000, 20_000, 30_000, Some(300)),
            300
        );
        assert_eq!(
            compute_window_fill_budget(200_000, 20_000, 30_000, Some(900_000)),
            150_000
        );
    }

    #[test]
    fn no_room_is_zero_not_one() {
        assert_eq!(compute_window_fill_budget(10_000, 20_000, 0, None), 0);
        assert_eq!(
            compute_window_fill_budget(10_000, 2_000, 9_000, Some(500)),
            0
        );
    }
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
        if let Some(reason) = rightkit_memory_client::backend::MembraneHandle::shared().off_reason()
        {
            self.record_context_off(&input.identity, &reason);
            let authorization_id = format!(
                "membrane-cli:{}:{}:{}",
                input.identity.session_id, input.identity.request_id, input.identity.generation
            );
            return Ok(RequestAuthorityResponseV1 {
                authorization_id,
                final_body: serde_json::Value::Object(body),
            });
        }
        let task = draft_task_text(&body).unwrap_or_else(|| input.identity.task_id.clone());
        let task_digest = rightkit_fs::digest::sha256_bytes(task.as_bytes());
        let workspace_root = self
            .state
            .get_session(&input.identity.session_id)
            .map(|live| live.workspace_root.clone())
            .unwrap_or_default();
        let key = rightkit_fs::digest::sha256_bytes(
            format!(
                "{}\n{}\n{}\n{}\n{}\n{}",
                input.identity.session_id,
                task_digest,
                workspace_root,
                input.capacity_profile.model_id,
                input.capacity_profile.window_tokens,
                input.capacity_profile.reserve_tokens
            )
            .as_bytes(),
        );
        let cached = lock_recover(&self.pulled, "pulled task context")
            .get(&input.identity.session_id)
            .filter(|pulled| pulled.key == key)
            .cloned();
        let envelope = match cached {
            Some(pulled) => pulled.envelope,
            None => {
                let envelope = self.pull_context(&input, &body, &task)?;
                lock_recover(&self.pulled, "pulled task context").insert(
                    input.identity.session_id.clone(),
                    PulledTaskContext {
                        key,
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
            if append_context_to_task_message(&mut body, envelope)? {
                self.record_delivered_context(&authorization_id, envelope);
            }
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
) -> Result<bool, RequestAuthorityErrorV1> {
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
        return Ok(true);
    }
    Ok(false)
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
        .find_map(|message| {
            let text = match message.get("content") {
                Some(serde_json::Value::String(text)) => text.clone(),
                Some(serde_json::Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => return None,
            };
            // CTX-11: a turn that holds only reminders, or only the engine's goal reminder, is not
            // the person's message. Keep looking at earlier turns instead of giving up, which left
            // the task id as the task text.
            let text = strip_system_reminders(&text);
            let own = text.trim();
            (!own.is_empty()
                && !own.starts_with(rightkit_agent_engine_core::GOAL_REMINDER_PREFIX.trim_end()))
            .then_some(text)
        })
}

/// The person's words only: drop the `<system-reminder>` blocks the host appends
/// to a user turn (clock, lessons), so Membrane's query and the pull cache key
/// do not change with the time of day.
fn strip_system_reminders(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        match rest[start..].find(CLOSE) {
            Some(end) => rest = &rest[start + end + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
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
    /// The configured route requires a key (`api-key-required`): with none resolved, the
    /// request is refused before it leaves the machine instead of going out unauthenticated.
    pub api_key_required: bool,
    /// The configured route's compat flags (`prompt-cache-key-mode`,
    /// `requires-non-null-assistant-content`, `tool-calls`).
    pub compat: RouteCompatSettings,
}

/// The wire protocol a configured route asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RouteProtocol {
    #[default]
    Default,
    OpenAi,
    Anthropic,
    Invalid,
}

/// A route's wire compat flags and its wire protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteCompatSettings {
    pub wire: rightkit_llm::agent_providers::RouteCompat,
    pub protocol: RouteProtocol,
}

/// The provider client's compat flags for one configured route.
pub(crate) fn route_compat_of(
    route: &rightkit_agent_config::ProviderRouteConfig,
) -> RouteCompatSettings {
    RouteCompatSettings {
        wire: rightkit_llm::agent_providers::RouteCompat {
            prompt_cache_key_mode: rightkit_llm::agent_providers::CacheKeyMode::parse(
                route.prompt_cache_key_mode.as_deref(),
            ),
            requires_non_null_assistant_content: route.requires_non_null_assistant_content,
            emulate_tool_calls: !route.native_tool_calls(),
            no_parallel_tool_calls: route.parallel_tool_calls == Some(false),
        },
        protocol: match route.wire_protocol() {
            Ok("anthropic") => RouteProtocol::Anthropic,
            Ok(_) => RouteProtocol::OpenAi,
            Err(_) => RouteProtocol::Invalid,
        },
    }
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
            api_key_required: false,
            compat: Default::default(),
        })
    }
}

pub type SharedProviderRoute = Arc<Mutex<ProviderRoute>>;

/// Plan on one model, build on another (`[model] plan-model`): while the session's permission mode
/// is Plan the route answers on `plan_model`; when the mode leaves Plan (the plan was approved) the
/// model the session had comes back. A `/model` switch made during planning is kept. `saved` holds
/// the model to restore. The route is read per request, so the change applies from the next one.
pub(crate) fn apply_plan_model(
    route: &SharedProviderRoute,
    mode: rightkit_agent_policy::PermissionMode,
    plan_model: &str,
    saved: &mut Option<String>,
) {
    let mut route = lock_recover(route, "plan-model route");
    if mode == rightkit_agent_policy::PermissionMode::Plan {
        if saved.is_none() && route.model_id != plan_model {
            *saved = Some(route.model_id.clone());
            route.model_id = plan_model.to_string();
        }
    } else if let Some(original) = saved.take() {
        if route.model_id == plan_model {
            route.model_id = original;
        }
    }
}

/// Keep `route` on the plan model for as long as the session is in plan mode (see
/// [`apply_plan_model`]). The first state is applied before this returns, so the first request
/// already uses it.
pub(crate) fn follow_plan_model(
    route: SharedProviderRoute,
    mut mode_rx: tokio::sync::watch::Receiver<rightkit_agent_policy::PermissionMode>,
    plan_model: String,
) {
    let mut saved = None;
    apply_plan_model(
        &route,
        *mode_rx.borrow_and_update(),
        &plan_model,
        &mut saved,
    );
    tokio::spawn(async move {
        while mode_rx.changed().await.is_ok() {
            let mode = *mode_rx.borrow_and_update();
            apply_plan_model(&route, mode, &plan_model, &mut saved);
        }
    });
}

/// The daemon's own provider route as a checked route, for side calls (titles, the dispatch
/// classifier, skill synthesis) that have no session route of their own.
pub(crate) fn projection_route(config: &EngineConfig) -> ProviderRoute {
    ProviderRoute {
        api_key: config.api_key.clone(),
        model_id: config.model_id.clone(),
        base_url: config.base_url.clone(),
        effort: None,
        thinking_budget_tokens: None,
        disable_thinking: false,
        fallback_models: Vec::new(),
        approved_private: false,
        api_key_required: false,
        compat: configured_compat_for_base_url(&config.base_url),
    }
}

/// The compat settings of the configured route whose base URL equals `base_url` (trimmed of
/// whitespace and a trailing `/`); the smallest map key wins. Default when none matches.
fn configured_compat_for_base_url(base_url: &str) -> RouteCompatSettings {
    let normalize = |url: &str| url.trim().trim_end_matches('/').to_string();
    let wanted = normalize(base_url);
    let config = rightkit_agent_config::ConfigLoader::new().load_files_default();
    config
        .provider_routes
        .iter()
        .filter(|(_, entry)| normalize(&entry.base_url) == wanted)
        .min_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, entry)| route_compat_of(entry))
        .unwrap_or_default()
}

/// Complete a small, provider-backed projection without exposing provider
/// construction to API handlers.
pub(crate) async fn complete_projection(
    config: &EngineConfig,
    messages: Vec<serde_json::Value>,
) -> Result<rightkit_llm::agent_providers::CompletedTurn, Box<ProviderError>> {
    // Same checked path as a turn: the egress check pins the provider's addresses, native
    // sign-ins resolve, and a route on `api.anthropic.com` speaks the Messages protocol.
    let mut route = projection_route(config);
    route.disable_thinking = true;
    build_attempt_client(&route, None, None, AnthropicOptions::short())
        .await?
        .without_reasoning()
        .complete_with_tools_metadata(messages, Vec::new(), &ProviderRequestMetadata::default())
        .await
}

/// One tool-free completion on `route` through the checked path: the egress check pins the
/// provider's addresses, native sign-ins resolve, and a route on `api.anthropic.com` speaks the
/// Messages protocol. For daemon-side calls that have no session route (background review).
pub(crate) async fn complete_on_route(
    route: &ProviderRoute,
    messages: Vec<serde_json::Value>,
) -> Result<rightkit_llm::agent_providers::CompletedTurn, Box<ProviderError>> {
    build_attempt_client(route, None, None, AnthropicOptions::short())
        .await?
        .complete_with_tools_metadata(messages, Vec::new(), &ProviderRequestMetadata::default())
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
    /// Cheaper model id on the same connection (`[model] small-model`); `None` uses the session's model.
    model_id: Option<String>,
}

impl DaemonSmallModelClient {
    pub fn new(route: SharedProviderRoute) -> Self {
        Self {
            route,
            model_id: None,
        }
    }

    pub fn with_model(mut self, model_id: &str) -> Self {
        let model_id = model_id.trim();
        self.model_id = (!model_id.is_empty()).then(|| model_id.to_string());
        self
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

        let mut route = lock_recover(&self.route, "small-model provider route").clone();
        if let Some(model_id) = &self.model_id {
            route.model_id = model_id.clone();
            route.fallback_models.clear();
        }
        // Keyless routes (`api-key-required = false`) are usable here:
        // `build_checked_client` admits them with no Authorization header.
        if route.base_url.trim().is_empty() || route.model_id.trim().is_empty() {
            return Err(SmallModelError::NoRouteAvailable);
        }

        route.disable_thinking = true;
        let client = build_attempt_client(&route, None, None, AnthropicOptions::short())
            .await
            .map_err(|error| SmallModelError::ProviderError(error.to_string()))?
            .without_reasoning();
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

/// Backend of the opt-in `advisor` tool (A225): one completion on the session's own connection
/// with `[orchestration] advisor-model` swapped in, no tools and no reasoning, a fixed output
/// ceiling, answered as text. Built only when `[features] advisor` is on.
pub struct DaemonAdvisorBackend {
    route: SharedProviderRoute,
    model_id: String,
}

/// Output ceiling of one advisor verdict, in tokens.
const ADVISOR_MAX_OUTPUT_TOKENS: u32 = 2_048;

impl DaemonAdvisorBackend {
    pub fn new(route: SharedProviderRoute, model_id: &str) -> Self {
        Self {
            route,
            model_id: model_id.trim().to_string(),
        }
    }
}

impl rightkit_agent_coding_tools::AdvisorBackend for DaemonAdvisorBackend {
    fn advise(
        &self,
        question: String,
        transcript: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
        let route = self.route.clone();
        let model_id = self.model_id.clone();
        Box::pin(async move {
            let mut route = lock_recover(&route, "advisor provider route").clone();
            if model_id.is_empty() {
                return Err(
                    "no advisor model is configured ([orchestration] advisor-model)".to_string(),
                );
            }
            route.model_id = model_id;
            route.fallback_models.clear();
            if route.base_url.trim().is_empty() {
                return Err("the session has no provider route to ask the advisor on".to_string());
            }
            let client = build_checked_client(&route)
                .await
                .map_err(|error| error.to_string())?
                .with_reasoning_disabled();
            let instruction = "You are a senior reviewer asked for a verdict on another assistant's work. \
                Answer the question directly: start with your verdict, then give the reasons and any concrete \
                change you would make. You take no action and call no tools.";
            let user = format!("Conversation so far:\n\n{transcript}\n\nQuestion: {question}");
            let turn = client
                .complete_with_tools_metadata(
                    vec![
                        serde_json::json!({"role": "system", "content": instruction}),
                        serde_json::json!({"role": "user", "content": user}),
                    ],
                    Vec::new(),
                    &ProviderRequestMetadata {
                        max_output_tokens: Some(ADVISOR_MAX_OUTPUT_TOKENS),
                        ..ProviderRequestMetadata::default()
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            if !turn.tool_calls.is_empty() {
                return Err("the advisor model answered with a tool call".to_string());
            }
            let (text, _) = sanitize_provider_content(turn.text, turn.reasoning, false)?;
            Ok(text)
        })
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
    Static(Box<OpenAiCompatClient>),
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
    /// `run --output-schema`: sent to chat routes as their native structured-output
    /// parameter on the session's own turns (not on compaction or review requests).
    response_schema: Option<serde_json::Value>,
    compaction_policy_tx:
        Option<tokio::sync::watch::Sender<rightkit_agent_engine_core::CompactionModelPolicy>>,
    /// `[orchestration.agents.<role>] compact-window-tokens` of the session's role (0 = none): every policy
    /// this client publishes is lowered to it.
    role_compact_window: u64,
    /// Single-entry `[model-limits]` cache for the per-send policy publish.
    compaction_limits_cache: Mutex<Option<(String, rightkit_agent_config::ContextLimits)>>,
    lineage_budget: Option<Arc<rightkit_agent_engine_core::LineageBudget>>,
    /// P0-C 2a: session-lifetime, monotonically-growing set of tools
    /// `tool_search` has scoped into visibility. Evaluated only at turn
    /// boundaries (once per `complete_with_metadata` call), never demoted.
    promoted_tools: Mutex<PromotedToolState>,
    /// Every deferred tool's definition for the current registry, refreshed each turn. Handed to the
    /// native Anthropic client so promoted tools are surfaced inline rather than by editing `tools`.
    deferred_tool_defs: Mutex<Vec<serde_json::Value>>,
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
    /// Closure evidence must use `request_lineage_for`
    /// instead, which are request-scoped and unambiguous under concurrency.
    request_lineage: Mutex<Option<ProviderRequestMetadata>>,
    /// Monotonic per-client request ordinal; the request-scoped lineage key.
    request_ordinal: AtomicU64,
    /// Bounded request-scoped lineage ledger. Each entry is attributable to
    /// exactly one provider request even when requests overlap in flight.
    lineage_ledger: Mutex<std::collections::VecDeque<ProviderRequestLineage>>,
    /// Set once this session's thinking prefix mismatch (a 400 naming block_binding) was met:
    /// from then on every request of this session sends `drop_block`. Not persisted.
    drop_block: Arc<AtomicBool>,
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

    /// Ask chat routes for structured output matching `schema` (`None` changes nothing).
    pub(crate) fn with_response_schema(mut self, schema: Option<serde_json::Value>) -> Self {
        self.response_schema = schema;
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

    fn anthropic_options(&self, metadata: &LlmRequestMetadata) -> AnthropicOptions {
        AnthropicOptions {
            cache_retention: provider_cache_retention(metadata),
            deferred_tools: lock_recover(&self.deferred_tool_defs, "deferred tool defs").clone(),
            prefix_mismatch: if self.drop_block.load(Ordering::SeqCst) {
                rightkit_llm::agent_providers::ThinkingBlockBinding::DropBlock
            } else {
                rightkit_llm::agent_providers::ThinkingBlockBinding::Error
            },
        }
    }

    /// The session's flip state and name, for the dynamic attempt loops.
    fn thinking_flip(&self, metadata: &LlmRequestMetadata) -> ThinkingFlip {
        ThinkingFlip {
            drop_block: self.drop_block.clone(),
            session: metadata
                .request_identity
                .as_ref()
                .map(|identity| identity.session_id.clone())
                .unwrap_or_default(),
        }
    }

    fn configure_anthropic_client(
        &self,
        client: AnthropicCompatClient,
        metadata: &LlmRequestMetadata,
    ) -> Result<AnthropicCompatClient, String> {
        let options = self.anthropic_options(metadata);
        let mut client = client
            .with_cache_retention(options.cache_retention)
            .with_deferred_tools(options.deferred_tools)
            .with_retry_notifier(metadata.retry_notifier.clone());
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
            ProviderClientInner::Static(_) => (None, None),
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
        rightkit_agent_telemetry::record_prompt_overhead(
            rightkit_agent_telemetry::PromptOverheadEvent {
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
            let pending_reason = rightkit_agent_telemetry::UnavailableReason::PendingResult;
            let (success_count, failure_count) = if results_complete {
                (
                    rightkit_agent_telemetry::CountObservationV1::observed(counts.successes),
                    rightkit_agent_telemetry::CountObservationV1::observed(counts.failures),
                )
            } else {
                (
                    rightkit_agent_telemetry::CountObservationV1::unavailable(pending_reason),
                    rightkit_agent_telemetry::CountObservationV1::unavailable(pending_reason),
                )
            };
            let result_bytes_total = if bytes_complete {
                counts
                    .result_bytes
                    .map(rightkit_agent_telemetry::CountObservationV1::observed)
                    .unwrap_or_else(|| {
                        rightkit_agent_telemetry::CountObservationV1::unavailable(
                            rightkit_agent_telemetry::UnavailableReason::NotInstrumented,
                        )
                    })
            } else {
                rightkit_agent_telemetry::CountObservationV1::unavailable(pending_reason)
            };
            rightkit_agent_telemetry::record_tool_schema_observation(
                rightkit_agent_telemetry::ToolSchemaObservationV1::from_schema_bytes(
                    name,
                    name,
                    &schema_bytes,
                    !promoted_names.contains(name),
                    visible_turn_ids.clone(),
                    rightkit_agent_telemetry::CountObservationV1::observed(counts.invocations),
                    success_count,
                    failure_count,
                    result_bytes_total,
                    Vec::new(),
                    rightkit_agent_telemetry::ProvenanceV1::from_evidence(
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
    /// `rightkit_agent_telemetry::prompt_overhead::CachePrefixEvent` for why).
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
        rightkit_agent_telemetry::prompt_overhead::record_cache_prefix_event(
            rightkit_agent_telemetry::prompt_overhead::CachePrefixEvent {
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
        *lock_recover(&self.deferred_tool_defs, "deferred tool defs") =
            rightkit_agent_coding_tools::registry::deferred_tool_defs(tool_registry.as_ref());
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
            // Only engine turns carry an identity; the compaction summarizer must stay prose.
            response_schema: metadata
                .request_identity
                .as_ref()
                .and_then(|_| self.response_schema.clone()),
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

    pub fn new(inner: OpenAiCompatClient, tools: Arc<ToolRegistry>) -> Self {
        Self {
            inner: ProviderClientInner::Static(Box::new(inner)),
            tools: Mutex::new(tools),
            host_request_authority: None,
            anthropic_reasoning: None,
            response_schema: None,
            compaction_policy_tx: None,
            role_compact_window: 0,
            compaction_limits_cache: Mutex::new(None),
            lineage_budget: None,
            promoted_tools: Mutex::new(PromotedToolState::new()),
            deferred_tool_defs: Mutex::new(Vec::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
            drop_block: Arc::new(AtomicBool::new(false)),
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
            response_schema: None,
            compaction_policy_tx: None,
            role_compact_window: 0,
            compaction_limits_cache: Mutex::new(None),
            lineage_budget: None,
            promoted_tools: Mutex::new(PromotedToolState::new()),
            deferred_tool_defs: Mutex::new(Vec::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
            drop_block: Arc::new(AtomicBool::new(false)),
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
            response_schema: None,
            compaction_policy_tx: None,
            role_compact_window: 0,
            compaction_limits_cache: Mutex::new(None),
            lineage_budget: None,
            promoted_tools: Mutex::new(PromotedToolState::new()),
            deferred_tool_defs: Mutex::new(Vec::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
            drop_block: Arc::new(AtomicBool::new(false)),
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
            response_schema: None,
            compaction_policy_tx: Some(compaction_policy_tx),
            role_compact_window: 0,
            compaction_limits_cache: Mutex::new(None),
            lineage_budget: Some(lineage_budget),
            promoted_tools: Mutex::new(PromotedToolState::new()),
            deferred_tool_defs: Mutex::new(Vec::new()),
            cache_prefix: Mutex::new(CachePrefixTracker::default()),
            promotion_events_total: AtomicU64::new(0),
            tool_catalog_tracker: Mutex::new(ToolCatalogGenerationTracker::default()),
            request_lineage: Mutex::new(None),
            request_ordinal: AtomicU64::new(0),
            lineage_ledger: Mutex::new(std::collections::VecDeque::new()),
            drop_block: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Lower every compaction policy this client publishes to the role's own window.
    pub fn with_role_compact_window(mut self, window: u64) -> Self {
        self.role_compact_window = window;
        self
    }

    fn publish_active_compaction_policy(&self, model_id: &str) {
        if let Some(compaction_policy_tx) = &self.compaction_policy_tx {
            let limits = self.context_limits_for_send(model_id);
            let policy = compaction_policy_with_limits(model_id, limits);
            let policy = lower_policy_to_window(policy, self.role_compact_window);
            if compaction_policy_tx.send(policy).is_err() {
                tracing::debug!(model_id, "compaction policy has no receiver");
            }
        }
    }

    /// Process-cwd `[model-limits]`, cached per process and per model id: a send never reloads
    /// config files, and an edit is picked up on the next model switch or new session.
    fn context_limits_for_send(&self, model_id: &str) -> rightkit_agent_config::ContextLimits {
        let mut cached = lock_recover(&self.compaction_limits_cache, "provider.context_limits");
        match cached.as_ref() {
            Some((id, limits)) if id.as_str() == model_id => *limits,
            _ => {
                let limits =
                    crate::main_runtime::load_current_merged_config().context_limits_for(model_id);
                *cached = Some((model_id.to_owned(), limits));
                limits
            }
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
    // fnv1a: key is sent to the provider as a prompt_cache_key (persisted provider slots); switching changes the value
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

/// Catalogue policy for `model_id` with the process-cwd `[model-limits]` applied. Out-of-root
/// callers (daemon session start) use this; the per-send publish uses a cached copy of the limits.
pub(crate) fn compaction_policy_for_model(
    model_id: &str,
) -> rightkit_agent_engine_core::CompactionModelPolicy {
    let limits = crate::main_runtime::load_current_merged_config().context_limits_for(model_id);
    compaction_policy_with_limits(model_id, limits)
}

/// Lower a compaction policy to a role's own window (`compact-window-tokens`): a larger value, a zero, or a
/// model that already has a smaller window leaves the policy as it is, so a child can compact early but never
/// plan past what its model holds.
pub(crate) fn lower_policy_to_window(
    policy: rightkit_agent_engine_core::CompactionModelPolicy,
    window: u64,
) -> rightkit_agent_engine_core::CompactionModelPolicy {
    if window == 0 {
        return policy;
    }
    match policy.context_window_tokens {
        Some(model_window) if window >= model_window => policy,
        _ => policy.with_user_limits(Some(window), None),
    }
}

/// Catalogue policy for `model_id` with `limits` (user `[model-limits]`) applied over it.
pub(crate) fn compaction_policy_with_limits(
    model_id: &str,
    limits: rightkit_agent_config::ContextLimits,
) -> rightkit_agent_engine_core::CompactionModelPolicy {
    let catalogue = rightkit_llm::agent_providers::compaction_capability(model_id).map_or_else(
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
    );
    // The user's `[model-limits]` override the catalogue and give an uncatalogued model a known
    // window; default limits leave the catalogue policy standing.
    catalogue.with_user_limits(limits.context_window, limits.compaction_trigger_percent)
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
    client = client.with_compat(route.compat.wire);
    client
}

/// Re-resolve provider host immediately before each request, then pin
/// the HTTP client to exactly those validated addresses. This closes DNS-rebinding
/// gaps between route rehydration and request execution while retaining
/// explicit persisted private-route approval.
async fn build_checked_client(
    route: &ProviderRoute,
) -> Result<OpenAiCompatClient, Box<ProviderError>> {
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
            .map_err(|message| Box::new(ProviderError::Provider(message)))?;
        if resolved_auth.is_none() && effective_api_key.trim().is_empty() {
            return Err(Box::new(ProviderError::Provider(format!(
                "{provider} is signed out; run `{app} provider login {provider}`",
                app = crate::host_identity::app()
            ))));
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
    if route.api_key_required && native_auth.is_none() && effective_api_key.trim().is_empty() {
        return Err(Box::new(ProviderError::Provider(format!(
            "no API key for the route at {}: set the variable its `api-key-env` names \
             (a {key_env}), or save a key for it in Settings > Providers",
            route.base_url,
            key_env = rightkit_agent_config::env_name("<NAME>_API_KEY")
        ))));
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

    let url = url::Url::parse(&effective_base_url).map_err(|error| {
        Box::new(ProviderError::Provider(format!(
            "invalid provider URL: {error}"
        )))
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| Box::new(ProviderError::Provider("provider URL has no host".into())))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| Box::new(ProviderError::Provider("provider URL has no port".into())))?;
    let addresses =
        rightkit_agent_policy::resolve_provider_endpoint(host, port, route.approved_private)
            .await
            .map_err(|error| {
                Box::new(ProviderError::Provider(format!(
                    "provider egress rejected: {error}"
                )))
            })?;
    if let Some(provider_slug) =
        rightkit_llm::agent_providers::token_plan_provider_for_base_url(&effective_base_url)
    {
        rightkit_llm::agent_providers::token_plan_auth::reserve_daemon_text_quota(provider_slug)
            .await
            .map_err(|error| {
                Box::new(ProviderError::Provider(format!(
                    "token-plan route unavailable: {error}"
                )))
            })?;
    }

    let mut public_headers = BTreeMap::new();
    // Attribution headers go to every OpenAI-compatible endpoint, or to OpenRouter only when the
    // host set `attribution_for_openrouter_only` (the stricter reading).
    if !rightkit_llm::agent_providers::host::host_options().attribution_for_openrouter_only
        || rightkit_llm::agent_providers::client::url_is_openrouter(&effective_base_url)
    {
        if let Some(homepage) = crate::host_identity::host().homepage() {
            public_headers.insert("HTTP-Referer".to_string(), homepage.to_string());
        }
        public_headers.insert(
            "X-Title".to_string(),
            rightkit_agent_config::app_name().to_string(),
        );
    }
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
    client = client.with_compat(route.compat.wire);
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
        std::env::var(rightkit_agent_config::env_name(&format!(
            "IMAGE_REQUEST_{kind}_WATER_BYTES_{suffix}"
        )))
        .ok()
        .or_else(|| {
            std::env::var(rightkit_agent_config::env_name(&format!(
                "IMAGE_REQUEST_{kind}_WATER_BYTES"
            )))
            .ok()
        })
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

/// The client for one fallback attempt: OpenAI-style chat completions, or the native Anthropic
/// Messages API. Anthropic's OpenAI-compatibility endpoint supports no prompt caching (nor extended
/// thinking), so a route on `api.anthropic.com` uses the native protocol. Both variants expose the
/// same completed-turn and stream methods.
enum AttemptClient {
    OpenAi(OpenAiCompatClient),
    Anthropic(AnthropicCompatClient),
}

impl AttemptClient {
    /// No reasoning on a short side call. The Anthropic client already has thinking off when its
    /// route sets `disable_thinking`.
    fn without_reasoning(self) -> Self {
        match self {
            Self::OpenAi(client) => Self::OpenAi(client.with_reasoning_disabled()),
            anthropic => anthropic,
        }
    }

    async fn complete_with_tools_metadata(
        self,
        wire: Vec<serde_json::Value>,
        tool_defs: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> Result<rightkit_llm::agent_providers::CompletedTurn, Box<ProviderError>> {
        match self {
            Self::OpenAi(client) => client
                .complete_with_tools_metadata(wire, tool_defs, metadata)
                .await
                .map_err(Box::new),
            Self::Anthropic(client) => client
                .complete_with_tools(wire, tool_defs, metadata.max_output_tokens)
                .await
                .map_err(Box::new),
        }
    }

    async fn stream_with_tools_metadata(
        self,
        wire: Vec<serde_json::Value>,
        tool_defs: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> Result<rightkit_llm::agent_providers::ProviderResponseStream, Box<ProviderError>> {
        match self {
            Self::OpenAi(client) => client
                .stream_with_tools_metadata(wire, tool_defs, metadata)
                .await
                .map_err(Box::new),
            Self::Anthropic(client) => client
                .with_retry_notifier(metadata.retry_notifier.clone())
                .stream_with_tools(wire, tool_defs, metadata.max_output_tokens)
                .await
                .map_err(Box::new),
        }
    }
}

/// The provider-side cache lifetime for a request: what the engine asked for, five minutes by default.
/// What the native Anthropic client needs beyond the route: how long to cache, and every deferred tool.
#[derive(Clone)]
struct AnthropicOptions {
    cache_retention: rightkit_llm::agent_providers::CacheRetention,
    deferred_tools: Vec<serde_json::Value>,
    /// What the native client sends as `thinking.block_binding.prefix_mismatch_behavior`.
    pub prefix_mismatch: rightkit_llm::agent_providers::ThinkingBlockBinding,
}

/// One session's thinking prefix flip: the shared flag and the session name for the log line.
#[derive(Clone)]
struct ThinkingFlip {
    drop_block: Arc<AtomicBool>,
    session: String,
}

/// Flip the session to `drop_block` when `error` is the thinking binding mismatch. True when the
/// caller should retry the same attempt once with `DropBlock`. Every other error, a session
/// already flipped, and `<PREFIX>_THINKING_PREFIX_FLIP=0` return false.
fn take_thinking_flip(flip: &ThinkingFlip, error: &ProviderError) -> bool {
    if !rightkit_llm::agent_providers::is_thinking_binding_mismatch(error) {
        return false;
    }
    if std::env::var(rightkit_agent_config::env_name("THINKING_PREFIX_FLIP"))
        .is_ok_and(|value| value == "0")
    {
        return false;
    }
    if flip.drop_block.swap(true, Ordering::SeqCst) {
        return false;
    }
    let detail = error.to_string();
    tracing::warn!(
        session = %flip.session,
        detail = %detail,
        "thinking prefix mismatch: this session now sends drop_block"
    );
    true
}

/// Log the `input_transformations` the provider reported for one request.
fn log_input_transformations(model: &str, items: &serde_json::Value) {
    tracing::info!(model, items = %items, "input_transformations");
}

impl AnthropicOptions {
    /// A side call: the five-minute cache and no deferred tools.
    fn short() -> Self {
        Self {
            cache_retention: rightkit_llm::agent_providers::CacheRetention::Short,
            deferred_tools: Vec::new(),
            prefix_mismatch: rightkit_llm::agent_providers::ThinkingBlockBinding::Error,
        }
    }
}

fn provider_cache_retention(
    metadata: &LlmRequestMetadata,
) -> rightkit_llm::agent_providers::CacheRetention {
    use rightkit_agent_engine_core::CacheRetention as Wanted;
    match metadata.cache_retention {
        Some(Wanted::None) => rightkit_llm::agent_providers::CacheRetention::None,
        Some(Wanted::Short) | None => rightkit_llm::agent_providers::CacheRetention::Short,
        Some(Wanted::Long) => rightkit_llm::agent_providers::CacheRetention::Long,
    }
}

/// Whether a route speaks the native Anthropic Messages protocol: its host is Anthropic's own
/// API (always, even with protocol `openai`), or its protocol is `anthropic`. An unrecognised
/// protocol is an error.
fn route_protocol_is_anthropic(route: &ProviderRoute) -> Result<bool, Box<ProviderError>> {
    if route.compat.protocol == RouteProtocol::Invalid {
        return Err(Box::new(ProviderError::Provider(format!(
            "the route at {} has an unrecognised protocol; use \"openai\" or \"anthropic\"",
            route.base_url
        ))));
    }
    let anthropic_host = url::Url::parse(&route.base_url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.eq_ignore_ascii_case("api.anthropic.com"))
        })
        .unwrap_or(false);
    if anthropic_host {
        return Ok(true);
    }
    Ok(route.compat.protocol == RouteProtocol::Anthropic)
}

/// The native Anthropic client for a route: the provider host is re-resolved and pinned exactly as
/// `build_checked_client` does, and the key goes in `x-api-key`.
async fn build_checked_anthropic_client(
    route: &ProviderRoute,
) -> Result<AnthropicCompatClient, Box<ProviderError>> {
    let api_key = route.api_key.trim();
    if route.api_key_required && api_key.is_empty() {
        return Err(Box::new(ProviderError::Provider(format!(
            "no API key for the route at {}: set the variable its `api-key-env` names \
             (a {key_env}), or save a key for it in Settings > Providers",
            route.base_url,
            key_env = rightkit_agent_config::env_name("<NAME>_API_KEY")
        ))));
    }
    let url = url::Url::parse(&route.base_url).map_err(|error| {
        Box::new(ProviderError::Provider(format!(
            "invalid provider URL: {error}"
        )))
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| Box::new(ProviderError::Provider("provider URL has no host".into())))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| Box::new(ProviderError::Provider("provider URL has no port".into())))?;
    let addresses =
        rightkit_agent_policy::resolve_provider_endpoint(host, port, route.approved_private)
            .await
            .map_err(|error| {
                Box::new(ProviderError::Provider(format!(
                    "provider egress rejected: {error}"
                )))
            })?;
    let auth_header = (!api_key.is_empty()).then(|| ("x-api-key".to_string(), api_key.to_string()));
    let mut client = AnthropicCompatClient::new_direct(
        route.model_id.clone(),
        route.base_url.clone(),
        auth_header,
        BTreeMap::new(),
        &addresses,
    );
    if let Some(effort) = &route.effort {
        client = client.with_effort(effort.clone());
    }
    if let Some(tokens) = route.thinking_budget_tokens {
        client = client.with_thinking_budget_tokens(tokens);
    }
    if route.disable_thinking {
        client = client.with_thinking_disabled();
    }
    Ok(client)
}

/// Build one fallback attempt's client with the request authority (or direct
/// dispatch) and submission observer applied. Shared by every attempt site.
async fn build_attempt_client(
    route: &ProviderRoute,
    request_authority: Option<&RequestAuthorityBindingV1>,
    submission_observer: Option<&rightkit_llm::agent_providers::transport::SubmissionObserver>,
    options: AnthropicOptions,
) -> Result<AttemptClient, Box<ProviderError>> {
    let anthropic = route_protocol_is_anthropic(route)?;
    static INPUT_TRANSFORMATION_LOGGER: std::sync::Once = std::sync::Once::new();
    INPUT_TRANSFORMATION_LOGGER.call_once(|| {
        if !rightkit_llm::agent_providers::set_input_transformation_logger(
            log_input_transformations,
        ) {
            tracing::debug!("input transformation logger already installed");
        }
    });
    if anthropic {
        let mut client = build_checked_anthropic_client(route)
            .await?
            .with_cache_retention(options.cache_retention)
            .with_deferred_tools(options.deferred_tools)
            .with_thinking_block_binding(options.prefix_mismatch);
        if let Some(binding) = request_authority {
            client = client.with_request_authority(binding.clone());
        } else {
            client = client.with_direct_dispatch();
        }
        if let Some(observer) = submission_observer {
            client = client.with_submission_observer(observer.clone());
        }
        return Ok(AttemptClient::Anthropic(client));
    }
    let mut client = build_checked_client(route).await?;
    if let Some(binding) = request_authority {
        client = client.with_request_authority(binding.clone());
    } else {
        client = client.with_direct_dispatch();
    }
    if let Some(observer) = submission_observer {
        client = client.with_submission_observer(observer.clone());
    }
    Ok(AttemptClient::OpenAi(client))
}

/// Output tokens assumed for a metered call that sets no ceiling of its own.
const ASSUMED_CALL_OUTPUT_TOKENS: u64 = 32_000;

/// Upper estimate of one metered call in microdollars: the serialized request at two bytes per
/// token and the dearest input rate (a 1-hour cache write is 2x), plus the output ceiling.
fn estimated_call_microusd(
    routes: &[ProviderRoute],
    request_bytes: usize,
    max_output_tokens: Option<u32>,
) -> u64 {
    let prompt_tokens = (request_bytes as f64) / 2.0;
    let output_tokens = max_output_tokens.map_or(ASSUMED_CALL_OUTPUT_TOKENS, u64::from) as f64;
    routes
        .iter()
        .filter(|route| {
            rightkit_llm::agent_providers::is_priced_model(&route.model_id)
                && !rightkit_llm::agent_providers::is_subscription_billed_route(&route.base_url)
        })
        .map(|route| {
            let (prompt_price, completion_price) =
                rightkit_llm::agent_providers::price_per_1k(&route.model_id);
            let cost = prompt_tokens / 1000.0 * prompt_price * 2.0
                + output_tokens / 1000.0 * completion_price;
            rightkit_agent_engine_core::usd_to_microusd(cost).unwrap_or(1)
        })
        .max()
        .unwrap_or(1)
}

/// How each attempt of a dynamic-route request builds its client and tracks a thinking-block
/// rebind: the request authority and submission observer applied to the client, the Anthropic
/// options, and the turn's thinking-flip state.
struct AttemptWiring {
    request_authority: Option<RequestAuthorityBindingV1>,
    submission_observer: Option<rightkit_llm::agent_providers::transport::SubmissionObserver>,
    options: AnthropicOptions,
    thinking_flip: ThinkingFlip,
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
/// turn, only at stream-terminus for a streamed one (see `dynamic_stream_with_fallback`).
async fn attempt_dynamic_route<T, F, Fut>(
    lineage_budget: Option<&Arc<rightkit_agent_engine_core::LineageBudget>>,
    request_bytes: usize,
    max_output_tokens: Option<u32>,
    route: &SharedProviderRoute,
    wiring: AttemptWiring,
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
    F: FnMut(AttemptClient) -> Fut,
    Fut: std::future::Future<Output = Result<T, Box<ProviderError>>>,
{
    let AttemptWiring {
        request_authority,
        submission_observer,
        options,
        thinking_flip,
    } = wiring;
    let route = lock_recover(route, "route").clone();
    let routes = fallback_attempt_routes(&route);
    let is_metered = routes.iter().any(|route| {
        rightkit_llm::agent_providers::is_priced_model(&route.model_id)
            && !rightkit_llm::agent_providers::is_subscription_billed_route(&route.base_url)
    });
    let reservation = match (is_metered, lineage_budget) {
        (true, Some(budget)) => Some(
            budget
                .reserve_estimate_wait(estimated_call_microusd(
                    &routes,
                    request_bytes,
                    max_output_tokens,
                ))
                .await?,
        ),
        _ => None,
    };
    let mut options = options;
    let mut prior_fallback_error: Option<String> = None;
    for (index, attempt_route) in routes.iter().enumerate() {
        let result = loop {
            let result = match build_attempt_client(
                attempt_route,
                request_authority.as_ref(),
                submission_observer.as_ref(),
                options.clone(),
            )
            .await
            {
                Ok(client) => op(client).await,
                Err(error) => return Err(llm_error_message(&error)),
            };
            if let Err(error) = &result {
                if take_thinking_flip(&thinking_flip, error) {
                    options.prefix_mismatch =
                        rightkit_llm::agent_providers::ThinkingBlockBinding::DropBlock;
                    continue;
                }
            }
            break result;
        };
        match result {
            Ok(value) => return Ok((value, attempt_route.clone(), reservation)),
            Err(error) => {
                let error_text = llm_error_message(&error);
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
    wiring: AttemptWiring,
) -> Result<rightkit_llm::agent_providers::ProviderResponseStream, String> {
    let AttemptWiring {
        request_authority,
        submission_observer,
        options,
        thinking_flip,
    } = wiring;
    let route = lock_recover(route, "route").clone();
    let routes = fallback_attempt_routes(&route);
    let is_metered = routes.iter().any(|route| {
        rightkit_llm::agent_providers::is_priced_model(&route.model_id)
            && !rightkit_llm::agent_providers::is_subscription_billed_route(&route.base_url)
    });
    let request_bytes = serde_json::to_string(&wire).map_or(0, |text| text.len())
        + serde_json::to_string(&tool_defs).map_or(0, |text| text.len());
    let max_output_tokens = request_metadata.max_output_tokens;
    let reservation = match (is_metered, lineage_budget) {
        (true, Some(budget)) => Some(
            budget
                .reserve_estimate_wait(estimated_call_microusd(
                    &routes,
                    request_bytes,
                    max_output_tokens,
                ))
                .await?,
        ),
        _ => None,
    };

    let mut options = options;
    let mut prior_fallback_error = None;
    let mut first_stream = None;
    let mut first_route = None;
    let mut next_index = 0usize;
    for (index, attempt_route) in routes.iter().enumerate() {
        let result = loop {
            let result = match build_attempt_client(
                attempt_route,
                request_authority.as_ref(),
                submission_observer.as_ref(),
                options.clone(),
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
                Err(error) => return Err(llm_error_message(&error)),
            };
            if let Err(error) = &result {
                if take_thinking_flip(&thinking_flip, error) {
                    options.prefix_mismatch =
                        rightkit_llm::agent_providers::ThinkingBlockBinding::DropBlock;
                    continue;
                }
            }
            break result;
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
                    || llm_error_message(&error),
                    |prior| {
                        format!(
                            "{prior}; {} failed: {}",
                            attempt_route.model_id,
                            llm_error_message(&error)
                        )
                    },
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
                    options.clone(),
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
                        yield Err(*error);
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
    // Cache reads and writes are priced per model (0.025x to 0.1x for reads). A
    // flat 0.1x made read-heavy runs reach the cost ceiling several times early.
    let (cache_read_price, cache_write_price) =
        rightkit_llm::agent_providers::cache_price_per_1k(&route.model_id)
            .unwrap_or((prompt_price * 0.1, prompt_price * 1.25));
    let (input_factor, output_factor) =
        match rightkit_llm::agent_providers::long_context_tier(&route.model_id) {
            Some((threshold, input, output))
                if u64::from(prompt_tokens)
                    + u64::from(cache_read_tokens)
                    + u64::from(cache_creation_tokens)
                    > threshold =>
            {
                (input, output)
            }
            _ => (1.0, 1.0),
        };
    let cost = ((prompt_tokens as f64 / 1000.0) * prompt_price
        + (cache_read_tokens as f64 / 1000.0) * cache_read_price
        + (cache_creation_tokens as f64 / 1000.0) * cache_write_price)
        * input_factor
        + (completion_tokens as f64 / 1000.0) * completion_price * output_factor;
    rightkit_agent_engine_core::usd_to_microusd(cost).unwrap_or(0)
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
        api_key_required: false,
        compat: route.compat,
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
            api_key_required: false,
            compat: route.compat,
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
/// refusal (HTTP 400, 401 or 403, the typed `Auth`, or `QuotaExceeded`: a quota/billing refusal is definite and
/// unbilled, and another route or account may have credit). Everything else may have been delivered and billed, or is
/// not a definite refusal, and gets no fallback: 408, 425, 429 (typed `RateLimited` too), any 5xx, a post-send
/// `Network` error, a cut stream (`Sse`), `DeliveryUncertain`, and an in-body `Provider` error. A retryable 5xx
/// (500, 502, 503, 504, 529) on the shared stream-establishment ladder was already retried on the same route, so
/// the turn is given up rather than failed over; any other 5xx, and a 5xx on a path with no ladder (Codex
/// streams, non-streamed requests), may have been billed.
fn provider_error_allows_model_fallback(error: &ProviderError) -> bool {
    match error {
        ProviderError::Http { status, .. } => matches!(*status, 400 | 401 | 403),
        ProviderError::NotDelivered(_) | ProviderError::Auth => true,
        // A quota/billing refusal is definite and unbilled; another route or account may have credit.
        ProviderError::QuotaExceeded(_) => true,
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

/// The text a `ProviderError` carries to the engine. The engine classifies overflow from this string, so a
/// provider-reported context overflow (for example a Groq 400 "reduce the length of the messages") gets a stable
/// "context length exceeded" prefix and compacts instead of failing the run.
fn llm_error_message(error: &ProviderError) -> String {
    if rightkit_llm::agent_providers::transport::classify_context_overflow(error) {
        format!("context length exceeded: {error}")
    } else {
        error.to_string()
    }
}

/// A completion the model declined: Anthropic ends it with `stop_reason: "refusal"` and no content. It is
/// reported as what it is (and never re-asked) instead of as a reasoning-only reply.
const MODEL_REFUSAL_MESSAGE: &str =
    "the model declined to answer this request (stop_reason: refusal); rephrase it or try another model";

fn is_refusal_finish(reason: &Option<String>) -> bool {
    reason
        .as_deref()
        .is_some_and(|reason| reason.eq_ignore_ascii_case("refusal"))
}

/// The provider stopped at its output limit. A reply cut there may have nothing visible left (a
/// cut-off tool call is dropped by the adapter, EFF-009); that is not a reasoning-only reply, so it
/// reaches the engine as an empty, incomplete turn and gets the output-limit retry.
fn is_output_limit_finish(reason: &Option<String>) -> bool {
    reason.as_deref().is_some_and(|reason| {
        matches!(
            reason.trim().to_ascii_lowercase().as_str(),
            "length" | "max_tokens" | "max_output_tokens" | "token_limit" | "output_limit"
        )
    })
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
        return Err(rightkit_llm::agent_providers::transport::REASONING_ONLY_MESSAGE.to_string());
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

    /// Whether the turn produced any visible text or tool call so far.
    fn produced_output(&self) -> bool {
        self.visible_started || self.has_tool_calls
    }

    fn finish(&mut self, cut_off: bool) -> Result<(String, Option<String>), &'static str> {
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
        if !self.visible_started && !self.has_tool_calls && !cut_off {
            return Err(rightkit_llm::agent_providers::transport::REASONING_ONLY_MESSAGE);
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
                rightkit_llm::agent_providers::ProviderStreamEvent::UsageCost { cost_usd } => {
                    yield rightkit_llm::agent_providers::ProviderStreamEvent::UsageCost { cost_usd };
                }
                rightkit_llm::agent_providers::ProviderStreamEvent::RateLimit(limit) => {
                    yield rightkit_llm::agent_providers::ProviderStreamEvent::RateLimit(limit);
                }
                partial @ rightkit_llm::agent_providers::ProviderStreamEvent::ToolArgumentsPartial { .. } => {
                    yield partial;
                }
                rightkit_llm::agent_providers::ProviderStreamEvent::Finished(reason) => {
                    if is_refusal_finish(&reason) && !normalizer.produced_output() {
                        Err(ProviderError::Provider(MODEL_REFUSAL_MESSAGE.into()))?;
                    }
                    let (text, reasoning) = normalizer
                        .finish(is_output_limit_finish(&reason))
                        .map_err(|message| ProviderError::Provider(message.into()))?;
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
                self.configure_openai_client(OpenAiCompatClient::clone(client), metadata)?
                    .complete_with_tools_metadata(wire, tool_defs, &request_metadata)
                    .await
                    .map_err(|e| llm_error_message(&e))?,
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
                .map_err(|error| llm_error_message(&error))?;
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
                let request_bytes = serde_json::to_string(&wire).map_or(0, |text| text.len())
                    + serde_json::to_string(&tool_defs).map_or(0, |text| text.len());
                let (turn, attempt_route, reservation) = attempt_dynamic_route(
                    self.lineage_budget.as_ref(),
                    request_bytes,
                    request_metadata.max_output_tokens,
                    route,
                    AttemptWiring {
                        request_authority,
                        submission_observer,
                        options: self.anthropic_options(metadata),
                        thinking_flip: self.thinking_flip(metadata),
                    },
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

        if is_refusal_finish(&turn.finish_reason)
            && turn.text.trim().is_empty()
            && turn.tool_calls.is_empty()
        {
            return Err(MODEL_REFUSAL_MESSAGE.to_string());
        }
        let (text, reasoning) = sanitize_provider_content(
            turn.text,
            turn.reasoning,
            !turn.tool_calls.is_empty() || is_output_limit_finish(&turn.finish_reason),
        )?;

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
                .configure_openai_client(OpenAiCompatClient::clone(client), metadata)?
                .stream_with_tools_metadata(wire, tool_defs, &request_metadata)
                .await
                .map_err(|error| llm_error_message(&error))?,
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
                    AttemptWiring {
                        request_authority,
                        submission_observer,
                        options: self.anthropic_options(metadata),
                        thinking_flip: self.thinking_flip(metadata),
                    },
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
                        .map_err(|error| llm_error_message(&error))?,
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
                        .map_err(|error| llm_error_message(&error))?,
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
                    rightkit_llm::agent_providers::ProviderStreamEvent::UsageCost { cost_usd } => {
                        LlmStreamEvent::UsageCost { cost_usd }
                    }
                    rightkit_llm::agent_providers::ProviderStreamEvent::RateLimit(limit) => {
                        LlmStreamEvent::RateLimit {
                            remaining_requests: limit.remaining_requests,
                            remaining_tokens: limit.remaining_tokens,
                            reset_requests_secs: limit.reset_requests_secs,
                            reset_tokens_secs: limit.reset_tokens_secs,
                        }
                    }
                    rightkit_llm::agent_providers::ProviderStreamEvent::Finished(reason) => {
                        LlmStreamEvent::Finished(reason)
                    }
                    rightkit_llm::agent_providers::ProviderStreamEvent::ToolArgumentsPartial {
                        id,
                        name,
                        arguments_json,
                    } => LlmStreamEvent::ToolArgumentsPartial {
                        id,
                        name,
                        arguments_json,
                    },
                })
                .map_err(|error| llm_error_message(&error))
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
    ) -> rightkit_agent_coding_tools::registry::PromotedToolState {
        lock_recover(&self.promoted_tools, "promoted tool state").clone()
    }

    /// Which provider (the route's host) and model the reply came from. Only routes that can
    /// change mid-session report it; a fixed client has nothing to distinguish. Reported only
    /// when the host wants it on replies (`host_options::message_origin_on_replies`).
    fn message_origin(&self) -> Option<rightkit_agent_engine_protocol::MessageOrigin> {
        if !crate::host_options::message_origin_on_replies() {
            return None;
        }
        let host = |url: &str| {
            url.split("://")
                .nth(1)
                .unwrap_or(url)
                .split('/')
                .next()
                .unwrap_or_default()
                .to_string()
        };
        match &self.inner {
            ProviderClientInner::Dynamic { route, .. } => {
                let route = lock_recover(route, "route");
                Some(rightkit_agent_engine_protocol::MessageOrigin {
                    provider: host(&route.base_url),
                    model: route.model_id.clone(),
                })
            }
            ProviderClientInner::Direct { route, .. } => {
                let route = lock_recover(route, "session.inference_route");
                Some(rightkit_agent_engine_protocol::MessageOrigin {
                    provider: host(&route.endpoint),
                    model: route.selection.model_id.clone(),
                })
            }
            ProviderClientInner::Static(_) => None,
        }
    }

    /// Live route identity for prompt-cache break attribution: a PATCHed model
    /// or route changes this between requests, which is an expected break.
    /// Fixed-model clients (`Static`) cannot switch.
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
            ProviderClientInner::Static(_) => None,
        }
    }

    fn image_budget_policy(&self) -> rightkit_agent_engine_core::image_eviction::ImageBudgetPolicy {
        match &self.inner {
            ProviderClientInner::Static(client) => {
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

#[cfg(test)]
mod plan_model_tests {
    use super::*;
    use rightkit_agent_policy::PermissionMode;

    fn route(model: &str) -> SharedProviderRoute {
        Arc::new(Mutex::new(
            ProviderRoute::from_model_auth(
                model,
                "http://127.0.0.1:9/v1",
                rightkit_llm::agent_providers::subscription_auth::ModelAuth::api_key(
                    "k".to_string(),
                ),
            )
            .expect("route"),
        ))
    }

    fn model_of(route: &SharedProviderRoute) -> String {
        route.lock().unwrap().model_id.clone()
    }

    #[test]
    fn plan_mode_uses_the_plan_model_and_approval_restores_the_session_model() {
        let route = route("build-model");
        let mut saved = None;
        apply_plan_model(&route, PermissionMode::Plan, "plan-model", &mut saved);
        assert_eq!(model_of(&route), "plan-model");
        // A repeated plan state changes nothing and does not lose the model to restore.
        apply_plan_model(&route, PermissionMode::Plan, "plan-model", &mut saved);
        assert_eq!(model_of(&route), "plan-model");
        apply_plan_model(&route, PermissionMode::Ask, "plan-model", &mut saved);
        assert_eq!(model_of(&route), "build-model");
        assert_eq!(saved, None);
    }

    #[test]
    fn a_model_switch_made_while_planning_is_kept() {
        let route = route("build-model");
        let mut saved = None;
        apply_plan_model(&route, PermissionMode::Plan, "plan-model", &mut saved);
        route.lock().unwrap().model_id = "user-picked".to_string();
        apply_plan_model(&route, PermissionMode::Ask, "plan-model", &mut saved);
        assert_eq!(model_of(&route), "user-picked");
    }

    #[test]
    fn other_modes_leave_the_route_alone() {
        let route = route("build-model");
        let mut saved = None;
        apply_plan_model(&route, PermissionMode::Ask, "plan-model", &mut saved);
        assert_eq!(model_of(&route), "build-model");
    }
}
