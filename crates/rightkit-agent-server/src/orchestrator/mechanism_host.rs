// Live conductor seam for deterministic mechanism resolution.
//
// Capability descriptors are projected from the daemon's existing default
// tool registry. This deliberately owns no second capability registry.
//
// PL-010 deterministic executor: the ROOT executor exists
// (`DaemonState::run_explicit_mechanism_goal`, a goal of the exact form `tool:`
// + a registered tool name + a space + a JSON object). The CHILD executor does
// not: a conductor child that resolves `Completed` still falls through to the
// model with its receipt.

use rightkit_agent_orchestration::coding::mechanism::{
    CapabilityDescriptor, CapabilitySource, MechanismDecision, MechanismRequest, MechanismResolver,
    StaticCapabilitySource, StructuredSignals,
};

fn conductor_capability_source() -> StaticCapabilitySource {
    let capabilities = rightkit_agent_coding_tools::default_tool_capabilities();
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

/// Parse the explicit mechanism form of a goal: `tool:` + a registered tool name
/// + a space + a JSON object. `Ok(None)` means an ordinary goal.
pub(crate) fn parse_explicit_mechanism_goal(
    goal: &str,
) -> Result<Option<(String, serde_json::Value)>, String> {
    let text = goal.trim();
    let Some(rest) = text.strip_prefix("tool:") else {
        return Ok(None);
    };
    let name_len = rest
        .char_indices()
        .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')))
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    let name = &rest[..name_len];
    if name.is_empty() {
        return Ok(None);
    }
    let remainder = &rest[name_len..];
    if !remainder.is_empty() && !remainder.starts_with(char::is_whitespace) {
        return Ok(None);
    }
    let input = if remainder.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str::<serde_json::Value>(remainder) {
            Ok(value) if value.is_object() => value,
            _ => return Err("explicit tool goal input must be a JSON object".to_string()),
        }
    };
    Ok(Some((name.to_string(), input)))
}

async fn resolve_explicit_tool_mechanism(
    session_id: &str,
    goal: &str,
    tool: &str,
) -> MechanismDecision {
    let capabilities = conductor_capability_source();
    let mut request = MechanismRequest::root(
        format!("conductor-mechanism-{session_id}"),
        session_id,
        goal,
    )
    .with_signals(StructuredSignals::default().with_exact_tool_name(tool.to_string()));
    request.observed_capability_revision = capabilities.revision().to_string();
    MechanismResolver::new()
        .resolve(&request, &capabilities, None)
        .await
}

fn publish_mechanism_execution_receipt(
    session: &LiveSession,
    session_id: &str,
    receipt: &serde_json::Value,
    reason: &str,
) {
    let text = serde_json::to_string(receipt).unwrap_or_else(|_| "{}".to_string());
    push_artifact(
        session,
        ProtocolArtifact {
            id: crate::daemon_state::next_artifact_id(),
            path: String::new(),
            kind: "mechanism_execution".into(),
            content_hash: content_hash(&text),
            shippable: false,
            trust_label: "unreviewed".to_string(),
            producer: "mechanism_executor".to_string(),
            provenance: session_id.to_string(),
            verdict: None,
            approvals: vec![],
            meta: mechanism_receipt_meta(&text, &content_hash(&text), reason),
            created_at: now_iso(),
        },
    );
}

