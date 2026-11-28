/// Publish a `TaskGraphUpdated` snapshot of the whole graph.
fn publish_taskgraph(
    conduit: &rightkit_agent_engine_core::Conduit,
    session_id: &str,
    graph: &TaskGraph,
    summary: &str,
    session: Option<&std::sync::Arc<crate::api::LiveSession>>,
) -> Result<(), String> {
    let tasks: Vec<TaskNode> = graph.tasks().cloned().collect();
    if let Some(session) = session {
        let snapshot = {
            let mut goal = lock_recover(&session.goal_state, "session.goal_state");
            goal.as_mut()
                .and_then(|goal| goal.apply_task_graph(&tasks).then(|| goal.snapshot()))
        };
        if let Some(snapshot) = snapshot {
            if let Some(storage) = session.storage.clone() {
                let goal_id = snapshot.goal_id.clone();
                let projection = snapshot.clone();
                let store = storage
                    .try_lock()
                    .map_err(|error| format!("goal projection storage unavailable: {error}"))?;
                if let Some(mut row) = store
                    .get_goal(&goal_id)
                    .map_err(|error| format!("load goal projection: {error}"))?
                {
                    row.decomposition_state = projection.decomposition_state;
                    row.plan_revision = projection.plan_revision;
                    row.now = projection.now;
                    row.next = projection.next;
                    row.steps = projection.steps;
                    row.verification_state = projection.verification_state;
                    store
                        .upsert_goal(&row)
                        .map_err(|error| format!("persist goal projection: {error}"))?;
                }
            }
            let seq = conduit.next_seq();
            conduit.publish(ProtocolEvent::GoalProgressUpdated {
                session_id: session_id.to_string(),
                seq,
                created_at: now_iso(),
                snapshot,
            });
        }
    }
    let seq = conduit.next_seq();
    conduit.publish(ProtocolEvent::TaskGraphUpdated {
        session_id: session_id.to_string(),
        seq,
        created_at: now_iso(),
        summary: summary.to_string(),
        tasks,
    });
    Ok(())
}

/// Projected USD cost of executing every routable task in `graph`, using each
/// task's routed agent model and a typical per-task token budget.
fn projected_cost(graph: &TaskGraph, registry: &AgentRegistry, router: &AgentRouter) -> f64 {
    graph
        .tasks()
        .filter_map(|t| router.route(registry, t))
        .map(|a| rightkit_llm::agent_providers::projected_task_cost(&a.model_id))
        .sum()
}

#[derive(Debug, Clone, PartialEq)]
struct TeamExecutionPreset {
    team_name: String,
    max_workers: Option<usize>,
    budget_usd: Option<f64>,
}

