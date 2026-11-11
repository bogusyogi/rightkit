//! Spawn-seam wiring for the capability/cost router (Lane R1a-L3).
//!
//! Wires [`rightkit_llm::agent_router`] into the per-sub-agent spawn pick at
//! `api_state_impl_04.rs` (the place where `descriptor.model_id` is consumed
//! to select the live model route). Design contract:
//! `docs/canon/daemon-master-canon-atoms.md` §D2 seam 1.
//!
//! Behavior matrix (SPEC 2):
//!  - flag off                              → today's behavior byte-identical
//!  - flag on + role has `agents` override → override wins (no routing)
//!  - flag on + no override + no session pin → routed (triage)
//!  - flag on + session pin set            → session pin is a hard wall
//!  - license feature `routing` absent     → no routing (feature gate)
//!
//! Sibling-stability invariant (SPEC 4): the routed model is picked ONCE per
//! conductor run per role/task-class and held stable across sibling workers
//! doing the same class of work. The run id prevents cross-run reuse while
//! the task class prevents an unrelated sibling from inheriting a stale pick.
//!
//! Escalate path is OUT OF SCOPE here: `orchestration/escalation.rs` never
//! reads `descriptor.model_id`, so it can never reach this seam and is
//! always-capable by construction (SPEC 5 invariant).

#![allow(clippy::doc_overindented_list_items)]
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use rightkit_agent_engine_protocol::{ModelRouteDecisionPayload, PriceSnapshot};
use rightkit_agent_orchestration::coding::registry::AgentDescriptor;
use rightkit_llm::agent_router::{
    BudgetCeiling, PinContext, RouteDecision, RoutingPolicy, TaskSignal, TaskSignalBuilder,
    TurnType,
};

use crate::api::DaemonState;

/// In-process cache keyed by `(conductor_run_id, role|task_class)`.
///
/// Sibling workers spawned from the same conductor carry the same
/// `parent_session_id` (the conductor-run id). The first spawn for a given
/// `(run_id, role|task_class)` pays the routing cost and writes the decision here;
/// every subsequent sibling worker reads the cached decision verbatim — this
/// is the structural guarantee that prefix-cache slots shared by sibling
/// workers do not silently shift mid-run (ADR §D2 seam-1 invariant +
/// `SpawnPlan::conductor_stable_cache_key`).
pub type RoutingCache = Arc<Mutex<HashMap<(String, String), String>>>;

const MAX_ROUTING_CACHE_ENTRIES: usize = 256;

#[derive(Debug, Default)]
struct RuntimeTaskSignals {
    scoped_file_count: usize,
    tool_call_density: u32,
    failure_streak: u32,
    budget_remaining_usd: Option<f64>,
    current_context_tokens: u64,
    task_class: String,
}

/// S:F6: live task signals resolved directly from a spawn's OWN in-progress
/// `tasks`/`transcript`/`lineage_budget` locals, for spawn callers that do
/// not have a registered `DaemonState.sessions` entry to key a lookup on —
/// most notably a root spawn (no `parent_session_id`), whose new session id
/// is guaranteed absent from `DaemonState.sessions` at spawn time (the
/// caller returns early on an existing id, so the id being routed here has
/// never been inserted yet). A state-registry lookup keyed on that id would
/// therefore always miss and silently fall back to all-zero signals, even
/// when this is a resume/re-entry whose `tasks`/`transcript` were just
/// seeded from storage. Root-spawn callers should pass those locals here
/// instead of hardcoding zeros.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LiveSessionContextSignals {
    pub(crate) tool_call_density: u32,
    pub(crate) failure_streak: u32,
    pub(crate) budget_remaining_usd: Option<f64>,
    pub(crate) current_context_tokens: u64,
    pub(crate) scoped_file_count: usize,
}

pub(crate) fn runtime_task_signals_from_parts(
    tasks: &[rightkit_agent_engine_protocol::TaskNode],
    transcript: &[rightkit_agent_engine_protocol::TranscriptMessage],
    lineage_budget: &rightkit_agent_engine_core::LineageBudget,
    role: &str,
) -> LiveSessionContextSignals {
    let signals = runtime_task_signals_from_slices(tasks, transcript, lineage_budget, role);
    LiveSessionContextSignals {
        tool_call_density: signals.tool_call_density,
        failure_streak: signals.failure_streak,
        budget_remaining_usd: signals.budget_remaining_usd,
        current_context_tokens: signals.current_context_tokens,
        scoped_file_count: signals.scoped_file_count,
    }
}

