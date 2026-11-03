impl DaemonState {
    /// F2 terminal: map the jury-gated victory-gate verdict onto the session's
    /// persistent goal, emit `GoalProgressUpdated`, and on a terminal status
    /// persist `closed_at` + fire an unattended-close desktop notification.
    ///
    /// Defect fix: this function now reads real signals instead of hardcoding
    /// them. `gds` comes from `session.goal_state.gds_score`,
    /// `verification_present`/`verification_approved` from the cached
    /// `CompletionEvidence` written by the conductor's jury arbitration,
    /// `typed_mutations` from the live engine's shared counter on
    /// `LiveSession::typed_mutations`, and `escalate`/`jury_consensus` from
    /// the `CompletionState` itself. The unbacked-victory gate handles all
    /// three `UnbackedVerdict` variants:
    /// - `Backed` → mapped_status passes through (Achieved if Accepted).
    /// - `Unbacked { attempts, .. }` → mapped_status = `Active`, session keeps
    ///   running, conductor retries on the next turn.
    /// - `Refused { reason, .. }` → mapped_status = `Unmet`, session closes, typed
    ///   rejection on the live bus.
    async fn finalize_goal(
        &self,
        session_id: &str,
        conduit: &rightkit_agent_engine_core::Conduit,
        state: rightkit_agent_orchestration::coding::CompletionState,
    ) {
        let Some(session) = self.get_session(session_id) else {
            return;
        };
        // Build `CompletionInput` from real signals — the conductor's jury
        // arbitration has already produced the verdict; we mirror its shape into
        // the input the arbiter consumes for the unbacked gate.
        let (gds, verification_present, verification_approved) = {
            let gs_guard = lock_recover(&session.goal_state, "session.goal_state");
            let evidence_guard =
                lock_recover(&session.completion_evidence, "session.completion_evidence");
            let gds = gs_guard.as_ref().map(|gs| gs.gds_score).unwrap_or(0.0);
            (
                gds,
                evidence_guard.verification_present,
                evidence_guard.verification_approved,
            )
        };
        let jury_consensus = state
            == rightkit_agent_orchestration::coding::CompletionState::Accepted
            || state == rightkit_agent_orchestration::coding::CompletionState::Rejected
            || state == rightkit_agent_orchestration::coding::CompletionState::Escalate
            || state == rightkit_agent_orchestration::coding::CompletionState::PartiallyComplete;
        let escalate = state == rightkit_agent_orchestration::coding::CompletionState::Escalate;
        let mut mapped_status = crate::goal_terminal::goal_status_from_completion(state);
        // The unbacked-victory gate runs whenever the conductor arbitration
        // reports the run claimed done (`state == Accepted`) but the typed
        // evidence is missing or insufficient. FM-188: the gate is now
        // itemized per stable task/job identity — each `TaskNode.id` (or job
        // id) must present its own typed-mutation evidence, and only tasks
        // lacking evidence are refused. Falls back to the aggregate
        // `typed_mutations` counter when no per-task evidence is present.
        let typed_mutations = session
            .typed_mutations
            .load(std::sync::atomic::Ordering::Relaxed);
        let claim_suggests_done =
            state == rightkit_agent_orchestration::coding::CompletionState::Accepted;
        let mut downgrade_to_unmet: Option<String> = None;
        let mut downgrade_to_active = false;
        if claim_suggests_done {
            // FM-188: assemble itemized per-task evidence from the live task
            // graph + per-task mutation map. Stable identity is TaskNode.id
            // (or job_id when the task was dispatched as a job).
            let task_evidence: Vec<
                rightkit_agent_orchestration::coding::unbacked_work_gate::TaskEvidence,
            > = {
                let tasks = lock_recover(&session.tasks, "session.tasks").clone();
                let per_task =
                    lock_recover(&session.task_typed_evidence, "session.task_typed_evidence")
                        .clone();
                let job_ids = lock_recover(&session.task_job_ids, "session.task_job_ids").clone();
                if tasks.is_empty() && per_task.is_empty() {
                    Vec::new()
                } else if tasks.is_empty() {
                    // No task graph but per-task map present (e.g. tests) — build from map
                    per_task
                        .into_iter()
                        .map(|(task_id, typed_mutations)| {
                            let job_id = job_ids.get(&task_id).cloned();
                            let mut ev =
                                rightkit_agent_orchestration::coding::unbacked_work_gate::TaskEvidence::new(
                                    task_id,
                                    typed_mutations,
                                    claim_suggests_done,
                                );
                            if let Some(jid) = job_id {
                                ev = ev.with_job(jid);
                            }
                            ev
                        })
                        .collect()
                } else {
                    tasks
                        .iter()
                        .map(|node| {
                            let typed = per_task.get(&node.id).copied().unwrap_or(0);
                            let job_id = job_ids.get(&node.id).cloned();
                            let mut ev =
                                rightkit_agent_orchestration::coding::unbacked_work_gate::TaskEvidence::new(
                                    node.id.clone(),
                                    typed,
                                    claim_suggests_done,
                                );
                            if let Some(jid) = job_id {
                                ev = ev.with_job(jid);
                            }
                            ev
                        })
                        .collect()
                }
            };
            let has_itemized = !task_evidence.is_empty();
            let governed_input = {
                let base = rightkit_agent_orchestration::coding::run_governance::GovernedCompletionInput::new(
                    rightkit_agent_orchestration::coding::run_governance::RunDomain::new(session_id, session_id),
                    rightkit_agent_orchestration::coding::completion::CompletionInput {
                        gds,
                        verification_present,
                        jury_consensus,
                        verification_approved,
                        escalate,
                    },
                )
                .with_typed_mutations(typed_mutations)
                .with_claim_suggests_done(claim_suggests_done);
                if has_itemized {
                    base.with_task_evidence(task_evidence)
                } else {
                    base
                }
            };
            let verdict = {
                let mut arbiter = session.governed_arbiter.lock().await;
                arbiter.arbitrate(&governed_input).unbacked
            };
            match verdict {
                rightkit_agent_orchestration::coding::run_governance::UnbackedVerdict::Backed => {
                    // mapped_status (likely Achieved) passes through.
                }
                rightkit_agent_orchestration::coding::run_governance::UnbackedVerdict::Unbacked {
                    attempts,
                    ..
                } => {
                    // The cap has not been reached. Keep the run going; the
                    // conductor will retry on the next turn. Surface the typed
                    // counterexample so the UI badge updates, but do NOT
                    // downgrade to a terminal status.
                    let seq = conduit.next_seq();
                    conduit.publish(ProtocolEvent::RunGovernanceRejected {
                        session_id: session_id.to_string(),
                        seq,
                        created_at: now_iso(),
                        kind: rightkit_agent_engine_protocol::RunFailureKind::OriginMissing,
                        tool_call_id: String::new(),
                        budget: None,
                        ownership: None,
                        origin: Some(rightkit_agent_engine_protocol::RunOriginFailure {
                            run_id: session_id.to_string(),
                            tool_call_id: String::new(),
                            reason: format!(
                                "unbacked attempt #{attempts}; awaiting typed mutations"
                            ),
                        }),
                    });
                    downgrade_to_active = true;
                }
                rightkit_agent_orchestration::coding::run_governance::UnbackedVerdict::Refused {
                    reason,
                    ..
                } => {
                    // Cap reached: fail closed. Surface the typed rejection
                    // AND downgrade to Unmet so the goal closes; a refused
                    // unbacked claim must not persist Achieved.
                    let seq = conduit.next_seq();
                    conduit.publish(ProtocolEvent::RunGovernanceRejected {
                        session_id: session_id.to_string(),
                        seq,
                        created_at: now_iso(),
                        kind: rightkit_agent_engine_protocol::RunFailureKind::OriginMissing,
                        tool_call_id: String::new(),
                        budget: None,
                        ownership: None,
                        origin: Some(rightkit_agent_engine_protocol::RunOriginFailure {
                            run_id: session_id.to_string(),
                            tool_call_id: String::new(),
                            reason: reason.clone(),
                        }),
                    });
                    downgrade_to_unmet = Some(reason);
                }
            }
        }
        if downgrade_to_active {
            mapped_status = rightkit_agent_engine_storage::GoalStatus::Active;
        }
        if downgrade_to_unmet.is_some() {
            mapped_status = rightkit_agent_engine_storage::GoalStatus::Unmet;
        }
        let (status, snapshot) = {
            let mut guard = lock_recover(&session.goal_state, "session.goal_state");
            let Some(gs) = guard.as_mut() else {
                return; // no active goal for this session
            };
            // The normalized verifier-gap guard is orthogonal to completion
            // arbitration. NeedsReview/Partial map to Active, but must not erase a
            // pause raised after the third identical unresolved gap.
            let status = if gs.status == "paused"
                && mapped_status == rightkit_agent_engine_storage::GoalStatus::Active
            {
                rightkit_agent_engine_storage::GoalStatus::Paused
            } else {
                mapped_status
            };
            gs.status = status.as_str().to_string();
            gs.verification_state = if status == rightkit_agent_engine_storage::GoalStatus::Achieved
                && gs.verification_approved
            {
                "verified".into()
            } else if gs.verification_approved {
                "pending".into()
            } else {
                "failed".into()
            };
            gs.closed_at = crate::goal_terminal::is_terminal(status).then(now_iso);
            (status, gs.snapshot())
        };
        let goal_id = snapshot.goal_id.clone();
        let snapshot_for_storage = snapshot.clone();

        if status == rightkit_agent_engine_storage::GoalStatus::Paused {
            if let Some(storage) = &session.storage {
                let _ = storage
                    .lock()
                    .await
                    .update_goal_status(&goal_id, status, None);
            }
        }

        if crate::goal_terminal::is_terminal(status) {
            if let Some(storage) = &session.storage {
                let store = storage.lock().await;
                if let Ok(Some(mut row)) = store.get_goal(&goal_id) {
                    row.status = status;
                    row.closed_at = snapshot_for_storage.closed_at.clone();
                    row.dismissed_at = snapshot_for_storage.dismissed_at.clone();
                    row.decomposition_state = snapshot_for_storage.decomposition_state.clone();
                    row.plan_revision = snapshot_for_storage.plan_revision;
                    row.now = snapshot_for_storage.now.clone();
                    row.next = snapshot_for_storage.next.clone();
                    row.steps = snapshot_for_storage.steps.clone();
                    row.verification_state = snapshot_for_storage.verification_state.clone();
                    let _ = store.upsert_goal(&row);
                } else {
                    let closed_at = snapshot_for_storage.closed_at.as_deref();
                    let _ = store.update_goal_status(&goal_id, status, closed_at);
                }
            }
            *lock_recover(&session.goal_state, "session.goal_state") = None;
            // [council P1] Unattended-close desktop notification.
            let _ = crate::api_notifications::append_desktop_notification(
                std::path::Path::new(&session.workspace_root),
                "goal",
                None,
                Some(session_id),
                format!("Goal {}", status.as_str()),
                format!("The goal closed with status: {}.", status.as_str()),
            );
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

/// Look up the team preset that applies to `goal_text` from a snapshot of
/// currently-known team definitions. Returns the first team whose
/// `name.to_lowercase()` appears as a substring of the lowercased goal text;
/// team presets without a `max_workers` value still count so a future
/// budget-derivation rule can pull `budget_usd` off the same hit. The
/// spawn budget builder consults this exact lookup so the conductor's
/// task-graph budget rule and the run-budget rule agree on which team
/// "owns" the run.
pub(crate) fn find_team_preset_for_goal(
    goal_text: &str,
    teams: &[rightkit_agent_tools::coding_tools::TeamDefinition],
) -> Option<rightkit_agent_tools::coding_tools::TeamDefinition> {
    let needle = goal_text.to_lowercase();
    teams
        .iter()
        .find(|team| {
            let name = team.name.trim();
            !name.is_empty() && needle.contains(&name.to_lowercase())
        })
        .cloned()
}

#[cfg(test)]
mod finalize_goal_integration_tests {
    //! Call-site integration coverage for `finalize_goal`'s three
    //! `UnbackedVerdict` branches.
    //!
    //! The function drives the unbacked-victory gate and surfaces the
    //! three outcomes on the live bus:
    //! - `Backed` → mapped_status (Achieved) passes through.
    //! - `Unbacked { attempts, .. }` → mapped_status = Active, session keeps
    //!   running.
    //! - `Refused { reason, .. }` → mapped_status = Unmet, session closes.
    //!
    //! These tests stand up a real `DaemonState` and a real `Conduit`, then
    //! drive `finalize_goal` with `CompletionState::Accepted` and watch the
    //! resulting `GoalStatus` on `LiveSession::goal_state`. They also
    //! subscribe to the conduit and assert the typed
    //! `RunGovernanceRejected` event fires for the unbacked branches.
    use crate::api::{lock_recover, DaemonState, LiveSession};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use rightkit_agent_engine_core::{Conduit, GoalState};
    use rightkit_agent_engine_protocol::event::GoalStepV1;
    use rightkit_agent_engine_protocol::ProtocolEvent;
    use rightkit_agent_orchestration::coding::CompletionState;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn seed_goal_state(live: &Arc<LiveSession>) {
        *lock_recover(&live.goal_state, "session.goal_state") = Some(GoalState {
            id: "goal-1".into(),
            text: "ship the agent loop".into(),
            status: "active".into(),
            budget_usd: 0.0,
            budget_tokens: 1000,
            tokens_used: 0,
            cost_usd: 0.0,
            continuation_count: 0,
            no_progress_count: 0,
            gds_score: 1.0,
            verification_present: false,
            verification_approved: false,
            started_at: "t0".into(),
            closed_at: None,
            dismissed_at: None,
            decomposition_state: "missing".into(),
            plan_revision: 0,
            now: None,
            next: None,
            steps: Vec::new(),
            verification_state: "unverified".into(),
        });
    }

    fn session_status(state: &DaemonState, id: &str) -> Option<String> {
        lock_recover(
            &state.get_session(id).expect("session exists").goal_state,
            "session.goal_state",
        )
        .as_ref()
        .map(|gs| gs.status.clone())
    }

    /// Real call-site integration: `Accepted` with zero typed mutations
    /// and no verification yields `Unbacked { attempts: 1, .. }` on the
    /// first call — the gate keeps the run going by mapping the status
    /// to `Active`. The session is NOT closed, and the typed
    /// `RunGovernanceRejected` event hits the conduit so the UI badge
    /// updates without parsing prose.
    #[tokio::test]
    async fn finalize_goal_unbacked_keeps_session_active_and_emits_typed_rejection() {
        let state = DaemonState::new("test-token-deadbeef");
        state.create_session("s1", None, "/tmp", "s1", None);
        seed_goal_state(state.get_session("s1").as_ref().unwrap());
        let conduit = Arc::new(Conduit::new(64));
        let mut sub = conduit.subscribe();

        state
            .finalize_goal("s1", &conduit, CompletionState::Accepted)
            .await;

        // Goal stays Active (mapped from Unbacked, attempts < cap).
        assert_eq!(
            session_status(&state, "s1").as_deref(),
            Some("active"),
            "unbacked verdict must keep the goal open"
        );
        // Typed rejection hit the bus; surface reason carries the attempt
        // counter so the operator UI can render the unbacked badge.
        let event = match tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv())
            .await
            .expect("RunGovernanceRejected must publish on unbacked attempt")
        {
            rightkit_agent_engine_core::conduit::ConduitReceive::Event(event) => event,
            other => panic!("conduit closed or lagged: {other:?}"),
        };
        match event {
            ProtocolEvent::RunGovernanceRejected { origin, .. } => {
                let origin = origin.expect("unbacked path carries origin payload");
                assert!(origin.reason.contains("unbacked attempt #1"));
            }
            other => panic!("expected RunGovernanceRejected, got {other:?}"),
        }
    }

    /// Real call-site integration: once the gate's attempt counter hits
    /// the policy cap (`max_unbacked_reenter = 3` by default), the next
    /// `Accepted` claim with zero typed mutations is refused — the goal
    /// is downgraded to `Unmet`, and the session closes (status
    /// transitions to `unmet` and is then cleared). Proves the third
    /// `UnbackedVerdict` branch is reachable from a real finalize_goal
    /// call, not just the arbiter unit test.
    #[tokio::test]
    async fn finalize_goal_refused_after_cap_downgrades_to_unmet() {
        let state = DaemonState::new("test-token-deadbeef");
        state.create_session("s1", None, "/tmp", "s1", None);
        seed_goal_state(state.get_session("s1").as_ref().unwrap());
        let conduit = Arc::new(Conduit::new(64));
        let mut sub = conduit.subscribe();

        // Drive 3 unbacked attempts (the cap is 3 by default). On the
        // 3rd the gate still returns Unbacked { attempts: 3 }; on the
        // 4th it returns Refused because the cap has been reached and
        // `fail_closed_on_unbacked = true`.
        for _ in 0..3 {
            state
                .finalize_goal("s1", &conduit, CompletionState::Accepted)
                .await;
        }
        // 4th call: cap reached → Refused → downgrade to Unmet.
        state
            .finalize_goal("s1", &conduit, CompletionState::Accepted)
            .await;

        // Goal closed with `unmet`. The session's `goal_state` slot is
        // cleared on terminal closure, so reading back must yield None.
        assert!(
            lock_recover(
                &state.get_session("s1").expect("session exists").goal_state,
                "session.goal_state"
            )
            .is_none(),
            "refused unbacked verdict must clear the in-memory goal slot"
        );
        // Pull the most recent rejection — the Refused branch publishes
        // its reason on the bus with the cap-exceeded message.
        let mut last_origin_reason: Option<String> = None;
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await {
                Ok(rightkit_agent_engine_core::conduit::ConduitReceive::Event(
                    ProtocolEvent::RunGovernanceRejected {
                        origin: Some(o), ..
                    },
                )) => {
                    last_origin_reason = Some(o.reason);
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        let reason = last_origin_reason.expect("refused verdict must publish rejection");
        assert!(reason.contains("unbacked-work cap"), "got: {reason}");
    }

    #[tokio::test]
    async fn victory_gate_terminal_agreement_persists_before_goal_progress_event() {
        let storage = Arc::new(tokio::sync::Mutex::new(
            rightkit_agent_engine_storage::Storage::open_in_memory().unwrap(),
        ));
        let state = DaemonState::new("test-token-deadbeef").with_storage(storage.clone());
        state.create_session("victory", None, "/tmp", "victory", None);
        for _ in 0..200 {
            if storage
                .lock()
                .await
                .get_session("victory")
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(storage
            .lock()
            .await
            .get_session("victory")
            .unwrap()
            .is_some());

        let app = crate::api::build_router(state.clone());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sessions/victory/goal")
                    .header("authorization", "Bearer test-token-deadbeef")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"text":"ship it"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let live = state.get_session("victory").expect("live session");
        let conduit = live.conduit.clone();
        {
            let mut goal = lock_recover(&live.goal_state, "session.goal_state");
            let goal = goal.as_mut().expect("set goal creates goal state");
            goal.gds_score = 1.0;
            goal.verification_present = true;
            goal.verification_approved = true;
            goal.decomposition_state = "usable".into();
            goal.plan_revision = 7;
            goal.steps = vec![GoalStepV1 {
                id: "finish".into(),
                label: "Finish it".into(),
                weight: 1,
                state: "done".into(),
                verification_state: "verified".into(),
            }];
        }
        {
            let mut evidence =
                lock_recover(&live.completion_evidence, "session.completion_evidence");
            evidence.verification_present = true;
            evidence.verification_approved = true;
        }
        live.typed_mutations
            .store(1, std::sync::atomic::Ordering::Relaxed);
        let mut events = conduit.subscribe();

        state
            .finalize_goal("victory", &conduit, CompletionState::Accepted)
            .await;

        let event = match tokio::time::timeout(std::time::Duration::from_millis(50), events.recv())
            .await
            .expect("terminal goal event")
        {
            rightkit_agent_engine_core::conduit::ConduitReceive::Event(event) => event,
            other => panic!("conduit closed or lagged: {other:?}"),
        };
        let ProtocolEvent::GoalProgressUpdated { snapshot, .. } = event else {
            panic!("expected terminal GoalProgressUpdated");
        };
        assert_eq!(snapshot.status, "achieved");
        assert_eq!(snapshot.verification_state, "verified");
        assert_eq!(snapshot.completion_percent, Some(100));
        assert_eq!(snapshot.plan_revision, 7);

        let persisted = storage
            .lock()
            .await
            .newest_displayable_goal_for_session("victory")
            .unwrap()
            .expect("terminal goal persisted before event");
        assert_eq!(
            persisted.status,
            rightkit_agent_engine_storage::GoalStatus::Achieved
        );
        assert_eq!(persisted.verification_state, "verified");
        assert_eq!(persisted.plan_revision, snapshot.plan_revision);
        assert_eq!(persisted.steps, snapshot.steps);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/sessions/victory/goal")
                    .header("authorization", "Bearer test-token-deadbeef")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let hydrated = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["goal"].clone();
        assert_eq!(hydrated["status"], "achieved");
        assert_eq!(hydrated["verificationState"], "verified");
        assert_eq!(hydrated["completionPercent"], 100);
        assert_eq!(hydrated["planRevision"], 7);
    }

    /// `find_team_preset_for_goal` is the single source of truth for
    /// team lookup that both the spawn budget builder and the conductor
    /// task graph consult. A goal mentioning "alpha" must hit the
    /// "alpha" team; a goal without a team name must return None. The
    /// case-insensitive substring rule is the contract — anything else
    /// breaks the budget cap.
    #[test]
    fn find_team_preset_for_goal_substring_is_case_insensitive() {
        let teams = vec![
            rightkit_agent_tools::coding_tools::TeamDefinition {
                name: "Alpha".into(),
                description: None,
                members: vec!["worker".into()],
                max_workers: Some(3),
                budget_usd: None,
            },
            rightkit_agent_tools::coding_tools::TeamDefinition {
                name: "Beta".into(),
                description: None,
                members: vec!["worker".into()],
                max_workers: Some(2),
                budget_usd: None,
            },
        ];
        let hit = super::find_team_preset_for_goal("Run the alpha team overnight", &teams)
            .expect("alpha must match");
        assert_eq!(hit.name, "Alpha");
        let none = super::find_team_preset_for_goal("solo session, no team", &teams);
        assert!(none.is_none(), "no team name → None");
        // Empty team name in the snapshot must not match a non-empty
        // goal text — defends against a malformed team def hijacking
        // every run.
        let teams_with_empty = vec![rightkit_agent_tools::coding_tools::TeamDefinition {
            name: "   ".into(),
            description: None,
            members: vec!["worker".into()],
            max_workers: Some(99),
            budget_usd: None,
        }];
        assert!(super::find_team_preset_for_goal("any goal text", &teams_with_empty).is_none());
    }
}
