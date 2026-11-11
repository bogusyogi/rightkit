//! Membrane federation gateway — Rust shell.
//!
//! Per dispatch §G3A + §G5: the authenticated local Membrane gateway
//! is the SOLE owner of provider fan-out and admission. Clients (Claude,
//! Codex, MCP) submit (task, repo_root, client, session, max_tokens, anchors,
//! scope_grant_id, remainingContextCeiling); the gateway invokes provider adapters in
//! parallel, runs the deterministic in-process admission, and emits a
//! content-free ContextPacket v1 plus per-candidate ContextReceipt v2.
//!
//! This Rust module is the native dispatcher and planner-envelope adapter.
//!
//! Provider payload formats and SQLite details never enter client
//! adapters. Cortex durable storage is never modified. Bearer tokens
//! are passed via the standard `RIGHTKIT_CONTEXT_API_TOKEN_FILE` env, never in
//! argv or stdout. ScopeGrant enforcement happens in native source bindings.

use super::federation_sources::RuntimeReleaseSource;
use super::{federation_sources, native_federation};
use crate::pull::packet_selection::WholeResponseError;
use crate::pull::planner::{plan, ContextCandidateSetV1, PlannerInput};
use rightkit_context_protocol::{
    LoadedContextIdentitiesV1, ObservationCoverageV1, PublicationFenceChangeV1,
    PublicationFenceStatusV1, PublicationFenceV1, RepresentationClassV1, RepresentationHandleV1,
};
use rightkit_fs::digest::sha256_bytes;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

const COMPACTION_DECISION_LEASE_MS: u64 = 30_000;
static COMPACTION_MONOTONIC_ORIGIN: OnceLock<Instant> = OnceLock::new();

fn attach_compaction_federation_decision(
    fields: &mut serde_json::Map<String, Value>,
    request: &Value,
    session: &str,
    repository_id: &str,
    selection: &crate::pull::selection::PacketReductionSelectionV1,
) -> Result<(), String> {
    let Some(marker) = request.get("requestCompaction") else {
        return Ok(());
    };
    let marker = marker
        .as_object()
        .ok_or_else(|| "requestCompaction must carry opId and sessionId".to_owned())?;
    let op_id = marker
        .get("opId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "requestCompaction requires opId".to_owned())?;
    let marker_session = marker
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "requestCompaction requires sessionId".to_owned())?;
    if marker_session != session {
        return Err("requestCompaction sessionId does not match federation session".to_owned());
    }
    let selected_item_ids = selection
        .selected_representation
        .content
        .get("blocks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let selection_hash = sha256_digest(&Value::Array(
        selected_item_ids
            .iter()
            .cloned()
            .map(Value::String)
            .collect(),
    ))?;
    let issued = COMPACTION_MONOTONIC_ORIGIN
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis() as u64;
    let restart_epoch = crate::runtime_receipt::current_snapshot()
        .map(|receipt| receipt.startup_generation)
        .ok_or_else(|| "compaction federation requires runtime startup identity".to_owned())?;
    let decision = rightkit_context_core::compaction::issue_compaction_federation_decision(
        op_id.to_owned(),
        format!("{session}:{op_id}"),
        repository_id.to_owned(),
        selected_item_ids,
        selection_hash,
        issued,
        COMPACTION_DECISION_LEASE_MS,
        restart_epoch,
    )
    .map_err(|error| error.to_string())?;
    fields.insert(
        "compactionFederationDecision".to_owned(),
        serde_json::to_value(decision)
            .map_err(|error| format!("serialize compaction federation decision: {error}"))?,
    );
    Ok(())
}

fn federation_session_id(session: Option<String>) -> String {
    session
        .unwrap_or_else(|| crate::store::opaque_correlation_token("anonymous-session", "session"))
}

/// Pull's two explicit request-time budget modes (implementation-contract
/// §"Pull budget contract", PUL-050). `BoundedResponse` fits the complete
/// rendered response under a positive caller- or configured-declared budget
/// and reports unobserved host capacity without failing; `HostFit` requires
/// a trusted, fresh, exact, identity-matched request-time H8 and never
/// silently downgrades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullBudgetMode {
    BoundedResponse,
    HostFit,
}

impl PullBudgetMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BoundedResponse => "bounded_response",
            Self::HostFit => "host_fit",
        }
    }
}

/// Budget-mode inputs carried through every bounded/host-fit emission path.
#[derive(Debug, Clone)]
pub struct BudgetFit {
    /// Caller-declared mode; `None` means the request did not declare one and
    /// the mode was mapped by supplied-observation compatibility rules.
    pub declared: Option<PullBudgetMode>,
    /// Positive declared response budget in `o200k_base/1` tokens.
    pub response_tokens: u64,
    /// Where the declared budget came from: `"caller"` or `"configured"`.
    pub provenance: &'static str,
    /// The request is an explicit pull (`membrane context pull`, one-shot or resident), whose
    /// consumer receives a complete response in one delivery mode: under a declared budget the
    /// representation is chosen by the exact bytes of that response, and Cortex recall may
    /// answer by the registry scan when the FTS query finds nothing. Hooks and the `/federate`
    /// routes set `false` and keep their serialized-packet selection and ambient recall.
    pub explicit_pull: bool,
    /// What the explicit pull's consumer receives (delivery mode and receipt id); `Some` exactly
    /// when `explicit_pull`.
    pub projection: Option<crate::hook::PullProjectionSpec>,
}

fn budget_mode_refusal(kind: &str, reason: String) -> (u16, String) {
    (
        400,
        serde_json::json!({
            "error": "request_time_selection_refused",
            "kind": kind,
            "reason": reason,
        })
        .to_string(),
    )
}

/// The caller's declared mode. `budgetMode` is the canonical field;
/// `budgetPolicy` is the legacy spelling (`configured_cap` → bounded,
/// `host_observed` → host-fit). An unrecognized or conflicting declaration
/// is a typed refusal — never an implicit fallback to either mode.
pub(crate) fn declared_budget_mode(body: &Value) -> Result<Option<PullBudgetMode>, (u16, String)> {
    let bounded = |value: &str| {
        matches!(
            value,
            "bounded_response" | "bounded-response" | "boundedResponse" | "configured_cap"
        )
    };
    let host_fit =
        |value: &str| matches!(value, "host_fit" | "host-fit" | "hostFit" | "host_observed");
    let parse = |raw: Option<&Value>| -> Option<Result<PullBudgetMode, (u16, String)>> {
        let raw = raw?;
        if raw.is_null() {
            return None;
        }
        match raw.as_str() {
            Some(mode) if bounded(mode) => Some(Ok(PullBudgetMode::BoundedResponse)),
            Some(mode) if host_fit(mode) => Some(Ok(PullBudgetMode::HostFit)),
            _ => Some(Err(budget_mode_refusal(
                "invalid_budget_mode",
                "declared budget mode must be bounded_response or host_fit".to_owned(),
            ))),
        }
    };
    let canonical = parse(body.get("budgetMode"));
    let legacy = parse(body.get("budgetPolicy"));
    match (canonical, legacy) {
        (Some(Ok(left)), Some(Ok(right))) if left != right => Err(budget_mode_refusal(
            "invalid_budget_mode",
            "budgetMode and budgetPolicy declare conflicting modes".to_owned(),
        )),
        (Some(result), _) | (None, Some(result)) => result.map(Some),
        (None, None) => Ok(None),
    }
}

/// Positive declared response budget for bounded-response. `responseBudget`
/// / `responseBudgetTokens` are the explicit spellings (tokens); `maxTokens`
/// is the caller/configured fallback. A supplied non-positive or non-numeric
/// budget is a typed refusal, never a zero-cap interpretation.
pub(crate) fn declared_response_budget(
    body: &Value,
    max_tokens: usize,
) -> Result<(u64, &'static str), (u16, String)> {
    for field in ["responseBudget", "responseBudgetTokens"] {
        if let Some(raw) = body.get(field) {
            return match raw.as_u64().filter(|value| *value > 0) {
                Some(tokens) => Ok((tokens.min(1_000_000), "caller")),
                None => Err(budget_mode_refusal(
                    "invalid_response_budget",
                    format!("{field} must be a positive token count"),
                )),
            };
        }
    }
    Ok((
        max_tokens as u64,
        if body.get("maxTokens").is_some() {
            "caller"
        } else {
            "configured"
        },
    ))
}

/// Uniform content-free budget receipts on every Pull envelope: requested &
/// effective mode, the declared response budget with estimator/unit and
/// provenance, and observed-or-unknown host capacity. Host capacity is
/// never invented.
fn insert_budget_mode_fields(
    fields: &mut serde_json::Map<String, Value>,
    mode: PullBudgetMode,
    declared: Option<PullBudgetMode>,
    response_budget_tokens: u64,
    response_budget_provenance: &str,
    ceiling: Option<&rightkit_context_protocol::RemainingContextCeilingV1>,
) {
    fields.insert(
        "budgetMode".to_owned(),
        Value::String(mode.as_str().to_owned()),
    );
    fields.insert(
        "requestedBudgetMode".to_owned(),
        Value::String(declared.unwrap_or(mode).as_str().to_owned()),
    );
    fields.insert(
        "responseBudget".to_owned(),
        serde_json::json!({
            "schemaVersion": 1,
            "value": response_budget_tokens,
            "unit": "tokens",
            "estimatorBasis": "o200k_base/1",
            "provenance": response_budget_provenance,
        }),
    );
    fields.insert(
        "hostCapacity".to_owned(),
        match ceiling {
            Some(ceiling) => serde_json::json!({
                "coverage": "complete",
                "ceilingId": ceiling.ceiling_id,
                "sessionId": ceiling.session_id,
                "remainingTokens": ceiling.remaining_tokens.estimate.value,
            }),
            None => serde_json::json!({
                "coverage": "unavailable",
                "reason": "host_observation_not_supplied",
            }),
        },
    );
}

/// The block text a host injects from one emitted packet — the rendered
/// response on bounded-response surfaces. Transport JSON wrappers are not
/// host-rendered content here; the serialized packet measure is reported
/// separately on the receipt.
fn rendered_block_text(packet: &Value) -> Option<String> {
    crate::pull::packet_selection::consumer_visible_text(packet)
}

#[derive(Debug)]
enum NativeRouteError {
    Internal(String),
    PolicyChanged(String),
    RequestTime(crate::pull::selection::PacketReductionRequestError),
}

impl From<String> for NativeRouteError {
    fn from(error: String) -> Self {
        Self::Internal(error)
    }
}

/// Run native federation end-to-end and emit its final planner envelope.
#[allow(clippy::too_many_arguments)]
pub fn run_federate(
    task: String,
    repo: PathBuf,
    max_tokens: usize,
    packet_char_budget_override: Option<usize>,
    packet_char_budget_model: Option<String>,
    client: String,
    session: Option<String>,
    anchors: Vec<String>,
    scope_grant_id: Option<String>,
    federation_script: Option<PathBuf>,
    accepted_receipt_versions: Vec<u32>,
) -> Result<(), String> {
    // Kept for V1 CLI call compatibility; native execution never consults it.
    let _ = federation_script;
    // Explicit one-shot federate is not latency-bound like an inline editor
    // request: with the Hub off it must cold-build the Blueprint generation
    // during freshness binding (measured ~28-40s on a real repo), then run
    // providers. 2s (and even 30s) exhausted during owner binding, so give
    // it a build-class budget. A resident Hub keeps freshness warm and
    // returns far faster; this ceiling only bounds the cold one-shot.
    let payload = run_federate_value(
        task,
        repo,
        max_tokens,
        packet_char_budget_override,
        packet_char_budget_model,
        client,
        session,
        anchors,
        scope_grant_id,
        accepted_receipt_versions,
        180_000,
        "explicit",
        None,
        None,
        None,
        None,
        None,
        BudgetFit {
            declared: None,
            response_tokens: max_tokens as u64,
            provenance: "caller",
            explicit_pull: false,
            projection: None,
        },
        None,
        None,
    )?;
    crate::cli::emit_stdout(format_args!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|e| format!("serialize: {e}"))?
    ));
    Ok(())
}

/// Ambient hook-mode federate: a latency-bound caller. When a host supplies a
/// validated H8 observation it is passed to the shared planner verbatim;
/// otherwise the receipt names the bounded configured-cap degradation policy.
pub fn hook_mode_federate(
    task: &str,
    repo: &Path,
    max_tokens: usize,
    client: &str,
    session: &str,
    deadline_ms: u64,
) -> Result<Value, String> {
    hook_mode_federate_with_observation(task, repo, max_tokens, client, session, deadline_ms, None)
}

/// Hook adapter carrying a host-produced H8 observation when available. The
/// planner receives this exact observation; it never fabricates capacity.
/// A supplied observation is bound to this request's session/task identity
/// and structural validity before use — a stale, inexact, malformed or
/// mismatched observation is a typed refusal, never a silent downgrade to
/// configured-cap.
pub fn hook_mode_federate_with_observation(
    task: &str,
    repo: &Path,
    max_tokens: usize,
    client: &str,
    session: &str,
    deadline_ms: u64,
    ceiling: Option<rightkit_context_protocol::RemainingContextCeilingV1>,
) -> Result<Value, String> {
    hook_mode_federate_authorized(
        task,
        repo,
        max_tokens,
        client,
        session,
        deadline_ms,
        ceiling,
        None,
        None,
    )
}