impl TeamExecutionPreset {
    fn describe_constraints(&self) -> String {
        let mut parts = Vec::new();
        if let Some(max_workers) = self.max_workers {
            parts.push(format!("maxWorkers={max_workers}"));
        }
        if let Some(budget_usd) = self.budget_usd {
            parts.push(format!("budgetUsd={budget_usd}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" ({})", parts.join(", "))
        }
    }
}

fn matching_team_execution_preset(
    goal: &str,
    teams: &[rightkit_agent_coding_tools::TeamDefinition],
) -> Option<TeamExecutionPreset> {
    // One selector for the task-graph budget and the run budget, so the two
    // never disagree on which preset owns the run (orchestration rule 25).
    let team = find_team_preset_for_goal(goal, teams)?;
    Some(TeamExecutionPreset {
        team_name: team.name.clone(),
        max_workers: team.max_workers.map(|n| n as usize).filter(|n| *n > 0),
        budget_usd: team.budget_usd.filter(|n| *n > 0.0),
    })
}

fn effective_budget_ceiling(configured: f64, team: Option<&TeamExecutionPreset>) -> f64 {
    match (configured > 0.0, team.and_then(|p| p.budget_usd)) {
        (true, Some(team_budget)) => configured.min(team_budget),
        (false, Some(team_budget)) => team_budget,
        _ => configured,
    }
}

fn effective_max_workers(configured: usize, team: Option<&TeamExecutionPreset>) -> usize {
    match (configured, team.and_then(|p| p.max_workers)) {
        (0, Some(team_max)) => team_max,
        (configured, Some(team_max)) => configured.min(team_max).max(1),
        _ => configured,
    }
}

/// Graceful Degradation Score: the fraction of a graph's tasks that reached
/// `Done` (equal weights). 1.0 when every task finished.
fn gds_of(graph: &TaskGraph) -> f64 {
    let total = graph.tasks().count();
    if total == 0 {
        return 0.0;
    }
    let done = graph
        .tasks()
        .filter(|t| t.state == rightkit_agent_engine_protocol::TaskState::Done)
        .count();
    done as f64 / total as f64
}

/// Append an artifact to a session's in-memory artifact store.
fn push_artifact(session: &LiveSession, artifact: ProtocolArtifact) {
    push_session_artifact(session, artifact);
}

/// Persist a promoted model as a per-agent override in `~/.{app}/config.toml`
/// so it becomes the role's model on the next daemon start.
fn persist_agent_model(role: &str, model_id: &str) {
    use rightkit_agent_config::{AgentOverride, ConfigLoader};
    let loader = ConfigLoader::new();
    // Read-modify-write under the shared config lock: a concurrent save is merged in.
    let saved = loader.update_user(|cfg| {
        cfg.orchestration.agents.insert(
            role.to_string(),
            AgentOverride {
                model_id: model_id.to_string(),
                provider_id: "openrouter".to_string(),
                effort: String::new(),
                toolset: String::new(),
                spawnable_roles: Vec::new(),
                ..Default::default()
            },
        );
    });
    if let Err(e) = saved {
        eprintln!("[promotion] failed to persist model for '{role}': {e}");
    }
}

/// Stream a worker recovery (stall/restart or failure) as an orchestration
/// observation so the UI can show that a task was retried.
fn publish_recovery(
    conduit: &rightkit_agent_engine_core::Conduit,
    session_id: &str,
    note: &rightkit_agent_orchestration::coding::RecoveryNote,
) {
    let mut fields = std::collections::HashMap::new();
    fields.insert("attempt".to_string(), note.attempt.to_string());
    fields.insert("timedOut".to_string(), note.timed_out.to_string());
    fields.insert("agent".to_string(), note.agent.clone());
    if let Some(step) = note.rollback_step {
        fields.insert("rollbackStep".to_string(), step.to_string());
    }
    let verb = if note.timed_out { "stalled" } else { "failed" };
    let suffix = note
        .rollback_step
        .map(|s| format!(" — rollback hint: step {s}"))
        .unwrap_or_default();
    let seq = conduit.next_seq();
    conduit.publish(ProtocolEvent::OrchestrationObservationRecorded {
        session_id: session_id.to_string(),
        seq,
        created_at: now_iso(),
        observation: rightkit_agent_engine_protocol::OrchestrationObservation {
            id: format!("recovery-{}", now_millis()),
            kind: "worker_recovery".into(),
            subject: note.task_id.clone(),
            summary: format!(
                "{} {verb} on task '{}' (attempt {}){suffix}; restarting",
                note.agent, note.task_id, note.attempt
            ),
            fields,
            created_at: now_iso(),
        },
    });
}

/// Build a `goal_verification` artifact carrying the auto-jury's verdict.
fn goal_verification_artifact(
    content_hash_val: &str,
    approved: bool,
    juror: &str,
    approvals: Vec<rightkit_agent_engine_protocol::ArtifactApproval>,
    verification_text: &str,
) -> ProtocolArtifact {
    ProtocolArtifact {
        id: format!("goalverify-{}", now_millis()),
        path: String::new(),
        kind: "goal_verification".into(),
        content_hash: content_hash_val.to_string(),
        shippable: approved,
        trust_label: "unreviewed".to_string(),
        producer: "signoff".into(),
        provenance: "victory-gate".into(),
        verdict: Some(rightkit_agent_engine_protocol::ArtifactVerdict {
            passed: approved,
            juror: juror.to_string(),
            bound_hash: content_hash_val.to_string(),
            summary: truncate(verification_text, 240),
            confidence: None,
        }),
        approvals,
        meta: String::new(),
        created_at: now_iso(),
    }
}

/// Parse an agent's verdict reply into `(approve/pass, confidence)`.
///
/// The LAST `VERDICT:` line decides (a reply may quote another agent's verdict
/// before giving its own), reading only the text after its final `VERDICT:`.
/// With no such line the whole reply is scanned. Either way only whole words
/// count — PASS/PASSED/APPROVE/APPROVED approve, FAIL/FAILED/REJECT/REJECTED
/// refuse — so "bypassed" or "compass" never approve (audit 2026-11-05
/// orchestration M4). Confidence is read from a `CONFIDENCE: <0-1>` token,
/// defaulting to 0.8. Conservative: anything ambiguous is treated as NOT
/// approved — completion is never granted by default.
///
/// A negated approve word is a refusal (audit 2026-11-05 round 2, M3): `NOT
/// APPROVED`, `cannot approve`, `can't approve`, `did not pass`, `never
/// passed`. A negator up to [`NEGATION_REACH`] words before an approve word
/// counts, so a negation that may or may not apply also refuses. Confidence
/// is read from the last `CONFIDENCE:` so it pairs with the deciding verdict.
fn parse_verdict(text: &str) -> (bool, f64) {
    /// Words before an approve word that a negator still reaches
    /// ("did NOT fully PASS", "NOT YET APPROVED").
    const NEGATION_REACH: usize = 3;
    let upper = text.to_uppercase();
    let scope = match upper.lines().rev().find(|l| l.contains("VERDICT:")) {
        Some(line) => line.rsplit("VERDICT:").next().unwrap_or(""),
        None => upper.as_str(),
    };
    let mut approves = false;
    let mut refuses = false;
    // Index (in non-empty words) of the most recent negator.
    let mut last_negator: Option<usize> = None;
    let words = scope
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty());
    for (index, word) in words.enumerate() {
        match word {
            "PASS" | "PASSED" | "APPROVE" | "APPROVED" => {
                if last_negator.is_some_and(|at| index - at <= NEGATION_REACH) {
                    refuses = true;
                } else {
                    approves = true;
                }
            }
            "FAIL" | "FAILED" | "REJECT" | "REJECTED" => refuses = true,
            // `T` is the tail of a split contraction: CAN'T, DIDN'T, DON'T.
            "NOT" | "NO" | "NEVER" | "CANNOT" | "T" | "UNABLE" | "NOR" | "NEITHER" => {
                last_negator = Some(index)
            }
            _ => {}
        }
    }
    let approve = approves && !refuses;

    let confidence = upper
        .rfind("CONFIDENCE:")
        .map(|i| &upper[i + "CONFIDENCE:".len()..])
        .and_then(|rest| {
            let token: String = rest
                .trim_start()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            token.parse::<f64>().ok()
        })
        .map(|c| c.clamp(0.0, 1.0))
        .unwrap_or(0.8);

    (approve, confidence)
}

/// A small, dependency-free content hash (FNV-1a, hex) for artifact binding.
fn content_hash(s: &str) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}")
}

