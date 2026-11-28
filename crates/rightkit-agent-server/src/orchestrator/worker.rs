const CONDUCTOR_RUNTIME_MODEL_IDS: [&str; 3] =
    ["z-ai/glm-5.2", "moonshotai/kimi-k2.6", "minimax/minimax-m3"];

// ponytail: fixed catalog budget; Codex renders its tool catalog at ~2% of
// context — make this context-relative if the catalog ever outgrows it.
const CATALOG_BUDGET_CHARS: usize = 4000;

/// Precedence: the model the session was explicitly created with (any id) >
/// a live session model the conductor runtime supports > the configured
/// conductor descriptor model (`[orchestration.agents.conductor]` or role
/// default) > the engine default. A non-explicit live model is only the
/// workspace chat default, so it is trusted only when listed in
/// `CONDUCTOR_RUNTIME_MODEL_IDS`.
fn resolve_conductor_runtime_model(
    explicit_session_model: Option<&str>,
    live_session_model: &str,
    configured_conductor_model: &str,
    engine_default_model: Option<&str>,
) -> String {
    if let Some(explicit) = explicit_session_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        return explicit.to_string();
    }
    let live_session_model = live_session_model.trim();
    if CONDUCTOR_RUNTIME_MODEL_IDS.contains(&live_session_model) {
        return live_session_model.to_string();
    }

    let configured_conductor_model = configured_conductor_model.trim();
    if !configured_conductor_model.is_empty() {
        return configured_conductor_model.to_string();
    }

    engine_default_model.unwrap_or_default().trim().to_string()
}

/// Add one attempt's typed-mutation count to the run's per-task evidence map
/// that `finalize_goal` reads. Retries of one task accumulate; an attempt with
/// no mutations still creates the entry so the task is itemized.
fn record_task_typed_evidence(
    evidence: &std::sync::Mutex<std::collections::HashMap<String, u32>>,
    task_id: &str,
    typed_mutations: u32,
) {
    let mut map = evidence
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = map.entry(task_id.to_string()).or_insert(0);
    *entry = entry.saturating_add(typed_mutations);
}

/// A [`Worker`] that executes a task by spawning a real `SessionEngine` agent.
///
/// `task.agent` is the agent name/role; [`DaemonState::spawn`] resolves it to
/// the agent's model, persona, and tool ceiling, runs the agent loop on the
/// task payload, and returns its final output.
pub struct SpawnWorker {
    state: DaemonState,
    parent_session_id: String,
}

#[async_trait]
impl Worker for SpawnWorker {
    async fn run(&self, task: &WorkerTask) -> WorkerResult {
        let failure_kind_slot = crate::daemon_state::SubagentFailureKindSlot::default();
        // Allocate the child id up front so this task's typed mutations can be
        // read off the child session afterwards (audit 2026-11-05
        // orchestration H2: nothing wrote `task_typed_evidence`).
        let child_session_id = crate::daemon_state::next_subagent_id();
        let options = rightkit_agent_coding_tools::SubagentSpawnOptions {
            child_session_id: Some(child_session_id.clone()),
            ..Default::default()
        };
        let outcome = self
            .state
            .spawn_with_failure_kind(
                &task.agent,
                &task.payload,
                &self.parent_session_id,
                options,
                failure_kind_slot.clone(),
            )
            .await;
        // Work a failed attempt already landed is still workspace evidence, so
        // record on both outcomes.
        if let (Some(parent), Some(child)) = (
            self.state.get_session(&self.parent_session_id),
            self.state.get_session(&child_session_id),
        ) {
            let count = child
                .typed_mutations
                .load(std::sync::atomic::Ordering::Relaxed);
            record_task_typed_evidence(&parent.task_typed_evidence, &task.task_id, count);
        }
        let typed_failure_kind: Option<String> = failure_kind_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        match outcome {
            Ok(output) => {
                // Post-hoc MOP detection: analyze the output for signs of a stall.
                // If the output shows a tight loop pattern (same action repeated),
                // mark mop_fired as true.
                let mop_fired = detect_mop_in_output(&output);
                WorkerResult {
                    task_id: task.task_id.clone(),
                    agent: task.agent.clone(),
                    success: true,
                    output,
                    input_tokens: token_count(&task.payload) as u32 + 1500,
                    mop_fired,
                    failure_kind: None,
                    agent_version: task.agent_version.clone(),
                    rollback_step: None,
                    outcome: WorkerOutcome::Complete,
                }
            }
            Err(e) => {
                // A sub-agent cut off by a rate limit, wall-clock ceiling, or
                // budget/context ceiling has PARTIAL work, not a hard API
                // failure — classify via the same kind vocabulary the
                // engine's `RunFailed` event already uses (parity P-2), so
                // this never invents a second failure taxonomy. Anything
                // else (auth, network, provider 5xx, unclassified) is a
                // genuine unrecovered API error.
                let kind = subagent_error_kind_typed(typed_failure_kind.as_deref(), &e);
                let outcome = match kind {
                    SubagentErrorKind::ResourceLimit(limit_kind) => {
                        WorkerOutcome::PartialWithLimit { limit_kind }
                    }
                    SubagentErrorKind::ApiError => WorkerOutcome::ApiError {
                        error: e.clone(),
                        partial_output: None,
                    },
                };
                WorkerResult {
                    task_id: task.task_id.clone(),
                    agent: task.agent.clone(),
                    success: false,
                    output: e.clone(),
                    input_tokens: 0,
                    mop_fired: false,
                    failure_kind: Some(FailureKind::infer_from_output(&e, false, false)),
                    agent_version: task.agent_version.clone(),
                    rollback_step: None,
                    outcome,
                }
            }
        }
    }
}