/// The explicit-pull sibling of [`hook_mode_federate_with_observation`]: the same latency-bound
/// federation, reading the Cortex scope the caller was authorized for. `authorized_cortex_scope`
/// is the binding descriptor `federation_sources::authorized_read_scope` returned at the pull
/// entry, decided once before selection; `None` is the unenrolled read (the scope derived from
/// the working directory) and is also what hooks pass, so a hook never changes its scope here.
/// `projection` marks an explicit `membrane context pull` (one-shot or resident) and names what
/// its consumer receives (delivery mode, receipt id), including for a root the registry does not
/// hold: only that request selects its representation by the complete response its consumer
/// receives and lets Cortex recall fall back to the registry scan. It is never inferred from
/// `authorized_cortex_scope`, which is `None` for hooks and for an unenrolled explicit pull
/// alike.
#[allow(clippy::too_many_arguments)]
pub fn hook_mode_federate_authorized(
    task: &str,
    repo: &Path,
    max_tokens: usize,
    client: &str,
    session: &str,
    deadline_ms: u64,
    ceiling: Option<rightkit_context_protocol::RemainingContextCeilingV1>,
    authorized_cortex_scope: Option<Value>,
    projection: Option<crate::hook::PullProjectionSpec>,
) -> Result<Value, String> {
    if let Some(observed) = ceiling.as_ref() {
        if let Err(error) = observed.validate() {
            return Err(format!("request_time_selection_refused:h8_invalid:{error}"));
        }
        if observed.session_id != session {
            return Err(format!(
                "request_time_selection_refused:h8_identity_mismatch:sessionId expected {session} observed {}",
                observed.session_id
            ));
        }
        match observed.task_id.value.as_deref() {
            Some(observed_task) if observed_task == task => {}
            Some(observed_task) => {
                return Err(format!(
                    "request_time_selection_refused:h8_identity_mismatch:taskId expected {task} observed {observed_task}"
                ));
            }
            None => {
                return Err(
                    "request_time_selection_refused:h8_invalid:taskId is not an exact observation"
                        .to_owned(),
                );
            }
        }
    }
    run_federate_value(
        task.to_owned(),
        repo.to_path_buf(),
        max_tokens,
        None,
        None,
        client.to_owned(),
        Some(session.to_owned()),
        Vec::new(),
        None,
        Vec::new(),
        deadline_ms,
        if ceiling.is_some() {
            "host_observed"
        } else {
            "configured_cap"
        },
        ceiling,
        None,
        None,
        None,
        None,
        BudgetFit {
            declared: None,
            response_tokens: max_tokens as u64,
            provenance: "configured",
            explicit_pull: projection.is_some(),
            projection,
        },
        authorized_cortex_scope,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_federate_value(
    task: String,
    repo: PathBuf,
    max_tokens: usize,
    packet_char_budget_override: Option<usize>,
    packet_char_budget_model: Option<String>,
    client: String,
    session: Option<String>,
    anchors: Vec<String>,
    scope_grant_id: Option<String>,
    accepted_receipt_versions: Vec<u32>,
    deadline_ms: u64,
    budget_policy: &str,
    ceiling: Option<rightkit_context_protocol::RemainingContextCeilingV1>,
    resident: Option<&crate::MemoryStore>,
    shared_runtime: Option<tokio::runtime::Handle>,
    inherited_cancellation: Option<tokio_util::sync::CancellationToken>,
    inherited_deadline: Option<rightkit_context_federation::deadline::Deadline>,
    fit: BudgetFit,
    authorized_cortex_scope: Option<Value>,
    push_resolver_token: Option<&str>,
) -> Result<Value, String> {
    let inherited = crate::serve::inherited_push_control();
    let inherited_deadline =
        inherited_deadline.or_else(|| inherited.as_ref().map(|control| control.deadline));
    let inherited_cancellation = inherited_cancellation
        .or_else(|| inherited.map(|control| control.cancellation))
        .unwrap_or_else(|| crate::service::lifecycle_control().cancellation_token());
    let shared_runtime = shared_runtime.or_else(|| tokio::runtime::Handle::try_current().ok());
    let started = Instant::now();
    // Per-stage gateway timings. Concurrent-Pull deadline misses cannot be
    // attributed (admission vs provider vs planner) without these; they ride
    // the response as `gatewayStageTimingsMs` alongside federationMetrics.
    let mut stage_timings = serde_json::Map::new();
    let mut mark_stage = |name: &str, from: Instant| {
        stage_timings.insert(
            name.to_owned(),
            Value::from(from.elapsed().as_millis() as u64),
        );
    };
    let local_deadline = started + std::time::Duration::from_millis(deadline_ms);
    let federation_deadline = rightkit_context_federation::deadline::Deadline::at(
        inherited_deadline
            .map(|deadline| deadline.instant().min(local_deadline))
            .unwrap_or(local_deadline),
    );
    check_pull_live(federation_deadline, &inherited_cancellation)?;
    let stage_started = Instant::now();
    let root = repo
        .canonicalize()
        .map_err(|error| format!("resolve repository root: {error}"))?;
    let session_id = federation_session_id(session);
    // Reduction needs a host-held resolver so omitted bytes stay recoverable;
    // bounded-response gets the same recovery binding as host-fit.
    let recovery_store = crate::pull::recovery::RecoveryStore::configured();
    let recovery_scope = push_resolver_token
        .and_then(|_| crate::pull::recovery::RecoveryScope::new(&root, &session_id).ok());
    let recovery = recovery_scope
        .as_ref()
        .zip(push_resolver_token)
        .map(
            |(scope, resolver)| crate::pull::selection::RecoveryContext {
                store: &recovery_store,
                scope,
                resolver_token: resolver,
            },
        );
    let release_generation = RuntimeReleaseSource::generation()?;
    let mut request = match ceiling.as_ref() {
        Some(observed) => native_request_with_h8(
            &task,
            &root,
            max_tokens,
            deadline_ms,
            release_generation,
            &client,
            &session_id,
            anchors,
            scope_grant_id.clone(),
            None,
            observed,
        ),
        None => native_request(
            &task,
            &root,
            max_tokens,
            deadline_ms,
            release_generation,
            &client,
            &session_id,
            anchors,
            scope_grant_id.clone(),
            None,
        ),
    };
    if let Some(descriptor) = authorized_cortex_scope.clone() {
        request
            .extensions
            .insert("cortexScopeDescriptor".to_owned(), descriptor);
    }
    mark_stage("request_setup", stage_started);
    let stage_started = Instant::now();
    let admitted_grant = admitted_publication_grant_until(&request, Some(federation_deadline))?;
    mark_stage("grant_admission", stage_started);
    let stage_started = Instant::now();
    let foreground = matches!(budget_policy, "configured_cap" | "host_observed");
    let store = match resident {
        Some(store) => store.clone(),
        None if foreground => crate::service::open_installed_lexical_store()?,
        None => crate::service::open_installed_store()?,
    };
    mark_stage("store_open", stage_started);
    let stage_started = Instant::now();
    // Resident dispatch borrows engine runtime; standalone compatibility
    // callers use the scoped bridge.
    let (response, native_metrics, freshness, cortex_recall) =
        native_federation::run_on_runtime(shared_runtime.as_ref(), async {
            let bindings = if foreground && resident.is_none() {
                federation_sources::NativeSourceBindings::with_ambient_store_and_deadline(
                    &root,
                    scope_grant_id.as_deref(),
                    store.clone(),
                    federation_deadline,
                    fit.explicit_pull,
                )?
            } else {
                federation_sources::NativeSourceBindings::with_store_and_deadline(
                    &root,
                    scope_grant_id.as_deref(),
                    store.clone(),
                    Some(federation_deadline),
                )?
            };
            let cortex_recall = bindings.cortex_recall.clone();
            let native = if foreground {
                native_federation::NativeFederation::hook(bindings)?
            } else {
                native_federation::NativeFederation::new(bindings)?
            };
            let response = native
                .federate_until(
                    &request,
                    inherited_cancellation.clone(),
                    federation_deadline,
                )
                .await?;
            let freshness = native
                .freshness_snapshot()
                .ok_or_else(|| "native freshness verdict unavailable".to_owned())?;
            Ok::<_, String>((
                response,
                native.metrics_snapshot(),
                freshness,
                cortex_recall,
            ))
        })?;
    mark_stage("native_federation", stage_started);
    check_pull_live(federation_deadline, &inherited_cancellation)?;
    let ccs = native_response_to_ccs(&response, &request, &freshness);
    let provisional_requirement_map = ccs.get("requirementEvidenceMap").cloned();
    let native_receipts = collect_native_receipts(&response);
    // How Cortex answered the lexical lane (`fts5`, `scan_fallback:...`, or a degraded mode), as the
    // memory source recorded it, so the receipt says which index served the memory evidence.
    let cortex_recall_mode = cortex_recall.lock().ok().and_then(|mode| mode.clone());
    // The first capacity refusal of an explicit pull's whole-response selection, kept so a refit
    // at a smaller packet allowance that admits nothing still reports the cause the caller can
    // act on (the size of the complete response), not the allowance it ended on.
    let mut first_capacity: Option<(Vec<(String, String)>, String)> = None;
    fit_native_plan(max_tokens, |packet_allowance| {
        let planner_started = Instant::now();
        check_pull_live(federation_deadline, &inherited_cancellation)?;
        let mut payload = envelope_from_ccs(
            &serde_json::to_string(&ccs)
                .map_err(|error| format!("serialize native candidates: {error}"))?,
            EnvelopeInput {
                max_tokens: packet_allowance,
                packet_char_budget_override,
                packet_char_budget_model: packet_char_budget_model.clone(),
                accepted_receipt_versions: if accepted_receipt_versions.is_empty() {
                    vec![2]
                } else {
                    accepted_receipt_versions.clone()
                },
                scope_grant_present: scope_grant_id.is_some(),
                consumer_resolvers: Vec::new(),
                scope_grant_fence: post_fusion_publication_fence_until(
                    &admitted_grant,
                    Some(federation_deadline),
                )?,
                gateway_process_ms: started.elapsed().as_secs_f64() * 1000.0,
            },
        )?;
        mark_stage_timing(&mut stage_timings, "envelope", planner_started);
        if let Some(fields) = payload.as_object_mut() {
            fields.insert("transport".to_owned(), Value::String("native".to_owned()));
            fields.insert(
                "federationMetrics".to_owned(),
                serde_json::json!(native_metrics),
            );
            fields.insert(
                "gatewayStageTimingsMs".to_owned(),
                Value::Object(stage_timings.clone()),
            );
            merge_native_receipts(fields, native_receipts.clone());
            let packet_omissions = fields
                .get("packet")
                .and_then(|packet| packet.get("omissions"))
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            fields.insert(
                "budgetReduction".to_owned(),
                serde_json::json!({
                    "misleadingFragmentRetained": false,
                    "omissionReasons": packet_omissions,
                    "droppedCandidateCount": fields
                        .get("packet")
                        .and_then(|packet| packet.get("omissions"))
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len),
                }),
            );
            let final_map = fields.get("requirementEvidenceMap").cloned();
            let final_packet = fields.get("packet").cloned();
            merge_bm10_accounting(
                fields,
                provisional_requirement_map.as_ref(),
                final_map.as_ref(),
                final_packet.as_ref(),
            );
            fields.insert(
                "budgetPolicy".to_owned(),
                Value::String(budget_policy.to_owned()),
            );
            fields.insert(
                "cortexRecall".to_owned(),
                serde_json::json!({
                    "lexicalMode": cortex_recall_mode,
                    "explicitPull": fit.explicit_pull,
                }),
            );
            // Pull budget contract (PUL-050/051): the emitted packet is always
            // fitted to the effective mode. A validated request-time H8 keeps
            // host-fit semantics — the largest complete representation under the
            // observed ceiling is emitted, or the request refuses typed. Without
            // one, the declared caller/configured response budget bounds what the
            // consumer receives, measured exactly under o200k_base/1: an explicit
            // pull counts the complete response in its delivery mode, a hook the
            // serialized packet. An over-budget response is a typed insufficiency,
            // never a silent over-budget emission.
            if let Some(packet_value) = fields.get("packet").cloned() {
                if let Ok(packet) = serde_json::from_value::<
                    rightkit_memory_core::planner::ContextPacketV1,
                >(packet_value)
                {
                    if packet.blocks.is_empty() {
                        // Candidates the planner dropped for want of budget are a capacity
                        // failure, never "nothing matched": genuine zero retrieval has none.
                        let exhausted = fields.get("receipts").and_then(Value::as_array).map_or(
                            0,
                            |receipts| {
                                receipts
                                    .iter()
                                    .filter(|receipt| {
                                        receipt.get("reason").and_then(Value::as_str)
                                            == Some("budget_exhausted")
                                    })
                                    .count()
                            },
                        );
                        if fit.explicit_pull && ceiling.is_none() && exhausted > 0 {
                            return Err(match first_capacity.clone() {
                                Some((protected, reason)) => {
                                    NativePlanError::Capacity { protected, reason }
                                }
                                None => NativePlanError::Capacity {
                                    protected: Vec::new(),
                                    reason: format!(
                                        "request_time_selection_refused:budget_insufficient:declared response budget {} tokens admits no evidence; {exhausted} candidate(s) exceed the packet allowance {packet_allowance}",
                                        fit.response_tokens
                                    ),
                                },
                            });
                        }
                    } else {
                        let whole_response = if fit.explicit_pull && ceiling.is_none() {
                            // An explicit pull's consumer receives one complete response
                            // in one delivery mode, so the representation is chosen by
                            // the exact bytes of that response (context, detail and any
                            // receipt), counted exactly with its own accounting settled.
                            let spec = fit.projection.clone().unwrap_or_default();
                            let base = &*fields;
                            let project = |content: &Value, accounting: &Value| {
                                crate::hook::project_pull_view(base, content, accounting, &spec)
                            };
                            let live =
                                || check_pull_live(federation_deadline, &inherited_cancellation);
                            match crate::pull::packet_selection::select_packet_for_whole_response(
                                &packet,
                                fit.response_tokens,
                                spec.delivery.as_str(),
                                &crate::pull::prep::PushPolicy::Control,
                                recovery.as_ref(),
                                &live,
                                &project,
                            ) {
                                Ok(whole) => Some(whole),
                                Err(WholeResponseError::Capacity(reason)) => {
                                    let protected = packet_protected_blocks(&packet);
                                    if first_capacity.is_none() {
                                        first_capacity = Some((protected.clone(), reason.clone()));
                                    }
                                    return Err(NativePlanError::Capacity { protected, reason });
                                }
                                Err(WholeResponseError::Fatal(reason)) => {
                                    return Err(NativePlanError::Fatal(reason));
                                }
                                Err(WholeResponseError::Plan(error)) => {
                                    return Err(NativePlanError::from_selection(error, &packet));
                                }
                            }
                        } else {
                            None
                        };
                        let (selection, whole_delivered) = match whole_response {
                            Some(whole) => (whole.selection, Some(whole.delivered_response)),
                            None => (
                                select_for_ceiling_or_budget(
                                    &packet,
                                    ceiling.as_ref(),
                                    fit.response_tokens,
                                    recovery.as_ref(),
                                )?,
                                None,
                            ),
                        };
                        fields.insert(
                            "packet".to_owned(),
                            selection.selected_representation.content.clone(),
                        );
                        crate::pull::packet_selection::finalize_receipts(
                            fields,
                            &selection.selected_representation.content,
                        );
                        fields.insert(
                            "packetReduction".to_owned(),
                            serde_json::to_value(&selection).map_err(|error| {
                                format!("serialize packet reduction selection: {error}")
                            })?,
                        );
                        if let Some(delivered) = whole_delivered {
                            // The settled accounting of the complete response that was
                            // counted and fits: nothing in the response changes after this
                            // except values the explicit pull's projection never reads.
                            fields.insert("deliveredResponse".to_owned(), delivered);
                        } else {
                            let rendered =
                                rendered_block_text(&selection.selected_representation.content)
                                    .unwrap_or_default();
                            let rendered_tokens =
                                rightkit_memory_core::ContextTokenAccounting::count_exact(&rendered)
                                    .map_err(|error| {
                                        format!("measure rendered response: {error}")
                                    })? as u64;
                            let packet_tokens = fields
                                .get("packet")
                                .and_then(|packet| {
                                    crate::pull::selection::measure_packet(
                                        packet,
                                        &rightkit_context_protocol::host_observation::EstimatorBasisV1::new(
                                            "o200k_base",
                                            "1",
                                        ),
                                    )
                                    .ok()
                                })
                                .unwrap_or(rendered_tokens);
                            // Both measurements, never one relabelled as the other:
                            // `renderedTokens` counts the consumer-visible text (the pull
                            // `context`), `packetTokens` the serialized packet JSON
                            // (metadata, accounting and omission records the consumer does
                            // not receive). Both are o200k_base/1.
                            fields.insert(
                                "deliveredResponse".to_owned(),
                                serde_json::json!({
                                    "schemaVersion": 1,
                                    "renderedTokens": rendered_tokens,
                                    "renderedBytes": rendered.len(),
                                    "packetTokens": packet_tokens,
                                    "unit": "tokens",
                                    "estimatorBasis": "o200k_base/1",
                                    "scope": "consumer_visible_context",
                                    "mapping": "renderedTokens: o200k_base/1 count of the delivered `context` (block texts joined by a blank line, recovery handles included); packetTokens: o200k_base/1 count of the serialized packet JSON",
                                    "selectedBy": "serialized_packet",
                                    "fitsBudget": rendered_tokens <= fit.response_tokens,
                                    "packetFitsBudget": packet_tokens <= fit.response_tokens,
                                }),
                            );
                            if rendered_tokens > fit.response_tokens {
                                return Err(format!(
                                    "request_time_selection_refused:budget_insufficient:declared response budget {} tokens; rendered evidence requires {rendered_tokens} tokens",
                                    fit.response_tokens
                                ).into());
                            }
                        }
                    }
                }
            }
            let resolved_mode = fit.declared.unwrap_or(if ceiling.is_some() {
                PullBudgetMode::HostFit
            } else {
                PullBudgetMode::BoundedResponse
            });
            insert_budget_mode_fields(
                fields,
                resolved_mode,
                fit.declared,
                fit.response_tokens,
                fit.provenance,
                ceiling.as_ref(),
            );
            // PUL-033: an empty packet is a versioned insufficient_confidence
            // result carrying the searched-lane accounting, never a bare
            // packet-without-selection that downstream surfaces must guess at.
            // The resident host-fit path already emits this shape; bounded
            // responses must produce the same typed insufficiency.
            let packet_empty = fields
                .get("packet")
                .and_then(|packet| packet.get("blocks"))
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty);
            if packet_empty && fields.get("status").is_none() {
                let candidate_count = ccs
                    .get("candidates")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                // Same accounting as the resident host-fit empty-packet path:
                // format the typed omission records — the wire schema drops
                // detail_id/stage, so packet.omissions would flatten every
                // distinct cause into the same generic code.
                let mut reasons = response
                    .omissions
                    .iter()
                    .map(|omission| {
                        let id = omission
                            .candidate_id
                            .clone()
                            .unwrap_or_else(|| omission.provider.as_str().to_owned());
                        let reason = omission.reason.as_str();
                        match (omission.detail_id.as_deref(), omission.stage.as_deref()) {
                            (Some(detail), Some(stage)) => {
                                format!("{id}:{reason}({detail}@{stage})")
                            }
                            (Some(detail), None) => format!("{id}:{reason}({detail})"),
                            (None, Some(stage)) => format!("{id}:{reason}(@{stage})"),
                            (None, None) => format!("{id}:{reason}"),
                        }
                    })
                    .collect::<Vec<_>>();
                let total = reasons.len();
                reasons.truncate(16);
                fields.insert(
                    "status".to_owned(),
                    Value::String("insufficient_confidence".to_owned()),
                );
                fields.insert(
                    "emptyEvidenceSummary".to_owned(),
                    serde_json::json!({
                        "candidateCount": candidate_count,
                        "omissions": reasons,
                        "elided": total.saturating_sub(16),
                    }),
                );
            }
        }
        check_pull_live(federation_deadline, &inherited_cancellation)?;
        Ok(payload)
    })
}

