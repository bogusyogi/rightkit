//! `MechanismResolver` — typed mechanism-first execution resolution (PL-010).
//!
//! Sits between topology routing (`crate::dispatch::DispatchRouter`, which
//! decides Direct vs Conduct) and model routing (`host_router`, which
//! picks *which* model). This resolver answers the question between them:
//! **does this work need a model at all?**
//!
//! Resolution order (canon, deterministic):
//! 1. **Authority gate** — upstream/inherited policy denials are terminal
//!    (`Denied`); they never escalate to a model.
//! 2. **Root-level exact fast path** — at the root, an explicit binding or an
//!    exact tool name binds immediately; the classifier is never consulted.
//! 3. **Explicit binding** — plan-supplied bindings honored after validation.
//! 4. **Exact capability match** — requirements matched against descriptors
//!    projected from the existing tool registry (no second registry).
//! 5. **Structured signals** — deterministic intent mapping (PAL/LLM+P shapes).
//! 6. **Bounded classifier last** — proposals over the closed class set; a
//!    proposal never binds an executor by itself.
//!
//! Typed outcomes: `Completed` / `Insufficient` / `Ambiguous` / `Unsupported`
//! / `Denied` / `Failed` / `Stale` / `HumanRequired`. `Insufficient` (and an
//! escalated `Failed`) hand off to model routing; everything else does not.

use serde_json::Value;

use super::capability::{AuthorityNeed, CapabilityDescriptor, CapabilitySource};
use super::class::MechanismClass;
use super::classifier::{ClassifiedMechanism, ClassifierError, MechanismClassifier};
use super::decision::{
    AvoidanceReceipt, EscalationTrigger, ExecutionRequirement, MechanismDecision,
    MechanismDecisionError, MechanismOutcome, ResolutionStage, ResolvedBinding,
    VerificationContract,
};
use super::requirement::{
    CapabilityRequirement, ExplicitBinding, MechanismRequest, RequestScope, StructuredIntent,
    StructuredSignals,
};

/// Deterministic resolver state (the classifier is injected per call).
#[derive(Debug, Clone, Copy, Default)]
pub struct MechanismResolver;

/// Deterministic verification result for a semantic/structured output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticVerification {
    pub passed: bool,
    pub failures: Vec<String>,
}

/// The semantic output presented back for deterministic verification.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticOutput {
    pub text: String,
    pub json: Option<Value>,
    /// `source_ref`s the answer actually consumed (Membrane coverage proof).
    pub consulted_source_refs: Vec<String>,
}

impl MechanismResolver {
    pub fn new() -> Self {
        Self
    }