/// Which bucket a sub-agent spawn error falls into: a resource ceiling that
/// merely cut the work off (rate limit, wall-clock, context/budget), or a
/// genuine unrecovered API/provider error. Reuses the engine core's
/// `classify_failure` keyword vocabulary — the same one `RunFailed.kind`
/// already surfaces — so this is one taxonomy, not two.
pub(crate) enum SubagentErrorKind {
    ResourceLimit(String),
    ApiError,
}

/// Prefix of the failure text published when a child's engine task panics.
/// A panic is a failed child, never a resource-limit cutoff, even when the
/// panic message happens to contain a limit keyword ("timeout", "429").
pub(crate) const SUBAGENT_PANIC_PREFIX: &str = "sub-agent task panicked";

/// Classify from the typed `RunFailed.kind` when the failure carried one;
/// keyword-match the text only as a fallback for legacy events or locally
/// synthesized failures that have no kind. A typed `other` is a typed
/// "not a limit": it is never re-derived from the message text.
pub(crate) fn subagent_error_kind_typed(kind: Option<&str>, error: &str) -> SubagentErrorKind {
    if error.starts_with(SUBAGENT_PANIC_PREFIX) {
        return SubagentErrorKind::ApiError;
    }
    match kind.map(str::trim) {
        Some("rate_limited") => SubagentErrorKind::ResourceLimit("rate_limited".into()),
        Some("timeout") => SubagentErrorKind::ResourceLimit("timeout".into()),
        Some("budget") => SubagentErrorKind::ResourceLimit("budget".into()),
        Some("wall_clock") => SubagentErrorKind::ResourceLimit("wall_clock".into()),
        Some("network" | "auth" | "provider_error" | "other") => SubagentErrorKind::ApiError,
        // Absent or unrecognized kind: legacy event, use the keyword fallback.
        _ => subagent_error_kind(error),
    }
}

pub(crate) fn subagent_error_kind(error: &str) -> SubagentErrorKind {
    if error.starts_with(SUBAGENT_PANIC_PREFIX) {
        return SubagentErrorKind::ApiError;
    }
    if error.contains("wall-clock ceiling") {
        return SubagentErrorKind::ResourceLimit("wall_clock".into());
    }
    match rightkit_agent_engine_core::classify_failure(error) {
        rightkit_agent_engine_core::FailureKind::RateLimited => {
            SubagentErrorKind::ResourceLimit("rate_limited".into())
        }
        rightkit_agent_engine_core::FailureKind::Timeout => {
            SubagentErrorKind::ResourceLimit("timeout".into())
        }
        rightkit_agent_engine_core::FailureKind::Budget => {
            SubagentErrorKind::ResourceLimit("budget".into())
        }
        rightkit_agent_engine_core::FailureKind::Network
        | rightkit_agent_engine_core::FailureKind::Auth
        | rightkit_agent_engine_core::FailureKind::ProviderError
        | rightkit_agent_engine_core::FailureKind::Other => SubagentErrorKind::ApiError,
    }
}