#[derive(Debug)]
pub(crate) enum NativePlanError {
    Capacity {
        protected: Vec<(String, String)>,
        reason: String,
    },
    Fatal(String),
}

impl NativePlanError {
    pub(crate) fn from_selection(
        error: crate::pull::selection::PacketReductionRequestError,
        packet: &rightkit_memory_core::planner::ContextPacketV1,
    ) -> Self {
        let reason = format!("request_time_selection_refused:{}:{error}", error.kind());
        match error {
            crate::pull::selection::PacketReductionRequestError::Selection(
                rightkit_context_protocol::push::PacketReductionSelectionError::NoRepresentationFits {
                    ..
                },
            ) => Self::Capacity {
                protected: packet_protected_blocks(packet),
                reason,
            },
            _ => Self::Fatal(reason),
        }
    }
}

impl From<String> for NativePlanError {
    fn from(error: String) -> Self {
        Self::Fatal(error)
    }
}

/// Selection for everything but an explicit pull: a validated request-time H8 keeps host-fit
/// semantics (the largest complete representation under the observed ceiling, which must also
/// fit the declared response budget); a hook or route keeps the conservative serialized-packet
/// measure (an upper bound of what it wraps and injects).
fn select_for_ceiling_or_budget(
    packet: &rightkit_memory_core::planner::ContextPacketV1,
    ceiling: Option<&rightkit_context_protocol::RemainingContextCeilingV1>,
    response_tokens: u64,
    recovery: Option<&crate::pull::selection::RecoveryContext<'_>>,
) -> Result<crate::pull::selection::PacketReductionSelectionV1, NativePlanError> {
    let policy = crate::pull::prep::PushPolicy::Control;
    match ceiling {
        Some(ceiling) => {
            let selection = crate::pull::selection::select_packet_for_h8_with_recovery(
                packet, ceiling, &policy, recovery,
            )
            .map_err(|error| NativePlanError::from_selection(error, packet))?;
            if selection.selected_representation.tokens > response_tokens {
                return Err(NativePlanError::Capacity {
                    protected: packet_protected_blocks(packet),
                    reason: format!(
                        "request_time_selection_refused:budget_insufficient:declared response budget {response_tokens} tokens; smallest fitting representation under host ceiling requires {} tokens",
                        selection.selected_representation.tokens
                    ),
                });
            }
            Ok(selection)
        }
        None => crate::pull::selection::select_packet_for_token_budget(
            packet,
            response_tokens,
            &policy,
            recovery,
        )
        .map_err(|error| NativePlanError::from_selection(error, packet)),
    }
}

fn packet_protected_blocks(
    packet: &rightkit_memory_core::planner::ContextPacketV1,
) -> Vec<(String, String)> {
    packet
        .blocks
        .iter()
        .filter(|block| block.protected)
        .map(|block| (block.id.clone(), block.text.clone()))
        .collect()
}

/// Fit transport framing by rerunning only the existing planner on one source
/// snapshot. The caller rebuilds admission receipts & checks its deadline.
pub(crate) fn fit_native_plan(
    max_tokens: usize,
    mut plan: impl FnMut(usize) -> Result<Value, NativePlanError>,
) -> Result<Value, String> {
    let mut packet_allowance = max_tokens;
    let mut protected: Vec<(String, String)> = Vec::new();
    let mut protected_baseline_captured = false;
    let mut last_capacity_reason = String::new();
    for _attempt in 0..16 {
        match plan(packet_allowance) {
            Ok(payload) => {
                // A refit after a Capacity refusal that admits no evidence at all is not
                // "nothing matched": the first attempt had candidates that did not fit.
                // Surface the typed refusal instead of an empty packet.
                let packet_empty = payload
                    .get("packet")
                    .and_then(|packet| packet.get("blocks"))
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty);
                if packet_empty && !last_capacity_reason.is_empty() {
                    let cause = last_capacity_reason
                        .strip_prefix("request_time_selection_refused:")
                        .unwrap_or(&last_capacity_reason);
                    return Err(format!(
                        "request_time_selection_refused:budget_insufficient:no complete representation fits the declared {max_tokens} tokens, and a smaller packet allowance ({packet_allowance}) admits no evidence; {cause}"
                    ));
                }
                return Ok(payload);
            }
            Err(NativePlanError::Capacity {
                protected: retry_protected,
                reason,
            }) => {
                // Per-block framing can exceed the budget even when text
                // reduction succeeds; replanning at a smaller allowance drops
                // lower-ranked blocks with recorded omissions. Protected evidence
                // cannot change between attempts.
                last_capacity_reason = reason;
                if !protected_baseline_captured {
                    protected = retry_protected;
                    protected_baseline_captured = true;
                } else if protected != retry_protected {
                    return Err("request_time_selection_refused:budget_insufficient:protected evidence changed during transport refit".into());
                }
                if packet_allowance <= 1 {
                    break;
                }
                packet_allowance = (packet_allowance / 2).max(1);
            }
            Err(NativePlanError::Fatal(error)) => return Err(error),
        }
    }
    if !last_capacity_reason.is_empty() {
        return Err(last_capacity_reason);
    }
    Err("request_time_selection_refused:budget_insufficient:no complete MCP response fits declared capacity".into())
}