    /// Resolve one request against the projected capability registry.
    pub async fn resolve(
        &self,
        request: &MechanismRequest,
        capabilities: &dyn CapabilitySource,
        classifier: Option<&dyn MechanismClassifier>,
    ) -> MechanismDecision {
        let request_fingerprint = request.request_fingerprint();
        let revision = capabilities.revision().to_string();
        let descriptors = capabilities.descriptors();

        // 0. Malformed Membrane bindings fail closed: provider authority
        //    cannot be honored without its revision stamp.
        for binding in &request.membrane_bindings {
            if let Err(error) = binding.validate() {
                return self.terminal_decision(
                    request,
                    &request_fingerprint,
                    &revision,
                    MechanismOutcome::Unsupported,
                    format!("membrane binding cannot honor provider authority: {error}"),
                );
            }
        }

        // 0b. Observed-revision mismatch → Stale. Re-resolution against the
        //     current projection is required before anything binds.
        if !request.observed_capability_revision.is_empty()
            && request.observed_capability_revision != revision
        {
            return self.terminal_decision(
                request,
                &request_fingerprint,
                &revision,
                MechanismOutcome::Stale,
                format!(
                    "observed capability revision `{}` no longer matches live projection `{}`",
                    request.observed_capability_revision, revision
                ),
            );
        }

        // 1. Authority gate — denials are terminal and never escalate.
        let referenced = request.referenced_capabilities();
        let mut denied: Vec<String> = Vec::new();
        for name in &referenced {
            let upstream_denied = request.denied_capabilities.iter().any(|d| d == name);
            let inherited_denied = match &request.scope {
                RequestScope::Child {
                    inherited_denials, ..
                } => inherited_denials.iter().any(|d| d == name),
                RequestScope::Root => false,
            };
            if (upstream_denied || inherited_denied) && !denied.contains(name) {
                denied.push(name.clone());
            }
        }
        if !denied.is_empty() {
            return MechanismDecision::assemble(
                request_fingerprint,
                request.node_id.clone(),
                &request.scope,
                ResolutionStage::AuthorityGate,
                MechanismOutcome::Denied,
                format!(
                    "capability denials are terminal and never escalate to a model: {}",
                    denied.join(", ")
                ),
                None,
                Vec::new(),
                None,
                None,
                revision,
                request.membrane_bindings.clone(),
            )
            .expect("denied decision without escalation is always valid")
            .with_denied_capabilities(denied);
        }

        // 2. Root-level exact fast path: explicit binding or exact tool name
        //    binds at the root without consulting any classifier.
        if matches!(request.scope, RequestScope::Root) {
            if let Some(binding) = &request.explicit_binding {
                if let Some(decision) = self.bind_explicit(
                    request,
                    &request_fingerprint,
                    &revision,
                    &descriptors,
                    binding,
                    ResolutionStage::RootExactFastPath,
                ) {
                    return decision;
                }
            } else if let Some(tool) = &request.signals.exact_tool_name {
                if let Some(decision) = self.bind_exact_tool(
                    request,
                    &request_fingerprint,
                    &revision,
                    &descriptors,
                    tool,
                    ResolutionStage::RootExactFastPath,
                ) {
                    return decision;
                }
            }
        }

        // 3. Explicit binding (child scope; at the root every explicit
        //    binding was already resolved — valid or not — by the fast path,
        //    fail-closed rather than silently re-routed).
        if let Some(binding) = &request.explicit_binding {
            if let Some(decision) = self.bind_explicit(
                request,
                &request_fingerprint,
                &revision,
                &descriptors,
                binding,
                ResolutionStage::ExplicitBinding,
            ) {
                return decision;
            }
        }

        // 4. Exact capability match, requirements in plan order.
        for requirement in &request.requirements {
            if let Some(decision) = self.bind_requirement(
                request,
                &request_fingerprint,
                &revision,
                &descriptors,
                requirement,
            ) {
                return decision;
            }
        }

        // 5. Structured signals (deterministic intent mapping).
        if let Some(decision) = self.bind_from_signals(request, &request_fingerprint, &revision) {
            return decision;
        }

        // 6. Bounded classifier last.
        self.classify_last(
            request,
            &request_fingerprint,
            &revision,
            &descriptors,
            classifier,
        )
        .await
    }

    /// Deterministic verification contract for the semantic path. The
    /// orchestrator attaches this to the model-routing handoff and presents
    /// the model output to [`MechanismResolver::verify_semantic_output`]
    /// afterwards. Membrane-bound sources must be acknowledged by the answer;
    /// they are never replaced by it.
    pub fn semantic_verification_contract(request: &MechanismRequest) -> VerificationContract {
        let mut contract = VerificationContract::default()
            .with_source_coverage(
                request
                    .membrane_bindings
                    .iter()
                    .map(|binding| binding.source_ref.clone())
                    .collect(),
            )
            .with_predicates({
                let mut predicates = vec!["non_empty".to_string()];
                if request.signals.output_schema.is_some() {
                    predicates.push("json_parses".to_string());
                }
                predicates
            });
        if let Some(schema) = &request.signals.output_schema {
            contract = contract.with_output_schema(schema.clone());
        }
        contract
    }

