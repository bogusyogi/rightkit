//! Capability-routed subagent spawn contracts.

use crate::coding::registry::{AgentDescriptor, AgentRegistry};
use crate::coding::subagent_routing::{
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

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, model: &str, caps: &[&str], tier: u8) -> AgentDescriptor {
        let mut agent = AgentDescriptor::new(name, name, model);
        agent.capabilities = caps.iter().map(|cap| cap.to_string()).collect();
        agent.escalation_tier = tier;
        agent
    }

    #[test]
    fn spawn_selects_descriptor_by_required_capability_tags_and_logs_model() {
        let mut registry = AgentRegistry::new();
        registry.register(agent("generalist", "cheap/general", &["coding"], 1));
        registry.register(agent(
            "security-specialist",
            "frontier/security",
            &["security", "threat-modeling"],
            2,
        ));

        let plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-1".into(),
                payload: "audit auth boundary".into(),
                required_capabilities: vec!["security".into(), "threat-modeling".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        )
        .expect("security agent should match required capabilities");

        assert_eq!(plan.agent, "security-specialist");
        assert_eq!(plan.model_id, "frontier/security");
        assert_eq!(
            plan.transcript_artifact.agent_descriptor_name,
            "security-specialist"
        );
        assert_eq!(plan.transcript_artifact.model_id, "frontier/security");
        assert_eq!(
            plan.transcript_artifact.matched_capabilities,
            vec!["security", "threat-modeling"]
        );
    }

    #[test]
    fn spawn_prefers_agent_with_more_capability_matches() {
        let mut registry = AgentRegistry::new();
        registry.register(agent("coder", "fast/code", &["coding"], 1));
        registry.register(agent(
            "backend-architect",
            "deep/backend",
            &["coding", "api-design", "schema"],
            2,
        ));

        let plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-2".into(),
                payload: "design api".into(),
                required_capabilities: vec!["coding".into(), "schema".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        )
        .unwrap();

        assert_eq!(plan.agent, "backend-architect");
        assert_eq!(plan.model_id, "deep/backend");
        assert_eq!(
            plan.transcript_artifact.matched_capabilities,
            vec!["coding", "schema"]
        );
    }

    #[test]
    fn spawn_returns_none_when_no_descriptor_has_required_capabilities() {
        let mut registry = AgentRegistry::new();
        registry.register(agent("coder", "fast/code", &["coding"], 1));

        let plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-3".into(),
                payload: "render hero".into(),
                required_capabilities: vec!["visual-qa".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        );

        assert!(plan.is_none());
    }

    #[test]
    fn spawn_propagates_conductor_stable_cache_key_verbatim() {
        // The conductor computes the shared stable cache key ONCE per run
        // and threads it into every sibling `SpawnRequest`. The spawn
        // seam must carry that key verbatim onto `SpawnPlan` — no
        // recompute, no per-agent re-derivation. This is the structural
        // guarantee behind cross-worker prefix-cache sharing.
        let mut registry = AgentRegistry::new();
        registry.register(agent("coder", "fast/code", &["coding"], 1));

        let shared_key = crate::coding::conductor_cache_key::conductor_stable_cache_key(
            "rightkit-agent-conductor-v1",
            "2026-06-30",
        );

        let plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-shared".into(),
                payload: "implement feature X".into(),
                required_capabilities: vec!["coding".into()],
                conductor_stable_cache_key: Some(shared_key.clone()),
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        )
        .expect("spawn should succeed with a registered capability");

        assert_eq!(
            plan.conductor_stable_cache_key,
            Some(shared_key.clone()),
            "SpawnPlan must carry the conductor's shared key verbatim (no recompute)"
        );

        // Sanity: a different SpawnRequest on the same conductor still
        // produces the SAME plan key — siblings share.
        let plan_b = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-shared-b".into(),
                payload: "implement feature Y".into(),
                required_capabilities: vec!["coding".into()],
                conductor_stable_cache_key: Some(shared_key.clone()),
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        )
        .unwrap();
        assert_eq!(
            plan.conductor_stable_cache_key,
            plan_b.conductor_stable_cache_key
        );
    }

    #[test]
    fn spawn_honours_explicit_provider_model_override_over_registry_default() {
        // End-to-end through spawn_with_capabilities (not resolve_invocation
        // in isolation): the subagent's plan must land on a different
        // provider/model than the routed agent's own registry default,
        // proving 13b as a parameter on the request rather than a
        // process-wide environment value.
        let mut registry = AgentRegistry::new();
        registry.register(agent("coder", "openrouter/fast-code", &["coding"], 1));

        let default_plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-default".into(),
                payload: "edit a file".into(),
                required_capabilities: vec!["coding".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        )
        .unwrap();
        assert_eq!(default_plan.provider_id, "openrouter");
        assert_eq!(default_plan.model_id, "openrouter/fast-code");

        let override_plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-override".into(),
                payload: "edit a file".into(),
                required_capabilities: vec!["coding".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: Some("anthropic".into()),
                requested_model_id: Some("claude-haiku".into()),
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        )
        .unwrap();

        assert_eq!(override_plan.provider_id, "anthropic");
        assert_eq!(override_plan.model_id, "claude-haiku");
        assert_eq!(override_plan.transcript_artifact.model_id, "claude-haiku");
        assert_ne!(override_plan.provider_id, default_plan.provider_id);
        assert_ne!(override_plan.model_id, default_plan.model_id);
        // Same agent selected both times — the override changed the route,
        // not which descriptor matched the capability tags.
        assert_eq!(override_plan.agent, default_plan.agent);
    }

    #[test]
    fn spawn_read_only_request_omits_project_instructions_in_context_request() {
        let mut registry = AgentRegistry::new();
        registry.register(agent("explorer", "cheap/explore", &["search"], 1));

        let read_only_plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-ro".into(),
                payload: "find references".into(),
                required_capabilities: vec!["search".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: true,
                include_project_instructions_override: None,
            },
        )
        .unwrap();
        assert!(!read_only_plan.context_request.include_project_instructions);

        let read_write_plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-rw".into(),
                payload: "apply a fix".into(),
                required_capabilities: vec!["search".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        )
        .unwrap();
        assert!(read_write_plan.context_request.include_project_instructions);
    }

    #[test]
    fn spawn_read_only_request_with_explicit_override_still_receives_project_instructions() {
        // 13d escape hatch, end-to-end through spawn_with_capabilities:
        // read-only does not imply "doesn't need project conventions" — a
        // read-only code-review subagent that sets the override must still
        // get project instruction files, overriding the read-only default.
        let mut registry = AgentRegistry::new();
        registry.register(agent("code-reviewer", "cheap/review", &["review"], 1));

        let plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-ro-override".into(),
                payload: "review this diff for style".into(),
                required_capabilities: vec!["review".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: true,
                include_project_instructions_override: Some(true),
            },
        )
        .unwrap();

        assert!(
            plan.context_request.include_project_instructions,
            "explicit override must win over the read-only default of false"
        );
    }

    #[test]
    fn spawn_with_none_conductor_key_returns_none_field() {
        // Single-worker / direct path: `conductor_stable_cache_key` is
        // None on the request → None on the plan. The worker must derive
        // its own stable key via `SessionEngine` rather than inheriting
        // a conductor-run key.
        let mut registry = AgentRegistry::new();
        registry.register(agent("coder", "fast/code", &["coding"], 1));

        let plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-direct".into(),
                payload: "edit a single file".into(),
                required_capabilities: vec!["coding".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        )
        .unwrap();

        assert_eq!(
            plan.conductor_stable_cache_key, None,
            "single-worker path must NOT carry an inherited conductor key"
        );
    }
    #[test]
    fn spawn_refuses_a_tier_above_the_routed_agents_ceiling() {
        // End-to-end through spawn_with_capabilities, not resolve_invocation in
        // isolation. The check existed as a unit-tested function nothing
        // called, so a request could name any tier and be honoured; this test
        // fails if the enforcement is ever lifted back out of the spawn path.
        let mut registry = AgentRegistry::new();
        registry.register(agent("coder", "openrouter/fast-code", &["coding"], 1));

        let over_ceiling = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-escalated".into(),
                payload: "edit a file".into(),
                required_capabilities: vec!["coding".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: Some(3),
                read_only: false,
                include_project_instructions_override: None,
            },
        );
        assert!(
            over_ceiling.is_none(),
            "a tier-3 request against a tier-1 agent must fail closed, not clamp"
        );

        // At the ceiling is allowed: this is a ceiling, not a ban.
        let at_ceiling = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-at-ceiling".into(),
                payload: "edit a file".into(),
                required_capabilities: vec!["coding".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: Some(1),
                read_only: false,
                include_project_instructions_override: None,
            },
        );
        assert!(
            at_ceiling.is_some(),
            "a request at the ceiling must be honoured"
        );
    }

    #[test]
    fn an_unspecified_tier_defaults_to_the_agents_own_and_always_spawns() {
        let mut registry = AgentRegistry::new();
        registry.register(agent("architect", "deep/model", &["design"], 3));

        let plan = spawn_with_capabilities(
            &registry,
            SpawnRequest {
                task_id: "spawn-default-tier".into(),
                payload: "design it".into(),
                required_capabilities: vec!["design".into()],
                conductor_stable_cache_key: None,
                requested_provider_id: None,
                requested_model_id: None,
                requested_tier: None,
                read_only: false,
                include_project_instructions_override: None,
            },
        );
        assert!(plan.is_some(), "omitting a tier must never fail the spawn");
    }
}
