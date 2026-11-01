//! The resolution request: portable capability requirements supplied by the
//! Legion plan, structured signals, explicit bindings and scope.
//!
//! Canon (PL-010): "Legion supplies portable capability requirements &
//! the host app binds executors." A [`MechanismRequest`] is exactly that
//! portable shape: it carries *what* must be satisfied (capabilities,
//! authority, signals, bindings, membrane sources) and never names an
//! executor. Binding executors is the resolver's job, deterministically.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::capability::AuthorityNeed;
use super::class::MechanismClass;
use super::membrane::MembraneBinding;

/// Where a resolution sits in the topology.
///
/// Canon (PL-010): "root-level exact fast path, per-child-node binding after
/// Conduct decomposition." A `Root` request may take the exact fast path; a
/// `Child` request carries its parent decision fingerprint and the denials
/// inherited from the root — a child never re-litigates a root denial.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RequestScope {
    Root,
    Child {
        /// Fingerprint of the parent (root or conductor-level) decision.
        parent_decision_fingerprint: String,
        /// Capabilities denied at the root; inherited verbatim.
        inherited_denials: Vec<String>,
    },
}

/// One portable capability requirement. `capability` is the primary exact
/// name; `alternatives` are acceptable fallback names tried in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CapabilityRequirement {
    pub capability: String,
    #[serde(default)]
    pub alternatives: Vec<String>,
    pub authority: AuthorityNeed,
}

impl CapabilityRequirement {
    pub fn new(capability: impl Into<String>, authority: AuthorityNeed) -> Self {
        Self {
            capability: capability.into(),
            alternatives: Vec::new(),
            authority,
        }
    }

    pub fn with_alternatives(mut self, alternatives: Vec<String>) -> Self {
        self.alternatives = alternatives;
        self
    }

    /// Every acceptable capability name in deterministic priority order.
    pub fn candidate_names(&self) -> Vec<String> {
        let mut names = vec![self.capability.clone()];
        names.extend(self.alternatives.iter().cloned());
        names
    }
}

/// Deterministic intent signal, derived by the caller from the task/plan.
/// Maps onto the deterministic resolution stages: `Compute`/`Transform` bias
/// toward structured-code-op execution (PAL/LLMCompiler shape), `Retrieve`
/// toward authoritative source resolution (LLM+P shape), `Observe` toward
/// builtins, `Decide` toward a semantic model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredIntent {
    Compute,
    Transform,
    Retrieve,
    Mutate,
    Communicate,
    Observe,
    Decide,
    Unknown,
}

/// Structured signals extracted before resolution. Everything here is
/// deterministic input — the resolver never infers intent itself; that is the
/// bounded classifier's job, and only when these signals bind nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct StructuredSignals {
    pub intent: StructuredIntent,
    /// Exact tool name requested by the plan/task (root fast path input).
    pub exact_tool_name: Option<String>,
    /// Known input contract, when the plan carries one.
    pub input_schema: Option<Value>,
    /// Known output contract; drives deterministic verification.
    pub output_schema: Option<Value>,
    /// Explicit human-approval requirement (the ONLY way `Human` binds
    /// without an explicit binding).
    pub requires_human_approval: bool,
}

impl Default for StructuredSignals {
    fn default() -> Self {
        Self {
            intent: StructuredIntent::Unknown,
            exact_tool_name: None,
            input_schema: None,
            output_schema: None,
            requires_human_approval: false,
        }
    }
}

impl StructuredSignals {
    pub fn with_intent(mut self, intent: StructuredIntent) -> Self {
        self.intent = intent;
        self
    }

    pub fn with_exact_tool_name(mut self, name: impl Into<String>) -> Self {
        self.exact_tool_name = Some(name.into());
        self
    }

    pub fn with_output_schema(mut self, schema: Value) -> Self {
        self.output_schema = Some(schema);
        self
    }

    pub fn requires_human_approval(mut self) -> Self {
        self.requires_human_approval = true;
        self
    }
}