/// Truncate to `n` chars on a char boundary, appending an ellipsis if cut.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}…")
    }
}

/// Capture the current git HEAD ref for the workspace, or `None` if not a git
/// repo or git is unavailable.
fn git_rev_parse_head(workspace_root: &str) -> Option<String> {
    let root = workspace_root.trim();
    if root.is_empty() {
        return None;
    }
    let output = managed_git_output(root, &["rev-parse", "HEAD"])?;
    output.success.then(|| output.stdout.trim().to_string())
}

/// Compute the git diff since `initial_ref` and run the eval-hack detector.
/// Returns an empty vec when detection is disabled or no diff is available.
fn run_eval_hack_detection(
    workspace_root: &str,
    initial_ref: Option<&str>,
    workers: &[rightkit_agent_orchestration::coding::WorkerResult],
) -> Vec<rightkit_agent_orchestration::coding::EvalHackFinding> {
    let ref_ = match initial_ref {
        Some(r) if !r.is_empty() => r,
        _ => return vec![],
    };
    let root = workspace_root.trim();
    if root.is_empty() {
        return vec![];
    }
    let output = managed_git_output(root, &["diff", ref_, "--"]);
    let output = match output {
        Some(output) if output.success => output.stdout,
        _ => return vec![],
    };
    if output.is_empty() {
        return vec![];
    }
    rightkit_agent_orchestration::coding::detect_eval_hacks(&output, workers)
}

