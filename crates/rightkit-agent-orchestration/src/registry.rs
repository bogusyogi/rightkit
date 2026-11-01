//! The agent registry — catalog of every agent: role, model, prompt, tools, escalation path.

/// The provider that actually serves a model slug.
///
/// The single source of truth for model → provider. Both descriptor
/// constructors used to hardcode `"openrouter"` regardless of the model, so an
/// agent pinned to `minimax/minimax-m3` still claimed OpenRouter served it.
/// Request routing did not care — it derives the provider from the slug — but
/// everything reading `provider_id` did, so the model picker told the user
/// "openrouter has no saved key" for a MiniMax model whose own key was saved
/// and working.
pub fn provider_id_for_model_slug(slug: &str) -> &'static str {
    let trimmed = slug.trim();
    if trimmed.eq_ignore_ascii_case("minimax/minimax-m3")
        || trimmed.eq_ignore_ascii_case("minimax-m3")
    {
        "minimax"
    } else if trimmed.starts_with("groq/") {
        "groq"
    } else if trimmed.starts_with("cerebras/") {
        "cerebras"
    } else if trimmed.starts_with("nvidia/") || trimmed.starts_with("nim/") {
        "nim"
    } else if trimmed.starts_with("openai/") {
        "openai"
    } else {
        "openrouter"
    }
}

/// One agent in the BREWS roster. Every field from the canonical schema is present.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDescriptor {
    /// Unique agent name, e.g. "coder".
    pub name: String,
    /// Role string used by the conductor for routing, e.g. "coder".
    pub role: String,
    /// Capability tags used for capability-based routing fallback.
    pub capabilities: Vec<String>,

    // ── Model assignment ──────────────────────────────────────────────────
    /// OpenRouter (or provider-native) model slug, e.g. "moonshotai/kimi-k2.6".
    pub model_id: String,
    /// Provider id from the PROVIDERS catalog, e.g. "openrouter".
    pub provider_id: String,

    // ── Instructions ─────────────────────────────────────────────────────
    /// Baked-in system prompt. Empty string = no extra prompt (general agent).
    pub system_prompt: String,
    /// One sentence: what this agent does and when NOT to use it.
    pub tool_description: String,

    // ── Governance ───────────────────────────────────────────────────────
    /// 1 = cheap/fast leaf worker, 2 = mid-tier specialist, 3 = architect/boss.
    pub escalation_tier: u8,
    /// Ordered agent names to escalate to when this agent cannot resolve.
    pub escalation_path: Vec<String>,
    /// Tool names this agent is allowed to invoke.
    pub allowed_tools: Vec<String>,
    /// Output confidence below this threshold triggers escalation.
    pub confidence_threshold: f64,
    /// Maximum retries before escalating.
    pub max_retries: u32,
    /// Tags that gate which juries review this agent's output.
    pub jury_tags: Vec<String>,
    /// Benchmark suite id for role benchmarks.
    pub benchmark_id: Option<String>,
    /// TEA-protocol version tag for this agent (e.g. "v1.2.0"). None = unversioned.
    pub version: Option<String>,
    /// Optional JSON Schema contract for the task envelope handed to this agent.
    pub input_schema: Option<serde_json::Value>,
    /// Optional JSON Schema contract for the result envelope returned by this agent.
    pub output_schema: Option<serde_json::Value>,
}

impl AgentDescriptor {
    /// Minimal constructor for ad-hoc / test descriptors.
    pub fn new(
        name: impl Into<String>,
        role: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        let name = name.into();
        let role = role.into();
        let model_id = model_id.into();
        let provider_id = provider_id_for_model_slug(&model_id).to_string();
        Self {
            role: role.clone(),
            name,
            model_id,
            provider_id,
            capabilities: vec![],
            system_prompt: String::new(),
            tool_description: String::new(),
            escalation_tier: 1,
            escalation_path: vec![],
            allowed_tools: vec![],
            confidence_threshold: 0.7,
            max_retries: 2,
            jury_tags: vec![],
            benchmark_id: None,
            version: None,
            input_schema: None,
            output_schema: None,
        }
    }

