const CONDUCTOR_RUNTIME_MODEL_IDS: [&str; 3] =
    ["z-ai/glm-5.2", "moonshotai/kimi-k2.6", "minimax/minimax-m3"];

// ponytail: fixed catalog budget; Codex renders its tool catalog at ~2% of
// context — make this context-relative if the catalog ever outgrows it.
const CATALOG_BUDGET_CHARS: usize = 4000;

fn resolve_conductor_runtime_model(
    live_session_model: &str,
    configured_conductor_model: &str,
    engine_default_model: Option<&str>,
) -> String {
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
        let outcome = self
            .state
            .spawn_with_failure_kind(
                &task.agent,
                &task.payload,
                &self.parent_session_id,
                rightkit_agent_tools::coding_tools::SubagentSpawnOptions::default(),
                failure_kind_slot.clone(),
            )
            .await;
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
/// genuine unrecovered API/provider error. Reuses `rightkit_agent_engine_core`'s
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
        rightkit_agent_engine_core::FailureKind::Budget => SubagentErrorKind::ResourceLimit("budget".into()),
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
    use rightkit_agent_orchestration::SequenceAnomalyDetector;

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
mod subagent_error_kind_tests {
    use super::*;

    fn is_resource_limit(kind: &SubagentErrorKind, expected_limit_kind: &str) -> bool {
        matches!(kind, SubagentErrorKind::ResourceLimit(k) if k == expected_limit_kind)
    }

    #[test]
    fn rate_limit_429_classifies_as_resource_limit() {
        let kind = subagent_error_kind("HTTP 429 rate limit exceeded, retry later");
        assert!(
            is_resource_limit(&kind, "rate_limited"),
            "a 429 must classify as a resource-limit cutoff (partial work), not an API error"
        );
    }

    #[test]
    fn wall_clock_ceiling_classifies_as_resource_limit() {
        let kind = subagent_error_kind("sub-agent exceeded the 30-minute wall-clock ceiling");
        assert!(is_resource_limit(&kind, "wall_clock"));
    }

    #[test]
    fn timeout_classifies_as_resource_limit() {
        let kind = subagent_error_kind("request timed out waiting for provider");
        assert!(is_resource_limit(&kind, "timeout"));
    }

    #[test]
    fn budget_ceiling_classifies_as_resource_limit() {
        let kind = subagent_error_kind("session budget exceeded");
        assert!(is_resource_limit(&kind, "budget"));
    }

    #[test]
    fn auth_error_classifies_as_api_error_not_resource_limit() {
        // A 401 is a hard, unrecovered API error — never a "the work was
        // merely cut off" resource limit. No partial work is meaningfully
        // recoverable from an auth failure.
        let kind = subagent_error_kind("401 Unauthorized: invalid api key");
        assert!(matches!(kind, SubagentErrorKind::ApiError));
    }

    #[test]
    fn provider_5xx_classifies_as_api_error() {
        let kind = subagent_error_kind("provider returned 503 Service Unavailable");
        assert!(matches!(kind, SubagentErrorKind::ApiError));
    }

    #[test]
    fn panic_is_failed_even_when_message_names_a_limit() {
        for text in [
            "sub-agent task panicked: index out of bounds",
            "sub-agent task panicked: request timed out, 429 rate limit, budget exceeded",
            "sub-agent task panicked: wall-clock ceiling",
        ] {
            assert!(
                matches!(subagent_error_kind(text), SubagentErrorKind::ApiError),
                "{text}"
            );
        }
    }

    #[test]
    fn typed_kind_wins_over_error_text() {
        // Text says "timeout" but the typed kind says provider_error: typed wins.
        assert!(matches!(
            subagent_error_kind_typed(Some("provider_error"), "gateway timeout upstream"),
            SubagentErrorKind::ApiError
        ));
        // Text has no limit keyword but the typed kind is a limit.
        assert!(is_resource_limit(
            &subagent_error_kind_typed(Some("rate_limited"), "slow down"),
            "rate_limited"
        ));
        assert!(is_resource_limit(
            &subagent_error_kind_typed(Some("budget"), "stopped"),
            "budget"
        ));
        assert!(is_resource_limit(
            &subagent_error_kind_typed(Some("timeout"), "stopped"),
            "timeout"
        ));
        assert!(matches!(
            subagent_error_kind_typed(Some("other"), "429 rate limit"),
            SubagentErrorKind::ApiError
        ));
    }

    #[test]
    fn missing_or_unknown_kind_falls_back_to_keywords() {
        assert!(is_resource_limit(
            &subagent_error_kind_typed(None, "HTTP 429 rate limit"),
            "rate_limited"
        ));
        assert!(is_resource_limit(
            &subagent_error_kind_typed(Some(""), "request timed out"),
            "timeout"
        ));
        assert!(is_resource_limit(
            &subagent_error_kind_typed(Some("future_kind"), "session budget exceeded"),
            "budget"
        ));
    }

    #[test]
    fn typed_limit_never_overrides_panic() {
        assert!(matches!(
            subagent_error_kind_typed(Some("timeout"), "sub-agent task panicked: x"),
            SubagentErrorKind::ApiError
        ));
    }

    #[test]
    fn unclassified_error_classifies_as_api_error() {
        let kind = subagent_error_kind("something weird happened");
        assert!(matches!(kind, SubagentErrorKind::ApiError));
    }
}
