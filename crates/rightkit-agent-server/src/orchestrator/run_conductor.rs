/// Build what the user actually reads when a conductor run finishes.
///
/// A run's real product is the agents' work — an audit report, a review, a
/// plan. That output lived only in sub-agent transcripts and handoff files, so
/// the session the user was watching showed a single verdict line and nothing
/// else. This folds each successful agent's output into the message, longest
/// first (the substantive deliverable, rather than a one-line signoff), with a
/// per-agent cap so one verbose worker can't bury the rest.
fn compose_run_report(
    summary: &str,
    results: &[rightkit_agent_orchestration::coding::WorkerResult],
) -> String {
    const MAX_PER_AGENT: usize = 8_000;
    const MAX_AGENTS: usize = 6;

    let mut substantive: Vec<&rightkit_agent_orchestration::coding::WorkerResult> = results
        .iter()
        .filter(|r| r.success && !r.output.trim().is_empty())
        .collect();
    if substantive.is_empty() {
        return summary.to_string();
    }
    substantive.sort_by_key(|r| std::cmp::Reverse(r.output.trim().len()));

    let mut out = String::from(summary);
    for r in substantive.iter().take(MAX_AGENTS) {
        let body = r.output.trim();
        out.push_str("\n\n---\n\n### ");
        out.push_str(&r.agent);
        out.push_str("\n\n");
        if body.chars().count() > MAX_PER_AGENT {
            let truncated: String = body.chars().take(MAX_PER_AGENT).collect();
            out.push_str(&truncated);
            out.push_str("\n\n_(truncated — full output in this agent's transcript)_");
        } else {
            out.push_str(body);
        }
    }
    if substantive.len() > MAX_AGENTS {
        out.push_str(&format!(
            "\n\n_({} more agent output(s) in their transcripts.)_",
            substantive.len() - MAX_AGENTS
        ));
    }
    out
}

/// Budget (tokens) for the conductor's one `context pull`: the planner prompt
/// takes the text whole, so the pull is capped well below any model window.
const CONDUCTOR_CONTEXT_PULL_BUDGET: usize = 4_000;

/// Deadline for the conductor's `context pull`; a slow pull degrades to
/// "context off" and never holds the run.
const CONDUCTOR_CONTEXT_PULL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// What the conductor's one `context pull` produced: the text for the planner
/// (`None` when there is none) and, when there is none, the reason the run
/// report states (orchestration rules 36 and 47, context rule 42).
#[derive(Debug, PartialEq, Eq)]
struct ConductorMemory {
    text: Option<String>,
    off_reason: Option<String>,
}

/// Turn a `context pull` result into the conductor's memory text or its
/// "context off" reason. Pure so the Pulse-absent and Pulse-present paths are
/// tested without a binary.
fn conductor_memory_from_pull(
    pull: Result<rightkit_memory_client::membrane::ContextPull, String>,
) -> ConductorMemory {
    match pull {
        Err(error) => ConductorMemory {
            text: None,
            off_reason: Some(format!("memory retrieval unavailable: {error}")),
        },
        Ok(pull) => {
            let text = pull.text.trim();
            if text.is_empty() {
                let reason = pull
                    .off_reason
                    .or(pull.reason)
                    .unwrap_or_else(|| "context pull returned no items".to_string());
                ConductorMemory {
                    text: None,
                    off_reason: Some(format!("memory retrieval unavailable: {reason}")),
                }
            } else {
                ConductorMemory {
                    text: Some(text.to_string()),
                    off_reason: None,
                }
            }
        }
    }
}

/// The conductor asks Membrane the way a turn does: one `context pull` for the
/// goal in the session workspace (orchestration rule 36; no host-side
/// retrieval exists on this path).
async fn pull_conductor_memory(workspace_root: &str, goal: &str) -> ConductorMemory {
    let root = std::path::Path::new(workspace_root.trim());
    let context =
        rightkit_memory_client::backend::CallContext::after(CONDUCTOR_CONTEXT_PULL_DEADLINE);
    let pull = rightkit_memory_client::backend::MembraneHandle::shared()
        .context_pull(
            &context,
            root,
            goal,
            Some(CONDUCTOR_CONTEXT_PULL_BUDGET),
            Some(root),
        )
        .await
        .map_err(|error| error.to_string());
    conductor_memory_from_pull(pull)
}

