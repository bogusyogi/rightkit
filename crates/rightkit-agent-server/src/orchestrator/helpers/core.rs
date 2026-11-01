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
    teams: &[rightkit_agent_tools::coding_tools::TeamDefinition],
) -> Option<TeamExecutionPreset> {
    let goal = goal.to_lowercase();
    teams.iter().find_map(|team| {
        let name = team.name.trim();
        if name.is_empty() || !goal.contains(&name.to_lowercase()) {
            return None;
        }
        Some(TeamExecutionPreset {
            team_name: team.name.clone(),
            max_workers: team.max_workers.map(|n| n as usize).filter(|n| *n > 0),
            budget_usd: team.budget_usd.filter(|n| *n > 0.0),
        })
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

/// Persist a promoted model as a per-agent override in `~/.coderight/config.toml`
/// so it becomes the role's model on the next daemon start.
fn persist_agent_model(role: &str, model_id: &str) {
    use rightkit_app_settings::agent_config::{AgentOverride, ConfigLoader};
    let loader = ConfigLoader::new();
    // Read-modify-write under the shared config lock: a concurrent save is merged in.
    let saved = loader.update_user(|cfg| {
        cfg.orchestration.agents.insert(
            role.to_string(),
            AgentOverride {
                model_id: model_id.to_string(),
                provider_id: "openrouter".to_string(),
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
    note: &rightkit_agent_orchestration::RecoveryNote,
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

#[cfg(test)]
mod core_helpers_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn taskgraph_goal_persistence_precedes_event() {
        let storage = Arc::new(tokio::sync::Mutex::new(
            rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
        ));
        let state = DaemonState::new("test-token-deadbeef").with_storage(storage.clone());
        state.create_session("graph", None, "/tmp", "graph", None);
        for _ in 0..200 {
            if storage.lock().await.get_session("graph").unwrap().is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(storage.lock().await.get_session("graph").unwrap().is_some());
        let app = crate::api::build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/graph/goal")
                    .header("authorization", "Bearer test-token-deadbeef")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"ship it"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let live = state.get_session("graph").expect("live session");
        let mut graph = TaskGraph::new();
        graph
            .add_task(TaskNode {
                id: "done".into(),
                title: "Finish it".into(),
                deps: Vec::new(),
                done_when: "complete".into(),
                done_when_predicates: Vec::new(),
                state: TaskState::Done,
                category: String::new(),
                estimated_duration: String::new(),
                assigned_role: String::new(),
                escalation_path: String::new(),
                open_questions: Vec::new(),
                created_at: "t0".into(),
                updated_at: "t1".into(),
            })
            .unwrap();

        let mut events = live.conduit.subscribe();
        let held_store = storage.lock().await;
        let error = publish_taskgraph(&live.conduit, "graph", &graph, "Plan", Some(&live))
            .expect_err("storage contention must reach run_conductor");
        assert!(error.contains("storage unavailable"), "{error}");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), events.recv())
                .await
                .is_err(),
            "must not publish before persistence"
        );
        drop(held_store);

        publish_taskgraph(&live.conduit, "graph", &graph, "Plan", Some(&live)).unwrap();
        let persisted = storage
            .lock()
            .await
            .active_goal_for_session("graph")
            .unwrap()
            .expect("projection persisted before GoalProgressUpdated");
        assert_eq!(persisted.decomposition_state, "usable");
        assert_eq!(persisted.plan_revision, 1);
        assert_eq!(persisted.steps.len(), 1);
        assert_eq!(persisted.steps[0].verification_state, "verified");

        let event = match tokio::time::timeout(std::time::Duration::from_millis(50), events.recv())
            .await
            .expect("GoalProgressUpdated published")
        {
            rightkit_agent_engine_core::conduit::ConduitReceive::Event(event) => event,
            other => panic!("conduit closed or lagged: {other:?}"),
        };
        let ProtocolEvent::GoalProgressUpdated { snapshot, .. } = event else {
            panic!("expected GoalProgressUpdated");
        };
        assert_eq!(snapshot.plan_revision, persisted.plan_revision);
        assert_eq!(snapshot.steps, persisted.steps);
    }
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
        }),
        approvals,
        meta: String::new(),
        created_at: now_iso(),
    }
}

/// Parse an agent's verdict reply into `(approve/pass, confidence)`.
///
/// Looks for an explicit `VERDICT:` line first (PASS/APPROVE → true,
/// FAIL/REJECT → false), falling back to whole-text keywords. Confidence is read
/// from a `CONFIDENCE: <0-1>` token, defaulting to 0.8. Conservative: anything
/// ambiguous is treated as NOT approved — completion is never granted by default.
fn parse_verdict(text: &str) -> (bool, f64) {
    let upper = text.to_uppercase();
    let verdict_line = upper.lines().find(|l| l.contains("VERDICT:")).unwrap_or("");
    let scope = if verdict_line.is_empty() {
        upper.as_str()
    } else {
        verdict_line
    };
    let approve = if scope.contains("REJECT") || scope.contains("FAIL") {
        false
    } else {
        scope.contains("APPROVE") || scope.contains("PASS")
    };

    let confidence = upper
        .find("CONFIDENCE:")
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
    workers: &[rightkit_agent_orchestration::WorkerResult],
) -> Vec<rightkit_agent_orchestration::EvalHackFinding> {
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
    rightkit_agent_orchestration::detect_eval_hacks(&output, workers)
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