fn managed_git_output(
    workspace_root: &str,
    args: &[&str],
) -> Option<crate::api_environments::ManagedProcessOutput> {
    let future = crate::api_environments::run_managed_process("git", args, workspace_root);
    let output = if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future)).ok()
    } else {
        tokio::runtime::Runtime::new().ok()?.block_on(future).ok()
    }?;
    Some(output)
}

#[cfg(test)]
mod parse_verdict_tests {
    //! Audit 2026-11-05 orchestration M4.
    use super::parse_verdict;

    #[test]
    fn incidental_substrings_do_not_approve() {
        assert!(!parse_verdict("I could not run the tests; the build step was bypassed").0);
        assert!(!parse_verdict("Pointing the compass north").0);
        assert!(!parse_verdict("This passes nothing useful").0);
    }

    #[test]
    fn last_verdict_line_wins() {
        let reply =
            "The verifier's VERDICT: PASS is wrong.\nThe diff breaks the build.\nVERDICT: REJECT";
        assert!(!parse_verdict(reply).0);
        let reply = "Earlier draft said VERDICT: FAIL\nRe-checked after the fix.\nVERDICT: APPROVE";
        assert!(parse_verdict(reply).0);
    }

    #[test]
    fn explicit_verdicts_still_parse() {
        assert!(parse_verdict("All good.\nVERDICT: PASS\nCONFIDENCE: 0.9").0);
        assert!(parse_verdict("verdict: approved").0);
        assert!(!parse_verdict("VERDICT: FAIL").0);
        assert!(!parse_verdict("VERDICT: PASS or FAIL, unsure").0);
        assert_eq!(parse_verdict("VERDICT: PASS\nCONFIDENCE: 0.4").1, 0.4);
    }

    #[test]
    fn whole_reply_fallback_uses_words() {
        assert!(parse_verdict("Checks passed. PASS").0);
        assert!(!parse_verdict("nothing decisive here").0);
    }

    /// Round 2 M3: negated approve words are refusals.
    #[test]
    fn negated_verdicts_are_not_approved() {
        for reply in [
            "VERDICT: NOT APPROVED",
            "VERDICT: not approved, the build is red",
            "I cannot approve this",
            "I can't approve this change",
            "I can\u{2019}t approve this change",
            "do not pass this change",
            "the tests did not pass",
            "The tests didn't pass",
            "VERDICT: NOT YET APPROVED",
            "It never passed in CI",
            "VERDICT: NOT APPROVED. PASS once fixed",
        ] {
            assert!(!parse_verdict(reply).0, "{reply}");
        }
    }

    #[test]
    fn negation_elsewhere_does_not_flip_a_clear_pass() {
        assert!(parse_verdict("No regressions found.\nVERDICT: PASS").0);
        assert!(parse_verdict("VERDICT: APPROVED - not a single failing test").0);
    }

    #[test]
    fn confidence_pairs_with_the_last_verdict() {
        let reply = "Quoted earlier: VERDICT: FAIL CONFIDENCE: 0.1\nVERDICT: PASS\nCONFIDENCE: 0.9";
        let (approve, confidence) = parse_verdict(reply);
        assert!(approve);
        assert_eq!(confidence, 0.9);
    }
}
