/// Tools whose presence in a role's allow-list means the role can change the
/// workspace.
const MUTATING_ROLE_TOOLS: &[&str] = &["write_file", "apply_patch", "edit_file", "shell"];

/// True when every task is assigned to a role that has no tool able to change
/// the workspace, so the goal cannot leave typed mutation evidence by design.
fn goal_tasks_are_read_only(
    registry: &rightkit_agent_orchestration::coding::registry::AgentRegistry,
    tasks: &[rightkit_agent_engine_protocol::TaskNode],
) -> bool {
    !tasks.is_empty() && tasks.iter().all(|task| task_is_read_only(registry, task))
}

/// True when the task's role has no tool able to change the workspace. An
/// unregistered role is treated as able to write.
fn task_is_read_only(
    registry: &rightkit_agent_orchestration::coding::registry::AgentRegistry,
    task: &rightkit_agent_engine_protocol::TaskNode,
) -> bool {
    registry.get(&task.assigned_role).is_some_and(|descriptor| {
        !descriptor
            .allowed_tools
            .iter()
            .any(|tool| MUTATING_ROLE_TOOLS.contains(&tool.as_str()))
    })
}

/// Tools that record typed workspace mutations (`ToolResult::with_mutations`).
/// `shell` can change files but never records a typed mutation, so a role whose
/// only writing tool is `shell` (signoff, verifier) cannot owe that evidence.
const TYPED_MUTATION_TOOLS: &[&str] = &["write_file", "apply_patch", "edit_file", "multi_edit"];

/// True when the task's role holds a tool that records typed mutations, so a
/// done-claim for it must be backed by its own evidence. An unregistered role
/// is held to it (fail closed).
fn task_owes_typed_mutations(
    registry: &rightkit_agent_orchestration::coding::registry::AgentRegistry,
    task: &rightkit_agent_engine_protocol::TaskNode,
) -> bool {
    registry.get(&task.assigned_role).is_none_or(|descriptor| {
        descriptor
            .allowed_tools
            .iter()
            .any(|tool| TYPED_MUTATION_TOOLS.contains(&tool.as_str()))
    })
}

/// Itemized per-task evidence for the unbacked-work gate: each task carries the
/// typed mutations its workers recorded in `task_typed_evidence` (audit
/// 2026-11-05 orchestration H2), and only a task whose role records typed
/// mutations is held to the done-claim.
fn itemize_task_evidence(
    registry: &rightkit_agent_orchestration::coding::registry::AgentRegistry,
    tasks: &[rightkit_agent_engine_protocol::TaskNode],
    per_task: &std::collections::HashMap<String, u32>,
    job_ids: &std::collections::HashMap<String, String>,
    claim_suggests_done: bool,
) -> Vec<rightkit_agent_orchestration::coding::unbacked_work_gate::TaskEvidence> {
    tasks
        .iter()
        .map(|node| {
            let typed = per_task.get(&node.id).copied().unwrap_or(0);
            let claim = claim_suggests_done && task_owes_typed_mutations(registry, node);
            let mut ev =
                rightkit_agent_orchestration::coding::unbacked_work_gate::TaskEvidence::new(
                    node.id.clone(),
                    typed,
                    claim,
                );
            if let Some(jid) = job_ids.get(&node.id) {
                ev = ev.with_job(jid.clone());
            }
            ev
        })
        .collect()
}

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
        // A goal whose tasks are all read-only (audit, research, review) has no
        // workspace mutation to back, so the unbacked-work gate does not apply to
        // it and must not mark it Unmet on its fourth verdict (orchestration
        // rule 13, D9).
        let goal_is_read_only = {
            let tasks = lock_recover(&session.tasks, "session.tasks").clone();
            goal_tasks_are_read_only(&self.agent_registry, &tasks)
        };
        let claim_suggests_done = state
            == rightkit_agent_orchestration::coding::CompletionState::Accepted
            && !goal_is_read_only;
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
                    itemize_task_evidence(
                        &self.agent_registry,
                        &tasks,
                        &per_task,
                        &job_ids,
                        claim_suggests_done,
                    )
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

