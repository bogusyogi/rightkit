//! Focused cross-cutting tests for PL-010 mechanism resolution.
//!
//! Covers the canon invariants: deterministic resolution order (explicit
//! binding → exact capability match → structured signals → bounded classifier
//! last), the root-level exact fast path, per-child-node inheritance of root
//! denials, `Denied` never escalating to a model, typed outcomes, receipts
//! with fingerprints, deterministic verification of semantic output, and the
//! avoidance/escalation receipt shapes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde_json::json;

use super::*;

// ── test doubles ────────────────────────────────────────────────────────

/// Counts invocations so tests can pin "the classifier was never consulted"
/// and "the classifier ran last".
struct CountingClassifier {
    calls: AtomicUsize,
    proposal: Mutex<Option<ClassifiedMechanism>>,
    error: Option<String>,
}

impl CountingClassifier {
    fn proposing(class: MechanismClass) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            proposal: Mutex::new(Some(ClassifiedMechanism {
                class,
                confidence: 0.9,
                reason: "counting proposal".to_string(),
            })),
            error: None,
        }
    }

    fn failing(message: &str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            proposal: Mutex::new(None),
            error: Some(message.to_string()),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl MechanismClassifier for CountingClassifier {
    async fn classify(
        &self,
        _request: &MechanismRequest,
    ) -> Result<ClassifiedMechanism, ClassifierError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = &self.error {
            return Err(ClassifierError::Unavailable(message.clone()));
        }
        Ok(self.proposal.lock().unwrap().clone().unwrap())
    }
}

fn registry() -> StaticCapabilitySource {
    StaticCapabilitySource::new(
        "registry-gen-1",
        vec![
            CapabilityDescriptor::from_registry_projection(
                "search",
                "Search workspace content",
                "search",
                true,
            ),
            CapabilityDescriptor::from_registry_projection(
                "edit_file",
                "Edit a file",
                "file",
                false,
            )
            .with_class_hint(MechanismClass::Scripted),
            CapabilityDescriptor::from_registry_projection(
                "shell",
                "Run a shell command",
                "shell",
                false,
            )
            .with_class_hint(MechanismClass::Process),
        ],
    )
}

fn empty_registry() -> StaticCapabilitySource {
    StaticCapabilitySource::new("registry-gen-empty", Vec::new())
}

fn resolver() -> MechanismResolver {
    MechanismResolver::new()
}

// ── resolution order & fast path ────────────────────────────────────────

#[tokio::test]
async fn root_exact_fast_path_binds_explicit_tool_without_classifier() {
    let classifier = CountingClassifier::proposing(MechanismClass::SemanticModel);
    let request = MechanismRequest::root("req-1", "root", "fix the typo in src/lib.rs")
        .with_signals(StructuredSignals::default().with_exact_tool_name("edit_file"));
    let decision = resolver()
        .resolve(&request, &registry(), Some(&classifier))
        .await;

    assert_eq!(decision.outcome, MechanismOutcome::Completed);
    assert_eq!(decision.stage, ResolutionStage::RootExactFastPath);
    assert_eq!(
        classifier.calls(),
        0,
        "fast path must not consult the classifier"
    );
    let binding = &decision.execution_requirement.as_ref().unwrap().binding;
    assert_eq!(binding.capability.as_deref(), Some("edit_file"));
    assert_eq!(binding.mechanism_class, MechanismClass::Scripted);
    assert!(!decision.needs_model());
    let avoidance = decision.avoidance.as_ref().unwrap();
    assert_eq!(avoidance.stage, ResolutionStage::RootExactFastPath);
}

#[tokio::test]
async fn root_exact_fast_path_binds_explicit_binding_without_classifier() {
    let classifier = CountingClassifier::proposing(MechanismClass::SemanticModel);
    let request = MechanismRequest::root("req-2", "root", "run the release script")
        // The declared class must agree with the registry's hint for `shell`
        // (Process); a conflicting class is correctly reported Ambiguous, which
        // is a different behavior than this fast-path test covers.
        .with_explicit_binding(ExplicitBinding::new(
            MechanismClass::Process,
            Some("shell".to_string()),
            "script://release",
        ));
    let decision = resolver()
        .resolve(&request, &registry(), Some(&classifier))
        .await;

    assert_eq!(decision.outcome, MechanismOutcome::Completed);
    assert_eq!(decision.stage, ResolutionStage::RootExactFastPath);
    assert_eq!(classifier.calls(), 0);
    let binding = &decision.execution_requirement.as_ref().unwrap().binding;
    assert_eq!(binding.binding_ref, "script://release");
    assert!(decision.avoidance.is_some());
}