fn check_pull_live(
    deadline: rightkit_context_federation::deadline::Deadline,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(), String> {
    if cancellation.is_cancelled() {
        return Err("federation request cancelled".to_owned());
    }
    if deadline.is_exhausted_at(Instant::now()) {
        return Err("federation deadline exhausted".to_owned());
    }
    Ok(())
}

/// Record one named gateway stage's duration into the response timing map.
fn mark_stage_timing(timings: &mut serde_json::Map<String, Value>, name: &str, from: Instant) {
    timings.insert(
        name.to_owned(),
        Value::from(from.elapsed().as_millis() as u64),
    );
}

/// Attach gateway stage timings to an error refusal body so deadline misses
/// that never reach a successful envelope remain attributable.
fn refusal_with_stage_timings(
    mut refusal: (u16, String),
    timings: &serde_json::Map<String, Value>,
) -> (u16, String) {
    if let Ok(mut value) = serde_json::from_str::<Value>(&refusal.1) {
        if let Some(fields) = value.as_object_mut() {
            fields.insert(
                "gatewayStageTimingsMs".to_owned(),
                Value::Object(timings.clone()),
            );
            if let Ok(body) = serde_json::to_string(&value) {
                refusal.1 = body;
            }
        }
    }
    refusal
}

/// Resident `/federate` entrypoint. Production routes call this native path;
/// legacy worker framing remains isolated for shadow qualification only.
pub fn native_route_response(body: &str) -> (u16, String) {
    native_route_response_with_store(body, None)
}

pub fn native_route_response_with_store(
    body: &str,
    resident: Option<&crate::MemoryStore>,
) -> (u16, String) {
    native_route_response_with_deadline(body, resident, None)
}

pub(crate) fn native_route_response_with_deadline(
    body: &str,
    resident: Option<&crate::MemoryStore>,
    inherited_deadline: Option<rightkit_context_federation::deadline::Deadline>,
) -> (u16, String) {
    let control = crate::serve::inherited_push_control();
    let deadline = match (inherited_deadline, control.as_ref()) {
        (Some(deadline), Some(control)) => {
            Some(rightkit_context_federation::deadline::Deadline::at(
                deadline.instant().min(control.deadline.instant()),
            ))
        }
        (deadline, control) => deadline.or_else(|| control.map(|control| control.deadline)),
    };
    let runtime = tokio::runtime::Handle::try_current().ok();
    native_route_response_with_deadline_and_control(
        body,
        resident,
        deadline,
        Some(
            control
                .map(|control| control.cancellation)
                .unwrap_or_else(|| crate::service::lifecycle_control().cancellation_token()),
        ),
        runtime.as_ref(),
        None,
    )
}

fn native_route_response_with_deadline_and_control(
    body: &str,
    resident: Option<&crate::MemoryStore>,
    inherited_deadline: Option<rightkit_context_federation::deadline::Deadline>,
    inherited_cancellation: Option<tokio_util::sync::CancellationToken>,
    shared_runtime: Option<&tokio::runtime::Handle>,
    authorized_cortex_scope: Option<Value>,
) -> (u16, String) {
    let inherited_cancellation = inherited_cancellation
        .unwrap_or_else(|| crate::service::lifecycle_control().cancellation_token());
    let started = Instant::now();
    let value: Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(_) => return (400, "{\"error\":\"invalid JSON body\"}".to_owned()),
    };
    let Some(task) = value.get("task").and_then(Value::as_str) else {
        return (400, "{\"error\":\"task required\"}".to_owned());
    };
    let Some(repo_text) = value.get("repo").and_then(Value::as_str) else {
        return (400, "{\"error\":\"repo required\"}".to_owned());
    };
    let root = match PathBuf::from(repo_text).canonicalize() {
        Ok(path) if path.is_dir() => path,
        _ => {
            return (
                400,
                "{\"error\":\"repo must be an existing directory\"}".to_owned(),
            );
        }
    };
    let max_tokens = value
        .get("maxTokens")
        .and_then(Value::as_u64)
        .map_or(4096, |n| n.clamp(1, 1_000_000) as usize);
    let deadline_ms = value
        .get("maxWaitMs")
        .and_then(Value::as_u64)
        .unwrap_or(2_000)
        .clamp(1, 60_000);
    let local_deadline = started + std::time::Duration::from_millis(deadline_ms);
    let deadline = rightkit_context_federation::deadline::Deadline::at(
        inherited_deadline
            .map(|inherited| inherited.instant().min(local_deadline))
            .unwrap_or(local_deadline),
    );
    // Same per-stage timings as run_federate_value: the host-observed (H8)
    // route is the production path the concurrency qualification exercises,
    // so its deadline misses must be attributable too. Timings also ride the
    // error refusals below, not only successful envelopes.
    let mut stage_timings = serde_json::Map::new();
    mark_stage_timing(&mut stage_timings, "request_parse", started);
    let client = value
        .get("client")
        .and_then(Value::as_str)
        .unwrap_or("claude")
        .to_owned();
    let session = federation_session_id(
        value
            .get("sessionId")
            .or_else(|| value.get("session"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    );
    let task_id = value
        .get("taskId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(task)
        .to_owned();
    let anchors = match value.get("anchors") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect(),
        Some(Value::String(text)) => text
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    };
    let scope_grant_id = value
        .get("scopeGrantId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let sufficiency_contract = planner_authored_sufficiency_contract(&value);
    let temporal_query = match value.get("cortexTemporalQuery") {
        Some(query) => {
            match serde_json::from_value::<rightkit_memory_store::TemporalFactQuery>(query.clone())
            {
                Ok(query) => Some(query),
                Err(error) => {
                    return (
                        400,
                        serde_json::json!({
                            "error": format!("invalid cortexTemporalQuery: {error}")
                        })
                        .to_string(),
                    );
                }
            }
        }
        None => None,
    };
    // Pull budget contract (PUL-050): two explicit modes. `budgetMode` is
    // canonical; `budgetPolicy` is the legacy spelling. Compatible legacy
    // requests map by supplied observation: a request-time H8 keeps host-fit
    // semantics; its absence maps to bounded-response under the declared
    // caller/configured response budget. A declaration always wins and is
    // never silently downgraded.
    let declared_mode = match declared_budget_mode(&value) {
        Ok(mode) => mode,
        Err(refusal) => return refusal_with_stage_timings(refusal, &stage_timings),
    };
    let h8_supplied = value
        .get("remainingContextCeiling")
        .is_some_and(|ceiling| !ceiling.is_null());
    let mode = declared_mode.unwrap_or(if h8_supplied {
        PullBudgetMode::HostFit
    } else {
        PullBudgetMode::BoundedResponse
    });
    // Supplied host evidence is strictly validated under both modes — a
    // missing (host-fit), malformed, stale, inexact or identity-mismatched
    // H8 is a typed refusal, never silently discarded to obtain success.
    // Under bounded-response a validated H8 remains a real ceiling and
    // tightens the emitted packet; under host-fit it is the required bound.
    let ceiling = match (mode, h8_supplied) {
        (PullBudgetMode::BoundedResponse, false) => None,
        _ => match crate::pull::selection::parse_request_time_h8(&value, &session, &task_id) {
            Ok(ceiling) => Some(ceiling),
            Err(error) => {
                return refusal_with_stage_timings(
                    request_time_refusal(crate::pull::selection::PacketReductionRequestError::H8(
                        error,
                    )),
                    &stage_timings,
                );
            }
        },
    };
    let (response_budget, response_budget_provenance) =
        match declared_response_budget(&value, max_tokens) {
            Ok(budget) => budget,
            Err(refusal) => return refusal_with_stage_timings(refusal, &stage_timings),
        };
    if mode == PullBudgetMode::BoundedResponse {
        // Bounded-response runs the shared ambient envelope path: the planner
        // packet is fitted under the declared budget (and under a supplied,
        // validated H8 when present) inside run_federate_value. Missing host
        // capacity is reported as unknown — never a failure here.
        let result = run_federate_value(
            task.to_owned(),
            root,
            max_tokens,
            value
                .get("packetCharBudget")
                .and_then(Value::as_u64)
                .map(|n| n as usize),
            value
                .get("packetCharBudgetModel")
                .and_then(Value::as_str)
                .map(str::to_owned),
            client,
            Some(session),
            anchors,
            scope_grant_id,
            vec![2],
            deadline
                .instant()
                .saturating_duration_since(Instant::now())
                .as_millis() as u64,
            "configured_cap",
            ceiling,
            resident,
            shared_runtime.cloned(),
            Some(inherited_cancellation.clone()),
            Some(deadline),
            BudgetFit {
                declared: declared_mode,
                response_tokens: response_budget,
                provenance: response_budget_provenance,
                explicit_pull: false,
                projection: None,
            },
            authorized_cortex_scope.clone(),
            value.get("pushResolverToken").and_then(Value::as_str),
        );
        return match result {
            Ok(payload) => (200, payload.to_string()),
            Err(error) => {
                let refusal =
                    if let Some(rest) = error.strip_prefix("request_time_selection_refused:") {
                        let mut parts = rest.splitn(2, ':');
                        (
                            400,
                            serde_json::json!({
                                "error": "request_time_selection_refused",
                                "kind": parts.next().unwrap_or("packet_reduction_refused"),
                                "reason": parts.next().unwrap_or(""),
                            })
                            .to_string(),
                        )
                    } else {
                        (
                            503,
                            serde_json::json!({"error":error,"reason":"membrane_retrieval_failed"})
                                .to_string(),
                        )
                    };
                refusal_with_stage_timings(refusal, &stage_timings)
            }
        };
    }
    // Host-fit: the validated request-time H8 above is the required bound.
    let Some(ceiling) = ceiling else {
        return refusal_with_stage_timings(
            request_time_refusal(crate::pull::selection::PacketReductionRequestError::H8(
                crate::pull::selection::RequestTimeH8Error::Missing,
            )),
            &stage_timings,
        );
    };
    let loaded_context = current_loaded_context(&value, &session);
    let result = (|| -> Result<Value, NativeRouteError> {
        check_pull_live(deadline, &inherited_cancellation)?;
        let stage_started = Instant::now();
        let release_generation = RuntimeReleaseSource::generation()?;
        let mut request = native_request_with_h8(
            task,
            &root,
            max_tokens,
            deadline_ms,
            release_generation,
            &client,
            &session,
            anchors,
            scope_grant_id.clone(),
            sufficiency_contract,
            &ceiling,
        );
        if let Some(descriptor) = authorized_cortex_scope.clone() {
            request
                .extensions
                .insert("cortexScopeDescriptor".to_owned(), descriptor);
        }
        request
            .extensions
            .insert("taskId".to_owned(), Value::String(task_id.clone()));
        if let Some(request_id) = value
            .get("requestId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            request.request_id = request_id.to_owned();
        }
        if let Some(generation) = value
            .get("generation")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            request.extensions.insert(
                "hostGeneration".to_owned(),
                Value::String(generation.to_owned()),
            );
        }
        if let Some(capabilities) = value.get("consumerCapabilities").cloned() {
            request
                .extensions
                .insert("consumerCapabilities".to_owned(), capabilities);
        }
        mark_stage_timing(&mut stage_timings, "request_setup", stage_started);
        if let Some(requirement_facts) = value.get("requirementFacts").cloned() {
            request
                .extensions
                .insert("requirementFacts".to_owned(), requirement_facts);
        }
        if let Some(temporal_query) = temporal_query.clone() {
            request.extensions.insert(
                "cortexTemporalQuery".to_owned(),
                serde_json::to_value(temporal_query)
                    .map_err(|error| format!("serialize cortexTemporalQuery: {error}"))?,
            );
        }
        let stage_started = Instant::now();
        let admitted_grant = admitted_publication_grant_until(&request, Some(deadline))?;
        mark_stage_timing(&mut stage_timings, "grant_admission", stage_started);
        let stage_started = Instant::now();
        let bindings = match resident {
            Some(store) => federation_sources::NativeSourceBindings::with_store_and_deadline(
                &root,
                scope_grant_id.as_deref(),
                store.clone(),
                Some(deadline),
            ),
            None => federation_sources::NativeSourceBindings::with_store_and_deadline(
                &root,
                scope_grant_id.as_deref(),
                crate::service::open_installed_store()?,
                Some(deadline),
            ),
        }?;
        mark_stage_timing(&mut stage_timings, "bindings", stage_started);
        let stage_started = Instant::now();
        let native = native_federation::NativeFederation::new(bindings)?;
        // Dispatch is already off scheduler workers; reuse its engine Handle.
        let response = native_federation::run_on_runtime(
            shared_runtime,
            native.federate_until(&request, inherited_cancellation.clone(), deadline),
        )?;
        mark_stage_timing(&mut stage_timings, "native_federation", stage_started);
        let stage_started = Instant::now();
        check_pull_live(deadline, &inherited_cancellation)?;
        let native_metrics = native.metrics_snapshot();
        let freshness = native
            .freshness_snapshot()
            .ok_or_else(|| "native freshness verdict unavailable".to_owned())?;
        let ccs = native_response_to_ccs(&response, &request, &freshness);
        let provisional_coverage = ccs.get("requirementEvidenceMap").cloned();
        let native_receipts = collect_native_receipts(&response);
        let mut payload = envelope_from_ccs(
            &serde_json::to_string(&ccs).map_err(|error| error.to_string())?,
            EnvelopeInput {
                max_tokens,
                packet_char_budget_override: value
                    .get("packetCharBudget")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize),
                packet_char_budget_model: value
                    .get("packetCharBudgetModel")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                accepted_receipt_versions: vec![2],
                scope_grant_present: scope_grant_id.is_some(),
                consumer_resolvers: negotiated_consumer_resolvers(&value),
                scope_grant_fence: post_fusion_publication_fence_until(
                    &admitted_grant,
                    Some(deadline),
                )?,
                gateway_process_ms: started.elapsed().as_secs_f64() * 1000.0,
            },
        )?;
        mark_stage_timing(&mut stage_timings, "envelope", stage_started);
        let mut packet = payload
            .get("packet")
            .cloned()
            .ok_or_else(|| "federation envelope omitted packet".to_owned())
            .and_then(|packet| {
                serde_json::from_value::<rightkit_memory_core::planner::ContextPacketV1>(packet)
                    .map_err(|error| format!("federation packet is invalid: {error}"))
            })?;
        // An empty packet is refused two frames later as "packet blocks is
        // empty", which says nothing about why federation produced nothing —
        // a provider that answered with candidates and a provider that was
        // skipped are indistinguishable there. Name the providers' own
        // accounting instead: candidate count, and the omission reasons they
        // recorded.
        if packet.blocks.is_empty() {
            let candidate_count = ccs
                .get("candidates")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            // Read the typed response rather than the serialized set: the
            // wire schema for an omission is closed over id/layer/reason, so
            // the provider's own `detail_id` -- the field that says which
            // failure this was -- never reaches the JSON. Every distinct
            // cause therefore printed as the same generic code. The typed
            // record still has it.
            let mut reasons = response
                .omissions
                .iter()
                .map(|omission| {
                    let id = omission
                        .candidate_id
                        .clone()
                        .unwrap_or_else(|| omission.provider.as_str().to_owned());
                    let reason = omission.reason.as_str();
                    match (omission.detail_id.as_deref(), omission.stage.as_deref()) {
                        (Some(detail), Some(stage)) => format!("{id}:{reason}({detail}@{stage})"),
                        (Some(detail), None) => format!("{id}:{reason}({detail})"),
                        (None, Some(stage)) => format!("{id}:{reason}(@{stage})"),
                        (None, None) => format!("{id}:{reason}"),
                    }
                })
                .collect::<Vec<_>>();
            // A lane can now record both its admission omission and its own
            // reason, so a fixed cap of eight silently hid whole providers.
            // Say how many were elided rather than dropping them unannounced.
            let total = reasons.len();
            reasons.truncate(16);
            let packet_omissions = payload
                .get("packet")
                .and_then(|packet| packet.get("omissions"))
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            let fields = payload
                .as_object_mut()
                .ok_or_else(|| "federation envelope is not an object".to_owned())?;
            fields.insert(
                "status".to_owned(),
                Value::String("insufficient_confidence".to_owned()),
            );
            fields.insert("transport".to_owned(), Value::String("native".to_owned()));
            fields.insert(
                "federationMetrics".to_owned(),
                serde_json::json!(native_metrics),
            );
            fields.insert(
                "gatewayStageTimingsMs".to_owned(),
                Value::Object(stage_timings.clone()),
            );
            fields.insert("emptyEvidenceSummary".to_owned(), serde_json::json!({
                "candidateCount": candidate_count, "omissions": reasons, "elided": total.saturating_sub(16),
            }));
            fields.insert(
                "budgetReduction".to_owned(),
                serde_json::json!({
                    "misleadingFragmentRetained": false,
                    "omissionReasons": packet_omissions,
                    "droppedCandidateCount": candidate_count,
                }),
            );
            merge_native_receipts(fields, native_receipts);
            let selected_packet = fields.get("packet").cloned().unwrap_or(Value::Null);
            crate::pull::packet_selection::finalize_receipts(fields, &selected_packet);
            let final_map = fields.get("requirementEvidenceMap").cloned();
            let final_packet = fields.get("packet").cloned();
            merge_bm10_accounting(
                fields,
                provisional_coverage.as_ref(),
                final_map.as_ref(),
                final_packet.as_ref(),
            );
            insert_budget_mode_fields(
                fields,
                PullBudgetMode::HostFit,
                declared_mode,
                response_budget,
                response_budget_provenance,
                Some(&ceiling),
            );
            check_pull_live(deadline, &inherited_cancellation)?;
            return Ok(payload);
        }
        let placement_receipt = crate::pull::placement::place(&mut packet);
        payload["packet"] = serde_json::to_value(&packet)
            .map_err(|error| format!("serialize placed packet: {error}"))?;
        payload["placementReceipt"] = serde_json::to_value(&placement_receipt)
            .map_err(|error| format!("serialize placement receipt: {error}"))?;
        let repository_id = rightkit_context_federation::root::canonical_repository_id(&root);
        let explicit_refresh = value
            .get("refresh")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let suppression_catalog = loaded_context
            .as_ref()
            .map(|_| publication_catalog())
            .transpose()?;
        let suppression_receipts = match (loaded_context.as_ref(), suppression_catalog.as_ref()) {
            (Some(loaded), Some(catalog)) => crate::pull::delivery_state::suppress_packet(
                &mut packet,
                catalog,
                &repository_id,
                &session,
                loaded,
                explicit_refresh,
            ),
            _ => Vec::new(),
        };
        if !suppression_receipts.is_empty() {
            payload["packet"] = serde_json::to_value(&packet)
                .map_err(|error| format!("serialize suppressed packet: {error}"))?;
            payload["suppressionReceipts"] = serde_json::to_value(&suppression_receipts)
                .map_err(|error| format!("serialize suppression receipts: {error}"))?;
        }
        if packet.blocks.is_empty() && !suppression_receipts.is_empty() {
            let packet_omissions = payload
                .get("packet")
                .and_then(|packet| packet.get("omissions"))
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            let packet_budget = payload
                .get("packet")
                .and_then(|packet| packet.get("budget"))
                .cloned()
                .unwrap_or(Value::Null);
            let fields = payload
                .as_object_mut()
                .ok_or_else(|| "federation envelope is not an object".to_owned())?;
            fields.insert(
                "status".to_owned(),
                Value::String("unchanged_context".to_owned()),
            );
            fields.insert("transport".to_owned(), Value::String("native".to_owned()));
            fields.insert(
                "federationMetrics".to_owned(),
                serde_json::json!(native_metrics),
            );
            fields.insert(
                "gatewayStageTimingsMs".to_owned(),
                Value::Object(stage_timings.clone()),
            );
            fields.insert(
                "finalAdmission".to_owned(),
                serde_json::json!({
                    "status": "insufficient",
                    "candidateCount": 0,
                    "omissions": packet_omissions.clone(),
                    "budget": packet_budget,
                }),
            );
            fields.insert(
                "budgetReduction".to_owned(),
                serde_json::json!({
                    "misleadingFragmentRetained": false,
                    "omissionReasons": packet_omissions,
                    "droppedCandidateCount": 0,
                }),
            );
            merge_native_receipts(fields, native_receipts);
            let selected_packet = fields.get("packet").cloned().unwrap_or(Value::Null);
            crate::pull::packet_selection::finalize_receipts(fields, &selected_packet);
            let final_map = fields.get("requirementEvidenceMap").cloned();
            let final_packet = fields.get("packet").cloned();
            merge_bm10_accounting(
                fields,
                provisional_coverage.as_ref(),
                final_map.as_ref(),
                final_packet.as_ref(),
            );
            insert_budget_mode_fields(
                fields,
                PullBudgetMode::HostFit,
                declared_mode,
                response_budget,
                response_budget_provenance,
                Some(&ceiling),
            );
            check_pull_live(deadline, &inherited_cancellation)?;
            return Ok(payload);
        }
        let push_policy = push_policy_for_request(&value, task);
        let recovery_store = crate::pull::recovery::RecoveryStore::configured();
        let recovery_scope = crate::pull::recovery::RecoveryScope::new(&root, &session).ok();
        let recovery = recovery_scope.as_ref().and_then(|scope| {
            value
                .get("pushResolverToken")
                .and_then(Value::as_str)
                .map(|token| crate::pull::selection::RecoveryContext {
                    store: &recovery_store,
                    scope,
                    resolver_token: token,
                })
        });
        check_pull_live(deadline, &inherited_cancellation)?;
        let stage_started = Instant::now();
        let selection = crate::pull::selection::select_packet_for_h8_with_recovery(
            &packet,
            &ceiling,
            &push_policy,
            recovery.as_ref(),
        )
        .map_err(NativeRouteError::RequestTime)?;
        mark_stage_timing(&mut stage_timings, "reduction_selection", stage_started);
        let selected_content = selection.selected_representation.content.clone();
        let previous_packet = loaded_context
            .as_ref()
            .zip(suppression_catalog.as_ref())
            .and_then(|(loaded, catalog)| {
                crate::pull::delivery_state::previous_packet(
                    catalog,
                    &repository_id,
                    &session,
                    loaded,
                )
            });
        let cache_prefix_diagnostic =
            crate::cache_prefix::diagnose_cache_prefix(&selected_content, previous_packet.as_ref());
        let fields = payload
            .as_object_mut()
            .ok_or_else(|| "federation envelope is not an object".to_owned())?;
        fields.insert("packet".to_owned(), selected_content.clone());
        fields.insert("transport".to_owned(), Value::String("native".to_owned()));
        fields.insert(
            "federationMetrics".to_owned(),
            serde_json::json!(native_metrics),
        );
        fields.insert(
            "gatewayStageTimingsMs".to_owned(),
            Value::Object(stage_timings.clone()),
        );
        merge_native_receipts(fields, native_receipts);
        crate::pull::packet_selection::finalize_receipts(fields, &selected_content);
        fields.insert(
            "packetReduction".to_owned(),
            serde_json::to_value(&selection)
                .map_err(|error| format!("serialize packet reduction selection: {error}"))?,
        );
        insert_budget_mode_fields(
            fields,
            PullBudgetMode::HostFit,
            declared_mode,
            response_budget,
            response_budget_provenance,
            Some(&ceiling),
        );
        attach_compaction_federation_decision(
            fields,
            &value,
            &session,
            &repository_id,
            &selection,
        )?;
        fields.insert(
            "cachePrefixDiagnostic".to_owned(),
            serde_json::to_value(cache_prefix_diagnostic)
                .map_err(|error| format!("serialize cache prefix diagnostic: {error}"))?,
        );
        let packet_blocks = selected_content
            .get("blocks")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let packet_omissions = selected_content
            .get("omissions")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        fields.insert(
            "finalAdmission".to_owned(),
            serde_json::json!({
                "status": if packet_blocks == 0 { "insufficient" } else if packet_omissions.as_array().is_some_and(Vec::is_empty) { "admitted" } else { "degraded" },
                "candidateCount": packet_blocks,
                "omissions": packet_omissions.clone(),
                "budget": selected_content.get("budget").cloned().unwrap_or(Value::Null),
                "budgetReduction": value.get("packetCharBudget").and_then(Value::as_u64).map(|requested| requested as usize).map(|requested| serde_json::json!({
                    "requested": requested,
                    "applied": true,
                    "droppedCandidateCount": selected_content.get("omissions").and_then(Value::as_array).map_or(0, Vec::len),
                    "reason": "packet_budget",
                })).unwrap_or(Value::Null),
            }),
        );
        fields.insert(
            "budgetReduction".to_owned(),
            serde_json::json!({
                "misleadingFragmentRetained": false,
                "omissionReasons": packet_omissions,
                "droppedCandidateCount": selected_content.get("omissions").and_then(Value::as_array).map_or(0, Vec::len),
            }),
        );
        check_pull_live(deadline, &inherited_cancellation)?;
        let final_fence = fence_packet_emission(post_fusion_publication_fence_until(
            &admitted_grant,
            Some(deadline),
        )?)
        .map_err(NativeRouteError::PolicyChanged)?;
        let provisional_coverage = fields.get("requirementEvidenceMap").cloned();
        fields.insert(
            "requirementEvidenceMap".to_owned(),
            final_requirement_evidence_map(
                provisional_coverage.clone(),
                &selected_content,
                final_fence.as_ref(),
            ),
        );
        let final_coverage = fields.get("requirementEvidenceMap").cloned();
        let selected_packet = fields.get("packet").cloned();
        merge_bm10_accounting(
            fields,
            provisional_coverage.as_ref(),
            final_coverage.as_ref(),
            selected_packet.as_ref(),
        );
        if let Some(context_epoch) = host_context_epoch(&value, loaded_context.as_ref()) {
            let representation_digest = sha256_digest(&selected_content)?;
            let packet_digest = sha256_digest(&selected_content)?;
            // The epoch may come from a bare `contextEpoch` on first delivery,
            // before any complete H9 exists — `suppression_catalog` is None in
            // that case. Open the catalog at point of use: the publication
            // path owns the binding, not the suppression path.
            let catalog = publication_catalog()?;
            let handle = RepresentationHandleV1 {
                schema_version: rightkit_context_protocol::HOST_OBSERVATION_SCHEMA_VERSION,
                handle: crate::store::opaque_correlation_token(
                    &format!(
                        "{}:{}:{}",
                        request.trace_id, context_epoch, representation_digest
                    ),
                    "representation-handle",
                ),
                class: match selection.selected_representation.id.as_str() {
                    "native" => RepresentationClassV1::Native,
                    "full" | "rendered_full" => RepresentationClassV1::RenderedFull,
                    "excerpt" | "floor" | "rendered_excerpt" => {
                        RepresentationClassV1::RenderedExcerpt
                    }
                    "resolver" | "resolver_backed" => RepresentationClassV1::ResolverBacked,
                    "metadata" | "metadata_only" => RepresentationClassV1::MetadataOnly,
                    _ => {
                        return Err(format!(
                            "unknown Membrane representation class: {}",
                            selection.selected_representation.id
                        )
                        .into());
                    }
                },
                installation_id: catalog.startup_report().catalog_installation_id.clone(),
                context_epoch,
                source_ref: format!("packet://{}", request.trace_id),
                source_digest: representation_digest.clone(),
            };
            handle.validate().map_err(|error| error.to_string())?;
            let publication_id = crate::store::opaque_correlation_token(
                &format!(
                    "{}:{}:{}:{}:{}",
                    request.request_id,
                    request.trace_id,
                    session,
                    representation_digest,
                    packet_digest,
                ),
                "pull-publication",
            );
            let publication = crate::catalog::PendingPullPublicationV1 {
                repository_id: repository_id.clone(),
                request_id: request.request_id.clone(),
                trace_id: request.trace_id.clone(),
                task_id: task_id.clone(),
                session_id: session.clone(),
                context_epoch,
                publication_id: publication_id.clone(),
                representation_identity: serde_json::to_string(&handle)
                    .map_err(|error| format!("serialize representation handle: {error}"))?,
                source_ref: format!("packet://{}", request.trace_id),
                representation_digest,
                packet_digest,
            };
            crate::catalog::record_pending_pull_publication_with_control(
                &catalog,
                &publication,
                &crate::serve::PushRequestControl {
                    deadline,
                    cancellation: inherited_cancellation.clone(),
                },
            )
            .map_err(|error| format!("persist final pending Pull publication: {error}"))?;
            fields.insert(
                "pullPublication".to_owned(),
                serde_json::json!({"schemaVersion": 1, "publicationId": publication_id}),
            );
            // Expose the exact Membrane-issued handle so CodeRight can retain
            // it verbatim after host serialization; clients never mint it.
            fields.insert(
                "representationHandle".to_owned(),
                serde_json::to_value(&handle)
                    .map_err(|error| format!("serialize representation handle: {error}"))?,
            );
            check_pull_live(deadline, &inherited_cancellation)?;
            crate::pull::delivery_state::record_selected_packet(&selected_content, publication);
        }
        check_pull_live(deadline, &inherited_cancellation)?;
        Ok(payload)
    })();
    match result {
        Ok(payload) => serde_json::to_string(&payload)
            .map(|body| (200, body))
            .unwrap_or_else(|_| {
                (
                    500,
                    "{\"error\":\"federation envelope serialization failed\"}".to_owned(),
                )
            }),
        Err(NativeRouteError::RequestTime(error)) => {
            refusal_with_stage_timings(request_time_refusal(error), &stage_timings)
        }
        Err(NativeRouteError::PolicyChanged(reason)) => refusal_with_stage_timings(
            (
                409,
                serde_json::json!({
                    "error": "policy_changed",
                    "kind": "publication_fence",
                    "reason": reason,
                })
                .to_string(),
            ),
            &stage_timings,
        ),
        Err(NativeRouteError::Internal(error)) => {
            if error.to_ascii_lowercase().contains("deadline") {
                refusal_with_stage_timings(
                    federation_timeout_refusal(error, started.elapsed().as_millis() as u64),
                    &stage_timings,
                )
            } else {
                refusal_with_stage_timings(
                    (502, serde_json::json!({"error": error}).to_string()),
                    &stage_timings,
                )
            }
        }
    }
}

/// A grant snapshot captured before provider fan-out. The policy epoch is a
/// deterministic fingerprint of the grant policy fields because the runtime
/// catalog's grant owner exposes no separate mutable epoch column. It is only
/// compared with the fresh owner read; it is never used as authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationGrantObservation {
    pub grant_id: String,
    pub policy_epoch: String,
    pub revoked: bool,
}