impl DaemonState {
    /// PL-010 root deterministic executor. A goal `tool:<name> {json}` resolves
    /// through the mechanism resolver and runs only through the governed tool
    /// dispatch; no outcome other than `Completed` ever reaches a model.
    pub(crate) async fn run_explicit_mechanism_goal(
        &self,
        session_id: &str,
        goal: &str,
    ) -> Result<Option<RunOutcome>, String> {
        let Some((tool, input)) = parse_explicit_mechanism_goal(goal)? else {
            return Ok(None);
        };
        let session = self
            .get_session(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let conduit = session.conduit.clone();
        let decision = resolve_explicit_tool_mechanism(session_id, goal, &tool).await;
        publish_conductor_mechanism_receipt(&session, session_id, None, &decision)?;
        let bound = decision
            .execution_requirement
            .as_ref()
            .and_then(|requirement| requirement.binding.capability.clone());
        if decision.outcome
            != rightkit_agent_orchestration::coding::mechanism::MechanismOutcome::Completed
            || bound.as_deref() != Some(tool.as_str())
        {
            return Err(format!(
                "explicit tool goal '{tool}' did not resolve to a deterministic mechanism (outcome: {}): {}",
                decision.outcome.as_str(),
                decision.reason
            ));
        }
        let call_id = format!("mechanism-{}", rightkit_agent_engine_protocol::uuid_v7());
        let result = crate::api_drivers::governed_tool_call(
            self,
            session_id,
            call_id.clone(),
            tool.clone(),
            input,
        )
        .await;
        let contract = decision
            .execution_requirement
            .as_ref()
            .map(|requirement| requirement.verification.clone())
            .unwrap_or_default();
        let verification =
            rightkit_agent_orchestration::coding::mechanism::MechanismResolver::new()
                .verify_semantic_output(
                    &contract,
                    &rightkit_agent_orchestration::coding::mechanism::SemanticOutput {
                        text: result.content.clone(),
                        json: serde_json::from_str(&result.content).ok(),
                        consulted_source_refs: Vec::new(),
                    },
                );
        publish_mechanism_execution_receipt(
            &session,
            session_id,
            &serde_json::json!({
                "decision_fingerprint": decision.decision_fingerprint,
                "tool": tool,
                "call_id": call_id,
                "is_error": result.is_error,
                "verification_passed": verification.passed,
                "failures": verification.failures,
                "output_chars": result.content.chars().count(),
            }),
            "deterministic executor result",
        );
        if result.is_error || !verification.passed {
            let mut message = format!(
                "explicit tool goal '{tool}' failed: {}",
                truncate(&result.content, 400)
            );
            if !verification.failures.is_empty() {
                message.push_str(&format!(" ({})", verification.failures.join("; ")));
            }
            return Err(message);
        }
        let seq = conduit.next_seq();
        conduit.publish(ProtocolEvent::AssistantMessageCompleted {
            session_id: session_id.to_string(),
            seq,
            created_at: now_iso(),
            message: TranscriptMessage::Assistant {
                id: format!("assistant-{}", now_millis()),
                created_at: now_iso(),
                content: format!(
                    "Ran {tool} through the deterministic executor (decision {}); no model was called.\n\n{}",
                    decision.decision_fingerprint, result.content
                ),
                reasoning_content: None,
                tool_calls: Vec::new(),
                pinned: false,
                origin: None,
            },
            phase: rightkit_agent_engine_protocol::AssistantMessagePhase::Final,
        });
        self.finalize_goal(
            session_id,
            &conduit,
            rightkit_agent_orchestration::coding::CompletionState::Accepted,
        )
        .await;
        Ok(Some(RunOutcome {
            completed: true,
            results: Vec::new(),
            failed: Vec::new(),
            stall_nudge: None,
        }))
    }
}

/// Largest artifact `meta` a mechanism receipt may carry.
const MECHANISM_RECEIPT_META_BYTES: usize = 2048;

/// The artifact `meta` for a serialized mechanism receipt (audit C15). Clients
/// parse it, so it is always valid JSON: the whole receipt when it fits,
/// otherwise a small summary that names the receipt's size and content hash.
/// It is never a receipt cut off in the middle of an array or string.
fn mechanism_receipt_meta(receipt_json: &str, content_hash: &str, reason: &str) -> String {
    if receipt_json.len() <= MECHANISM_RECEIPT_META_BYTES {
        return receipt_json.to_string();
    }
    serde_json::json!({
        "truncated": true,
        "bytes": receipt_json.len(),
        "contentHash": content_hash,
        "reason": truncate(reason, 400),
    })
    .to_string()
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
            id: crate::daemon_state::next_artifact_id(),
            path: String::new(),
            kind: "mechanism_decision".into(),
            content_hash: content_hash(&receipt),
            shippable: false,
            trust_label: "unreviewed".to_string(),
            producer: "mechanism_resolver".to_string(),
            provenance: session_id.to_string(),
            verdict: None,
            approvals: vec![],
            meta: mechanism_receipt_meta(&receipt, &content_hash(&receipt), &decision.reason),
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
/// `Completed` (a deterministic mechanism was bound) is NOT terminal here: the
/// ROOT executor exists (`run_explicit_mechanism_goal`) but the child executor
/// does not, so failing the child with `success:false` would lose work. It
/// falls through to the model, with the decision receipt already published and
/// bound into the task payload, until a child executor host can run the bound
/// mechanism.
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