#[tokio::test]
async fn exact_capability_match_satisfies_authority_or_denies() {
    // Mutating requirement binds the mutating descriptor deterministically.
    let request = MechanismRequest::root("req-3", "root", "edit the file").with_requirements(vec![
        CapabilityRequirement::new("edit_file", AuthorityNeed::Mutate),
    ]);
    let decision = resolver().resolve(&request, &registry(), None).await;
    assert_eq!(decision.outcome, MechanismOutcome::Completed);
    assert_eq!(decision.stage, ResolutionStage::ExactCapabilityMatch);
    assert!(!decision.needs_model());
    assert!(decision.avoidance.is_some());

    // The same mutating requirement against a read-only descriptor is an
    // authority denial: terminal, never a model fallback.
    let request = MechanismRequest::root("req-4", "root", "edit the file").with_requirements(vec![
        CapabilityRequirement::new("search", AuthorityNeed::Mutate),
    ]);
    let decision = resolver().resolve(&request, &registry(), None).await;
    assert_eq!(decision.outcome, MechanismOutcome::Denied);
    assert!(!decision.needs_model());
    assert!(decision.execution_requirement.is_none());
}

#[tokio::test]
async fn child_scope_inherits_root_denial_without_consulting_registry() {
    let classifier = CountingClassifier::proposing(MechanismClass::SemanticModel);
    let request = MechanismRequest::child_of(
        "req-5",
        "child-1",
        "run the denied command",
        "sha256:parent-decision",
        vec!["shell".to_string()],
    )
    .with_requirements(vec![CapabilityRequirement::new(
        "shell",
        AuthorityNeed::Execute,
    )]);
    // The empty registry proves the decision came from inheritance alone.
    let decision = resolver()
        .resolve(&request, &empty_registry(), Some(&classifier))
        .await;

    assert_eq!(decision.outcome, MechanismOutcome::Denied);
    assert_eq!(decision.stage, ResolutionStage::AuthorityGate);
    assert!(!decision.needs_model());
    assert!(decision.reason.contains("never escalate"));
    assert!(!decision.scope_is_root);
}

#[tokio::test]
async fn denied_decision_records_denied_capability_names_for_child_inheritance() {
    // A root/parent `Denied` decision must expose exactly which capability
    // names it denied so a per-child mechanism host can thread them into the
    // next request's `inherited_denials` (`RequestScope::Child`) without
    // parsing `reason` prose.
    let request = MechanismRequest::root("req-25", "root", "run the forbidden thing")
        .denying(vec!["shell".to_string()])
        .with_requirements(vec![CapabilityRequirement::new(
            "shell",
            AuthorityNeed::Execute,
        )]);
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Denied);
    assert_eq!(decision.denied_capabilities, vec!["shell".to_string()]);

    // Every other outcome carries no denied capabilities.
    let clean = MechanismRequest::root("req-26", "root", "edit the file").with_requirements(vec![
        CapabilityRequirement::new("edit_file", AuthorityNeed::Mutate),
    ]);
    let decision = resolver().resolve(&clean, &registry(), None).await;
    assert_eq!(decision.outcome, MechanismOutcome::Completed);
    assert!(decision.denied_capabilities.is_empty());
}

#[tokio::test]
async fn structured_compute_signal_binds_structured_code_op_with_schema_contract() {
    let schema = json!({"type": "object", "required": ["result"]});
    let request = MechanismRequest::root("req-6", "root", "compute the totals").with_signals(
        StructuredSignals::default()
            .with_intent(StructuredIntent::Compute)
            .with_output_schema(schema.clone()),
    );
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Completed);
    assert_eq!(decision.stage, ResolutionStage::StructuredSignals);
    let requirement = decision.execution_requirement.as_ref().unwrap();
    assert_eq!(
        requirement.binding.mechanism_class,
        MechanismClass::StructuredCodeOp
    );
    assert_eq!(requirement.verification.output_schema, Some(schema));
    assert!(requirement
        .verification
        .deterministic_predicates
        .contains(&"json_parses".to_string()));
    assert!(!decision.needs_model());
}

