//! Capability-routed subagent spawn contracts.

use crate::registry::{AgentDescriptor, AgentRegistry};
use crate::subagent_routing::{
    derive_context_request, resolve_invocation, CapabilityGrant, ContextRequest, Profile, TaskSpec,
};

/// Retry ceiling applied to capability-routed spawns. Retries are bounded
/// elsewhere per run; this exists so the profile is complete at the point the
/// tier ceiling is checked.
const DEFAULT_MAX_RETRIES_CEILING: u32 = 3;

/// A conductor request to spawn a subagent for a bounded task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnRequest {
    pub task_id: String,
    pub payload: String,
    pub required_capabilities: Vec<String>,
    /// Stable-prefix cache key inherited from the conductor run, if any.
    /// See [`SpawnPlan::conductor_stable_cache_key`] for the contract.
    pub conductor_stable_cache_key: Option<String>,
    /// Explicit per-call provider override (13b), independent of the
    /// spawning parent's own provider. `None` = fall back to the routed
    /// agent's registry-assigned `provider_id`.
    pub requested_provider_id: Option<String>,
    /// Explicit per-call model override (13b), paired with
    /// `requested_provider_id`. `None` = fall back to the routed agent's
    /// registry-assigned `model_id`.
    pub requested_model_id: Option<String>,
    /// Escalation tier this call asks to run at, checked against the routed
    /// agent's tier ceiling before the plan is issued. `None` = run at the
    /// agent's own tier, which trivially satisfies its ceiling.
    ///
    /// A request, never a grant: exceeding the ceiling fails the spawn rather
    /// than quietly clamping, because a silently downgraded worker looks like
    /// a working spawn and produces worse results for reasons nobody can see.
    pub requested_tier: Option<u8>,
    /// True when this call cannot mutate the workspace. Drives whether the
    /// resulting [`SpawnPlan::context_request`] includes project
    /// instruction files (13d) — unless overridden below.
    pub read_only: bool,
    /// Explicit per-call escape hatch for `SpawnPlan::context_request`:
    /// when set, wins outright over the `read_only`-derived default in
    /// either direction (e.g. a read-only code-review subagent that still
    /// needs CLAUDE.md style rules). `None` = use the `read_only` default.
    pub include_project_instructions_override: Option<bool>,
}

/// Audit artifact written at spawn time so a transcript can explain why a
/// specific typed agent/model ran the task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnTranscriptArtifact {
    pub task_id: String,
    pub agent_descriptor_name: String,
    pub model_id: String,
    pub required_capabilities: Vec<String>,
    pub matched_capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnPlan {
    pub task_id: String,
    pub agent: String,
    pub payload: String,
    pub model_id: String,
    /// Provider actually serving `model_id` for this call: the request's
    /// `requested_provider_id` override when present, else the routed
    /// agent's registry-assigned `provider_id` (13b).
    pub provider_id: String,
    pub agent_version: Option<String>,
    pub transcript_artifact: SpawnTranscriptArtifact,
    /// Stable-prefix cache key inherited from the conductor run, if any.
    ///
    /// When the conductor spawns multiple sibling workers, every
    /// `SpawnPlan` carries the same `Some(key)` value so each worker
    /// cache-reads against the parent's prefix slot rather than each
    /// independently recomputing (cache-break) on its own. `None` for
    /// single-worker / capability-routed spawns outside a conductor run.
    pub conductor_stable_cache_key: Option<String>,
    /// What this call starts from, derived from `read_only` (13d):
    /// read-only spawns omit project instruction files.
    pub context_request: ContextRequest,
}

pub fn spawn_with_capabilities(
    registry: &AgentRegistry,
    request: SpawnRequest,
) -> Option<SpawnPlan> {
    let required = normalized_tags(&request.required_capabilities);
    let selected = registry
        .all()
        .iter()
        .filter_map(|agent| {
            let matched = matched_capabilities(agent, &required);
            if matched.is_empty() {
                None
            } else {
                Some((agent, matched))
            }
        })
        .max_by_key(|(agent, matched)| (matched.len(), agent.escalation_tier));

    let (agent, matched) = selected?;

    // 13c: the routed agent's registry tier IS its profile ceiling, and a
    // per-call tier request is checked against it here, at the only place a
    // spawn plan is issued.
    //
    // This check previously existed only as a unit-tested function nothing
    // called, which left the ceiling unenforced everywhere it mattered: a
    // request could name any tier and be honoured. Failing closed (returning
    // `None`) matches how every other rejection in this function behaves.
    let profile = Profile::new(agent.escalation_tier, DEFAULT_MAX_RETRIES_CEILING);
    let task = TaskSpec {
        task_id: request.task_id.clone(),
        payload: request.payload.clone(),
        requested_provider_id: request.requested_provider_id.clone(),
        requested_model_id: request.requested_model_id.clone(),
        requested_tier: request.requested_tier.unwrap_or(agent.escalation_tier),
    };
    let resolved = resolve_invocation(&task, &profile).ok()?;

    // 13b: an explicit per-call route wins over the registry default,
    // independent of whatever the spawning parent is running — a
    // parameter on this request, never a process-wide environment value.
    // Taken from the ceiling-checked resolution above rather than re-read from
    // the raw request, so the check cannot be bypassed by a later edit that
    // forgets it ran.
    let resolved_model_id = resolved.model_id.unwrap_or_else(|| agent.model_id.clone());
    let resolved_provider_id = resolved
        .provider_id
        .unwrap_or_else(|| agent.provider_id.clone());

    // 13d: derive what this call starts from purely from what it may
    // touch — read-only spawns omit project instruction files.
    let grant = CapabilityGrant {
        allowed_tools: agent.allowed_tools.clone(),
        read_only: request.read_only,
    };
    let context_request = derive_context_request(
        &grant,
        request.conductor_stable_cache_key.clone(),
        request.include_project_instructions_override,
    );

    Some(SpawnPlan {
        task_id: request.task_id.clone(),
        agent: agent.name.clone(),
        payload: request.payload,
        model_id: resolved_model_id.clone(),
        provider_id: resolved_provider_id,
        agent_version: agent.version.clone(),
        transcript_artifact: SpawnTranscriptArtifact {
            task_id: request.task_id,
            agent_descriptor_name: agent.name.clone(),
            model_id: resolved_model_id,
            required_capabilities: request.required_capabilities,
            matched_capabilities: matched,
        },
        conductor_stable_cache_key: request.conductor_stable_cache_key,
        context_request,
    })
}

fn normalized_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .map(|tag| tag.trim().to_ascii_lowercase())
        .filter(|tag| !tag.is_empty())
        .collect()
}

fn matched_capabilities(agent: &AgentDescriptor, required: &[String]) -> Vec<String> {
    agent
        .capabilities
        .iter()
        .filter_map(|capability| {
            let normalized = capability.trim().to_ascii_lowercase();
            if required.iter().any(|required| required == &normalized) {
                Some(capability.clone())
            } else {
                None
            }
        })
        .collect()
}