    /// Set the baked-in system prompt (persona/charter). Builder-style.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Full builder used by the built-in definitions.
    #[allow(clippy::too_many_arguments)]
    pub fn builtin(
        name: &'static str,
        model_id: &'static str,
        tier: u8,
        tool_desc: &'static str,
        tools: &[&'static str],
        capabilities: &[&'static str],
        escalation_path: &[&'static str],
        jury_tags: &[&'static str],
        benchmark_id: Option<&'static str>,
    ) -> Self {
        Self {
            name: name.into(),
            role: name.into(),
            model_id: model_id.into(),
            provider_id: provider_id_for_model_slug(model_id).to_string(),
            capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
            system_prompt: String::new(),
            tool_description: tool_desc.into(),
            escalation_tier: tier,
            escalation_path: escalation_path.iter().map(|s| s.to_string()).collect(),
            allowed_tools: tools.iter().map(|s| s.to_string()).collect(),
            confidence_threshold: 0.7,
            max_retries: 2,
            jury_tags: jury_tags.iter().map(|s| s.to_string()).collect(),
            benchmark_id: benchmark_id.map(|s| s.to_string()),
            version: None,
            input_schema: None,
            output_schema: None,
        }
    }

    pub fn with_version(mut self, v: impl Into<String>) -> Self {
        self.version = Some(v.into());
        self
    }
}

/// A registry of agents with stable insertion order.
/// Call `AgentRegistry::with_builtins()` to get the pre-populated BREWS roster.
#[derive(Debug, Default)]
pub struct AgentRegistry {
    agents: Vec<AgentDescriptor>,
    /// Version history per role: role -> [(version_id, descriptor)] in insertion order.
    version_history: std::collections::HashMap<String, Vec<(String, AgentDescriptor)>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a registry pre-populated with the canonical BREWS built-in roster.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        for a in crate::builtins::builtin_agents() {
            r.register(a);
        }
        r
    }

    /// Register an agent. Replaces any existing agent with the same name.
    pub fn register(&mut self, descriptor: AgentDescriptor) {
        if let Some(existing) = self.agents.iter_mut().find(|a| a.name == descriptor.name) {
            *existing = descriptor;
        } else {
            self.agents.push(descriptor);
        }
    }

    pub fn get(&self, name: &str) -> Option<&AgentDescriptor> {
        self.agents.iter().find(|a| a.name == name)
    }

    pub fn all(&self) -> &[AgentDescriptor] {
        &self.agents
    }

    pub fn len(&self) -> usize {
        self.agents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }

    /// Apply per-agent model overrides from config.
    /// For each entry in `overrides`, if an agent with that name is registered,
    /// its `model_id` (and optionally `provider_id`) is replaced.
    /// Unknown agent names are silently ignored (user may have removed a custom agent).
    pub fn apply_config_overrides(
        &mut self,
        overrides: &std::collections::HashMap<String, (String, String)>,
    ) {
        for (name, (model_id, provider_id)) in overrides {
            if let Some(agent) = self.agents.iter_mut().find(|a| &a.name == name) {
                if !model_id.is_empty() {
                    agent.model_id = model_id.clone();
                    // A model override without an explicit provider must move
                    // the provider too, or the agent keeps claiming whoever
                    // served the PREVIOUS model — the same stale-attribution
                    // bug the constructors had.
                    agent.provider_id = provider_id_for_model_slug(model_id).to_string();
                }
                if !provider_id.is_empty() {
                    agent.provider_id = provider_id.clone();
                }
            }
        }
    }

    /// Register a versioned agent, recording it in the version history.
    /// Per TEA protocol (arXiv 2506.12508): each registration creates a version
    /// checkpoint that can be rolled back to.
    pub fn register_versioned(&mut self, descriptor: AgentDescriptor) {
        let role = descriptor.role.clone();
        let version_id = descriptor
            .version
            .clone()
            .unwrap_or_else(|| format!("unversioned-{}", self.agents.len()));
        self.register(descriptor.clone());
        self.version_history
            .entry(role)
            .or_default()
            .push((version_id, descriptor));
    }

    /// Roll back an agent to a previous version by role name and version string.
    /// Returns the prior descriptor if found, None if no such version exists.
    pub fn rollback(&self, role: &str, version: &str) -> Option<&AgentDescriptor> {
        self.version_history
            .get(role)
            .and_then(|history| history.iter().find(|(v, _)| v == version))
            .map(|(_, desc)| desc)
    }

    /// Full version history for a role (oldest first).
    pub fn version_history(&self, role: &str) -> &[(String, AgentDescriptor)] {
        self.version_history
            .get(role)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Return a canned clarification question template for common role pairs.
    ///
    /// The template is sent from `source_role` to `target_role` before the target
    /// begins work, to surface missing context that would otherwise cause an incorrect
    /// or wasted run. Returns `None` for pairs with no registered template.
    ///
    /// Templates use `{task}` as a placeholder for the task description the source
    /// is handing off.
    pub fn clarification_query_for(source_role: &str, target_role: &str) -> Option<String> {
        let src = source_role.to_lowercase();
        let tgt = target_role.to_lowercase();

        match (src.as_str(), tgt.as_str()) {
            ("planner", "coder") | ("planner", "engineer") | ("planner", "architect") => Some(
                "Before starting: confirm the exact output files expected and the \
                 acceptance criteria. What constitutes a passing result for this task?"
                    .into(),
            ),

            ("conductor", "coder") | ("conductor", "engineer") => Some(
                "Before starting: confirm the scope ceiling — which files may be modified, \
                 and is the task self-contained or does it depend on in-progress work?"
                    .into(),
            ),

            ("architect", "coder")
            | ("architect", "engineer")
            | ("software-architect", "coder")
            | ("software-architect", "engineer")
            | ("backend-architect", "coder")
            | ("backend-architect", "engineer") => Some(
                "Before starting: confirm you have read the design spec and understand the \
                 interface contracts. Are there any ambiguous types or edge cases in the \
                 design that need clarification before you write the first line?"
                    .into(),
            ),

            ("qa-analyst", "coder") | ("qa-analyst", "engineer") => Some(
                "Before starting: confirm the implementation exposes the hooks the test suite \
                 needs — are there any private internals that must become package-visible or \
                 injectable for the planned tests to work?"
                    .into(),
            ),

            ("code-reviewer", "coder") | ("code-reviewer", "engineer") => Some(
                "Before starting: confirm which findings from the review are in-scope for \
                 this fix pass and which are deferred. What is the priority order?"
                    .into(),
            ),

            ("security-engineer", "coder") | ("security-engineer", "engineer") => Some(
                "Before starting: confirm that the threat mitigations identified in the \
                 security review are all captured in the task. Are there any mitigations \
                 that require architectural changes outside your scope?"
                    .into(),
            ),

            ("devops-worker", "coder") | ("coder", "devops-worker") => Some(
                "Before starting: confirm the build artifact format and deployment contract. \
                 What does the pipeline expect as input, and what environment variables or \
                 secrets are assumed to be present?"
                    .into(),
            ),

            ("technical-writer", "coder")
            | ("technical-writer", "engineer")
            | ("technical-writer", "backend-architect") => Some(
                "Before starting: confirm the authoritative source of truth for the behaviour \
                 being documented. Is the implementation stable, or are interfaces still \
                 changing?"
                    .into(),
            ),

            ("product-analyst", "planner") => Some(
                "Before starting: confirm that the requirements are frozen for this planning \
                 cycle. Are there any open product decisions that could change the plan after \
                 it is produced?"
                    .into(),
            ),

            ("product-analyst", "product-lead") => Some(
                "Before starting: provide the product findings, supporting evidence, and the \
                 specific claims or launch decisions that need final approval."
                    .into(),
            ),

            ("signoff", _) | ("challenger", _) => Some(
                "Before starting: provide the acceptance criteria and the list of artifacts \
                 to inspect. What constitutes a clear pass or fail for this verification?"
                    .into(),
            ),

            _ => None,
        }
    }
}

#[cfg(test)]
#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