#[tokio::test]
async fn retrieve_signal_binds_source_resolver_to_membrane_sources() {
    let request = MechanismRequest::root("req-7", "root", "what changed in lib.rs")
        .with_signals(StructuredSignals::default().with_intent(StructuredIntent::Retrieve))
        .with_membrane_bindings(vec![MembraneBinding::new(
            MembraneSurface::SourceRead,
            "repo://src/lib.rs",
            "auth-rev-3",
        )]);
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Completed);
    let requirement = decision.execution_requirement.as_ref().unwrap();
    assert_eq!(
        requirement.binding.mechanism_class,
        MechanismClass::SourceResolver
    );
    assert_eq!(
        requirement.verification.require_source_coverage,
        vec!["repo://src/lib.rs".to_string()]
    );
}

#[tokio::test]
async fn conflicting_class_hints_return_ambiguous() {
    // Registry anomaly: one name, two descriptors with different hints. The
    // resolver refuses to pick silently.
    let source = StaticCapabilitySource::new(
        "registry-gen-1",
        vec![
            CapabilityDescriptor::from_registry_projection("run", "", "misc", false)
                .with_class_hint(MechanismClass::Process),
            CapabilityDescriptor::from_registry_projection("run", "", "misc", false)
                .with_class_hint(MechanismClass::Scripted),
        ],
    );
    let request = MechanismRequest::root("req-8", "root", "run it").with_requirements(vec![
        CapabilityRequirement::new("run", AuthorityNeed::Execute),
    ]);
    let decision = resolver().resolve(&request, &source, None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Ambiguous);
    assert_eq!(decision.candidates.len(), 2);
    assert!(!decision.needs_model());
}

#[tokio::test]
async fn stale_observed_revision_reports_stale() {
    let request = MechanismRequest::root("req-9", "root", "anything")
        .with_observed_capability_revision("registry-gen-0");
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Stale);
    assert_eq!(decision.stage, ResolutionStage::Terminal);
    assert_eq!(
        decision.resolved_against_revision, "registry-gen-1",
        "the receipt stamps the live revision it refused to bind against"
    );
}

#[tokio::test]
async fn unsupported_when_binding_names_missing_capability() {
    let request = MechanismRequest::child_of(
        "req-10",
        "child",
        "use the missing tool",
        "sha256:p",
        Vec::new(),
    )
    .with_explicit_binding(ExplicitBinding::new(
        MechanismClass::Scripted,
        Some("nonexistent_tool".to_string()),
        "script://x",
    ));
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Unsupported);
    assert!(!decision.needs_model());
}

// ── bounded classifier last ─────────────────────────────────────────────

#[tokio::test]
async fn bounded_classifier_runs_last_and_cannot_bind_without_executor() {
    // Deterministic stages bind nothing (no requirements, unknown intent).
    // `Builtin` has no hinted executor in the registry, so the proposal
    // cannot fabricate a binding.
    let classifier = CountingClassifier::proposing(MechanismClass::Builtin);
    let request = MechanismRequest::root("req-11", "root", "mystery work");
    let decision = resolver()
        .resolve(&request, &registry(), Some(&classifier))
        .await;

    assert_eq!(
        classifier.calls(),
        1,
        "classifier runs only after deterministic stages"
    );
    assert_eq!(decision.outcome, MechanismOutcome::Insufficient);
    assert!(
        decision.reason.contains("no executor binds"),
        "a proposal without an executor must not fabricate a binding: {}",
        decision.reason
    );
    assert!(
        decision.needs_model(),
        "insufficient hands off to model routing"
    );

    // A human proposal is never actionable from a heuristic.
    let classifier = CountingClassifier::proposing(MechanismClass::Human);
    let decision = resolver()
        .resolve(&request, &registry(), Some(&classifier))
        .await;
    assert_eq!(decision.outcome, MechanismOutcome::Insufficient);
    assert!(!decision.reason.contains("human executor bound"));
}