fn publication_grant_observation(
    grant: &crate::catalog::ScopeGrant,
) -> PublicationGrantObservation {
    let policy_epoch = serde_json::json!({
        "issuer": &grant.issuer,
        "client": &grant.client,
        "repositoryIds": &grant.repository_ids,
        "permittedEdgeTypes": &grant.permitted_edge_types,
        "taskId": &grant.task_id,
        "sessionId": &grant.session_id,
        "nonce": &grant.nonce,
        "manifestDigest": &grant.manifest_digest,
    })
    .to_string();
    PublicationGrantObservation {
        grant_id: grant.id.clone(),
        policy_epoch,
        revoked: !grant.permits(),
    }
}

/// Compare the admission snapshot with the grant owner's fresh post-fusion
/// observation. This is the producer of the typed publication verdict; the
/// caller cannot supply or override it.
pub fn publication_fence_for_observations(
    admitted: Option<&PublicationGrantObservation>,
    current: Option<&PublicationGrantObservation>,
) -> Result<Option<PublicationFenceV1>, String> {
    let Some(admitted) = admitted else {
        return Ok(None);
    };
    let Some(current) = current else {
        return Ok(Some(PublicationFenceV1::policy_changed(
            PublicationFenceChangeV1::GrantIdentity,
        )));
    };
    if current.grant_id != admitted.grant_id {
        return Ok(Some(PublicationFenceV1::policy_changed(
            PublicationFenceChangeV1::GrantIdentity,
        )));
    }
    if current.policy_epoch != admitted.policy_epoch {
        return Ok(Some(PublicationFenceV1::policy_changed(
            PublicationFenceChangeV1::PolicyEpoch,
        )));
    }
    if current.revoked != admitted.revoked || current.revoked {
        return Ok(Some(PublicationFenceV1::policy_changed(
            PublicationFenceChangeV1::Revocation,
        )));
    }
    Ok(Some(PublicationFenceV1::held()))
}

fn publication_catalog() -> Result<crate::catalog::ContextCatalog, String> {
    let path = crate::catalog::default_catalog_path()
        .map_err(|error| format!("resolve context catalog for publication fence: {error}"))?;
    crate::catalog::ContextCatalog::open(path)
        .map_err(|error| format!("open context catalog for publication fence: {error}"))
}

fn publication_grant_lookup(
    id: &str,
    deadline: Option<rightkit_context_federation::deadline::Deadline>,
) -> Result<Option<crate::catalog::ScopeGrant>, String> {
    if let Some(deadline) = deadline {
        let path = crate::catalog::default_catalog_path().map_err(|error| error.to_string())?;
        crate::catalog::lookup_grant_until(&path, id, deadline)
    } else {
        crate::catalog::lookup_grant(&publication_catalog()?, id).map_err(|error| error.to_string())
    }
}

fn admitted_publication_grant_until(
    request: &rightkit_context_protocol::FederationRequestV1,
    deadline: Option<rightkit_context_federation::deadline::Deadline>,
) -> Result<Option<PublicationGrantObservation>, String> {
    let Some(grant_id) = request.scope_grant_id.as_deref() else {
        return Ok(None);
    };
    let grant = publication_grant_lookup(grant_id, deadline)
        .map_err(|error| format!("publication fence grant lookup failed: {error}"))?
        .ok_or_else(|| "publication fence grant missing: scope_grant_missing".to_owned())?;
    if !grant.permits() {
        return Err(format!(
            "publication fence grant admission refused: scope_grant_inactive:{}",
            grant.status.as_str()
        ));
    }
    Ok(Some(publication_grant_observation(&grant)))
}

fn post_fusion_publication_fence_until(
    admitted: &Option<PublicationGrantObservation>,
    deadline: Option<rightkit_context_federation::deadline::Deadline>,
) -> Result<Option<PublicationFenceV1>, String> {
    let Some(admitted) = admitted.as_ref() else {
        return publication_fence_for_observations(None, None);
    };
    let current = publication_grant_lookup(&admitted.grant_id, deadline)
        .map_err(|error| format!("post-fusion publication grant lookup failed: {error}"))?
        .map(|grant| publication_grant_observation(&grant));
    publication_fence_for_observations(Some(admitted), current.as_ref())
}

/// Author a corrective-retrieval contract only from an explicit structured
/// planner field. Task text is deliberately never inspected: absent planner
/// requirements preserve the existing `not_evaluated` behavior. An already
/// authored `sufficiencyContract` remains verbatim on the transport.
fn planner_authored_sufficiency_contract(body: &Value) -> Option<Value> {
    if let Some(contract) = body.get("sufficiencyContract") {
        return Some(contract.clone());
    }
    let requirements = body.get("plannerRequirements")?.clone();
    Some(serde_json::json!({
        "schemaVersion": 1,
        "policy": "membrane-sufficiency-v1",
        "requirements": requirements,
        "maxCorrectiveStages": body
            .get("maxCorrectiveStages")
            .and_then(Value::as_u64)
            .unwrap_or(1),
    }))
}

/// Select control vs query-aware `reduced_1` Push from the same `/federate`
/// request body the planner already sent. Control remains the default arm:
/// query-aware is reachable only when the request explicitly opts in via
/// `pushPolicy: "queryAware"`, carrying the request's own `task` as the
/// query-admitted metadata. Membrane never derives this from task prose on
/// its own — the opt-in is an explicit planner signal, not an inference.
fn push_policy_for_request(body: &Value, task: &str) -> crate::pull::prep::PushPolicy {
    let opts_into_query_aware = body
        .get("pushPolicy")
        .and_then(Value::as_str)
        .is_some_and(|policy| policy.eq_ignore_ascii_case("queryAware"));
    if opts_into_query_aware && !task.trim().is_empty() {
        // A mode request is not admission/freshness proof. Until the owner
        // supplies a receipt-bound policy, this is a terminal exact refusal.
        crate::pull::prep::PushPolicy::query_aware(task.to_owned(), false, false)
    } else {
        crate::pull::prep::PushPolicy::Control
    }
}

/// H9 is a host observation, never a client retention assertion.  Pull can
/// bind a pending publication to an epoch only when that observation is exact
/// for this session; otherwise later delivery stays eligible by construction.
fn current_loaded_context(body: &Value, session: &str) -> Option<LoadedContextIdentitiesV1> {
    let loaded = serde_json::from_value::<LoadedContextIdentitiesV1>(
        body.get("loadedContextIdentities")?.clone(),
    )
    .ok()?;
    (loaded.validate().is_ok()
        && loaded.session_id == session
        && matches!(
            loaded.compaction_generation.coverage,
            ObservationCoverageV1::Complete
        ))
    .then_some(loaded)
}

fn current_context_epoch(loaded: &LoadedContextIdentitiesV1) -> Option<u64> {
    loaded.compaction_generation.value
}

/// The first delivery may have no prior complete H9 snapshot. Accept only a
/// host-supplied epoch from the request in that case; never synthesize zero or
/// infer an epoch from Membrane state.
fn host_context_epoch(body: &Value, loaded: Option<&LoadedContextIdentitiesV1>) -> Option<u64> {
    loaded
        .and_then(current_context_epoch)
        .or_else(|| body.get("contextEpoch").and_then(Value::as_u64))
}

fn sha256_digest(value: &Value) -> Result<String, String> {
    let canonical = serde_json::to_string(value)
        .map_err(|error| format!("serialize final Pull representation for digest: {error}"))?;
    Ok(format!(
        "sha256:{}",
        rightkit_adapt::canonical::sha256_hex(canonical.as_bytes())
    ))
}

/// Recompute Pull's existing requirement journeys from final selected bytes.
/// Admission is not delivery: any candidate absent from this representation is
/// a dropped, non-emitted journey, so its required dimension stays unsatisfied.
fn final_requirement_evidence_map(
    provisional: Option<Value>,
    selected: &Value,
    fence: Option<&PublicationFenceV1>,
) -> Value {
    let Some(provisional) = provisional else {
        return Value::Null;
    };
    let Ok(mut coverage) = serde_json::from_value::<
        rightkit_context_federation::RequirementEvidenceMapV1,
    >(provisional) else {
        return Value::Null;
    };
    let fence_held =
        fence.is_none_or(|fence| matches!(fence.status, PublicationFenceStatusV1::Held));
    let blocks = selected
        .get("blocks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for journey in &mut coverage.journeys {
        let selected_block = blocks.iter().find(|block| {
            block.get("id").and_then(Value::as_str) == Some(journey.evidence_id.as_str())
                && block.get("sourceHash").and_then(Value::as_str)
                    == Some(journey.source_hash.as_str())
        });
        journey.represented = selected_block.is_some();
        journey.fenced = journey.represented && fence_held;
        journey.emitted = journey.fenced;
        journey.retained = false;
        journey.dropped = !journey.emitted;
        if let Some(block) = selected_block {
            journey.representation_digest = sha256_digest(block).unwrap_or_default();
            journey.target_ref = block
                .get("sourceRef")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if !matches!(
                journey.state,
                rightkit_context_federation::requirements::CandidateJourneyStateV1::Stale
            ) {
                journey.state =
                    rightkit_context_federation::requirements::CandidateJourneyStateV1::DiscoveredAccepted;
            }
        } else if matches!(
            journey.state,
            rightkit_context_federation::requirements::CandidateJourneyStateV1::DiscoveredAccepted
        ) {
            let rejection = selected
                .get("omissions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|omission| {
                    omission.get("id").and_then(Value::as_str) == Some(journey.evidence_id.as_str())
                })
                .and_then(|omission| omission.get("reason"))
                .and_then(Value::as_str)
                .is_some_and(|reason| reason != "budget_exhausted");
            journey.state = if rejection {
                rightkit_context_federation::requirements::CandidateJourneyStateV1::DiscoveredRejected
            } else {
                rightkit_context_federation::requirements::CandidateJourneyStateV1::DiscoveredBudgetDropped
            };
        }
    }
    let satisfied_dimensions = coverage
        .journeys
        .iter()
        .filter(|journey| {
            journey.acquired
                && journey.eligible
                && journey.admitted
                && journey.represented
                && journey.fenced
                && journey.emitted
                && !journey.dropped
        })
        .map(|journey| journey.dimension.clone())
        .collect::<Vec<_>>();
    coverage
        .unsatisfied
        .retain(|dimension| !satisfied_dimensions.contains(dimension));
    serde_json::to_value(coverage).unwrap_or(Value::Null)
}