/// A pre-resolved binding supplied by the plan (Legion) or by a prior
/// decision. Explicit bindings are honored *after validation*: an explicit
/// binding may pin the class and optionally the capability, but it can never
/// resurrect a denied capability or invent a missing executor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ExplicitBinding {
    pub mechanism_class: MechanismClass,
    /// Capability the binding pins (must exist in the projected registry when
    /// the class is executor-backed).
    pub capability: Option<String>,
    /// Opaque executor reference (script id, builtin id, program handle…).
    pub binding_ref: String,
}

impl ExplicitBinding {
    pub fn new(
        mechanism_class: MechanismClass,
        capability: Option<String>,
        binding_ref: impl Into<String>,
    ) -> Self {
        Self {
            mechanism_class,
            capability,
            binding_ref: binding_ref.into(),
        }
    }
}

/// A mechanism-resolution request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct MechanismRequest {
    pub request_id: String,
    /// Node identity: the root task id, or the conductor child node id.
    pub node_id: String,
    /// The task payload (digest input only; never logged by the resolver).
    pub payload: String,
    pub explicit_binding: Option<ExplicitBinding>,
    pub requirements: Vec<CapabilityRequirement>,
    pub signals: StructuredSignals,
    /// Provider-authoritative source bindings (honored, never synthesized).
    pub membrane_bindings: Vec<MembraneBinding>,
    /// The capability-registry revision the caller observed when building
    /// this request. Empty means "no observation"; non-empty mismatches make
    /// the resolution [`super::decision::MechanismOutcome::Stale`].
    pub observed_capability_revision: String,
    /// Capabilities already denied upstream (policy/authority). Terminal:
    /// resolution returns `Denied` and never escalates to a model.
    pub denied_capabilities: Vec<String>,
    pub scope: RequestScope,
}

impl Default for MechanismRequest {
    fn default() -> Self {
        Self {
            request_id: String::new(),
            node_id: String::new(),
            payload: String::new(),
            explicit_binding: None,
            requirements: Vec::new(),
            signals: StructuredSignals::default(),
            membrane_bindings: Vec::new(),
            observed_capability_revision: String::new(),
            denied_capabilities: Vec::new(),
            scope: RequestScope::Root,
        }
    }
}

