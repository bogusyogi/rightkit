/// Build what the user actually reads when a conductor run finishes.
///
/// A run's real product is the agents' work — an audit report, a review, a
/// plan. That output lived only in sub-agent transcripts and handoff files, so
/// the session the user was watching showed a single verdict line and nothing
/// else. This folds each successful agent's output into the message, longest
/// first (the substantive deliverable, rather than a one-line signoff), with a
/// per-agent cap so one verbose worker can't bury the rest.
fn compose_run_report(summary: &str, results: &[rightkit_agent_orchestration::WorkerResult]) -> String {
    const MAX_PER_AGENT: usize = 8_000;
    const MAX_AGENTS: usize = 6;

    let mut substantive: Vec<&rightkit_agent_orchestration::WorkerResult> = results
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
        let workspace_config =
            load_workspace_config_with_env(std::path::Path::new(session.workspace_root.trim()));

        // Capture the current git HEAD so the victory gate can diff against it
        // for eval-hack detection.
        let initial_git_ref = git_rev_parse_head(&session.workspace_root);

        let mut registry = self.agent_registry_for_workspace(&session.workspace_root);
        let budget_cfg = workspace_config.orchestration.budget.clone();
        let configured_conductor_model = self
            .agent_descriptor_with_workspace_config(&workspace_config, "conductor")
            .map(|descriptor| descriptor.model_id)
            .unwrap_or_default();
        let live_session_model = lock_recover(&session.model, "session.model").clone();
        let conductor_model = resolve_conductor_runtime_model(
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

        // Evolution harness: traces every worker result to ~/.coderight/traces/<session_id>.jsonl
        // and tracks failures for the ratchet loop.
        let traces_dir = rightkit_app_settings::agent_config::state_root()
            .map(|root| root.join("traces"))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/coderight/traces"));
        let mut evolution_harness = EvolutionHarness::new(traces_dir, session_id);

        // Build an LLM client for the conductor's configured model.
        let llm = self.build_llm_for_role_in_workspace(
            "conductor",
            &workspace_config,
            Some(&conductor_model),
        )?;

        // Retrieve relevant past experience to inform planning and execution.
        let memory_injection = crate::api::retrieve_session_memory(
            self,
            session_id,
            &session.workspace_root,
            goal,
            5,
            "conductor",
        )
        .await;
        let memory_ctx = memory_injection
            .as_ref()
            .map(|injection| injection.context.as_str());
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
                            rightkit_agent_orchestration::CompletionState::Accepted,
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
                            },
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
                            },
                        });
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        // 1. Plan (informed by memory).
        let plan = plan_goal(llm.as_ref(), goal, memory_ctx, &registry, &team_templates).await?;

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
        // Duration-aware seam: when the roster has multiple agents with the
        // same role, the DurationAwareRouter picks the one with the highest
        // recorded reliability in this task's expected duration bucket.
        // It transparently forwards to the inner AgentRouter otherwise.
        let duration_aware_router = DurationAwareRouter::new();
        let _ = duration_aware_router; // wired for the routing path; future lanes will populate `register_taste` from the harness.
        let projected = projected_cost(&graph, &registry, &router);
        let team_preset = matching_team_execution_preset(goal, &team_templates);
        let per_goal_ceiling =
            { effective_budget_ceiling(budget_cfg.per_goal_max_usd, team_preset.as_ref()) };

        if dry_run {
            return Ok(self.publish_conductor_dry_run(
                session_id,
                &conduit,
                &graph,
                &registry,
                &router,
                projected,
                team_preset.as_ref(),
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
                    },
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
                goal,
                memory_ctx,
                &mut graph,
                &registry,
                &router,
                &budget_cfg,
                team_preset.as_ref(),
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
            let cfg = rightkit_agent_orchestration::CompletionConfig::default();
            let state = if gds >= cfg.partial_threshold {
                rightkit_agent_orchestration::CompletionState::PartiallyComplete
            } else {
                rightkit_agent_orchestration::CompletionState::Rejected
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

        // 6. Report.
        //
        // The verdict line alone ("Accepted (GDS 1.00)…") used to be the ENTIRE
        // visible result of a run: a /audit that produced a 187-line report
        // across six agents left the user's session showing one cryptic
        // sentence, with the actual findings reachable only by opening
        // sub-agent transcripts or the handoff-results files on disk. Backend
        // success is not UI success — the work has to land where the human is
        // looking.
        let content = compose_run_report(&summary, &outcome.results);
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
            },
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
        let learning_summary = rightkit_adapt::SessionSummary {
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

        // Memorize the experience through native Membrane memory. Keep this
        // best-effort: a completed run must remain visible even when the
        // bounded backend write is unavailable, with no local fallback.
        let memory_content = format!(
            "Conductor run\nGoal: {goal}\nOutcome: {}\nSummary: {summary}",
            if goal_complete {
                "completed"
            } else {
                "incomplete"
            },
        );
        let memory_context =
            rightkit_memory::backend::CallContext::after(std::time::Duration::from_secs(5));
        if let Err(error) = self
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
            tracing::warn!(
                session_id = %session_id,
                error = %error,
                error_kind = ?error.kind(),
                "native Membrane memory write failed after conductor run"
            );
        }

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
                use rightkit_agent_orchestration::{RolloutEvent, RolloutStage};
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

#[cfg(test)]
mod run_report_tests {
    use super::compose_run_report;
    use rightkit_agent_orchestration::{WorkerOutcome, WorkerResult};

    fn result(agent: &str, output: &str, success: bool) -> WorkerResult {
        WorkerResult {
            task_id: format!("t-{agent}"),
            agent: agent.to_string(),
            success,
            output: output.to_string(),
            input_tokens: 0,
            mop_fired: false,
            failure_kind: None,
            agent_version: None,
            rollback_step: None,
            outcome: WorkerOutcome::Complete,
        }
    }

    #[test]
    fn folds_agent_output_into_the_visible_message() {
        // A /audit produced a 187-line report across six agents, and the user's
        // session showed only "Accepted (GDS 1.00)…" — the work was invisible.
        let out = compose_run_report(
            "Accepted (GDS 1.00).",
            &[
                result("signoff", "VERDICT: APPROVE", true),
                result(
                    "auditor",
                    "# Audit Report\n\nF-01 leaked key in config.",
                    true,
                ),
            ],
        );
        assert!(out.starts_with("Accepted (GDS 1.00)."));
        assert!(out.contains("F-01 leaked key in config."));
        assert!(out.contains("### auditor"));
        // Longest output first: the deliverable outranks a one-line signoff.
        assert!(out.find("### auditor").unwrap() < out.find("### signoff").unwrap());
    }

    #[test]
    fn skips_failed_and_empty_agents() {
        let out = compose_run_report(
            "Rejected.",
            &[result("coder", "boom", false), result("noop", "   ", true)],
        );
        assert_eq!(out, "Rejected.");
    }

    #[test]
    fn caps_a_verbose_agent_so_it_cannot_bury_the_rest() {
        let long = "x".repeat(20_000);
        let out = compose_run_report("Accepted.", &[result("auditor", &long, true)]);
        assert!(out.contains("truncated"));
        assert!(out.chars().count() < 9_000);
    }
}