/// Inputs the seam needs at per-sub-agent spawn time. Kept narrow so
/// `apply_capability_routing` stays a small surface.
#[derive(Debug, Clone)]
pub struct RoutingInputs<'a> {
    pub role: Option<&'a str>,
    pub workspace_config: &'a rightkit_agent_config::HostConfig,
    pub goal_text: &'a str,
    pub scoped_files: &'a [String],
    pub tool_call_density: u32,
    pub failure_streak: u32,
    pub budget_remaining_usd: f64,
    pub current_context_tokens: u64,
    /// Per-session pin (hard wall). `None` = no session-level pin.
    pub session_pin: Option<&'a str>,
    /// The conductor-run id for sibling-stability cache lookups. Use
    /// `parent_session_id` at the call site (it IS the conductor-run id);
    /// `None` for top-level / single-worker spawns, which bypass the cache.
    pub conductor_run_id: Option<&'a str>,
}

/// Outcome returned by the seam — caller inspects to decide what to log.
///
/// `model_id` is the routed model when a routing decision fired; `None` means
/// the seam either bailed (flag off / license absent / pin / override) or
/// `triage()` itself yielded no candidate (roster empty or context/budget
/// exhausted every row). Callers should keep the original descriptor's
/// `model_id` in the latter case — the seam is intentionally non-fatal.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingOutcome {
    pub model_id: Option<String>,
    pub source: RoutingSource,
    pub reason: String,
    /// Populated only for a fresh [`RoutingSource::TriageDecision`] — the
    /// caller publishes this as `ProtocolEvent::ModelRouteDecision` at the
    /// spawn seam (ADR §D5 observability). `None` on every bypass path and
    /// on a sibling-cache hit (the cache hit reuses a decision that was
    /// already emitted once, by the sibling that paid the triage cost).
    pub protocol_event: Option<ModelRouteDecisionPayload>,
}

/// Where the routed `model_id` came from — exposed so the spawn-site log
/// (the `// R1a-L4 wires ProtocolEvent here` marker) can name the level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingSource {
    /// Flag off / license absent / per-agent override / session pin — seam
    /// bailed; the caller uses the descriptor's `model_id` unchanged.
    Bypassed,
    /// Cached sibling decision read from the per-run cache.
    ConductorStableCache,
    /// Fresh triage decision written to the cache.
    TriageDecision,
}