#[tokio::test]
async fn classifier_proposal_binds_only_when_executor_exists() {
    let classifier = CountingClassifier::proposing(MechanismClass::Process);
    let request = MechanismRequest::root("req-12", "root", "mystery process work");
    let decision = resolver()
        .resolve(&request, &registry(), Some(&classifier))
        .await;

    assert_eq!(decision.outcome, MechanismOutcome::Completed);
    assert_eq!(decision.stage, ResolutionStage::BoundedClassifier);
    let binding = &decision.execution_requirement.as_ref().unwrap().binding;
    assert_eq!(binding.capability.as_deref(), Some("shell"));
    assert_eq!(binding.mechanism_class, MechanismClass::Process);
    assert!(decision.avoidance.is_some());
}

#[tokio::test]
async fn classifier_unavailable_yields_insufficient_with_reason() {
    let classifier = CountingClassifier::failing("provider offline");
    let request = MechanismRequest::root("req-13", "root", "anything");
    let decision = resolver()
        .resolve(&request, &registry(), Some(&classifier))
        .await;

    assert_eq!(decision.outcome, MechanismOutcome::Insufficient);
    assert!(decision
        .reason
        .contains("classifier unavailable: provider offline"));
    assert!(decision.needs_model());
}

#[tokio::test]
async fn no_classifier_and_no_deterministic_mechanism_yields_insufficient() {
    let request = MechanismRequest::root("req-14", "root", "anything");
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Insufficient);
    assert!(decision.reason.contains("no classifier configured"));
    assert!(decision.needs_model());
}

// ── denials, humans & unsupported never reach a model ───────────────────

#[tokio::test]
async fn denied_outcome_never_escalates_or_needs_model() {
    let request = MechanismRequest::root("req-15", "root", "run the forbidden thing")
        .denying(vec!["shell".to_string()])
        .with_requirements(vec![CapabilityRequirement::new(
            "shell",
            AuthorityNeed::Execute,
        )]);
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Denied);
    assert_eq!(decision.stage, ResolutionStage::AuthorityGate);
    assert!(!decision.needs_model());
    assert!(decision.escalation.is_none());
    assert!(decision.execution_requirement.is_none());

    // Escalating a denial is a typed error, not a fallback.
    let error = resolver()
        .escalate_after_failure(
            decision,
            EscalationTrigger::DeterministicBindingFailed,
            None,
        )
        .unwrap_err();
    assert_eq!(error, MechanismDecisionError::DeniedWithEscalation);
}

#[tokio::test]
async fn human_required_only_from_explicit_binding_or_signal() {
    // Explicit signal.
    let request = MechanismRequest::root("req-16", "root", "deploy to production")
        .with_signals(StructuredSignals::default().requires_human_approval());
    let decision = resolver().resolve(&request, &registry(), None).await;
    assert_eq!(decision.outcome, MechanismOutcome::HumanRequired);
    assert!(!decision.needs_model());

    // Explicit binding.
    let request = MechanismRequest::root("req-17", "root", "human review").with_explicit_binding(
        ExplicitBinding::new(MechanismClass::Human, None, "human://review"),
    );
    let decision = resolver().resolve(&request, &registry(), None).await;
    assert_eq!(decision.outcome, MechanismOutcome::HumanRequired);
    let binding = &decision.execution_requirement.as_ref().unwrap().binding;
    assert_eq!(binding.mechanism_class, MechanismClass::Human);
    assert!(!decision.needs_model());

    // A requirement naming human approval also parks for a human.
    let request = MechanismRequest::root("req-18", "root", "approve this").with_requirements(vec![
        CapabilityRequirement::new("shell", AuthorityNeed::HumanApproval),
    ]);
    let decision = resolver().resolve(&request, &registry(), None).await;
    assert_eq!(decision.outcome, MechanismOutcome::HumanRequired);
}

#[tokio::test]
async fn secret_authority_is_never_bound_by_resolution() {
    let request =
        MechanismRequest::root("req-19", "root", "read the credentials").with_requirements(vec![
            CapabilityRequirement::new("shell", AuthorityNeed::Secret),
        ]);
    let decision = resolver().resolve(&request, &registry(), None).await;
    assert_eq!(decision.outcome, MechanismOutcome::Denied);
    assert!(!decision.needs_model());
}