    /// Deterministic verification of semantic output (canon). Every check is
    /// local and bounded; unknown predicates fail closed.
    pub fn verify_semantic_output(
        &self,
        contract: &VerificationContract,
        output: &SemanticOutput,
    ) -> SemanticVerification {
        let mut failures: Vec<String> = Vec::new();

        for predicate in &contract.deterministic_predicates {
            match predicate.as_str() {
                "non_empty" => {
                    if output.text.trim().is_empty() {
                        failures.push("predicate `non_empty` failed: output is empty".into());
                    }
                }
                "json_parses" => {
                    if output.json.is_none() {
                        failures.push(
                            "predicate `json_parses` failed: output carries no parsed JSON".into(),
                        );
                    }
                }
                other => failures.push(format!("unknown deterministic predicate `{other}`")),
            }
        }

        if output.text.chars().count() > contract.max_output_chars {
            failures.push(format!(
                "output length {} exceeds contract bound {}",
                output.text.chars().count(),
                contract.max_output_chars
            ));
        }

        if let Some(schema) = &contract.output_schema {
            match (&output.json, required_keys(schema)) {
                (None, _) => {
                    failures
                        .push("schema contract present but output carries no parsed JSON".into());
                }
                (Some(_), Err(message)) => failures.push(message),
                (Some(json), Ok(required)) => {
                    for key in required {
                        if json.get(&key).is_none() {
                            failures.push(format!(
                                "schema `required` key `{key}` missing from output JSON"
                            ));
                        }
                    }
                }
            }
        }

        for source_ref in &contract.require_source_coverage {
            if !output
                .consulted_source_refs
                .iter()
                .any(|consulted| consulted == source_ref)
            {
                failures.push(format!(
                    "membrane-bound source `{source_ref}` was not acknowledged; a semantic \
                     answer may never replace a bound source"
                ));
            }
        }

        SemanticVerification {
            passed: failures.is_empty(),
            failures,
        }
    }

    /// The one bounded escalation after a deterministic binding failed. Mirrors
    /// `host_router::bounded_escalation`: exactly one attempt, typed
    /// trigger, request fingerprint preserved. A `Denied` decision can never
    /// be escalated.
    pub fn escalate_after_failure(
        &self,
        decision: MechanismDecision,
        trigger: EscalationTrigger,
        from_class: Option<MechanismClass>,
    ) -> Result<MechanismDecision, MechanismDecisionError> {
        decision.with_escalation(trigger, from_class)
    }

    // ── stages ──────────────────────────────────────────────────────────