fn request_time_refusal(
    error: crate::pull::selection::PacketReductionRequestError,
) -> (u16, String) {
    (
        400,
        serde_json::json!({
            "error": "request_time_selection_refused",
            "kind": error.kind(),
            "reason": error.to_string(),
        })
        .to_string(),
    )
}

fn federation_timeout_refusal(error: String, elapsed_ms: u64) -> (u16, String) {
    let omission = serde_json::json!({
        "id": "federation:timeout",
        "layer": Value::Null,
        "reason": "provider_timeout",
        "detailId": error,
        "stage": "federation",
    });
    (
        504,
        serde_json::json!({
            // Hosts surface `error`; without it a timeout reads as an opaque rejection (T-11).
            "error": format!("federation deadline exceeded after {elapsed_ms} ms"),
            "kind": "provider_timeout",
            "transport": "native",
            "sourceResponse": {
                "schemaVersion": 1,
                "provider": "federation",
                "complete": false,
                "warnings": [{"code": "provider_timeout", "detailId": "deadline_exceeded"}],
                "value": {"candidates": [], "omissions": [omission.clone()]},
            },
            "finalAdmission": {
                "status": "blocked",
                "candidateCount": 0,
                "omissions": [omission.clone()],
            },
            "packet": {
                "schemaVersion": 1,
                "blocks": [],
                "omissions": [omission],
            },
            "providerDiagnostics": [{
                "provider": "federation",
                "status": "timeout",
                "generation": "unknown",
                "freshness": "unknown",
                "elapsedMs": elapsed_ms,
                "cancellation": false,
                "errors": ["provider_timeout"],
                "fallback": "none",
                "candidateCount": 0,
            }],
        })
        .to_string(),
    )
}