/// Add the one-line "context off" note to a run report when the conductor's
/// memory was unavailable (orchestration rule 47, CR-19: a turn with no
/// context binding runs visibly degraded, with the reason).
fn report_with_memory_note(report: String, memory_error: Option<&str>) -> String {
    match memory_error {
        None => report,
        Some(reason) => {
            let reason: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
            let reason = truncate(&reason, 200);
            format!("{report}\n\n_Context off: conductor memory unavailable ({reason})._")
        }
    }
}

impl DaemonState {
    /// Run a full conductor goal: plan → execute the task graph with real agents
    /// → report. Progress streams to the session's conduit as `TaskGraphUpdated`
    /// and sub-agent events.
    pub async fn run_conductor(
        &self,
        session_id: &str,
        goal: &str,
        dry_run: bool,
    ) -> Result<RunOutcome, String> {
        let run_start = std::time::Instant::now();
        let session = self
            .get_session(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let conduit = session.conduit.clone();
        let workspace_config = crate::daemon_state::load_workspace_config_granted(
            std::path::Path::new(session.workspace_root.trim()),
            crate::daemon_state::session_config_granted(session_id),
        );

        // The sub-agent cumulative limit is per run (orchestration rule 6): a
        // new goal in the same chat starts from zero, in memory and in storage.
        {
            let mut ledger = session.run_budget.lock().await;
            ledger.reset_cumulative();
            if let Some(storage) = self.storage.as_ref() {
                if let Err(error) = storage.lock().await.upsert_run_budget(session_id, 0) {
                    tracing::warn!(session_id = %session_id, error = %error, "failed to persist the reset sub-agent budget");
                }
            }
        }
        // Per-task typed-mutation evidence is per run too: the planner reuses
        // ids `t1…tN`, so an earlier goal's `t2` must not back this one's.
        lock_recover(&session.task_typed_evidence, "session.task_typed_evidence").clear();
        // The unbacked-work arbiter counts attempts per goal (audit 2026-11-05
        // orchestration M2): a different goal in the same chat starts with
        // fresh attempt counters and a fresh aggregate typed-mutation count,
        // while a retry of the same goal keeps both so the cap still applies.
        {
            let persistent_goal = lock_recover(&session.goal, "session.goal").clone();
            let goal_key = conductor_goal_key(persistent_goal.as_deref(), goal);
            let mut arbiter = session.governed_arbiter.lock().await;
            if arbiter.begin_goal(session_id, &goal_key) {
                session
                    .typed_mutations
                    .store(0, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // Capture the current git HEAD so the victory gate can diff against it
        // for eval-hack detection.
        let initial_git_ref = git_rev_parse_head(&session.workspace_root);

        let mut registry = self.agent_registry_with_workspace_config(&workspace_config);
        let budget_cfg = workspace_config.orchestration.budget.clone();
        let configured_conductor_model = self
            .agent_descriptor_with_workspace_config(&workspace_config, "conductor")
            .map(|descriptor| descriptor.model_id)
            .unwrap_or_default();
        let live_session_model = lock_recover(&session.model, "session.model").clone();
        // The model/provider the session was explicitly created with (`--model`, `provider::model`),
        // persisted at creation. `session.model` cannot say so: it is overwritten below after a run.
        let (explicit_model, explicit_provider) = {
            let row = match self.storage.as_ref() {
                Some(storage) => storage.lock().await.get_session(session_id).ok().flatten(),
                None => None,
            };
            let config: serde_json::Value = row
                .and_then(|row| serde_json::from_str(&row.config_json).ok())
                .unwrap_or(serde_json::Value::Null);
            let field = |key: &str| {
                config
                    .get(key)
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            };
            let model = field("explicitModel");
            let provider = if model.is_some() {
                field("explicitProvider")
            } else {
                None
            };
            (model, provider)
        };
        let conductor_model = resolve_conductor_runtime_model(
            explicit_model.as_deref(),
            &live_session_model,
            &configured_conductor_model,
            self.engine_cfg.as_ref().map(|cfg| cfg.model_id.as_str()),
        );
        let split_available =
            crate::model_policy::split_entitled(|feature| self.has_license_feature(feature));
        crate::model_policy::apply_effective_role_model_policy_with_override(
            &mut registry,
            &workspace_config,
            split_available,
            self.engine_cfg.as_ref().map(|cfg| cfg.model_id.as_str()),
            Some(&conductor_model),
        );
        if !conductor_model.is_empty() {
            if let Err(status) = update_session_model(self, session_id, &conductor_model).await {
                tracing::warn!(
                    session_id = %session_id,
                    model = %conductor_model,
                    ?status,
                    "failed to stamp conductor model into session metadata"
                );
            }
        }

        // Evolution harness: traces every worker result to <state root>/traces/<session_id>.jsonl
        // and tracks failures for the ratchet loop. No state root means no traces directory.
        let traces_dir = rightkit_agent_config::state_root_required().join("traces");
        let mut evolution_harness = EvolutionHarness::new(traces_dir, session_id);

        // PL-010: an explicit `tool:` followed by a tool name, a space and a JSON object goal runs through the deterministic executor host and never reaches a model.
        if !dry_run {
            if let Some(outcome) = self.run_explicit_mechanism_goal(session_id, goal).await? {
                return Ok(outcome);
            }
        }
        // Build an LLM client for the conductor's configured model.
        let llm = self.build_llm_for_role_in_workspace(
            "conductor",
            &workspace_config,
            Some(&conductor_model),
            explicit_provider.as_deref(),
        )?;

        // Also picks up teams another process saved since this one read the file.
        crate::api_stores::refresh_team_store(&self.team_store_path, &self.live_teams);
        let mut team_templates: Vec<_> = lock_recover(&self.live_teams, "self.live_teams")
            .values()
            .cloned()
            .collect();
        team_templates.sort_by(|a, b| a.name.cmp(&b.name));

        // 0.5 Conductor trigger-routing (gated, default off). Match the task to an
        // agent-catalog description; a DirectEdit goes straight to the lean coder
        // arm, everything else falls through to the planned ArchitectPlan path.
        if workspace_config.orchestration.conductor_routing() {
            let route = self
                .decide_route(session_id, &registry, llm.as_ref(), goal)
                .await;
            if matches!(route, RoutePath::DirectEdit) {
                match self
                    .run_direct_edit(session_id, goal, &workspace_config)
                    .await
                {
                    Ok(Some(outcome)) => {
                        // DirectEdit landed (gate + thin-diff approved) — finalize
                        // and return without the planned-build victory gate.
                        self.finalize_goal(
                            session_id,
                            &conduit,
                            rightkit_agent_orchestration::coding::CompletionState::Accepted,
                        )
                        .await;
                        let seq = conduit.next_seq();
                        conduit.publish(ProtocolEvent::AssistantMessageCompleted {
                            session_id: session_id.to_string(),
                            seq,
                            created_at: now_iso(),
                            message: TranscriptMessage::Assistant {
                                id: format!("assistant-{}", now_millis()),
                                created_at: now_iso(),
                                content: "Direct edit complete: change landed, deterministic gate \
                                          and thin diff review passed."
                                    .to_string(),
                                reasoning_content: None,
                                tool_calls: Vec::new(),
                                pinned: false,
                                origin: None,
                            },
                            phase: rightkit_agent_engine_protocol::AssistantMessagePhase::Final,
                        });
                        return Ok(outcome);
                    }
                    // Escalate (one hop): fall through to the planned path below.
                    Ok(None) => {
                        let seq = conduit.next_seq();
                        conduit.publish(ProtocolEvent::AssistantMessageCompleted {
                            session_id: session_id.to_string(),
                            seq,
                            created_at: now_iso(),
                            message: TranscriptMessage::Assistant {
                                id: format!("assistant-{}", now_millis()),
                                created_at: now_iso(),
                                content:
                                    "Direct edit escalated to a planned build (gate failed or \
                                          review rejected)."
                                        .to_string(),
                                reasoning_content: None,
                                tool_calls: Vec::new(),
                                pinned: false,
                                origin: None,
                            },
                            phase: rightkit_agent_engine_protocol::AssistantMessagePhase::Interim,
                        });
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        // 1. Plan (informed by memory): the conductor keeps its memory path and
        // calls `context pull`. With Membrane absent the run goes on and the
        // report says "context off" with the reason (orchestration rules 36, 47).
        let ConductorMemory {
            text: memory_ctx,
            off_reason: memory_off_reason,
        } = pull_conductor_memory(&session.workspace_root, goal).await;
        // Tell the planner the sub-agent caps up front (W2-04); same wording as the session note.
        let budget = crate::daemon_state::effective_run_budget(self, session_id);
        let budget_note = format!(
            "Sub-agent budget for this session: at most {} sub-agents run at once (extra spawns wait for a free slot); at most {} sub-agents in total. Plan your delegation to fit within {}.",
            budget.concurrent_max, budget.cumulative_max, budget.cumulative_max
        );
        let plan = plan_goal(
            llm.as_ref(),
            goal,
            memory_ctx.as_deref(),
            &registry,
            &team_templates,
            Some(&budget_note),
        )
        .await?;

        // 2. Build and validate the graph.
        let mut graph = TaskGraph::new();
        for node in plan {
            graph
                .add_task(node)
                .map_err(|e| format!("invalid plan: {e}"))?;
        }
        graph.validate().map_err(|e| format!("invalid plan: {e}"))?;

        // Announce the initial plan.
        let live_goal_session = self.get_session(session_id);
        publish_taskgraph(
            &conduit,
            session_id,
            &graph,
            "Plan ready",
            live_goal_session.as_ref(),
        )?;

        let router = AgentRouter::new();
        let projected = projected_cost(&graph, &registry, &router);
        let team_preset = matching_team_execution_preset(goal, &team_templates);
        let per_goal_ceiling =
            { effective_budget_ceiling(budget_cfg.per_goal_max_usd, team_preset.as_ref()) };

        if dry_run {
            return Ok(self.publish_conductor_dry_run(
                session_id,
                &conduit,
                &graph,
                &ConductorPlan {
                    registry: &registry,
                    router: &router,
                    budget_cfg: &budget_cfg,
                    team_preset: team_preset.as_ref(),
                },
                projected,
            ));
        }

        if per_goal_ceiling > 0.0 && projected > per_goal_ceiling {
            return Err(self.publish_budget_exceeded(
                session_id,
                &conduit,
                projected,
                per_goal_ceiling,
            ));
        }

        // 2.4 Plan-gate (gated): jury the PLAN before building. A rejected plan
        // blocks the run — the user must resubmit with amended intent. The
        // end-audit (the victory gate, after the build) is a separate stage.
        if workspace_config.orchestration.conductor_routing() {
            let plan_text = graph
                .tasks()
                .map(|t| {
                    let deps = if t.deps.is_empty() {
                        String::new()
                    } else {
                        format!(" (after {})", t.deps.join(", "))
                    };
                    format!("- {}{}", t.title, deps)
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !self.plan_jury(session_id, goal, &plan_text).await {
                let seq = conduit.next_seq();
                conduit.publish(ProtocolEvent::AssistantMessageCompleted {
                    session_id: session_id.to_string(),
                    seq,
                    created_at: now_iso(),
                    message: TranscriptMessage::Assistant {
                        id: format!("assistant-{}", now_millis()),
                        created_at: now_iso(),
                        content: "Plan rejected by the plan-jury; not building. Resubmit with \
                                  amended intent."
                            .to_string(),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                        pinned: false,
                        origin: None,
                    },
                    phase: rightkit_agent_engine_protocol::AssistantMessagePhase::Final,
                });
                return Ok(RunOutcome {
                    completed: false,
                    results: vec![],
                    failed: vec!["plan-jury rejected the plan".to_string()],
                    stall_nudge: None,
                });
            }
        }

        if let Some(outcome) = self
            .run_pre_gate_or_report(session_id, &session, &conduit, &workspace_config)
            .await
        {
            return Ok(outcome);
        }

        let outcome = self
            .execute_conductor_graph(
                session_id,
                &session.workspace_root,
                ConductorGoal {
                    goal,
                    memory_ctx: memory_ctx.as_deref(),
                },
                &mut graph,
                &ConductorPlan {
                    registry: &registry,
                    router: &router,
                    budget_cfg: &budget_cfg,
                    team_preset: team_preset.as_ref(),
                },
                &conduit,
            )
            .await?;

        self.record_evolution_and_worker_artifacts(
            session_id,
            run_start,
            &mut evolution_harness,
            &outcome,
        );

        // 5. Victory gate. All tasks Done is necessary but NOT sufficient: the
        // goal is only complete when an independent goal_verification artifact is
        // produced AND the auto-jury approves it. Never complete on a worker's
        // self-assessment.
        // Graceful Degradation Score: weighted fraction of tasks that finished.
        let gds = gds_of(&graph);
        let (state, summary) = if outcome.completed {
            self.run_victory_gate(
                session_id,
                goal,
                &outcome,
                gds,
                &workspace_config,
                initial_git_ref.as_deref(),
            )
            .await
        } else {
            // Not all tasks finished — band by GDS rather than a flat failure.
            let cfg = rightkit_agent_orchestration::coding::CompletionConfig::default();
            let state = if gds >= cfg.partial_threshold {
                rightkit_agent_orchestration::coding::CompletionState::PartiallyComplete
            } else {
                rightkit_agent_orchestration::coding::CompletionState::Rejected
            };
            (
                state,
                format!(
                    "{} (GDS {gds:.2}): {} task(s) failed or unroutable ({}).",
                    state.as_str(),
                    outcome.failed.len(),
                    outcome.failed.join(", ")
                ),
            )
        };
        let goal_complete = state.is_complete();

        // 5b. F2: close/advance the persistent goal from this jury-gated verdict.
        // The ONLY path to `achieved`.
        self.finalize_goal(session_id, &conduit, state).await;

        // Memorize the experience through native Membrane memory before the
        // report is composed, so an unavailable backend is named in the report
        // (orchestration rule 47) instead of only in the log. A completed run
        // must stay visible even when the bounded backend write is unavailable,
        // with no local fallback.
        let memory_content = format!(
            "Conductor run\nGoal: {goal}\nOutcome: {}\nSummary: {summary}",
            if goal_complete {
                "completed"
            } else {
                "incomplete"
            },
        );
        let memory_context =
            rightkit_memory_client::backend::CallContext::after(std::time::Duration::from_secs(5));
        let memory_error = match self
            .memory_backend
            .remember(
                &memory_context,
                &memory_content,
                vec![
                    "conductor".to_string(),
                    if goal_complete {
                        "completed".to_string()
                    } else {
                        "incomplete".to_string()
                    },
                ],
            )
            .await
        {
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %error,
                    error_kind = ?error.kind(),
                    "native Membrane memory write failed after conductor run"
                );
                Some(error.to_string())
            }
        };

        // 6. Report.
        //
        // The verdict line alone ("Accepted (GDS 1.00)…") used to be the ENTIRE
        // visible result of a run: a /audit that produced a 187-line report
        // across six agents left the user's session showing one cryptic
        // sentence, with the actual findings reachable only by opening
        // sub-agent transcripts or the handoff-results files on disk. Backend
        // success is not UI success — the work has to land where the human is
        // looking.
        let memory_note = match (memory_off_reason.as_deref(), memory_error.as_deref()) {
            (Some(off), Some(error)) => Some(format!("{off}; memory write failed: {error}")),
            (Some(off), None) => Some(off.to_string()),
            (None, Some(error)) => Some(format!("memory write failed: {error}")),
            (None, None) => None,
        };
        let mut content = report_with_memory_note(
            compose_run_report(&summary, &outcome.results),
            memory_note.as_deref(),
        );
        if let Some(preset) = team_preset.as_ref() {
            // The preset that shaped this run is named with the reason it was
            // picked (orchestration rule 25).
            content.push_str(&format!(
                "\n\n_Team preset: {} (its name appears in the goal)._",
                preset.team_name
            ));
        }
        let seq = conduit.next_seq();
        conduit.publish(ProtocolEvent::AssistantMessageCompleted {
            session_id: session_id.to_string(),
            seq,
            created_at: now_iso(),
            message: TranscriptMessage::Assistant {
                id: format!("assistant-{}", now_millis()),
                created_at: now_iso(),
                content,
                reasoning_content: None,
                tool_calls: Vec::new(),
                pinned: false,
                origin: None,
            },
            phase: rightkit_agent_engine_protocol::AssistantMessagePhase::Final,
        });

        // 7. Self-evaluating harness: feed the finished run back into learning
        // (per-role performance + extracted proposals).
        let role_results: Vec<(String, bool)> = outcome
            .results
            .iter()
            .map(|r| (r.agent.clone(), r.success))
            .collect();
        let errors: Vec<String> = outcome
            .results
            .iter()
            .filter(|r| !r.success)
            .map(|r| truncate(&r.output, 200))
            .collect();
        let learning_summary = rightkit_agent_learning::SessionSummary {
            session_id: session_id.to_string(),
            succeeded: goal_complete,
            tool_calls: outcome.results.len() as u32,
            errors,
            notable_decisions: if goal_complete {
                vec![format!("achieved goal: {}", truncate(goal, 160))]
            } else {
                vec![]
            },
        };
        self.learning.record_run(&learning_summary, &role_results);

        // Observation layer: one execution record per participating role,
        // feeding the per-role TasteScore.
        let bucket = crate::observation::bucket_of_ms(run_start.elapsed().as_millis());
        for r in &outcome.results {
            let model = self
                .agent_registry
                .get(&r.agent)
                .map(|d| d.model_id.clone())
                .unwrap_or_default();
            self.observations
                .record(crate::observation::AgentExecutionEvent {
                    role: r.agent.clone(),
                    model: model.clone(),
                    bucket,
                    gds,
                    input_tokens: r.input_tokens,
                    mop_fired: r.mop_fired,
                    completed_without_escalation: r.success,
                    quality: if r.success { 1.0 } else { 0.0 },
                    created_at: now_iso(),
                });

            // Capture a fine-tuning trajectory for successful runs (export is
            // GDS-filtered, so only the best become training examples).
            if r.success {
                self.trajectories.record(&crate::trajectory::Trajectory {
                    role: r.agent.clone(),
                    model,
                    prompt: goal.to_string(),
                    output: r.output.clone(),
                    gds,
                    agent_version: r.agent_version.clone(),
                });
            }

            // Feed any staged rollout for this role; auto-roll-back on degradation.
            if let Some((event, snap)) = self.rollouts.record(&r.agent, r.success) {
                use rightkit_agent_orchestration::coding::{RolloutEvent, RolloutStage};
                match event {
                    RolloutEvent::RolledBack => {
                        persist_agent_model(&snap.role, &snap.from_model);
                        self.audit.append(
                            "rollout_rolledback",
                            &snap.role,
                            &format!("reverted to {} (canary degraded)", snap.from_model),
                        );
                    }
                    RolloutEvent::Advanced(RolloutStage::Full) => {
                        self.audit.append(
                            "rollout_full",
                            &snap.role,
                            &format!("{} fully rolled out", snap.to_model),
                        );
                    }
                    _ => {}
                }
            }
        }

        // Governance audit: record the run's verdict in the write-once log.
        self.audit.append(
            if goal_complete {
                "goal_completed"
            } else {
                "goal_incomplete"
            },
            session_id,
            &truncate(&format!("{goal} — {summary}"), 240),
        );

        Ok(RunOutcome {
            completed: goal_complete,
            results: outcome.results,
            failed: outcome.failed,
            stall_nudge: outcome.stall_nudge,
        })
    }
}

/// The key the governed arbiter's per-goal counters are bound to (round 2
/// L6). It joins the persistent session goal with this run's goal: keyed on
/// the persistent goal alone, every run under it shared one key, so a reused
/// task id such as `t2` inherited an exhausted unbacked-attempt counter from
/// an earlier, different request. A retry of the same request keeps its key,
/// so the cap still applies across retries.
fn conductor_goal_key(persistent_goal: Option<&str>, run_goal: &str) -> String {
    let run_goal = run_goal.trim();
    match persistent_goal
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        Some(persistent) => format!("{persistent}\n{run_goal}"),
        None => run_goal.to_string(),
    }
}

#[cfg(test)]
mod conductor_goal_key_tests {
    use super::conductor_goal_key;

    #[test]
    fn a_new_request_under_a_persistent_goal_gets_a_new_key() {
        let first = conductor_goal_key(Some("ship v2"), "fix the login bug");
        let second = conductor_goal_key(Some("ship v2"), "add the export button");
        assert_ne!(first, second);
    }

    #[test]
    fn a_retry_keeps_its_key() {
        assert_eq!(
            conductor_goal_key(Some("ship v2"), "fix the login bug"),
            conductor_goal_key(Some(" ship v2 "), "fix the login bug ")
        );
        assert_eq!(
            conductor_goal_key(None, "task"),
            conductor_goal_key(Some("  "), "task")
        );
    }
}