    fn bind_explicit(
        &self,
        request: &MechanismRequest,
        request_fingerprint: &str,
        revision: &str,
        descriptors: &[CapabilityDescriptor],
        binding: &ExplicitBinding,
        stage: ResolutionStage,
    ) -> Option<MechanismDecision> {
        let assemble = |outcome: MechanismOutcome,
                        reason: String,
                        requirement: Option<ExecutionRequirement>,
                        candidates: Vec<ResolvedBinding>,
                        avoidance: Option<AvoidanceReceipt>| {
            MechanismDecision::assemble(
                request_fingerprint.to_string(),
                request.node_id.clone(),
                &request.scope,
                stage,
                outcome,
                reason,
                requirement,
                candidates,
                avoidance,
                None,
                revision.to_string(),
                request.membrane_bindings.clone(),
            )
            .expect("resolver never pairs Denied with an escalation")
        };

        if binding.binding_ref.trim().is_empty() {
            return Some(assemble(
                MechanismOutcome::Unsupported,
                "explicit binding carries no executor reference".to_string(),
                None,
                Vec::new(),
                None,
            ));
        }

        match binding.mechanism_class {
            MechanismClass::SemanticModel => Some(assemble(
                MechanismOutcome::Insufficient,
                "explicit binding requires a semantic model; model routing runs next".to_string(),
                None,
                Vec::new(),
                None,
            )),
            MechanismClass::Human => {
                let requirement = Some(ExecutionRequirement {
                    binding: ResolvedBinding {
                        mechanism_class: MechanismClass::Human,
                        capability: binding.capability.clone(),
                        binding_ref: binding.binding_ref.clone(),
                        authority_revision: revision.to_string(),
                    },
                    verification: VerificationContract::default(),
                    required_capabilities: binding.capability.clone().into_iter().collect(),
                });
                Some(assemble(
                    MechanismOutcome::HumanRequired,
                    "explicit binding designates a human executor".to_string(),
                    requirement,
                    Vec::new(),
                    None,
                ))
            }
            MechanismClass::Unsupported => Some(assemble(
                MechanismOutcome::Unsupported,
                "explicit binding names the unsupported mechanism class".to_string(),
                None,
                Vec::new(),
                None,
            )),
            class if class.is_deterministic() => {
                let Some(capability) = &binding.capability else {
                    // Class-only bindings carry their executor in binding_ref.
                    let requirement = Some(ExecutionRequirement {
                        binding: ResolvedBinding {
                            mechanism_class: class,
                            capability: None,
                            binding_ref: binding.binding_ref.clone(),
                            authority_revision: revision.to_string(),
                        },
                        verification: VerificationContract::default(),
                        required_capabilities: Vec::new(),
                    });
                    return Some(assemble(
                        MechanismOutcome::Completed,
                        format!("explicit binding honors `{class}` executor"),
                        requirement,
                        Vec::new(),
                        Some(avoidance(stage, class)),
                    ));
                };
                let matches: Vec<&CapabilityDescriptor> = descriptors
                    .iter()
                    .filter(|descriptor| &descriptor.name == capability)
                    .collect();
                if matches.is_empty() {
                    return Some(assemble(
                        MechanismOutcome::Unsupported,
                        format!(
                            "explicit binding names capability `{capability}` absent from the projected registry"
                        ),
                        None,
                        Vec::new(),
                        None,
                    ));
                }
                if let Some(candidates) = ambiguous_if_class_conflict(
                    matches.as_slice(),
                    capability,
                    Some(binding.mechanism_class),
                ) {
                    return Some(assemble(
                        MechanismOutcome::Ambiguous,
                        format!(
                            "explicit binding class conflicts with projected descriptor hints for `{capability}`"
                        ),
                        None,
                        candidates,
                        None,
                    ));
                }
                let descriptor = matches[0];
                // Authority: a binding that demands a mutating/ executing
                // mechanism cannot bind a read-only descriptor.
                if matches!(class, MechanismClass::Process | MechanismClass::Scripted)
                    && descriptor.read_only
                {
                    return Some(assemble(
                        MechanismOutcome::Denied,
                        format!(
                            "explicit binding requires a mutating executor but `{capability}` is read-only; authority mismatch is terminal"
                        ),
                        None,
                        Vec::new(),
                        None,
                    ));
                }
                let requirement = Some(ExecutionRequirement {
                    binding: ResolvedBinding {
                        mechanism_class: class,
                        capability: Some(capability.clone()),
                        binding_ref: binding.binding_ref.clone(),
                        authority_revision: revision.to_string(),
                    },
                    verification: VerificationContract::default(),
                    required_capabilities: vec![capability.clone()],
                });
                Some(assemble(
                    MechanismOutcome::Completed,
                    format!("explicit binding honored for capability `{capability}`"),
                    requirement,
                    Vec::new(),
                    Some(avoidance(stage, class)),
                ))
            }
            _ => Some(assemble(
                MechanismOutcome::Unsupported,
                "explicit binding class cannot be honored".to_string(),
                None,
                Vec::new(),
                None,
            )),
        }
    }

    fn bind_exact_tool(
        &self,
        request: &MechanismRequest,
        request_fingerprint: &str,
        revision: &str,
        descriptors: &[CapabilityDescriptor],
        tool: &str,
        stage: ResolutionStage,
    ) -> Option<MechanismDecision> {
        let matches: Vec<&CapabilityDescriptor> = descriptors
            .iter()
            .filter(|descriptor| descriptor.name == tool)
            .collect();
        if matches.is_empty() {
            return None;
        }
        let class = matches[0].class_hint.unwrap_or(MechanismClass::Scripted);
        let requirement = Some(ExecutionRequirement {
            binding: ResolvedBinding {
                mechanism_class: class,
                capability: Some(tool.to_string()),
                binding_ref: format!("tool://{tool}"),
                authority_revision: revision.to_string(),
            },
            verification: VerificationContract::default(),
            required_capabilities: vec![tool.to_string()],
        });
        Some(
            MechanismDecision::assemble(
                request_fingerprint.to_string(),
                request.node_id.clone(),
                &request.scope,
                stage,
                MechanismOutcome::Completed,
                format!("root exact fast path bound tool `{tool}`"),
                requirement,
                Vec::new(),
                Some(avoidance(stage, class)),
                None,
                revision.to_string(),
                request.membrane_bindings.clone(),
            )
            .expect("completed decision without escalation is always valid"),
        )
    }

