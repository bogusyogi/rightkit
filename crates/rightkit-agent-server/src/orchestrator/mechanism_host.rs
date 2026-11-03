// Live conductor seam for deterministic mechanism resolution.
//
// Capability descriptors are projected from the daemon's existing default
// tool registry. This deliberately owns no second capability registry.

use rightkit_agent_orchestration::coding::mechanism::{
    CapabilityDescriptor, CapabilitySource, MechanismDecision, MechanismRequest, MechanismResolver,
    StaticCapabilitySource, StructuredSignals,
};

fn conductor_capability_source() -> StaticCapabilitySource {
    let capabilities = rightkit_agent_tools::coding_tools::default_tool_capabilities();
    let descriptors = capabilities
        .iter()
        .map(|capability| {
            CapabilityDescriptor::from_registry_projection(
                &capability.name,
                &capability.description,
                &capability.category,
                capability.read_only,
            )
        })
        .collect();
    let material = capabilities
        .iter()
        .map(|capability| {
            format!(
                "{}\0{}\0{}\0{}",
                capability.name, capability.description, capability.category, capability.read_only,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let revision = format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(material.as_bytes())
    );
    StaticCapabilitySource::new(revision, descriptors)
}

/// Resolve a root conductor goal against the live tool-registry projection.
///
/// Unknown goals remain `Insufficient`, which is the only outcome permitted
/// to enter model routing. Any terminal decision is handled by caller without
/// falling through to a model worker.
async fn resolve_conductor_mechanism(session_id: &str, goal: &str) -> MechanismDecision {
    let capabilities = conductor_capability_source();
    let mut request = MechanismRequest::root(
        format!("conductor-mechanism-{session_id}"),
        session_id,
        goal,
    );
    request.observed_capability_revision = capabilities.revision().to_string();
    MechanismResolver::new()
        .resolve(&request, &capabilities, None)
        .await
}

async fn resolve_conductor_child_mechanism(
    session_id: &str,
    task: &WorkerTask,
    parent: &MechanismDecision,
) -> MechanismDecision {
    let capabilities = conductor_capability_source();
    // Inherit exactly what the parent denied: a root-level `Denied` must
    // still be terminal for every child under it, never merely for the root
    // task itself. `task.agent` doubles as the child's exact-tool signal —
    // when Conduct decomposition assigns a task directly to a capability
    // name (a builtin/scripted executor role rather than a generic model
    // persona), this lets the deterministic exact-capability-match stage
    // bind (or the authority gate deny) without ever reaching the
    // classifier; an agent name that names no registered capability simply
    // finds no match and resolution proceeds to the next stage as before.
    let mut request = MechanismRequest::child_of(
        format!("conductor-mechanism-{session_id}-{}", task.task_id),
        &task.task_id,
        &task.payload,
        &parent.decision_fingerprint,
        parent.denied_capabilities.clone(),
    )
    .with_signals(StructuredSignals::default().with_exact_tool_name(task.agent.clone()));
    request.observed_capability_revision = capabilities.revision().to_string();
    MechanismResolver::new()
        .resolve(&request, &capabilities, None)
        .await
}

fn publish_conductor_mechanism_receipt(
    session: &LiveSession,
    session_id: &str,
    parent_decision_fingerprint: Option<&str>,
    decision: &MechanismDecision,
) -> Result<(), String> {
    let receipt = serde_json::json!({
        "parent_decision_fingerprint": parent_decision_fingerprint,
        "decision": decision,
    });
    let receipt = serde_json::to_string(&receipt)
        .map_err(|error| format!("serialize conductor mechanism receipt: {error}"))?;
    push_artifact(
        session,
        ProtocolArtifact {
            id: format!("art-{}", now_millis()),
            path: String::new(),
            kind: "mechanism_decision".into(),
            content_hash: content_hash(&receipt),
            shippable: false,
            trust_label: "unreviewed".to_string(),
            producer: "mechanism_resolver".to_string(),
            provenance: session_id.to_string(),
            verdict: None,
            approvals: vec![],
            meta: truncate(&receipt, 400),
            created_at: now_iso(),
        },
    );
    Ok(())
}

/// Resolves each Conduct child before its model worker can start.
///
/// The child request inherits its parent's decision fingerprint. Terminal
/// results never reach `SpawnWorker`; a deterministic executor host is
/// required before `Completed` can become a successful child result.
struct ConductorMechanismWorker {
    inner: Arc<dyn Worker>,
    state: DaemonState,
    session_id: String,
    root_decision: MechanismDecision,
}

#[async_trait::async_trait]
impl Worker for ConductorMechanismWorker {
    async fn run(&self, task: &WorkerTask) -> WorkerResult {
        let decision =
            resolve_conductor_child_mechanism(&self.session_id, task, &self.root_decision).await;
        if let Some(session) = self.state.get_session(&self.session_id) {
            if let Err(error) = publish_conductor_mechanism_receipt(
                &session,
                &self.session_id,
                Some(&self.root_decision.decision_fingerprint),
                &decision,
            ) {
                return mechanism_receipt_failure(task, error);
            }
        } else {
            return mechanism_receipt_failure(
                task,
                "session not found for mechanism receipt".into(),
            );
        }

        // Persist the terminal decision on the task result. The conductor's
        // checkpoint store records WorkerResult output, so replay can retain
        // the exact decision identity/outcome instead of reinterpreting a
        // child as model-eligible. Denied is called out explicitly: policy
        // denial is terminal and must never fall through to SpawnWorker.
        if mechanism_decision_is_terminal(&decision) {
            return mechanism_terminal_result(task, &decision);
        }

        let binding = match serde_json::to_string(&serde_json::json!({
            "parent_decision_fingerprint": &self.root_decision.decision_fingerprint,
            "decision": &decision,
        })) {
            Ok(binding) => binding,
            Err(error) => return mechanism_receipt_failure(task, error.to_string()),
        };
        let mut resolved_task = task.clone();
        resolved_task.payload = format!("{}\n\n[MECHANISM_DECISION]\n{}", task.payload, binding,);
        self.inner.run(&resolved_task).await
    }
}

/// Whether a resolved child mechanism decision must stop here, before the
/// conductor ever reaches the model-routed [`SpawnWorker`]. `Denied` is
/// called out explicitly per canon PL-010: a policy denial is terminal and
/// must never fall through to model routing, independent of whatever
/// `needs_model()` would otherwise report for that outcome.
///
/// `Completed` (a deterministic mechanism was bound) is NOT terminal here: no
/// deterministic executor exists yet, so failing the child with
/// `success:false` would lose work. It falls through to the model, with the
/// decision receipt already published and bound into the task payload, until
/// an executor host can run the bound mechanism.
fn mechanism_decision_is_terminal(decision: &MechanismDecision) -> bool {
    use rightkit_agent_orchestration::coding::mechanism::MechanismOutcome;
    decision.outcome == MechanismOutcome::Denied
        || (!decision.needs_model() && decision.outcome != MechanismOutcome::Completed)
}

fn mechanism_receipt_failure(task: &WorkerTask, error: String) -> WorkerResult {
    WorkerResult {
        task_id: task.task_id.clone(),
        agent: task.agent.clone(),
        success: false,
        output: format!("mechanism resolution receipt failed: {error}"),
        input_tokens: 0,
        mop_fired: false,
        failure_kind: Some(rightkit_agent_orchestration::coding::FailureKind::Unknown),
        agent_version: task.agent_version.clone(),
        rollback_step: None,
        outcome: WorkerOutcome::ApiError {
            error,
            partial_output: None,
        },
    }
}

fn mechanism_terminal_result(task: &WorkerTask, decision: &MechanismDecision) -> WorkerResult {
    WorkerResult {
        task_id: task.task_id.clone(),
        agent: task.agent.clone(),
        success: false,
        output: format!(
            "[MECHANISM_DECISION fingerprint={} outcome={}]
mechanism resolution ended terminally before model routing: {}",
            decision.decision_fingerprint,
            decision.outcome.as_str(),
            decision.reason
        ),
        input_tokens: 0,
        mop_fired: false,
        failure_kind: Some(rightkit_agent_orchestration::coding::FailureKind::Unknown),
        agent_version: task.agent_version.clone(),
        rollback_step: None,
        outcome: WorkerOutcome::ApiError {
            error: decision.reason.clone(),
            partial_output: None,
        },
    }
}