impl MechanismRequest {
    /// A root-level request (direct-loop path).
    pub fn root(
        request_id: impl Into<String>,
        node_id: impl Into<String>,
        payload: impl Into<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            node_id: node_id.into(),
            payload: payload.into(),
            ..Self::default()
        }
    }

    /// A per-child-node request after Conduct decomposition, linked to its
    /// parent decision and inheriting the root's denials.
    pub fn child_of(
        request_id: impl Into<String>,
        node_id: impl Into<String>,
        payload: impl Into<String>,
        parent_decision_fingerprint: impl Into<String>,
        inherited_denials: Vec<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            node_id: node_id.into(),
            payload: payload.into(),
            scope: RequestScope::Child {
                parent_decision_fingerprint: parent_decision_fingerprint.into(),
                inherited_denials,
            },
            ..Self::default()
        }
    }

    pub fn with_explicit_binding(mut self, binding: ExplicitBinding) -> Self {
        self.explicit_binding = Some(binding);
        self
    }

    pub fn with_requirements(mut self, requirements: Vec<CapabilityRequirement>) -> Self {
        self.requirements = requirements;
        self
    }

    pub fn with_signals(mut self, signals: StructuredSignals) -> Self {
        self.signals = signals;
        self
    }

    pub fn with_membrane_bindings(mut self, bindings: Vec<MembraneBinding>) -> Self {
        self.membrane_bindings = bindings;
        self
    }

    pub fn with_observed_capability_revision(mut self, revision: impl Into<String>) -> Self {
        self.observed_capability_revision = revision.into();
        self
    }

    pub fn denying(mut self, denied: Vec<String>) -> Self {
        self.denied_capabilities = denied;
        self
    }

    /// Every capability name this request could bind, in deterministic order:
    /// explicit binding capability, exact tool name, then requirements
    /// (primary name then alternatives).
    pub fn referenced_capabilities(&self) -> Vec<String> {
        let mut names = Vec::new();
        if let Some(binding) = &self.explicit_binding {
            if let Some(capability) = &binding.capability {
                names.push(capability.clone());
            }
        }
        if let Some(tool) = &self.signals.exact_tool_name {
            names.push(tool.clone());
        }
        for requirement in &self.requirements {
            names.extend(requirement.candidate_names());
        }
        names
    }

    /// Deterministic request fingerprint (canon: receipts with fingerprints).
    /// Same inputs → identical fingerprint; any payload/requirements/signals/
    /// bindings/authority change → a different fingerprint. The payload enters
    /// the digest only as bytes — it is never stored in the fingerprint.
    pub fn request_fingerprint(&self) -> String {
        let canonical = serde_json::json!({
            "v": super::decision::MECHANISM_DECISION_SCHEMA_VERSION,
            "request_id": &self.request_id,
            "node_id": &self.node_id,
            "payload_sha256": Self::sha256_hex(self.payload.as_bytes()),
            "explicit_binding": &self.explicit_binding,
            "requirements": &self.requirements,
            "signals": {
                "intent": self.signals.intent,
                "exact_tool_name": &self.signals.exact_tool_name,
                "input_schema": &self.signals.input_schema,
                "output_schema": &self.signals.output_schema,
                "requires_human_approval": self.signals.requires_human_approval,
            },
            "membrane_bindings": &self.membrane_bindings,
            "denied_capabilities": &self.denied_capabilities,
            "scope": &self.scope,
        });
        format!(
            "sha256:{}",
            Self::sha256_hex(canonical.to_string().as_bytes())
        )
    }

    pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
        rightkit_fs::digest::sha256_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mechanism::membrane::MembraneSurface;

    fn sample_request() -> MechanismRequest {
        MechanismRequest::root("req-1", "node-1", "fix the typo in src/lib.rs")
            .with_signals(StructuredSignals::default().with_exact_tool_name("edit_file"))
            .with_requirements(vec![CapabilityRequirement::new(
                "edit_file",
                AuthorityNeed::Mutate,
            )])
    }

    #[test]
    fn request_fingerprint_is_deterministic_and_payload_sensitive() {
        let first = sample_request();
        let second = sample_request();
        assert_eq!(first.request_fingerprint(), second.request_fingerprint());

        let changed = MechanismRequest::root("req-1", "node-1", "different payload entirely");
        assert_ne!(first.request_fingerprint(), changed.request_fingerprint());
    }

    #[test]
    fn child_scope_carries_parent_and_inherited_denials() {
        let request = MechanismRequest::child_of("req-2", "child-1", "subtask", "sha256:parent", {
            let mut denied = vec!["shell".to_string()];
            denied.sort();
            denied
        });
        match &request.scope {
            RequestScope::Child {
                parent_decision_fingerprint,
                inherited_denials,
            } => {
                assert_eq!(parent_decision_fingerprint, "sha256:parent");
                assert_eq!(inherited_denials, &["shell".to_string()]);
            }
            RequestScope::Root => panic!("expected child scope"),
        }
    }

    #[test]
    fn referenced_capabilities_are_ordered_and_deduped_by_caller_contract() {
        let request = MechanismRequest::root("req-3", "node-3", "payload")
            .with_signals(StructuredSignals::default().with_exact_tool_name("shell"))
            .with_explicit_binding(ExplicitBinding::new(
                MechanismClass::Process,
                Some("process".to_string()),
                "builtin://process",
            ))
            .with_requirements(vec![CapabilityRequirement::new(
                "shell",
                AuthorityNeed::Execute,
            )
            .with_alternatives(vec!["powershell".to_string()])]);
        let names = request.referenced_capabilities();
        assert_eq!(names.len(), 4);
        assert_eq!(names[0], "process");
        assert_eq!(names[1], "shell");
        assert_eq!(names[2], "shell");
        assert_eq!(names[3], "powershell");
    }

    #[test]
    fn membrane_bindings_serialize_with_the_request() {
        let request = sample_request().with_membrane_bindings(vec![MembraneBinding::new(
            MembraneSurface::SourceRead,
            "repo://src/lib.rs",
            "auth-rev-3",
        )]);
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("source_read"));
        let round_tripped: MechanismRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, request);
    }
}