    fn bind_requirement(
        &self,
        request: &MechanismRequest,
        request_fingerprint: &str,
        revision: &str,
        descriptors: &[CapabilityDescriptor],
        requirement: &CapabilityRequirement,
    ) -> Option<MechanismDecision> {
        for name in requirement.candidate_names() {
            let matches: Vec<&CapabilityDescriptor> = descriptors
                .iter()
                .filter(|descriptor| descriptor.name == name)
                .collect();
            if matches.is_empty() {
                continue;
            }

            // Authority-first: a human-approval requirement is satisfied by a
            // human, never by a tool; a secret requirement never binds through
            // plain resolution.
            if requirement.authority == AuthorityNeed::HumanApproval {
                return Some(self.terminal_decision(
                    request,
                    request_fingerprint,
                    revision,
                    MechanismOutcome::HumanRequired,
                    format!(
                        "capability `{name}` requires human approval; no deterministic executor may satisfy it"
                    ),
                ));
            }
            if requirement.authority == AuthorityNeed::Secret {
                return Some(self.terminal_decision(
                    request,
                    request_fingerprint,
                    revision,
                    MechanismOutcome::Denied,
                    format!(
                        "capability `{name}` requires secret authority; resolution never binds secrets"
                    ),
                ));
            }

            if let Some(candidates) = ambiguous_if_class_conflict(matches.as_slice(), &name, None) {
                return Some(
                    MechanismDecision::assemble(
                        request_fingerprint.to_string(),
                        request.node_id.clone(),
                        &request.scope,
                        ResolutionStage::ExactCapabilityMatch,
                        MechanismOutcome::Ambiguous,
                        format!(
                            "capability `{name}` matches descriptors with conflicting class hints"
                        ),
                        None,
                        candidates,
                        None,
                        None,
                        revision.to_string(),
                        request.membrane_bindings.clone(),
                    )
                    .expect("ambiguous decision without escalation is always valid"),
                );
            }

            let descriptor = matches[0];
            if !descriptor.satisfies(requirement.authority) {
                return Some(self.terminal_decision(
                    request,
                    request_fingerprint,
                    revision,
                    MechanismOutcome::Denied,
                    format!(
                        "capability `{name}` cannot satisfy required authority `{:?}` from a read-only descriptor",
                        requirement.authority
                    ),
                ));
            }

            let class = descriptor.class_hint.unwrap_or(MechanismClass::Scripted);
            let binding = Some(ExecutionRequirement {
                binding: ResolvedBinding {
                    mechanism_class: class,
                    capability: Some(name.clone()),
                    binding_ref: format!("tool://{name}"),
                    authority_revision: revision.to_string(),
                },
                verification: VerificationContract::default(),
                required_capabilities: vec![name.clone()],
            });
            return Some(
                MechanismDecision::assemble(
                    request_fingerprint.to_string(),
                    request.node_id.clone(),
                    &request.scope,
                    ResolutionStage::ExactCapabilityMatch,
                    MechanismOutcome::Completed,
                    format!("exact capability match bound `{name}`"),
                    binding,
                    Vec::new(),
                    Some(avoidance(ResolutionStage::ExactCapabilityMatch, class)),
                    None,
                    revision.to_string(),
                    request.membrane_bindings.clone(),
                )
                .expect("completed decision without escalation is always valid"),
            );
        }
        None
    }