/// True when `phrase` occurs in `text` with no letter or digit touching either end, so a team
/// named `ui` does not match "quiz" and `api` does not match "capital".
fn contains_whole_phrase(text: &str, phrase: &str) -> bool {
    let mut from = 0;
    while let Some(found) = text[from..].find(phrase) {
        let start = from + found;
        let end = start + phrase.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric) {
            return true;
        }
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// Look up the team preset that applies to `goal_text` from a snapshot of
/// currently-known team definitions. Returns the first team whose
/// `name.to_lowercase()` appears as a whole word or phrase in the lowercased goal text;
/// team presets without a `max_workers` value still count so a future
/// budget-derivation rule can pull `budget_usd` off the same hit. The
/// spawn budget builder consults this exact lookup so the conductor's
/// task-graph budget rule and the run-budget rule agree on which team
/// "owns" the run.
pub(crate) fn find_team_preset_for_goal(
    goal_text: &str,
    teams: &[rightkit_agent_coding_tools::TeamDefinition],
) -> Option<rightkit_agent_coding_tools::TeamDefinition> {
    let needle = goal_text.to_lowercase();
    // Deterministic whatever the input order (orchestration rule 25): when
    // several preset names occur in the goal, the longest name wins as the most
    // specific, and equal lengths fall back to the alphabetically first name.
    teams
        .iter()
        .filter(|team| {
            let name = team.name.trim();
            !name.is_empty() && contains_whole_phrase(&needle, &name.to_lowercase())
        })
        .max_by(|a, b| {
            a.name
                .trim()
                .len()
                .cmp(&b.name.trim().len())
                .then_with(|| b.name.cmp(&a.name))
        })
        .cloned()
}

#[cfg(test)]
mod team_preset_tests {
    use super::contains_whole_phrase;

    #[test]
    fn a_short_team_name_inside_another_word_does_not_match() {
        assert!(!contains_whole_phrase("build a quiz app", "ui"));
        assert!(contains_whole_phrase("polish the ui today", "ui"));
        assert!(contains_whole_phrase("ui", "ui"));
        assert!(contains_whole_phrase("fix the web-ui", "ui"));
    }
}

#[cfg(test)]
mod itemized_evidence_tests {
    //! Audit 2026-11-05 orchestration H2: a jury-accepted plan of `coder` then
    //! `signoff` must be fully backed once the coder's typed mutations are
    //! recorded, and must stay unbacked when they are not.
    use super::itemize_task_evidence;
    use rightkit_agent_orchestration::coding::registry::AgentRegistry;
    use rightkit_agent_orchestration::coding::unbacked_work_gate::{
        PerTaskVerdict, UnbackedWorkGate,
    };
    use std::collections::HashMap;

    fn node(id: &str, role: &str) -> rightkit_agent_engine_protocol::TaskNode {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "title": id,
            "doneWhen": "",
            "state": "done",
            "assignedRole": role,
            "createdAt": "",
            "updatedAt": "",
        }))
        .expect("task node")
    }

    fn plan() -> Vec<rightkit_agent_engine_protocol::TaskNode> {
        vec![node("t1", "coder"), node("t2", "signoff")]
    }

    #[test]
    fn recorded_coder_mutations_back_the_accepted_claim() {
        let registry = AgentRegistry::with_builtins();
        let per_task = HashMap::from([("t1".to_string(), 1u32)]);
        let evidence = itemize_task_evidence(&registry, &plan(), &per_task, &HashMap::new(), true);
        let mut gate = UnbackedWorkGate::default();
        let verdicts = gate.check("run", &evidence);
        assert!(UnbackedWorkGate::is_fully_backed(&verdicts), "{verdicts:?}");
    }

    #[test]
    fn signoff_is_not_held_to_typed_mutations() {
        let registry = AgentRegistry::with_builtins();
        let evidence =
            itemize_task_evidence(&registry, &plan(), &HashMap::new(), &HashMap::new(), true);
        assert!(evidence[0].claim_suggests_done, "coder owes evidence");
        assert!(
            !evidence[1].claim_suggests_done,
            "signoff (shell only) does not"
        );
    }

    #[test]
    fn missing_coder_evidence_stays_unbacked() {
        let registry = AgentRegistry::with_builtins();
        let evidence =
            itemize_task_evidence(&registry, &plan(), &HashMap::new(), &HashMap::new(), true);
        let mut gate = UnbackedWorkGate::default();
        let verdicts = gate.check("run", &evidence);
        assert!(matches!(
            verdicts.as_slice(),
            [PerTaskVerdict::Unbacked { task_id, .. }] if task_id == "t1"
        ));
    }

    #[test]
    fn unregistered_role_is_held_to_evidence() {
        let registry = AgentRegistry::with_builtins();
        let evidence = itemize_task_evidence(
            &registry,
            &[node("t1", "no-such-role")],
            &HashMap::new(),
            &HashMap::new(),
            true,
        );
        assert!(evidence[0].claim_suggests_done);
    }
}
