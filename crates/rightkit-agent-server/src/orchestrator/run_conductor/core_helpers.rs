impl DaemonState {
    #[allow(clippy::too_many_arguments)]
    fn publish_conductor_dry_run(
        &self,
        session_id: &str,
        conduit: &rightkit_agent_engine_core::Conduit,
        graph: &TaskGraph,
        registry: &AgentRegistry,
        router: &AgentRouter,
        projected: f64,
        team_preset: Option<&TeamExecutionPreset>,
    ) -> RunOutcome {
        let mut lines = vec![format!(
            "Dry run — would execute {} task(s) (projected ${projected:.4}):",
            graph.len()
        )];
        if let Some(preset) = team_preset {
            lines.push(format!(
                "Team preset: {}{}",
                preset.team_name,
                preset.describe_constraints()
            ));
        }
        for t in graph.tasks() {
            let agent = router
                .route(registry, t)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| format!("UNROUTABLE role '{}'", t.assigned_role));
            let deps = if t.deps.is_empty() {
                String::new()
            } else {
                format!(" (after {})", t.deps.join(", "))
            };
            lines.push(format!("• {} → {}{}", t.title, agent, deps));
        }
        let seq = conduit.next_seq();
        conduit.publish(ProtocolEvent::AssistantMessageCompleted {
            session_id: session_id.to_string(),
            seq,
            created_at: now_iso(),
            message: TranscriptMessage::Assistant {
                id: format!("assistant-{}", now_millis()),
                created_at: now_iso(),
                content: lines.join("\n"),
                reasoning_content: None,
                tool_calls: Vec::new(),
                pinned: false,
            },
        });
        RunOutcome {
            completed: false,
            results: vec![],
            failed: vec![],
            stall_nudge: None,
        }
    }

    fn publish_budget_exceeded(
        &self,
        session_id: &str,
        conduit: &rightkit_agent_engine_core::Conduit,
        projected: f64,
        per_goal_ceiling: f64,
    ) -> String {
        let seq = conduit.next_seq();
        conduit.publish(ProtocolEvent::BudgetExceeded {
            session_id: session_id.to_string(),
            seq,
            created_at: now_iso(),
            scope: "goal".into(),
            current_usd: projected,
            ceiling_usd: per_goal_ceiling,
            hint: "Raise the per-goal budget or narrow the goal.".into(),
        });
        format!("projected cost ${projected:.4} exceeds per-goal ceiling ${per_goal_ceiling:.4}")
    }

    async fn run_pre_gate_or_report(
        &self,
        session_id: &str,
        session: &LiveSession,
        conduit: &rightkit_agent_engine_core::Conduit,
        workspace_config: &rightkit_app_settings::agent_config::HostConfig,
    ) -> Option<RunOutcome> {
        let pre_gate_specs =
            crate::pre_gate::specs_from_config(&workspace_config.orchestration.pre_gate_checks);
        if pre_gate_specs.is_empty() {
            return None;
        }
        let ctx = rightkit_agent_tools::coding_tools::ToolContext::new(
            session_id,
            session.workspace_root.trim(),
            rightkit_agent_policy::PermissionMode::Bypass,
            session.workspace_trust,
        );
        if let rightkit_agent_tools::coding_tools::check_runner::PreGateDecision::SkipJuryFailed(reasons) =
            crate::pre_gate::run_pre_gate(conduit, session_id, &ctx, &pre_gate_specs).await
        {
            let seq = conduit.next_seq();
            conduit.publish(ProtocolEvent::AssistantMessageCompleted {
                session_id: session_id.to_string(),
                seq,
                created_at: now_iso(),
                message: TranscriptMessage::Assistant {
                    id: format!("assistant-{}", now_millis()),
                    created_at: now_iso(),
                    content: format!(
                        "Pre-gate checks failed; skipping the jury and stopping the run:\n- {}",
                        reasons.join("\n- ")
                    ),
                    reasoning_content: None,
                    tool_calls: Vec::new(),
                    pinned: false,
                },
            });
            return Some(RunOutcome {
                completed: false,
                results: vec![],
                failed: reasons,
                stall_nudge: None,
            });
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_conductor_graph(
        &self,
        session_id: &str,
        workspace_root: &str,
        goal: &str,
        memory_ctx: Option<&str>,
        graph: &mut TaskGraph,
        registry: &AgentRegistry,
        router: &AgentRouter,
        budget_cfg: &rightkit_app_settings::agent_config::BudgetConfig,
        team_preset: Option<&TeamExecutionPreset>,
        conduit: &Arc<rightkit_agent_engine_core::Conduit>,
    ) -> Result<RunOutcome, String> {
        let root_mechanism = resolve_conductor_mechanism(session_id, goal).await;
        let root_session = self
            .get_session(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        publish_conductor_mechanism_receipt(&root_session, session_id, None, &root_mechanism)?;
        if !root_mechanism.needs_model() {
            return Err(format!(
                "conductor root mechanism resolution ended terminally: {}",
                root_mechanism.reason
            ));
        }
        let spawner = Arc::new(SpawnWorker {
            state: self.clone(),
            parent_session_id: session_id.to_string(),
        });
        let mechanism_worker = Arc::new(ConductorMechanismWorker {
            inner: spawner,
            state: self.clone(),
            session_id: session_id.to_string(),
            root_decision: root_mechanism,
        });
        let max_attempts = budget_cfg.max_retries_per_role + 1;
        let stall_timeout = Duration::from_secs(300);
        let recovery_conduit = conduit.clone();
        let recovery_sid = session_id.to_string();
        let worker = Arc::new(
            rightkit_agent_orchestration::coding::RecoveringWorker::new(
                mechanism_worker,
                max_attempts,
                stall_timeout,
            )
            .with_hook(Arc::new(move |note| {
                publish_recovery(&recovery_conduit, &recovery_sid, &note);
            })),
        );
        let sid = session_id.to_string();
        let conduit_for_updates = conduit.clone();
        let live_goal_session = self.get_session(session_id);
        let projection_error = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let projection_error_for_updates = projection_error.clone();
        let exec_goal = match memory_ctx {
            Some(m) => format!("{goal}\n\n[Relevant past experience]\n{m}"),
            None => goal.to_string(),
        };
        let max_concurrent = effective_max_workers(budget_cfg.max_workers as usize, team_preset);
        let wall_clock_minutes = budget_cfg.max_wall_clock_minutes;
        let wall_clock_limit = Duration::from_secs((wall_clock_minutes as u64).max(1) * 60);
        let checkpoint_run_id = conductor_checkpoint_run_id(session_id, &exec_goal);
        let checkpoint_store =
            GraphCheckpointStore::open(&self.database_path).map_err(|error| error.to_string())?;
        match tokio::time::timeout(
            wall_clock_limit,
            // Cross-worker prefix-cache sharing (R5): siblings under this one
            // conductor run inherit a shared stable-prefix cache key derived
            // from (session_id, today) so identical preludes hit cache-read
            // instead of each worker recomputing its own key.
            run_graph_resumable_with_conductor_and_handoff_archive(
                &checkpoint_run_id,
                &exec_goal,
                graph,
                registry,
                router,
                worker,
                max_concurrent,
                &checkpoint_store,
                |g| {
                    if let Err(error) = publish_taskgraph(
                        &conduit_for_updates,
                        &sid,
                        g,
                        "Progress",
                        live_goal_session.as_ref(),
                    ) {
                        *projection_error_for_updates
                            .lock()
                            .expect("projection error mutex") = Some(error);
                    }
                },
                Some(sid.clone()),
                Some(conductor_day_from_iso(&now_iso()).to_string()),
                Some(HandoffArchiveConfig::for_workspace(std::path::Path::new(
                    workspace_root,
                ))),
            ),
        )
        .await
        {
            Ok(Ok(outcome)) => {
                if let Some(error) = projection_error
                    .lock()
                    .expect("projection error mutex")
                    .take()
                {
                    return Err(error);
                }
                if outcome.completed {
                    checkpoint_store
                        .delete(&checkpoint_run_id)
                        .map_err(|error| error.to_string())?;
                }
                Ok(outcome)
            }
            Ok(Err(error)) => Err(format!("conductor checkpoint failed: {error}")),
            Err(_elapsed) => {
                let seq = conduit.next_seq();
                conduit.publish(ProtocolEvent::AssistantMessageCompleted {
                    session_id: session_id.to_string(),
                    seq,
                    created_at: now_iso(),
                    message: TranscriptMessage::Assistant {
                        id: format!("assistant-{}", now_millis()),
                        created_at: now_iso(),
                        content: format!(
                            "Conductor run exceeded the {wall_clock_minutes}-minute \
                             wall-clock limit and was stopped."
                        ),
                        reasoning_content: None,
                        tool_calls: Vec::new(),
                        pinned: false,
                    },
                });
                Err(format!(
                    "conductor run exceeded wall-clock limit of {wall_clock_minutes} minutes"
                ))
            }
        }
    }

    fn record_evolution_and_worker_artifacts(
        &self,
        session_id: &str,
        run_start: std::time::Instant,
        evolution_harness: &mut EvolutionHarness,
        outcome: &RunOutcome,
    ) {
        let total_ms = run_start.elapsed().as_millis() as u64;
        let per_task_ms = if outcome.results.is_empty() {
            total_ms
        } else {
            total_ms / outcome.results.len() as u64
        };
        for r in &outcome.results {
            evolution_harness.record_result(r, per_task_ms);
        }
        if evolution_harness.should_ratchet() {
            eprintln!(
                "[evolution] session {session_id}: ratchet threshold reached \
                 ({} failures) — proposer pipeline should be triggered",
                outcome.results.iter().filter(|r| !r.success).count()
            );
            evolution_harness.reset_ratchet();
        }
        evolution_harness.finalize();

        if let Some(session) = self.get_session(session_id) {
            for r in outcome.results.iter().filter(|r| r.success) {
                push_artifact(
                    &session,
                    ProtocolArtifact {
                        id: format!("art-{}", now_millis()),
                        path: String::new(),
                        kind: "worker_result".into(),
                        content_hash: content_hash(&r.output),
                        shippable: false,
                        trust_label: "unreviewed".to_string(),
                        producer: r.agent.clone(),
                        provenance: r.task_id.clone(),
                        verdict: None,
                        approvals: vec![],
                        meta: String::new(),
                        created_at: now_iso(),
                    },
                );
            }
        }
    }
}

fn conductor_checkpoint_run_id(session_id: &str, goal: &str) -> String {
    let digest = rightkit_crypto::sha256(format!("{session_id}\0{goal}").as_bytes());
    format!("conductor-{}", rightkit_crypto::hex_encode(&digest))
}

#[cfg(test)]
mod conductor_checkpoint_id_tests {
    use super::conductor_checkpoint_run_id;

    #[test]
    fn checkpoint_identity_is_stable_and_goal_scoped() {
        let first = conductor_checkpoint_run_id("session-a", "ship it");
        assert_eq!(first, conductor_checkpoint_run_id("session-a", "ship it"));
        assert_ne!(first, conductor_checkpoint_run_id("session-a", "different"));
        assert_ne!(first, conductor_checkpoint_run_id("session-b", "ship it"));
        assert_eq!(first.len(), "conductor-".len() + 64);
    }
}