    fn bind_from_signals(
        &self,
        request: &MechanismRequest,
        request_fingerprint: &str,
        revision: &str,
    ) -> Option<MechanismDecision> {
        let signals: &StructuredSignals = &request.signals;

        if signals.requires_human_approval {
            return Some(self.terminal_decision(
                request,
                request_fingerprint,
                revision,
                MechanismOutcome::HumanRequired,
                "structured signal requires human approval".to_string(),
            ));
        }

        let assemble_binding = |class: MechanismClass,
                                binding_ref: String,
                                verification: VerificationContract,
                                reason: String| {
            let binding = Some(ExecutionRequirement {
                binding: ResolvedBinding {
                    mechanism_class: class,
                    capability: None,
                    binding_ref,
                    authority_revision: revision.to_string(),
                },
                verification,
                required_capabilities: Vec::new(),
            });
            MechanismDecision::assemble(
                request_fingerprint.to_string(),
                request.node_id.clone(),
                &request.scope,
                ResolutionStage::StructuredSignals,
                MechanismOutcome::Completed,
                reason,
                binding,
                Vec::new(),
                Some(avoidance(ResolutionStage::StructuredSignals, class)),
                None,
                revision.to_string(),
                request.membrane_bindings.clone(),
            )
            .expect("completed decision without escalation is always valid")
        };

        match signals.intent {
            StructuredIntent::Observe => Some(assemble_binding(
                MechanismClass::Builtin,
                "builtin://observe".to_string(),
                VerificationContract::default(),
                "observe intent binds the builtin observation mechanism".to_string(),
            )),
            StructuredIntent::Compute | StructuredIntent::Transform => {
                // PAL/LLMCompiler shape: the runtime executes the operation
                // and verifies the output against the declared schema.
                let verification = VerificationContract::default();
                let verification = match &signals.output_schema {
                    Some(schema) => verification.with_output_schema(schema.clone()),
                    None => verification,
                };
                let verification = verification
                    .with_predicates(vec!["non_empty".to_string(), "json_parses".to_string()]);
                Some(assemble_binding(
                    MechanismClass::StructuredCodeOp,
                    "structured-code-op://program".to_string(),
                    verification,
                    "compute/transform intent binds deterministic structured-code execution"
                        .to_string(),
                ))
            }
            StructuredIntent::Retrieve if !request.membrane_bindings.is_empty() => {
                // LLM+P shape: answer derived from authoritative sources and
                // verified by coverage, not by model judgment.
                let verification = VerificationContract::default()
                    .with_source_coverage(
                        request
                            .membrane_bindings
                            .iter()
                            .map(|binding| binding.source_ref.clone())
                            .collect(),
                    )
                    .with_predicates(vec!["non_empty".to_string()]);
                Some(assemble_binding(
                    MechanismClass::SourceResolver,
                    "source-resolver://coverage".to_string(),
                    verification,
                    "retrieve intent binds authoritative source resolution".to_string(),
                ))
            }
            StructuredIntent::Retrieve => Some(
                self.terminal_decision(
                    request,
                    request_fingerprint,
                    revision,
                    MechanismOutcome::Insufficient,
                    "retrieve intent carries no authoritative source binding; deterministic \
                 resolution cannot retrieve"
                        .to_string(),
                ),
            ),
            StructuredIntent::Mutate | StructuredIntent::Communicate => {
                // Mutating/communicating work needs an authority-carrying
                // executor; with no capability match, deterministic resolution
                // stops here.
                Some(self.terminal_decision(
                    request,
                    request_fingerprint,
                    revision,
                    MechanismOutcome::Insufficient,
                    format!(
                        "{:?} intent needs an authority-carrying executor and none was matched",
                        signals.intent
                    ),
                ))
            }
            StructuredIntent::Decide | StructuredIntent::Unknown => None,
        }
    }