// ── fingerprints ────────────────────────────────────────────────────────

#[tokio::test]
async fn fingerprints_are_deterministic_and_payload_sensitive() {
    let request = MechanismRequest::root("req-20", "root", "fix the typo").with_requirements(vec![
        CapabilityRequirement::new("edit_file", AuthorityNeed::Mutate),
    ]);
    let first = resolver().resolve(&request, &registry(), None).await;
    let second = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(first.request_fingerprint, second.request_fingerprint);
    assert_eq!(first.decision_fingerprint, second.decision_fingerprint);
    assert!(first.request_fingerprint.starts_with("sha256:"));
    assert!(first.decision_fingerprint.starts_with("sha256:"));

    let other = resolver()
        .resolve(
            &MechanismRequest::root("req-20", "root", "a different task"),
            &registry(),
            None,
        )
        .await;
    assert_ne!(first.request_fingerprint, other.request_fingerprint);
    assert_eq!(first.schema_version, MECHANISM_DECISION_SCHEMA_VERSION);
}

// ── semantic output verification ────────────────────────────────────────

#[test]
fn semantic_output_verification_is_deterministic_and_fail_closed() {
    let resolver = resolver();
    let contract = VerificationContract::default()
        .with_output_schema(json!({"type": "object", "required": ["answer"]}))
        .with_source_coverage(vec!["repo://src/lib.rs".to_string()])
        .with_predicates(vec![
            "non_empty".to_string(),
            "json_parses".to_string(),
            "made_up_predicate".to_string(),
        ]);

    // Unknown predicate fails closed even for a perfect answer.
    let output = SemanticOutput {
        text: "the answer".to_string(),
        json: Some(json!({"answer": 42})),
        consulted_source_refs: vec!["repo://src/lib.rs".to_string()],
    };
    let verification = resolver.verify_semantic_output(&contract, &output);
    assert!(!verification.passed);
    assert!(verification
        .failures
        .iter()
        .any(|failure| failure.contains("unknown deterministic predicate `made_up_predicate`")));

    // Missing source coverage fails: a semantic answer never replaces a
    // bound source.
    let output = SemanticOutput {
        text: "the answer".to_string(),
        json: Some(json!({"answer": 42})),
        consulted_source_refs: Vec::new(),
    };
    let verification = resolver.verify_semantic_output(&contract, &output);
    assert!(!verification.passed);
    assert!(verification
        .failures
        .iter()
        .any(|failure| failure.contains("repo://src/lib.rs")));

    // Missing required schema key fails.
    let output = SemanticOutput {
        text: "the answer".to_string(),
        json: Some(json!({"wrong": 1})),
        consulted_source_refs: vec!["repo://src/lib.rs".to_string()],
    };
    let verification = resolver.verify_semantic_output(&contract, &output);
    assert!(!verification.passed);
    assert!(verification
        .failures
        .iter()
        .any(|failure| failure.contains("`answer` missing")));

    // A conforming answer with the unknown predicate removed passes.
    let contract = VerificationContract::default()
        .with_output_schema(json!({"type": "object", "required": ["answer"]}))
        .with_source_coverage(vec!["repo://src/lib.rs".to_string()])
        .with_predicates(vec!["non_empty".to_string(), "json_parses".to_string()]);
    let output = SemanticOutput {
        text: "the answer".to_string(),
        json: Some(json!({"answer": 42})),
        consulted_source_refs: vec!["repo://src/lib.rs".to_string()],
    };
    let verification = resolver.verify_semantic_output(&contract, &output);
    assert_eq!(verification.failures, Vec::<String>::new());
    assert!(verification.passed);

    // The size bound is enforced.
    let bounded = VerificationContract::default().with_max_output_chars(4);
    let output = SemanticOutput {
        text: "too long".to_string(),
        json: None,
        consulted_source_refs: Vec::new(),
    };
    let verification = resolver.verify_semantic_output(&bounded, &output);
    assert!(!verification.passed);
    assert!(verification
        .failures
        .iter()
        .any(|failure| failure.contains("exceeds contract bound 4")));
    assert_eq!(DEFAULT_MAX_SEMANTIC_OUTPUT_CHARS, 100_000);
}