/// Detect whether an agent's output transcript shows a meltdown-onset-point stall.
/// Scans the output for repeating action patterns (tool calls, commands) that
/// indicate the agent is looping.
fn detect_mop_in_output(output: &str) -> bool {
    use rightkit_agent_engine_core::mop::{signature, MopDetector};
    use rightkit_agent_orchestration::coding::SequenceAnomalyDetector;

    // Extract action-like patterns from the transcript: tool calls, bash commands, etc.
    // Each line or action-like unit becomes a signature in the MOP detector.
    let mut detector = MopDetector::new(10, 3); // window of 10, trigger on 3+ repeats
    let mut sequence = SequenceAnomalyDetector::new(10);
    sequence.train_sequence(&[
        "read".to_string(),
        "edit".to_string(),
        "shell".to_string(),
        "verify".to_string(),
        "finalize".to_string(),
    ]);
    let mut recent_actions = Vec::new();

    // Simple heuristic: look for lines that look like actions (tool calls, commands).
    // Tool calls typically appear as "Tool: <name>" or similar patterns.
    let mut found_mop = false;
    for line in output.lines() {
        let line_lower = line.to_lowercase();
        // Detect action patterns: tool calls, bash commands, file operations
        if line_lower.contains("tool:")
            || line_lower.contains("calling")
            || line_lower.contains("executing")
            || line_lower.contains("running")
        {
            let sig = signature(line);
            let action = action_label(line);
            if let Some(action) = action {
                sequence.observe(&action);
                recent_actions.push(action);
            }
            if detector.record(sig) {
                found_mop = true;
                break;
            }
        }
    }

    found_mop || sequence.is_anomalous(&recent_actions, 0.75)
}

fn action_label(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    if lower.contains("read_file") || lower.contains("read ") {
        Some("read".to_string())
    } else if lower.contains("edit_file")
        || lower.contains("apply_patch")
        || lower.contains("write_file")
        || lower.contains("edit ")
    {
        Some("edit".to_string())
    } else if lower.contains("shell") || lower.contains("running") || lower.contains("executing") {
        Some("shell".to_string())
    } else if lower.contains("test") || lower.contains("verify") || lower.contains("cargo") {
        Some("verify".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod task_typed_evidence_tests {
    //! Audit 2026-11-05 orchestration H2: the per-task evidence map that
    //! `finalize_goal` itemizes on must actually be written.
    use super::record_task_typed_evidence;
    use rightkit_agent_orchestration::coding::unbacked_work_gate::TaskEvidence;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[test]
    fn records_and_accumulates_per_task() {
        let map = Mutex::new(HashMap::new());
        record_task_typed_evidence(&map, "t1", 2);
        record_task_typed_evidence(&map, "t1", 1);
        record_task_typed_evidence(&map, "t2", 0);
        let map = map.into_inner().unwrap();
        assert_eq!(map.get("t1"), Some(&3));
        assert_eq!(map.get("t2"), Some(&0), "a zero attempt is still itemized");
    }

    #[test]
    fn recorded_writer_task_counts_as_backed_evidence() {
        let map = Mutex::new(HashMap::new());
        record_task_typed_evidence(&map, "t1", 1);
        let typed = *map.lock().unwrap().get("t1").unwrap();
        // Same construction `finalize_goal` uses for a jury-accepted claim.
        let evidence = TaskEvidence::new("t1", typed, true);
        assert!(evidence.is_backed());
    }

    #[test]
    fn saturates_instead_of_overflowing() {
        let map = Mutex::new(HashMap::new());
        record_task_typed_evidence(&map, "t1", u32::MAX);
        record_task_typed_evidence(&map, "t1", 5);
        assert_eq!(map.lock().unwrap().get("t1"), Some(&u32::MAX));
    }
}