    async fn classify_last(
        &self,
        request: &MechanismRequest,
        request_fingerprint: &str,
        revision: &str,
        descriptors: &[CapabilityDescriptor],
        classifier: Option<&dyn MechanismClassifier>,
    ) -> MechanismDecision {
        let insufficient = |reason: String| {
            self.terminal_decision(
                request,
                request_fingerprint,
                revision,
                MechanismOutcome::Insufficient,
                reason,
            )
        };

        let Some(classifier) = classifier else {
            return insufficient(
                "no classifier configured and deterministic resolution found no mechanism; \
                 a semantic model is needed"
                    .to_string(),
            );
        };

        let proposal: ClassifiedMechanism = match classifier.classify(request).await {
            Ok(proposal) => proposal,
            Err(ClassifierError::Unavailable(message)) => {
                return insufficient(format!(
                    "classifier unavailable: {message}; deterministic resolution found no mechanism"
                ));
            }
        };

        let confidence = proposal.confidence.clamp(0.0, 1.0);
        match proposal.class {
            // Model needed: hand off to model routing without binding.
            MechanismClass::SemanticModel => insufficient(format!(
                "classifier proposes a semantic model (confidence {confidence:.2}): {}",
                proposal.reason
            )),
            // Fail-closed: heuristics may never summon a human or declare a
            // mechanism unsupported.
            MechanismClass::Human | MechanismClass::Unsupported => insufficient(format!(
                "classifier proposal `{}` is not actionable without an explicit signal; \
                 deterministic resolution found no mechanism",
                proposal.class.as_str()
            )),
            class if class.is_deterministic() => {
                let bound = descriptors
                    .iter()
                    .find(|descriptor| descriptor.class_hint == Some(class))
                    .map(|descriptor| descriptor.name.clone());
                let Some(capability) = bound else {
                    return insufficient(format!(
                        "classifier proposes `{}` but no executor binds; deterministic \
                         resolution found no mechanism",
                        class.as_str()
                    ));
                };
                let binding = Some(ExecutionRequirement {
                    binding: ResolvedBinding {
                        mechanism_class: class,
                        capability: Some(capability.clone()),
                        binding_ref: format!("tool://{capability}"),
                        authority_revision: revision.to_string(),
                    },
                    verification: VerificationContract::default(),
                    required_capabilities: vec![capability],
                });
                MechanismDecision::assemble(
                    request_fingerprint.to_string(),
                    request.node_id.clone(),
                    &request.scope,
                    ResolutionStage::BoundedClassifier,
                    MechanismOutcome::Completed,
                    format!("classifier proposal bound `{class}` executor"),
                    binding,
                    Vec::new(),
                    Some(avoidance(ResolutionStage::BoundedClassifier, class)),
                    None,
                    revision.to_string(),
                    request.membrane_bindings.clone(),
                )
                .expect("completed decision without escalation is always valid")
            }
            _ => insufficient(
                "classifier proposal cannot be honored; deterministic resolution found no \
                 mechanism"
                    .to_string(),
            ),
        }
    }

    // ── helpers ─────────────────────────────────────────────────────────

    fn terminal_decision(
        &self,
        request: &MechanismRequest,
        request_fingerprint: &str,
        revision: &str,
        outcome: MechanismOutcome,
        reason: String,
    ) -> MechanismDecision {
        MechanismDecision::assemble(
            request_fingerprint.to_string(),
            request.node_id.clone(),
            &request.scope,
            ResolutionStage::Terminal,
            outcome,
            reason,
            None,
            Vec::new(),
            None,
            None,
            revision.to_string(),
            request.membrane_bindings.clone(),
        )
        .expect("terminal decision without escalation is always valid")
    }
}

fn avoidance(stage: ResolutionStage, class: MechanismClass) -> AvoidanceReceipt {
    AvoidanceReceipt {
        stage,
        mechanism_class: class,
        reason: format!(
            "model routing avoided by deterministic {} binding",
            class.as_str()
        ),
    }
}

/// Ambiguity guard: when descriptors sharing one name carry conflicting class
/// hints (a registry anomaly), the resolver refuses to pick and returns the
/// candidate set. `expected` pins the guard to explicit bindings.
fn ambiguous_if_class_conflict(
    matches: &[&CapabilityDescriptor],
    name: &str,
    expected: Option<MechanismClass>,
) -> Option<Vec<ResolvedBinding>> {
    let hints: Vec<MechanismClass> = matches
        .iter()
        .filter_map(|descriptor| descriptor.class_hint)
        .collect();
    let conflicting = match expected {
        Some(expected) => hints.iter().any(|hint| *hint != expected),
        None => {
            hints
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                > 1
        }
    };
    if !conflicting {
        return None;
    }
    Some(
        matches
            .iter()
            .map(|descriptor| ResolvedBinding {
                mechanism_class: descriptor.class_hint.unwrap_or(MechanismClass::Scripted),
                capability: Some(name.to_string()),
                binding_ref: format!("tool://{name}"),
                authority_revision: String::new(),
            })
            .collect(),
    )
}

fn required_keys(schema: &Value) -> Result<Vec<String>, String> {
    let Some(required) = schema.get("required") else {
        return Ok(Vec::new());
    };
    let Some(required) = required.as_array() else {
        return Err("schema `required` is not an array".to_string());
    };
    required
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| "schema `required` entries must be strings".to_string())
        })
        .collect()
}