#[test]
fn semantic_contract_derives_from_request_signals_and_bindings() {
    let request = MechanismRequest::root("req-21", "root", "answer from sources")
        .with_signals(
            StructuredSignals::default()
                .with_intent(StructuredIntent::Retrieve)
                .with_output_schema(json!({"type": "object", "required": ["summary"]})),
        )
        .with_membrane_bindings(vec![
            MembraneBinding::new(MembraneSurface::SourceRead, "repo://a.rs", "rev-1"),
            MembraneBinding::new(MembraneSurface::Ledger, "ledger://facts", "rev-1"),
        ]);
    let contract = MechanismResolver::semantic_verification_contract(&request);
    assert_eq!(
        contract.require_source_coverage,
        vec!["repo://a.rs".to_string(), "ledger://facts".to_string()]
    );
    assert!(contract.output_schema.is_some());
    assert!(contract
        .deterministic_predicates
        .contains(&"json_parses".to_string()));
}

// ── bounded escalation after failure ────────────────────────────────────

#[tokio::test]
async fn escalation_after_failure_is_bounded_to_one() {
    // A structured-code-op binding whose output then fails verification:
    // the decision becomes Failed and escalates exactly once.
    let schema = json!({"type": "object", "required": ["result"]});
    let request = MechanismRequest::root("req-22", "root", "compute the totals").with_signals(
        StructuredSignals::default()
            .with_intent(StructuredIntent::Compute)
            .with_output_schema(schema),
    );
    let bound = resolver().resolve(&request, &registry(), None).await;
    assert_eq!(bound.outcome, MechanismOutcome::Completed);

    // (The orchestrator executes the bound mechanism, verification fails, and
    // re-enters with a Failed receipt. Modeled here through the typed path.)
    let failed = MechanismDecision::assemble(
        bound.request_fingerprint.clone(),
        bound.node_id.clone(),
        &RequestScope::Root,
        ResolutionStage::Terminal,
        MechanismOutcome::Failed,
        "deterministic verification of the structured output failed".to_string(),
        bound.execution_requirement.clone(),
        Vec::new(),
        None,
        None,
        "registry-gen-1",
        Vec::new(),
    )
    .unwrap();

    let escalated = resolver()
        .escalate_after_failure(
            failed,
            EscalationTrigger::VerificationFailed,
            Some(MechanismClass::StructuredCodeOp),
        )
        .unwrap();
    assert_eq!(escalated.outcome, MechanismOutcome::Failed);
    assert!(
        escalated.needs_model(),
        "escalated failure routes to a model"
    );
    let receipt = escalated.escalation.as_ref().unwrap();
    assert_eq!(receipt.attempt, 1);
    assert_eq!(receipt.to_class, MechanismClass::SemanticModel);
    assert_eq!(receipt.request_fingerprint, bound.request_fingerprint);

    // The bound is exactly one: a second escalation is a typed error.
    let error = resolver()
        .escalate_after_failure(
            escalated,
            EscalationTrigger::DeterministicBindingFailed,
            None,
        )
        .unwrap_err();
    assert_eq!(error, MechanismDecisionError::AlreadyEscalated);
}

#[tokio::test]
async fn membrane_binding_without_authority_revision_fails_closed() {
    let request = MechanismRequest::root("req-23", "root", "answer from sources")
        .with_membrane_bindings(vec![MembraneBinding::new(
            MembraneSurface::Ledger,
            "ledger://facts",
            "",
        )]);
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Unsupported);
    assert!(
        decision.reason.contains("provider authority"),
        "fail-closed reason must name the authority violation: {}",
        decision.reason
    );
    assert!(!decision.needs_model());
}

#[tokio::test]
async fn explicit_semantic_model_binding_hands_off_to_model_routing() {
    let request =
        MechanismRequest::root("req-24", "root", "explain this design").with_explicit_binding(
            ExplicitBinding::new(MechanismClass::SemanticModel, None, "model://any"),
        );
    let decision = resolver().resolve(&request, &registry(), None).await;

    assert_eq!(decision.outcome, MechanismOutcome::Insufficient);
    assert!(decision.needs_model());
    assert!(decision.execution_requirement.is_none());
    assert!(
        decision.reason.contains("model routing"),
        "the receipt must name the handoff: {}",
        decision.reason
    );
}