fn negotiated_consumer_resolvers(body: &Value) -> Vec<String> {
    // Intersection, not caller assertion: only resolver surfaces owned by this
    // runtime can be negotiated into Pull representation eligibility.  Today
    // Source & canonical-memory readers are runtime-owned resolver surfaces.
    const SUPPORTED: &[&str] = &["membrane_source_read", "membrane_memory_read"];
    let mut resolvers = body
        .pointer("/consumerCapabilities/resolvers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| SUPPORTED.contains(value))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    resolvers.sort();
    resolvers.dedup();
    resolvers
}

fn collect_native_receipts(response: &rightkit_context_protocol::FederationResponseV1) -> Value {
    let mut receipts = serde_json::Map::new();
    for key in [
        "fusionReceipt",
        "correctiveRetrieval",
        "publicationFence",
        "insufficientConfidence",
        "requirementEvidenceMap",
    ] {
        if let Some(value) = response.extensions.get(key) {
            receipts.insert(key.to_owned(), value.clone());
        }
    }
    Value::Object(receipts)
}

fn merge_native_receipts(fields: &mut serde_json::Map<String, Value>, receipts: Value) {
    if let Value::Object(receipts) = receipts {
        for (key, value) in receipts {
            fields.insert(key, value);
        }
    }
}

/// Emit BM10 accounting from the native requirement map and final selected
/// packet. Expected evidence is captured before representation selection;
/// conversion/omission rows are derived from final journey flags and packet
/// identity, while host/model use remains explicitly unobserved.
fn merge_bm10_accounting(
    fields: &mut serde_json::Map<String, Value>,
    provisional: Option<&Value>,
    final_map: Option<&Value>,
    packet: Option<&Value>,
) {
    let Some(provisional) = provisional else {
        return;
    };
    let Some(final_map) = final_map else {
        return;
    };
    let Some(expected_journeys) = provisional.get("journeys").and_then(Value::as_array) else {
        return;
    };
    let Some(journeys) = final_map.get("journeys").and_then(Value::as_array) else {
        return;
    };
    let blocks = packet
        .and_then(|value| value.get("blocks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let expected_evidence = expected_journeys
        .iter()
        .filter_map(|journey| {
            journey
                .get("evidenceId")
                .and_then(Value::as_str)
                .map(|evidence_id| serde_json::json!({"evidenceId": evidence_id, "required": true}))
        })
        .collect::<Vec<_>>();
    let mut conversion_receipts = Vec::new();
    let mut omission_receipts = Vec::new();
    let mut delivery_attribution = Vec::new();
    for journey in journeys {
        let Some(evidence_id) = journey.get("evidenceId").and_then(Value::as_str) else {
            continue;
        };
        let state = journey
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN");
        let represented = journey
            .get("represented")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let emitted = journey
            .get("emitted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if represented {
            let digest = journey
                .get("representationDigest")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            conversion_receipts.push(serde_json::json!({"receiptId": format!("pull-convert-{evidence_id}"), "evidenceId": evidence_id, "reason": "represented", "representationDigest": digest}));
        } else {
            omission_receipts.push(serde_json::json!({"receiptId": format!("pull-omit-{evidence_id}"), "evidenceId": evidence_id, "reason": state.to_ascii_lowercase()}));
        }
        let host_included = emitted
            && blocks
                .iter()
                .any(|block| block.get("id").and_then(Value::as_str) == Some(evidence_id));
        delivery_attribution.push(serde_json::json!({"evidenceId": evidence_id, "emitted": emitted, "hostIncluded": host_included, "modelUsed": false, "helped": false}));
    }
    fields.insert(
        "expectedEvidence".to_owned(),
        Value::Array(expected_evidence),
    );
    fields.insert(
        "conversionReceipts".to_owned(),
        Value::Array(conversion_receipts),
    );
    fields.insert(
        "omissionReceipts".to_owned(),
        Value::Array(omission_receipts),
    );
    fields.insert(
        "deliveryAttribution".to_owned(),
        Value::Array(delivery_attribution),
    );
}

#[allow(clippy::too_many_arguments)]
fn native_request(
    task: &str,
    root: &Path,
    max_tokens: usize,
    deadline_ms: u64,
    release_generation: String,
    client: &str,
    session: &str,
    anchors: Vec<String>,
    scope_grant_id: Option<String>,
    sufficiency_contract: Option<Value>,
) -> rightkit_context_protocol::FederationRequestV1 {
    let repository_root = root.to_string_lossy().into_owned();
    let request_id = crate::store::opaque_correlation_token(task, "federation");
    let mut extensions = std::collections::BTreeMap::from([
        (
            "repositoryId".to_owned(),
            serde_json::json!(rightkit_context_federation::root::canonical_repository_id(
                root
            )),
        ),
        (
            "worktreeRoot".to_owned(),
            serde_json::json!(repository_root),
        ),
    ]);
    if let Some(contract) = sufficiency_contract {
        extensions.insert("sufficiencyContract".to_owned(), contract);
    }
    rightkit_context_protocol::FederationRequestV1 {
        schema_version: rightkit_context_protocol::FEDERATION_REQUEST_SCHEMA_VERSION,
        request_id,
        // Every packet built from this request is refused without one, so an
        // empty trace id meant no context request could ever produce a packet
        // — the reduction plan rejects `traceId` before it looks at anything
        // else. The same generator already stamps the verify path.
        trace_id: new_trace_id(),
        task: task.to_owned(),
        repository_root: repository_root.clone(),
        client: client.to_owned(),
        session_id: session.to_owned(),
        deadline_ms,
        max_tokens: max_tokens.min(u32::MAX as usize) as u32,
        anchors,
        scope_grant_id,
        manifest_digest: None,
        release_generation: Some(release_generation),
        blueprint_generation: None,
        skills_generation: None,
        extensions,
    }
}

#[allow(clippy::too_many_arguments)]
fn native_request_with_h8(
    task: &str,
    root: &Path,
    max_tokens: usize,
    deadline_ms: u64,
    release_generation: String,
    client: &str,
    session: &str,
    anchors: Vec<String>,
    scope_grant_id: Option<String>,
    sufficiency_contract: Option<Value>,
    ceiling: &rightkit_context_protocol::host_observation::RemainingContextCeilingV1,
) -> rightkit_context_protocol::FederationRequestV1 {
    let mut request = native_request(
        task,
        root,
        max_tokens,
        deadline_ms,
        release_generation,
        client,
        session,
        anchors,
        scope_grant_id,
        sufficiency_contract,
    );
    request.extensions.insert(
        "remainingContextCeiling".to_owned(),
        serde_json::to_value(ceiling).expect("RemainingContextCeilingV1 is serializable"),
    );
    request
}

pub fn native_response_to_ccs(
    response: &rightkit_context_protocol::FederationResponseV1,
    request: &rightkit_context_protocol::FederationRequestV1,
    freshness: &rightkit_context_protocol::FreshnessSnapshotV1,
) -> Value {
    let mut candidates =
        serde_json::to_value(&response.candidates).unwrap_or_else(|_| Value::Array(Vec::new()));
    if freshness.stale {
        mark_stale_blueprint_candidates(&mut candidates);
    }
    let mut source_resolutions = std::collections::BTreeMap::new();
    let mut atomic_evidence_paths = Vec::new();
    // Source-resolution receipts are bound to the provider's content
    // generation (Blueprint's graph identity), while release generation is
    // the Membrane build identity.  CCS generationId and overlayIdentity must
    // use the former when a Blueprint lane answered; using freshness's release
    // fallback makes an internally coherent stale receipt look mismatched.
    let source_generation = response
        .providers
        .iter()
        .find(|provider| provider.provider == rightkit_context_protocol::ProviderId::Blueprint)
        .and_then(|provider| provider.generation.clone())
        .or_else(|| freshness.generation.clone())
        .or_else(|| request.release_generation.clone());
    for provider in &response.providers {
        if let Some(items) = provider
            .extensions
            .get("sourceResolutions")
            .and_then(Value::as_array)
        {
            for item in items {
                if let Some(id) = item.get("candidateId").and_then(Value::as_str) {
                    source_resolutions.insert(id.to_owned(), item.clone());
                }
            }
        }
        if let Some(items) = provider
            .extensions
            .get("atomicEvidencePaths")
            .and_then(Value::as_array)
        {
            atomic_evidence_paths.extend(items.iter().cloned());
        }
    }
    if let Some(items) = candidates.as_array_mut() {
        for candidate in items {
            // Native providers may omit cost metadata for otherwise valid
            // evidence. A zero cost makes planner admission unbounded, so a
            // 12k-token request can retain every candidate and later overflow
            // the serialized MCP response. Derive only missing cost from the
            // exact delivered text; preserve provider estimates when present.
            if candidate.get("estimatedTokens").and_then(Value::as_u64) == Some(0) {
                let text_tokens = candidate
                    .get("text")
                    .and_then(Value::as_str)
                    .and_then(|text| {
                        rightkit_memory_core::ContextTokenAccounting::count_exact(text).ok()
                    })
                    .unwrap_or(0)
                    .max(1)
                    .min(u32::MAX as usize) as u64;
                candidate["estimatedTokens"] = Value::from(text_tokens);
            }
            if let Some(id) = candidate.get("id").and_then(Value::as_str) {
                if let Some(receipt) = source_resolutions.get(id) {
                    candidate["sourceResolution"] = receipt.clone();
                }
            }
        }
    }
    let omissions = response
        .omissions
        .iter()
        .enumerate()
        .map(|(index, omission)| {
            let reason = omission.reason.as_str();
            let mut value = serde_json::json!({
                // A provider-level omission carries no candidate id, so this
                // read `omission:0`, `omission:1` and so on — which says a
                // provider was dropped but never which one. The provider is
                // right there on the record.
                "id": omission.candidate_id.clone().unwrap_or_else(|| {
                    format!("{}:{index}", omission.provider.as_str())
                }),
                "layer": Value::Null,
                "reason": reason,
            });
            if let Some(detail_id) = omission.detail_id.as_ref() {
                value["detailId"] = Value::String(detail_id.clone());
            }
            if let Some(stage) = omission.stage.as_ref() {
                value["stage"] = Value::String(stage.clone());
            }
            value
        })
        .collect::<Vec<_>>();
    let provider_outputs = response
        .extensions
        .get("providerOutputs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let unsupported_capability = request
        .extensions
        .get("consumerCapabilities")
        .and_then(|value| value.get("resolvers"))
        .and_then(Value::as_array)
        .is_some_and(|resolvers| {
            resolvers.iter().filter_map(Value::as_str).any(|resolver| {
                !matches!(resolver, "membrane_source_read" | "membrane_memory_read")
            })
        });
    let blueprint = provider_outputs
        .iter()
        .find(|output| output.get("provider").and_then(Value::as_str) == Some("blueprint"));
    let source_response = blueprint.map(|output| {
        let output_omissions = output
            .get("omissions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let output_warnings = output
            .get("warnings")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        let status_complete = output
            .get("status")
            .and_then(Value::as_str)
            == Some("complete");
        serde_json::json!({
            "schemaVersion": 1,
            "provider": "blueprint",
            "generation": output.get("generation").cloned().unwrap_or(Value::Null),
            "complete": status_complete && output_omissions.is_empty(),
            "warnings": output_warnings,
            "value": {
                "generation": output.get("generation").cloned().unwrap_or(Value::Null),
                "candidates": output.get("candidates").cloned().unwrap_or_else(|| Value::Array(Vec::new())),
                "omissions": output_omissions,
                "diagnostics": output.get("diagnostics").cloned().unwrap_or(Value::Null),
            }
        })
    }).unwrap_or_else(|| serde_json::json!({
        "schemaVersion": 1,
        "provider": "blueprint",
        "generation": Value::Null,
        "complete": false,
        "warnings": [{"code": "provider_unavailable", "detailId": "blueprint_output_missing"}],
        "value": {"candidates": [], "omissions": [{"reason": "provider_unavailable"}]}
    }));
    let provider_diagnostics = response
        .diagnostics
        .as_ref()
        .map(|diagnostics| {
            diagnostics
                .providers
                .iter()
                .map(|provider| serde_json::to_value(provider).unwrap_or(Value::Null))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            provider_outputs
                .iter()
                .filter_map(|output| output.get("diagnostics").cloned())
                .collect::<Vec<_>>()
        });
    let provider_diagnostics = provider_diagnostics
        .into_iter()
        .map(|mut value| {
            // Native protocol keeps diagnostic attributes content-free strings;
            // Pull's public projection exposes typed fields.
            if let Some(attributes) = value.get_mut("attributes").and_then(Value::as_object_mut) {
                if let Some(cancellation) = attributes.get("cancellation").and_then(Value::as_str) {
                    if let Ok(parsed) = cancellation.parse::<bool>() {
                        attributes.insert("cancellation".to_owned(), Value::Bool(parsed));
                    }
                }
                if let Some(candidate_count) =
                    attributes.get("candidateCount").and_then(Value::as_str)
                {
                    if let Ok(parsed) = candidate_count.parse::<u64>() {
                        attributes
                            .insert("candidateCount".to_owned(), Value::Number(parsed.into()));
                    }
                }
                if let Some(errors) = attributes.get("errors").and_then(Value::as_str) {
                    if let Ok(parsed @ Value::Array(_)) = serde_json::from_str::<Value>(errors) {
                        attributes.insert("errors".to_owned(), parsed);
                    }
                }
            }
            value
        })
        .collect::<Vec<_>>();
    let source_complete = source_response
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && response.omissions.is_empty();
    let mut source_response = source_response;
    source_response["complete"] = Value::Bool(source_complete);
    let requested_generation = request
        .extensions
        .get("hostGeneration")
        .and_then(Value::as_str);
    let observed_generation = freshness.generation.as_deref();
    let host_generation_stale = requested_generation
        .zip(observed_generation)
        .is_some_and(|(requested, observed)| requested != observed);
    if host_generation_stale {
        source_response["complete"] = Value::Bool(false);
        if let Some(warnings) = source_response
            .get_mut("warnings")
            .and_then(Value::as_array_mut)
        {
            warnings.push(serde_json::json!({
                "code": "blueprint_stale",
                "detailId": "host_generation_mismatch",
            }));
        }
    }
    if unsupported_capability {
        source_response["complete"] = Value::Bool(false);
        if let Some(warnings) = source_response
            .get_mut("warnings")
            .and_then(Value::as_array_mut)
        {
            warnings.push(serde_json::json!({
                "code": "provider_capability_missing",
                "detailId": "consumer_resolver_unavailable",
            }));
        }
    }
    if let Some(warnings) = source_response
        .get_mut("warnings")
        .and_then(Value::as_array_mut)
    {
        for omission in &response.omissions {
            warnings.push(serde_json::json!({
                "code": omission.reason.as_str(),
                "detailId": omission.detail_id.clone(),
            }));
        }
        for warning in &response.warnings {
            warnings.push(serde_json::json!({
                "code": warning.reason.as_str(),
                "detailId": warning.detail_id.clone(),
            }));
        }
    }
    let final_admission = serde_json::json!({
        "status": if response.candidates.is_empty() { "insufficient" } else if response.omissions.is_empty() { "admitted" } else { "degraded" },
        "candidateCount": response.candidates.len(),
        "omissions": omissions.clone(),
        "warnings": response.warnings.clone(),
        "generation": freshness.generation.clone().or_else(|| request.release_generation.clone()),
    });
    let query_projection = provider_outputs
        .iter()
        .find(|output| output.get("provider").and_then(Value::as_str) == Some("blueprint"));
    let baseline_projection = provider_outputs
        .iter()
        .find(|output| output.get("provider").and_then(Value::as_str) == Some("cortex"));
    let projection = |output: Option<&Value>, authority: &str| {
        let candidates = output
            .and_then(|value| value.get("candidates"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let omissions = output
            .and_then(|value| value.get("omissions"))
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        serde_json::json!({
            "applicable": output.is_some(),
            "scope": if authority == "blueprint" { "task" } else { "governed-baseline" },
            "authority": authority,
            "budget": request.max_tokens,
            "candidateCount": candidates,
            "omissions": omissions,
        })
    };
    let candidate_ids = response
        .candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    let ambiguity = response.omissions.iter().any(|omission| {
        omission.detail_id.as_deref() == Some("same_tier_ambiguity")
            || omission.reason.as_str() == "candidate_identity_conflict"
    });
    let ambiguity_disposition = serde_json::json!({
        "action": if ambiguity { "block" } else if candidate_ids.is_empty() { "noop" } else { "allow" },
        "candidates": candidate_ids,
        "selectedId": Value::Null,
    });
    let contradiction_pairs = response
        .candidates
        .first()
        .zip(response.candidates.get(1))
        .map(|(left, right)| {
            serde_json::json!([{
                "leftId": left.id,
                "rightId": right.id,
                "required": false,
                "present": true,
            }])
        })
        .unwrap_or_else(|| Value::Array(Vec::new()));
    serde_json::json!({
        "schemaVersion": 1,
        "traceId": request.trace_id,
        "indexedAt": freshness.snapshot_id.clone().unwrap_or_else(|| request.release_generation.clone().unwrap_or_default()),
        "task": request.task,
        "mode": "native",
        "provider": "federation",
        "generationId": source_generation.clone().unwrap_or_default(),
        "atomicEvidencePaths": atomic_evidence_paths,
        "freshness": {
            "revision": source_generation.clone().unwrap_or_default(),
            "indexedAt": freshness.snapshot_id.clone().unwrap_or_default(),
            "stale": freshness.stale,
            "snapshotId": freshness.snapshot_id.clone(),
            "baseCommit": freshness.base_commit.clone(),
            "overlayDigest": freshness.overlay_digest.clone(),
            "overlayIdentity": freshness.overlay_digest.as_ref().map(|digest| serde_json::json!({
                "sessionId": request.session_id,
                "worktreePath": request.repository_root,
                "generationId": source_generation.clone().unwrap_or_default(),
                "overlayDigest": digest,
            })),
        },
        "providerCeiling": {"maxCandidates": 256, "maxEstimatedTokens": request.max_tokens},
        "candidates": candidates,
        "omissions": omissions,
        "sourceResponse": source_response,
        "finalAdmission": final_admission,
        "admissionComparison": {
            "independent": query_projection.is_some() && baseline_projection.is_some(),
            "queryDriven": projection(query_projection, "blueprint"),
            "baseline": projection(baseline_projection, "cortex"),
        },
        "providerDiagnostics": provider_diagnostics,
        "contradictionPairs": contradiction_pairs,
        "ambiguityDisposition": ambiguity_disposition,
    })
}

/// Planner-side half of one federation cycle, shared verbatim by the CLI
/// (`run_federate`) and the resident `/federate` route: parse the gateway's
/// CCS line, surface fail-closed aborts, run the in-process planner, and
/// assemble the client envelope.
pub struct EnvelopeInput {
    pub max_tokens: usize,
    pub packet_char_budget_override: Option<usize>,
    pub packet_char_budget_model: Option<String>,
    pub accepted_receipt_versions: Vec<u32>,
    pub scope_grant_present: bool,
    /// Resolver mechanisms the consumer has negotiated and can call.
    pub consumer_resolvers: Vec<String>,
    /// Publication fence input (pending §17.2). The grant owner re-validated
    /// the bound scope grant after fusion and passes the typed verdict here:
    /// `None` is honest only when no grant was ever bound (scope-free
    /// request); `Some(tripped)` refuses packet emission fail-closed.
    pub scope_grant_fence: Option<PublicationFenceV1>,
    /// Wall time spent obtaining the CCS (process spawn or worker roundtrip).
    pub gateway_process_ms: f64,
}

/// Publication fence for the runtime packet seam (pending §17.2).
///
/// Grant identity, policy epoch and revocation are re-checked after fusion,
/// immediately before packet emission. A tripped fence publishes typed
/// `policy_changed` and refuses to emit the packet authorized under the
/// superseded grant; `None` means no grant was bound and the fence is a
/// no-op, never a silent bypass.
pub fn fence_packet_emission(
    fence: Option<PublicationFenceV1>,
) -> Result<Option<PublicationFenceV1>, String> {
    match fence {
        None => Ok(None),
        Some(fence) => {
            if matches!(fence.status, PublicationFenceStatusV1::PolicyChanged) {
                Err(format!(
                    "publication fenced after fusion: policy_changed ({:?}); \
                     the stale-authorized packet is not emitted",
                    fence.change
                ))
            } else {
                Ok(Some(fence))
            }
        }
    }
}

pub fn envelope_from_ccs(stdout: &str, input: EnvelopeInput) -> Result<Value, String> {
    // The gateway may emit a fail-closed envelope (exit 2) when a
    // ScopeGrant is rejected. Detect that envelope and surface it
    // before attempting strict CCS deserialization.
    let parse_started = Instant::now();
    let mut raw_value: Value = match serde_json::from_str(stdout) {
        Ok(v) => v,
        Err(e) => {
            return Err(format!(
                "federation gateway returned non-JSON payload: {e}; first 200 bytes: {}",
                stdout.chars().take(200).collect::<String>()
            ));
        }
    };
    if raw_value.get("_membrane").is_some() {
        if let Some(abort_reason) = raw_value
            .get("_membrane")
            .and_then(|v| v.get("abortReason"))
            .and_then(|v| v.as_str())
        {
            return Err(format!(
                "federation aborted by gateway: abortReason={abort_reason}; abortDetail={}",
                raw_value
                    .get("_membrane")
                    .and_then(|v| v.get("abortDetail"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("(none)")
            ));
        }
    }
    // Publication fence (pending §17.2): the grant owner re-validated the
    // bound grant after fusion. A tripped fence refuses packet emission here —
    // the last admission boundary before the packet reaches a client.
    let publication_fence = fence_packet_emission(input.scope_grant_fence)?;
    let observability = gateway_observability(&raw_value);
    let source_response_projection = raw_value.get("sourceResponse").cloned();
    let final_admission_projection = raw_value.get("finalAdmission").cloned();
    let admission_comparison_projection = raw_value.get("admissionComparison").cloned();
    let contradiction_pairs_projection = raw_value.get("contradictionPairs").cloned();
    let ambiguity_disposition_projection = raw_value.get("ambiguityDisposition").cloned();
    let provider_diagnostics_projection = raw_value.get("providerDiagnostics").cloned();
    let provider_outputs_projection = raw_value.get("providerOutputs").cloned();
    let budget_reduction_projection = raw_value.get("budgetReduction").cloned();
    let journey_projection = raw_value.get("requirementEvidenceMap").cloned();
    let atomic_evidence_paths = raw_value
        .as_object_mut()
        .and_then(|object| object.remove("atomicEvidencePaths"))
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let source_resolution_receipts =
        crate::source_resolution::gate_source_resolutions(&mut raw_value);
    let ccs: ContextCandidateSetV1 = match serde_json::from_value(raw_value) {
        Ok(v) => v,
        Err(e) => {
            return Err(format!(
                "federation gateway returned non-CCS payload: {e}; first 200 bytes: {}",
                stdout.chars().take(200).collect::<String>()
            ));
        }
    };
    let rust_parse_ms = parse_started.elapsed().as_secs_f64() * 1000.0;
    let planner_input = PlannerInput {
        candidate_set: ccs,
        max_tokens: input.max_tokens,
        packet_char_budget_override: input.packet_char_budget_override,
        packet_char_budget_model: input.packet_char_budget_model,
        accepted_receipt_versions: input.accepted_receipt_versions,
        trace_id_override: None,
        scope_grant_present: input.scope_grant_present,
        consumer_resolvers: input.consumer_resolvers,
        // PUL-026: normal production has retired memory/skill lane
        // reservations; `None` remains available only to explicit planner
        // callers as the isolated migration rollback control.
        reserved_lanes: Some(Vec::new()),
    };
    let planner_started = Instant::now();
    let out = match plan(&planner_input) {
        Ok(o) => o,
        Err(e) => return Err(format!("planner rejected federation CSS: {e}")),
    };
    let rust_planner_ms = planner_started.elapsed().as_secs_f64() * 1000.0;
    let mut payload = serde_json::json!({
        "packet": out.packet,
        "receipts": out.receipts,
        "providerStatus": out.provider_status,
        "fallbackMode": out.fallback_mode,
        "degradationReason": out.degradation_reason,
        "sourceGeneration": out.source_generation,
        "expectedReleaseGeneration": out.expected_release_generation,
        "observedReleaseGeneration": out.observed_release_generation,
        "releaseGenerationStatus": out.release_generation_status,
        "structuredEvent": out.structured_event,
        "sourceResolutionReceipts": source_resolution_receipts,
        "atomicEvidencePaths": atomic_evidence_paths,
    });
    if let Some(value) = source_response_projection {
        payload["sourceResponse"] = value;
    }
    if let Some(value) = final_admission_projection {
        payload["finalAdmission"] = value;
    }
    if let Some(value) = admission_comparison_projection {
        payload["admissionComparison"] = value;
    }
    if let Some(value) = contradiction_pairs_projection {
        // Contradiction records are only atomic when both sides survive into
        // final packet blocks.  Never publish pair IDs for budget-dropped
        // candidates.
        let block_ids = payload
            .get("packet")
            .and_then(|packet| packet.get("blocks"))
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block.get("id").and_then(Value::as_str))
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();
        let filtered = match value {
            Value::Array(pairs) => Value::Array(
                pairs
                    .into_iter()
                    .filter(|pair| {
                        let left = pair.get("leftId").and_then(Value::as_str);
                        let right = pair.get("rightId").and_then(Value::as_str);
                        left.is_some_and(|left| block_ids.contains(left))
                            && right.is_some_and(|right| block_ids.contains(right))
                    })
                    .collect(),
            ),
            value => value,
        };
        payload["contradictionPairs"] = filtered;
    }
    if let Some(value) = ambiguity_disposition_projection {
        payload["ambiguityDisposition"] = value;
    }
    if let Some(value) = provider_diagnostics_projection {
        payload["providerDiagnostics"] = value;
    }
    if let Some(value) = provider_outputs_projection {
        payload["providerOutputs"] = value;
    }
    if let Some(value) = budget_reduction_projection {
        payload["budgetReduction"] = value;
    }
    if let Some(value) = journey_projection {
        payload["requirementEvidenceMap"] = value;
    }
    // A bounded final representation is a partial outer response even when
    // Blueprint itself answered completely. Keep this projection tied to the
    // planner's actual omission rather than to the request flag alone.
    if input.packet_char_budget_override.is_some()
        && payload
            .get("packet")
            .and_then(|packet| packet.get("omissions"))
            .and_then(Value::as_array)
            .is_some_and(|omissions| !omissions.is_empty())
    {
        let source = payload["sourceResponse"].as_object_mut();
        if let Some(source) = source {
            source.insert("complete".to_owned(), Value::Bool(false));
            source
                .entry("warnings".to_owned())
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(warnings) = source.get_mut("warnings").and_then(Value::as_array_mut) {
                warnings.push(
                    serde_json::json!({"code": "provider_partial", "detailId": "packet_budget"}),
                );
            }
        }
    }
    if let Some(fence) = publication_fence {
        payload["publicationFence"] = serde_json::to_value(fence)
            .map_err(|error| format!("serialize publication fence: {error}"))?;
    }
    if let Some(packet) = payload.get("packet") {
        payload["cachePrefixDiagnostic"] =
            serde_json::to_value(crate::cache_prefix::diagnose_cache_prefix(packet, None))
                .expect("cache prefix diagnostic serializes");
    }
    if let (Some(payload_fields), Some(observability_fields)) =
        (payload.as_object_mut(), observability.as_object())
    {
        payload_fields.extend(observability_fields.clone());
        let stages = payload_fields
            .entry("stageElapsedMs".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(stage_fields) = stages.as_object_mut() {
            stage_fields.insert(
                "gateway_process".to_string(),
                input.gateway_process_ms.into(),
            );
            stage_fields.insert("rust_parse".to_string(), rust_parse_ms.into());
            stage_fields.insert("rust_planner".to_string(), rust_planner_ms.into());
        }
    }
    Ok(payload)
}

/// Membrane Cortex durable-memory candidate provider. Pure in-process
/// read of eligible MemoryEntry rows normalised into ContextCandidateSet v1
/// records (Layer 7, sourceKind "memory", trustClass "agent_verified").
#[allow(clippy::too_many_arguments)]
pub fn run_memory_candidates(
    task: String,
    repo: PathBuf,
    scope: Option<String>,
    max_candidates: usize,
    _scope_grant_id: Option<String>,
    db: Option<&Path>,
) -> Result<(), String> {
    let canonical_repo = repo
        .canonicalize()
        .map_err(|e| format!("resolve repo: {e}"))?;
    let store = match db {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create candidate DB parent: {e}"))?;
            }
            crate::MemoryStore::try_open(
                crate::MemDb::open(path).map_err(|e| format!("open candidate DB: {e}"))?,
            )?
        }
        None => crate::service::open_installed_store()?,
    };

    let scope_id = scope.clone().unwrap_or_else(|| "D--Claude".to_string());
    // The CLI always has a real, already-canonicalized repo root in hand (canonicalize()
    // above already succeeded or this function would have returned), so this call site
    // always has a genuine freshness signal available — unlike the resident-serve HTTP
    // route, which does not (see the /memory-candidates handler in serve.rs).
    let payload = memory_candidates_payload(
        &store,
        &task,
        &scope_id,
        max_candidates,
        Some(&canonical_repo),
    );
    crate::cli::emit_stdout(format_args!(
        "{}",
        serde_json::to_string_pretty(&payload).map_err(|e| format!("serialize: {e}"))?
    ));
    Ok(())
}

/// Testable core: build the Cortex ContextCandidateSet from REAL relevance-ranked memories.
///
/// Uses `recall_scored` (the same full-corpus hybrid retriever that backs live `context_for`),
/// not an arbitrary `entries(max)` slice — so results are relevant. Emits `text` = a bounded
/// word-boundary content preview (the old code emitted `text = e.id`, i.e. a useless slug), and a
/// real content hash. Feedback-rail vetoes are applied via `gate_history_for` so a memory the agent
/// marked `contradicted` never surfaces here either. Adapt proposals can reach this Pull surface
/// only after Cortex admission, through this `MemoryStore` recall boundary; Pull has no Adapt
/// candidate or mutation input.
pub fn memory_candidates_payload(
    store: &crate::MemoryStore,
    task: &str,
    scope_id: &str,
    max_candidates: usize,
    repo_root: Option<&Path>,
) -> serde_json::Value {
    memory_candidates_payload_for_descriptor(
        store,
        task,
        &crate::scope::ScopeDescriptorV1::filesystem(scope_id),
        max_candidates,
        repo_root,
    )
    .unwrap_or_else(|error| serde_json::json!({"error": error, "candidates": []}))
}

/// Reason code for a memory candidate that scored well enough to be considered but was cut by
/// the caller's `max_candidates` ceiling. Mirrors the `Omission.reason` convention in
/// `memory_provider.rs::reasons`, scoped locally since this function's admission model (a single
/// recall pass, not `consider_entries`/`partition`) has no other omission source to report.
const OMISSION_REASON_CEILING_TRUNCATED: &str = "ceiling_truncated";

enum CortexCandidateHit {
    Memory(rightkit_memory_core::MemoryEntry, f32),
    Temporal(rightkit_memory_store::TemporalFact, f32),
}

impl CortexCandidateHit {
    fn omission_id(&self) -> String {
        match self {
            Self::Memory(entry, _) => format!("memory:role:{}", entry.id),
            Self::Temporal(fact, _) => format!("memory:temporal:{}", fact.fact_id),
        }
    }
}

/// Descriptor-aware candidate surface. Virtual scope ancestry is exact and opaque; a legacy
/// string is deliberately represented as a filesystem descriptor by the compatibility wrapper.
pub fn memory_candidates_payload_for_descriptor(
    store: &crate::MemoryStore,
    task: &str,
    descriptor: &crate::scope::ScopeDescriptorV1,
    max_candidates: usize,
    repo_root: Option<&Path>,
) -> Result<serde_json::Value, String> {
    memory_candidates_payload_for_descriptor_inner(
        store,
        task,
        descriptor,
        max_candidates,
        repo_root,
        None,
        None,
        false,
        false,
    )
}

pub fn memory_candidates_payload_for_descriptor_cancellable(
    store: &crate::MemoryStore,
    task: &str,
    descriptor: &crate::scope::ScopeDescriptorV1,
    max_candidates: usize,
    repo_root: Option<&Path>,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<serde_json::Value, String> {
    memory_candidates_payload_for_descriptor_cancellable_with_temporal(
        store,
        task,
        descriptor,
        max_candidates,
        repo_root,
        cancellation,
        None,
    )
}

pub fn memory_candidates_payload_for_descriptor_cancellable_with_temporal(
    store: &crate::MemoryStore,
    task: &str,
    descriptor: &crate::scope::ScopeDescriptorV1,
    max_candidates: usize,
    repo_root: Option<&Path>,
    cancellation: &tokio_util::sync::CancellationToken,
    temporal: Option<rightkit_memory_store::TemporalFactQuery>,
) -> Result<serde_json::Value, String> {
    memory_candidates_payload_for_descriptor_inner(
        store,
        task,
        descriptor,
        max_candidates,
        repo_root,
        Some(cancellation),
        temporal,
        false,
        false,
    )
}

/// Latency-bound Cortex candidates for the foreground (hook and explicit-pull) path, read from the
/// persisted lexical index. `explicit_pull` selects the explicit-pull recall
/// (`MemoryStore::recall_lexical_bounded_explicit`); hooks pass `false` and keep
/// `recall_lexical_bounded`.
pub fn memory_candidates_payload_for_descriptor_lexical(
    store: &crate::MemoryStore,
    task: &str,
    descriptor: &crate::scope::ScopeDescriptorV1,
    max_candidates: usize,
    repo_root: Option<&Path>,
    explicit_pull: bool,
) -> Result<serde_json::Value, String> {
    memory_candidates_payload_for_descriptor_inner(
        store,
        task,
        descriptor,
        max_candidates,
        repo_root,
        None,
        None,
        true,
        explicit_pull,
    )
}

#[allow(clippy::too_many_arguments)]
fn memory_candidates_payload_for_descriptor_inner(
    store: &crate::MemoryStore,
    task: &str,
    descriptor: &crate::scope::ScopeDescriptorV1,
    max_candidates: usize,
    repo_root: Option<&Path>,
    cancellation: Option<&tokio_util::sync::CancellationToken>,
    temporal: Option<rightkit_memory_store::TemporalFactQuery>,
    lexical_only: bool,
    explicit_pull: bool,
) -> Result<serde_json::Value, String> {
    // Canonicalize whatever the caller sent (raw filesystem path, slug, or `global`) into the full
    // visibility chain: self + ancestor scopes that hold rows + global. Before 2026-07-16 this
    // passed the raw string into recall (clients send paths like `D:\Claude`), so project-scoped
    // rows never matched and the rich path recalled from the global corpus only (Sol audit P0).
    let scope_started = Instant::now();
    let scopes = descriptor
        .resolve_chain(&store.scopes())
        .map_err(|error| format!("invalid scope descriptor: {error}"))?;
    let scope_ms = scope_started.elapsed().as_secs_f64() * 1000.0;
    // Shared recall owns one-hop augmentation, its bounded graph lane, and effectiveness vetoes.
    // Keeping those policies here as well would double-expand candidates and split behavior across
    // live recall, replay, and federation.
    //
    // F11: request one MORE than the ceiling so a real ceiling-truncation can be told apart from
    // "nothing else scored" without changing what gets served — the first `max_candidates` hits of
    // an N+1 request are the same top-N a request for exactly N would have returned (ranking is
    // deterministic; see `recall_scored`'s doc comment), so this changes zero user-visible output.
    let probe_limit = max_candidates.saturating_add(1);
    let (mut hits, mut stage_elapsed, mut completeness): (
        Vec<CortexCandidateHit>,
        crate::store::RecallStageElapsed,
        crate::store::CortexCompletenessV1,
    ) = if lexical_only {
        let (hits, completeness) = if explicit_pull {
            store.recall_lexical_bounded_explicit(task, probe_limit, &scopes)
        } else {
            store.recall_lexical_bounded(task, probe_limit, &scopes)
        };
        (
            hits.into_iter()
                .map(|(entry, score)| CortexCandidateHit::Memory(entry, score))
                .collect(),
            crate::store::RecallStageElapsed::default(),
            completeness,
        )
    } else if let Some(cancellation) = cancellation {
        if temporal.is_some() {
            let page = store.recall_typed_bounded(
                task,
                probe_limit,
                &scopes,
                temporal,
                true,
                cancellation,
            );
            (
                page.items
                    .into_iter()
                    .map(|item| match item {
                        crate::store::RecallResult::Memory { entry, score } => {
                            CortexCandidateHit::Memory(entry, score)
                        }
                        crate::store::RecallResult::Temporal { fact, score } => {
                            CortexCandidateHit::Temporal(fact, score)
                        }
                    })
                    .collect(),
                crate::store::RecallStageElapsed::default(),
                page.completeness,
            )
        } else {
            let (hits, elapsed, completeness) = store.recall_scored_detailed_timed_cancellable(
                task,
                probe_limit,
                &scopes,
                true,
                cancellation,
            );
            (
                hits.into_iter()
                    .map(|hit| CortexCandidateHit::Memory(hit.entry, hit.score))
                    .collect(),
                elapsed,
                completeness,
            )
        }
    } else {
        let (hits, elapsed) = store.recall_scored_timed(task, probe_limit, &scopes);
        let completeness = crate::store::CortexCompletenessV1::exact(hits.len(), hits.len(), 0);
        (
            hits.into_iter()
                .map(|(entry, score)| CortexCandidateHit::Memory(entry, score))
                .collect(),
            elapsed,
            completeness,
        )
    };
    stage_elapsed.recall_ms += scope_ms;
    let dropped_by_ceiling = if hits.len() > max_candidates {
        hits.split_off(max_candidates)
    } else {
        Vec::new()
    };
    if !dropped_by_ceiling.is_empty() {
        completeness.state = crate::store::CortexCompletenessState::LowerBound;
        completeness.counts_exact = false;
        if !completeness
            .causes
            .iter()
            .any(|cause| cause == OMISSION_REASON_CEILING_TRUNCATED)
        {
            completeness
                .causes
                .push(OMISSION_REASON_CEILING_TRUNCATED.to_owned());
        }
        completeness.considered_count = hits.len().saturating_add(1);
        completeness.returned_count = hits.len();
        completeness.dropped_count = completeness.dropped_count.max(1);
    } else {
        completeness.returned_count = hits.len();
    }
    let rank_started = Instant::now();
    let candidates: Vec<serde_json::Value> = hits
        .iter()
        .map(|hit| match hit {
            CortexCandidateHit::Memory(e, score) => {
                let preview = memory_preview(&e.content);
                serde_json::json!({
                    "id": format!("memory:role:{}", e.id),
                    "layer": 7,
                    "sourceKind": "memory",
                    "sourceRef": e.scope_id.clone(),
                    "sourceHash": sha256_bytes(e.content.as_bytes()),
                    "trustClass": "agent_verified",
                    "instructionPolicy": "data_only",
                    "providerScore": score.clamp(0.0, 1.0),
                    "scoreComponents": {"structural": score.clamp(0.0, 1.0), "relevance": score.clamp(0.0, 1.0)},
                    "estimatedTokens": std::cmp::max(1, preview.chars().count() / 4),
                    "protected": false,
                    "exact": false,
                    "recoverable": true,
                    "resolver": format!(
                        "membrane_memory_read:membrane_memory:get:{}:sha256:{}",
                        e.id,
                        sha256_bytes(e.content.as_bytes())
                    ),
                    "text": preview,
                })
            }
            CortexCandidateHit::Temporal(fact, score) => {
                let text = serde_json::to_string(fact).unwrap_or_else(|_| "{}".to_owned());
                serde_json::json!({
                    "id": format!("memory:temporal:{}", fact.fact_id),
                    "layer": 7,
                    "sourceKind": "memory",
                    "sourceRef": fact.scope_id,
                    "sourceHash": sha256_bytes(text.as_bytes()),
                    "trustClass": "agent_verified",
                    "instructionPolicy": "data_only",
                    "providerScore": score.clamp(0.0, 1.0),
                    "scoreComponents": {"temporal": score.clamp(0.0, 1.0)},
                    "estimatedTokens": std::cmp::max(1, text.chars().count() / 4),
                    "protected": false,
                    "exact": true,
                    "recoverable": false,
                    "resolver": format!("cortex-temporal:inline:{}", fact.fact_id),
                    "text": text,
                })
            }
        })
        .collect();
    let mut omissions: Vec<serde_json::Value> = dropped_by_ceiling
        .iter()
        .map(|hit| {
            serde_json::json!({
                "id": hit.omission_id(),
                "layer": 7,
                "reason": OMISSION_REASON_CEILING_TRUNCATED,
            })
        })
        .collect();
    omissions.extend(
        completeness
            .causes
            .iter()
            .filter(|cause| {
                !matches!(
                    cause.as_str(),
                    "ceiling_truncated"
                        | "temporal_ceiling_truncated"
                        | crate::store::LEXICAL_SCAN_FALLBACK_CAUSE
                )
            })
            .map(|cause| {
                serde_json::json!({
                    "id": format!("memory:temporal-omission:{cause}"),
                    "layer": 7,
                    "reason": cause,
                })
            }),
    );
    stage_elapsed.rank_ms += rank_started.elapsed().as_secs_f64() * 1000.0;

    // F11: reuse the freshness verdict machinery `/freshness` already exposes rather than
    // inventing a second staleness concept. `stable` is the verdict's own truth-in-observation
    // signal (the epoch sandwich did or did not hold across the read); its negation is `stale`.
    // When no repo root is available at this call site at all, that is itself an unverifiable
    // condition — express it honestly as `stale: true`, never fall back to `false`.
    let stale = if lexical_only {
        // Cortex rows are read from its persisted index; Blueprint graph
        // freshness is accounted for by its own source. Do not rebuild that
        // graph as a side effect of foreground memory recall.
        false
    } else {
        repo_root.is_none_or(|root| {
            !crate::freshness::evaluate_repository_freshness(store, root.to_path_buf()).stable
        })
    };

    let indexed_at = iso_now();
    Ok(serde_json::json!({
        "schemaVersion": 1,
        "traceId": new_trace_id(),
        "indexedAt": indexed_at,
        "task": task,
        "mode": "verify",
        "provider": "cortex",
        "freshness": {
            "revision": "membrane-0.1.1-federation",
            "indexedAt": indexed_at,
            "stale": stale,
        },
        "providerCeiling": {
            "maxCandidates": max_candidates,
            "maxEstimatedTokens": 4096,
        },
        "candidates": candidates,
        "omissions": omissions,
        "completeness": completeness,
        "scope": scopes.first().cloned().unwrap_or_default(),
        "_membrane": {
            "stageElapsedMs": {
                "embed": stage_elapsed.embed_ms,
                "recall": stage_elapsed.recall_ms,
                "rank": stage_elapsed.rank_ms,
            }
        },
    }))
}

/// Bounded, word-boundary content preview for a memory candidate's delivered text.
fn memory_preview(content: &str) -> String {
    const CAP: usize = 200;
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= CAP {
        return normalized;
    }
    let truncated: String = normalized.chars().take(CAP).collect();
    let cut = truncated.rfind(' ').unwrap_or(truncated.len());
    format!("{}…", &truncated[..cut])
}

fn new_trace_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("rc-mem-{ts:x}")
}

fn iso_now() -> String {
    crate::time::now_iso()
}

fn gateway_observability(raw: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    if let Some(membrane) = raw.get("_membrane") {
        for field in [
            "providerCounts",
            "providerWarnings",
            "providerElapsedMs",
            "providerStageElapsedMs",
            "serviceGeneration",
            "firstAfterIdle",
            "idleGapMs",
            "stageElapsedMs",
        ] {
            if let Some(value) = membrane.get(field) {
                fields.insert(field.to_string(), value.clone());
            }
        }
    }
    if let Some(graph_state) = raw
        .get("freshness")
        .and_then(|freshness| freshness.get("graphState"))
    {
        fields.insert("graphState".to_string(), graph_state.clone());
    }
    Value::Object(fields)
}

fn mark_stale_blueprint_candidates(candidates: &mut Value) {
    let Some(items) = candidates.as_array_mut() else {
        return;
    };
    for candidate in items {
        let Some(object) = candidate.as_object_mut() else {
            continue;
        };
        if object.get("provider").and_then(Value::as_str) != Some("blueprint") {
            continue;
        }
        let source_kind = object
            .get("sourceKind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(source_kind.as_str(), "graph" | "vector") {
            object.insert(
                "freshnessClass".to_owned(),
                Value::String("stale_snapshot".to_owned()),
            );
        }
    }
}