/// Apply the capability router at the spawn seam. Returns a [`RoutingOutcome`]
/// describing what happened; `descriptor.model_id` is updated in place when a
/// routing decision fires.
///
/// `TaskSignal` population choices (R1a-L3 SPEC 2 — populated vs defaulted):
///  - `turn_type`            → populated (`FreshReasoning` at spawn is the
///                              safest; the seam runs before any tool work)
///  - `tool_call_density`    → populated from caller
///  - `failure_streak`       → populated from caller (task graph, if the
///                              caller wires it; default 0 here)
///  - `remaining_budget_usd` → populated from caller
///  - `touches_sensitive`    → derived from prompt text via the builder's
///                              `from_prompt_text` helper (auth/money/...
///                              keywords per ADR §D1)
///  - `scoped_file_count`    → derived from prompt text path references
///  - `prompt_shape`         → derived from prompt text
///  - `current_context_tokens` → populated from caller
pub fn apply_capability_routing(
    state: &DaemonState,
    descriptor: &mut AgentDescriptor,
    inputs: &RoutingInputs<'_>,
) -> RoutingOutcome {
    // SPEC 1: flag off → today's behavior byte-identical. Bail FIRST so the
    // descriptor path is touched only when routing actually engages.
    let routing_cfg = &inputs.workspace_config.orchestration.routing;
    if !routing_cfg.capability_router() {
        return RoutingOutcome {
            model_id: None,
            source: RoutingSource::Bypassed,
            reason: "capability_router flag off — descriptor.model_id unchanged".to_string(),
            protocol_event: None,
        };
    }

    // SPEC 3: Pro-only feature gate. A bare daemon (no signed token) must
    // never route even if the flag is on.
    if !state.has_license_feature("routing") {
        return RoutingOutcome {
            model_id: None,
            source: RoutingSource::Bypassed,
            reason: "license feature `routing` absent — seam gated, descriptor unchanged"
                .to_string(),
            protocol_event: None,
        };
    }

    // SPEC 2: per-agent override wins (OrchestrationConfig.agents override
    // IS an explicit user pin per ADR §D5 — the router must never override it).
    // Detect by looking up the override entry directly; `model_id` empty
    // means no override for this role.
    if let Some(role) = inputs.role {
        if let Some(override_cfg) = inputs.workspace_config.orchestration.agents.get(role) {
            if !override_cfg.model_id.trim().is_empty() {
                return RoutingOutcome {
                    model_id: None,
                    source: RoutingSource::Bypassed,
                    reason: format!(
                        "role `{role}` has a config override pin — router does not run"
                    ),
                    protocol_event: None,
                };
            }
        }
    }

    // SPEC 4: sibling-stability cache hit short-circuits BEFORE we even
    // build a signal. Sibling workers of one conductor run must see the
    // exact same routed model — read from cache if present, write to
    // cache on a fresh triage call. A newly stalled run is the one explicit
    // exception: it must invalidate a prior cheap pick so the bounded pivot
    // can reach triage (FM-050; FM-061's existing hysteresis remains intact).
    let cache = routing_cache(state);
    let live_for_cache = inputs
        .conductor_run_id
        .map(|run_id| runtime_task_signals(state, run_id, inputs.role.unwrap_or("")))
        .unwrap_or_default();
    let observed_failure_streak = inputs.failure_streak.max(live_for_cache.failure_streak);
    let stall_state = rightkit_llm::agent_router::stall_state(observed_failure_streak);
    let stalled = matches!(stall_state, rightkit_llm::agent_router::StallState::Stalled);
    if let Some(run_id) = inputs.conductor_run_id {
        if let Some(role) = inputs.role {
            let task_class = live_for_cache.task_class.clone();
            let key = (run_id.to_string(), cache_scope(role, &task_class));
            if !stalled {
                if let Ok(map) = cache.lock() {
                    if let Some(cached) = map.get(&key).filter(|model| {
                        rightkit_llm::agent_providers::model_metadata::capability_of(model)
                            .is_some()
                    }) {
                        let model_id = cached.clone();
                        descriptor.model_id = model_id.clone();
                        return RoutingOutcome {
                            model_id: Some(model_id),
                            source: RoutingSource::ConductorStableCache,
                            reason: format!(
                                "sibling-stability cache hit for run `{run_id}` role `{role}`"
                            ),
                            protocol_event: None,
                        };
                    }
                }
            }
        }
    }

    // Build the TaskSignal from prompt text + caller-supplied runtime state.
    let prompt_text = inputs.goal_text;
    let live = inputs
        .conductor_run_id
        .map(|_| live_for_cache)
        .unwrap_or_default();
    let budget_remaining = live
        .budget_remaining_usd
        .unwrap_or(inputs.budget_remaining_usd);
    let mut builder = TaskSignalBuilder::new()
        .turn_type(TurnType::FreshReasoning)
        .tool_call_density(inputs.tool_call_density.max(live.tool_call_density))
        .failure_streak(inputs.failure_streak.max(live.failure_streak))
        .remaining_budget_usd(budget_remaining)
        .current_context_tokens(
            inputs
                .current_context_tokens
                .max(live.current_context_tokens),
        )
        .from_prompt_text(prompt_text);
    let scoped_file_count = inputs.scoped_files.len().max(live.scoped_file_count);
    if scoped_file_count > 0 {
        // Caller-provided scoped files take priority over the builder's
        // text-derived count — the caller may have richer file-scope
        // context (e.g. resolved from a workspace-wide scan) than the
        // raw prompt text exposes.
        builder = builder.scoped_file_count(scoped_file_count);
    }
    let signal = builder.build();

    // SPEC 5: pin precedence (ADR §D5). per_turn_pin is None at the spawn
    // seam (a sub-agent's spawn does not carry a per-turn user pin); the
    // configured_default is the descriptor's own model_id (the only
    // fallback if triage() returns nothing).
    let pins = PinContext {
        per_turn_pin: None,
        per_agent_override: None,
        session_pin: inputs.session_pin.map(str::to_string),
        configured_default: Some(descriptor.model_id.clone()),
    };

    // Load the named policy preset + the bundled model roster.
    let policy: RoutingPolicy = RoutingPolicy::preset_by_name(&routing_cfg.aggressiveness);
    let roster = rightkit_llm::agent_providers::model_metadata::table();
    let budget = BudgetCeiling::default();

    let decision: Option<RouteDecision> =
        match rightkit_llm::agent_router::route_checked(&signal, roster, &policy, &budget, &pins) {
            Ok(decision) => decision,
            Err(error) => {
                return RoutingOutcome {
                    model_id: None,
                    source: RoutingSource::Bypassed,
                    reason: format!("routing rejected before execution: {error}"),
                    protocol_event: None,
                };
            }
        };

    let Some(decision) = decision else {
        return RoutingOutcome {
            model_id: None,
            source: RoutingSource::Bypassed,
            reason: "triage returned no candidate — descriptor.model_id unchanged".to_string(),
            protocol_event: None,
        };
    };

    // Apply the routed model to the descriptor. The decision's `model_id`
    // may equal `descriptor.model_id` (e.g. when the policy routes UP to
    // the default tier); that is fine — the seam is idempotent.
    let previous = descriptor.model_id.clone();
    descriptor.model_id = decision.model_id.clone();

    // Cache the decision for sibling workers of the same conductor run.
    if let (Some(run_id), Some(role)) = (inputs.conductor_run_id, inputs.role) {
        if let Ok(mut map) = cache.lock() {
            if map.len() >= MAX_ROUTING_CACHE_ENTRIES {
                if let Some(oldest_key) = map.keys().min().cloned() {
                    map.remove(&oldest_key);
                }
            }
            map.insert(
                (run_id.to_string(), cache_scope(role, &live.task_class)),
                decision.model_id.clone(),
            );
        }
    }

    let alternatives = decision
        .candidates
        .iter()
        .map(|candidate| {
            format!(
                "{}:value={:.3}:cost=${:.6}:qualified={}",
                candidate.model_id,
                candidate.expected_value,
                candidate.expected_cost_usd,
                candidate.qualified
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let verifier = decision.verifier.as_ref().map_or_else(
        || "verifier=unassigned".to_string(),
        |receipt| {
            format!(
                "verifier={}:independent={}:degradation={}",
                receipt.model_id.as_deref().unwrap_or("none"),
                receipt.independent,
                receipt.degradation_reason.as_deref().unwrap_or("none")
            )
        },
    );
    let route_reason = format!(
        "routed {} -> {} (tier={:?}, confidence={:.2}, schema_version={}, fingerprint={}); {}; alternatives=[{}]; {}",
        previous,
        decision.model_id,
        decision.tier,
        decision.confidence,
        decision.schema_version,
        decision.route_fingerprint,
        decision.reason,
        alternatives,
        verifier,
    );

    let payload = build_model_route_decision_payload(
        inputs.role.unwrap_or("<none>"),
        &signal,
        &routing_cfg.aggressiveness,
        &decision,
        roster,
        &route_reason,
    );

    let outcome = RoutingOutcome {
        model_id: Some(decision.model_id.clone()),
        source: RoutingSource::TriageDecision,
        reason: route_reason,
        protocol_event: Some(payload),
    };

    // SPEC 5: emit the decision through a plain tracing/log line for now.
    // R1a-L4 wires `ProtocolEvent::ModelRouteDecision` here.
    tracing::info!(
        target: "coderight.router_seam",
        role = inputs.role.unwrap_or("<none>"),
        previous_model_id = %previous,
        routed_model_id = %decision.model_id,
        tier = ?decision.tier,
        confidence = decision.confidence,
        schema_version = decision.schema_version,
        enforcement = ?decision.enforcement,
        reason = %decision.reason,
        // R1a-L4 wires ProtocolEvent here: RoutingOutcome::protocol_event
        // carries the ModelRouteDecisionPayload; the caller at the spawn
        // site (api_state_impl_04.rs) publishes it via the session conduit.
        "capability router: routed sub-agent spawn decision"
    );

    outcome
}

/// Build the wire payload for `ProtocolEvent::ModelRouteDecision` from a
/// fresh triage decision. Pure function so it's independently testable from
/// the seam's flag/pin/cache plumbing.
fn build_model_route_decision_payload(
    role: &str,
    signal: &TaskSignal,
    aggressiveness: &str,
    decision: &RouteDecision,
    roster: &[rightkit_llm::agent_providers::model_metadata::ModelCapability],
    reason: &str,
) -> ModelRouteDecisionPayload {
    let signals_digest = digest_signal(role, signal, aggressiveness);
    let price_snapshot = price_snapshot_for(&decision.model_id, roster);
    let est_cost_usd = estimate_cost_usd(&price_snapshot, signal);
    ModelRouteDecisionPayload {
        agent_role: role.to_string(),
        model_id: decision.model_id.clone(),
        tier: tier_label(decision.tier),
        reason: reason.to_string(),
        signals_digest,
        price_snapshot,
        est_cost_usd,
        schema_version: decision.schema_version,
    }
}

fn tier_label(tier: rightkit_llm::agent_providers::model_metadata::ModelTier) -> String {
    use rightkit_llm::agent_providers::model_metadata::ModelTier;
    match tier {
        ModelTier::Cheap => "cheap".to_string(),
        ModelTier::Capable => "capable".to_string(),
    }
}

/// Short, non-reversible digest of the signal that produced a routing
/// decision. Deliberately NOT the raw signal (which carries prompt-derived
/// shape data) — enough for a hover tooltip / support trail to confirm two
/// chips came from the same inputs, not a full replay artifact.
fn digest_signal(role: &str, signal: &TaskSignal, aggressiveness: &str) -> String {
    let mut hasher = DefaultHasher::new();
    role.hash(&mut hasher);
    aggressiveness.hash(&mut hasher);
    // TaskSignal implements Serialize but not Hash; hash its stable JSON
    // representation instead of hand-rolling a Hash impl across every field.
    if let Ok(json) = serde_json::to_string(signal) {
        json.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Pull the pricing row for the routed model out of the live roster and stamp
/// a `PriceSnapshot`. R1a pricing is the bundled static table (ADR §D4) — so
/// `source` is always `"bundled"` here; R1c adds live-refreshed sources.
fn price_snapshot_for(
    model_id: &str,
    roster: &[rightkit_llm::agent_providers::model_metadata::ModelCapability],
) -> PriceSnapshot {
    let row = roster.iter().find(|m| m.slug == model_id);
    let (prompt_per_m, completion_per_m) = row
        .map(|r| (r.prompt_per_m, r.completion_per_m))
        .unwrap_or((0.0, 0.0));
    let mut hasher = DefaultHasher::new();
    model_id.hash(&mut hasher);
    prompt_per_m.to_bits().hash(&mut hasher);
    completion_per_m.to_bits().hash(&mut hasher);
    PriceSnapshot {
        prompt_per_m,
        completion_per_m,
        source: format!(
            "{:?}@{}",
            rightkit_llm::agent_providers::model_metadata::catalog_status().source,
            rightkit_llm::agent_providers::model_metadata::catalog_status().version
        )
        .to_ascii_lowercase(),
        fetched_at: rightkit_agent_engine_protocol::now_iso(),
        hash: format!("{:016x}", hasher.finish()),
    }
}

/// Pre-call cost estimate from the signal's context footprint and the routed
/// model's per-1M pricing. This is an ESTIMATE, not an actual — the UI chip's
/// "est." label is mandatory precisely because this number is computed
/// before the call, not reconciled against real usage (that happens via
/// `UsageUpdated`).
fn estimate_cost_usd(snapshot: &PriceSnapshot, signal: &TaskSignal) -> f64 {
    let prompt_tokens = signal.current_context_tokens as f64;
    // No output-token estimate exists at spawn time; use a conservative
    // completion budget proportional to a typical sub-agent turn rather than
    // inventing a number with no basis — small and clearly a floor, not a cap.
    let assumed_completion_tokens = 2_000.0_f64;
    let prompt_cost = (prompt_tokens / 1_000_000.0) * snapshot.prompt_per_m;
    let completion_cost = (assumed_completion_tokens / 1_000_000.0) * snapshot.completion_per_m;
    prompt_cost + completion_cost
}

/// Accessor for the per-process routing cache stored on [`DaemonState`].
/// Created on first access and capped at [`MAX_ROUTING_CACHE_ENTRIES`]. The
/// conductor completion owner calls [`evict_routing_cache_for_run`] so normal
/// entries end with their run rather than waiting for bounded eviction.
pub fn routing_cache(state: &DaemonState) -> RoutingCache {
    // Stored as a single Arc<Mutex<...>> field on DaemonState; if it's
    // somehow unset (defensive), spin up a fresh one so the seam stays
    // non-fatal.
    state.routing_cache.clone()
}

/// Lifecycle hook for the conductor owner. Route decisions are stable only
/// for the run that produced them and must not survive run completion.
pub fn evict_routing_cache_for_run(state: &DaemonState, run_id: &str) -> usize {
    routing_cache(state)
        .lock()
        .map(|mut cache| {
            let before = cache.len();
            cache.retain(|(cached_run, _), _| cached_run != run_id);
            before.saturating_sub(cache.len())
        })
        .unwrap_or_default()
}

fn cache_scope(role: &str, task_class: &str) -> String {
    format!(
        "{role}|{}",
        if task_class.is_empty() {
            "unclassified"
        } else {
            task_class
        }
    )
}

fn runtime_task_signals(state: &DaemonState, run_id: &str, role: &str) -> RuntimeTaskSignals {
    let parent = state
        .sessions
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(run_id).cloned());
    let Some(parent) = parent else {
        return RuntimeTaskSignals::default();
    };

    let tasks = parent
        .tasks
        .lock()
        .map(|tasks| tasks.clone())
        .unwrap_or_default();
    let transcript = parent
        .transcript
        .lock()
        .map(|transcript| transcript.clone())
        .unwrap_or_default();
    runtime_task_signals_from_slices(&tasks, &transcript, &parent.lineage_budget, role)
}

/// Shared core: derive [`RuntimeTaskSignals`] from already-fetched
/// `tasks`/`transcript`/`lineage_budget`, independent of whether they came
/// from a `DaemonState.sessions` lookup (conductor/subagent path) or from a
/// spawn's own locals (root-spawn path, S:F6 — see
/// [`runtime_task_signals_from_parts`]).
fn runtime_task_signals_from_slices(
    tasks: &[rightkit_agent_engine_protocol::TaskNode],
    transcript: &[rightkit_agent_engine_protocol::TranscriptMessage],
    lineage_budget: &rightkit_agent_engine_core::LineageBudget,
    role: &str,
) -> RuntimeTaskSignals {
    let relevant = tasks
        .iter()
        .filter(|task| task.assigned_role.is_empty() || task.assigned_role == role)
        .collect::<Vec<_>>();
    let scoped_file_count = relevant
        .iter()
        .map(|task| {
            rightkit_llm::agent_router::prompt_shape::path_reference_count(&format!(
                "{} {}",
                task.title, task.done_when
            ))
        })
        .sum();
    let failure_streak = relevant
        .iter()
        .rev()
        .take_while(|task| task.state == rightkit_agent_engine_protocol::TaskState::Failed)
        .count()
        .min(3) as u32;
    let task_class = relevant
        .iter()
        .find(|task| !task.category.trim().is_empty())
        .map(|task| task.category.trim().to_ascii_lowercase())
        .unwrap_or_default();

    let recent_tools = transcript
        .iter()
        .rev()
        .take(4)
        .map(|message| match message {
            rightkit_agent_engine_protocol::TranscriptMessage::Assistant { tool_calls, .. } => {
                tool_calls.len() as u32
            }
            rightkit_agent_engine_protocol::TranscriptMessage::Tool { .. } => 1,
            _ => 0,
        })
        .sum();
    let bytes = transcript
        .iter()
        .map(|message| match message {
            rightkit_agent_engine_protocol::TranscriptMessage::System { content, .. }
            | rightkit_agent_engine_protocol::TranscriptMessage::User { content, .. }
            | rightkit_agent_engine_protocol::TranscriptMessage::Assistant { content, .. } => {
                content.len()
            }
            rightkit_agent_engine_protocol::TranscriptMessage::Tool { summary, .. }
            | rightkit_agent_engine_protocol::TranscriptMessage::Compact { summary, .. }
            | rightkit_agent_engine_protocol::TranscriptMessage::Fold { summary, .. } => {
                summary.len()
            }
        })
        .sum::<usize>();
    let (tool_call_density, current_context_tokens) = (recent_tools, bytes.div_ceil(4) as u64);
    let budget_remaining_usd = lineage_budget
        .remaining_microusd()
        .map(|value| value as f64 / 1_000_000.0);

    RuntimeTaskSignals {
        scoped_file_count,
        tool_call_density,
        failure_streak,
        budget_remaining_usd,
        current_context_tokens,
        task_class,
    }
}
