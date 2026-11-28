/// The `completionArbitrated` event: GDS, jury consensus, jury approval and the
/// arbiter's state, recorded for every goal verification (orchestration rule
/// 10). It rides the observation event so clients and the journal need no new
/// event kind.
fn completion_arbitrated_event(
    session_id: &str,
    seq: i64,
    gds: f64,
    consensus: bool,
    approved: bool,
    state: &str,
) -> ProtocolEvent {
    let now = now_iso();
    let mut fields = std::collections::HashMap::new();
    fields.insert("gds".to_string(), format!("{gds:.4}"));
    fields.insert("consensus".to_string(), consensus.to_string());
    fields.insert("approved".to_string(), approved.to_string());
    fields.insert("state".to_string(), state.to_string());
    ProtocolEvent::OrchestrationObservationRecorded {
        session_id: session_id.to_string(),
        seq,
        created_at: now.clone(),
        observation: rightkit_agent_engine_protocol::OrchestrationObservation {
            id: format!("completion-arbitrated-{seq}"),
            kind: "completionArbitrated".into(),
            subject: session_id.to_string(),
            summary: format!(
                "completion arbitrated: {state} (GDS {gds:.2}, consensus {consensus}, approved {approved})"
            ),
            fields,
            created_at: now,
        },
    }
}

/// The arbiter's answer when the same unresolved criterion survives repeated
/// independent verifications: an explicit escalation (orchestration rule 11).
fn stalled_verification_arbitration(gds: f64) -> rightkit_agent_orchestration::coding::Arbitration {
    rightkit_agent_orchestration::coding::arbitrate(
        &rightkit_agent_orchestration::coding::CompletionInput {
            gds,
            verification_present: true,
            jury_consensus: true,
            verification_approved: false,
            escalate: true,
        },
        &rightkit_agent_orchestration::coding::CompletionConfig::default(),
    )
}

/// The report line for a juror that returned no verdict (orchestration rule
/// 9): the missing juror is named with the reason instead of being dropped.
fn juror_no_verdict_line(role: &str, reason: &str) -> String {
    let reason: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("juror {role}: no verdict ({})", truncate(&reason, 200))
}

impl DaemonState {
    /// Spawn an agent with a hard timeout so a slow/stuck verifier or juror
    /// cannot hang the victory gate forever.
    async fn spawn_with_timeout(
        &self,
        role: &str,
        prompt: &str,
        session_id: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        use rightkit_agent_coding_tools::SubagentSpawner;
        match tokio::time::timeout(
            timeout,
            self.spawn(
                role,
                prompt,
                session_id,
                rightkit_agent_coding_tools::SubagentSpawnOptions::default(),
            ),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(format!("{role} timed out after {timeout:?}")),
        }
    }

    /// Agent-to-agent query: `from_role` asks `to_role` a question. Emits
    /// protocol events around a real spawned answer. Returns (answer, confidence).
    async fn agent_query(
        &self,
        from_role: &str,
        to_role: &str,
        query: &str,
        session_id: &str,
    ) -> Result<(String, f64), String> {
        let query_id = format!("q-{}", now_millis());
        if let Some(session) = self.get_session(session_id) {
            let seq = session.conduit.next_seq();
            session.conduit.publish(ProtocolEvent::AgentQueryIssued {
                session_id: session_id.to_string(),
                seq,
                created_at: now_iso(),
                query_id: query_id.clone(),
                from_role: from_role.to_string(),
                to_role: to_role.to_string(),
                query_type: "adjudication".into(),
                expected_format: "VERDICT + CONFIDENCE".into(),
            });
        }
        let answer = self
            .spawn_with_timeout(to_role, query, session_id, Duration::from_secs(180))
            .await?;
        let (_, confidence) = parse_verdict(&answer);
        if let Some(session) = self.get_session(session_id) {
            let seq = session.conduit.next_seq();
            session.conduit.publish(ProtocolEvent::AgentQueryResponded {
                session_id: session_id.to_string(),
                seq,
                created_at: now_iso(),
                query_id,
                from_role: to_role.to_string(),
                to_role: from_role.to_string(),
                confidence,
            });
        }
        Ok((answer, confidence))
    }

    /// The victory gate: an independent verifier produces a `goal_verification`
    /// artifact, then the auto-jury (jurors that are NOT the producer) reviews
    /// it; the completion arbiter maps GDS + that evidence to a terminal state.
    async fn run_victory_gate(
        &self,
        session_id: &str,
        goal: &str,
        outcome: &RunOutcome,
        gds: f64,
        workspace_config: &rightkit_agent_config::HostConfig,
        initial_git_ref: Option<&str>,
    ) -> (
        rightkit_agent_orchestration::coding::CompletionState,
        String,
    ) {
        use rightkit_agent_orchestration::coding::CompletionState;

        let session = match self.get_session(session_id) {
            Some(s) => s,
            None => {
                return (
                    CompletionState::NeedsReview,
                    "session no longer exists".into(),
                );
            }
        };
        let conduit = session.conduit.clone();

        let evidence = outcome
            .results
            .iter()
            .filter(|r| r.success)
            .map(|r| format!("[{}] {}", r.agent, r.output))
            .collect::<Vec<_>>()
            .join("\n\n");

        // 1. Independent verification (the producer of the goal_verification).
        let verify_prompt = format!(
            "Goal: {goal}\n\nThe workers report:\n{evidence}\n\nIndependently verify whether the \
         goal is genuinely and completely achieved by inspecting the ACTUAL deliverables in \
         the workspace (read the files, run read-only checks). Do not trust the workers' \
         claims. Begin your reply with exactly 'VERDICT: PASS' or 'VERDICT: FAIL', then state \
         precisely what you checked and why. If the verdict is FAIL, end with one or more \
         machine-readable lines `UNRESOLVED_CRITERION: <stable criterion id>` and \
         `MISSING_EVIDENCE: <stable check id or workspace-relative path:line[:column]>`. Reuse \
         the same criterion IDs on later checks of the same goal."
        );
        let gate_timeout = Duration::from_secs(180);
        let verification = match self
            .spawn_with_timeout("signoff", &verify_prompt, session_id, gate_timeout)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                return (
                    CompletionState::NeedsReview,
                    format!("NeedsReview: verification could not run: {e}"),
                );
            }
        };
        let (passed, _) = parse_verdict(&verification);
        let vhash = content_hash(&verification);

        let seq = conduit.next_seq();
        conduit.publish(ProtocolEvent::VerificationFinished {
            session_id: session_id.to_string(),
            seq,
            created_at: now_iso(),
            summary: truncate(&verification, 200),
            passed,
            tool: "signoff".into(),
            issue_count: 0,
            issues: vec![],
        });

        // Feed the real independent-verifier output into the normalized gap guard.
        // A pass resets the streak. Three identical unresolved criterion/evidence
        // sets pause the goal; a changed set starts a fresh streak.
        let gap_observation = {
            let mut tracker = lock_recover(&session.goal_gap_tracker, "session.goal_gap_tracker");
            if passed {
                tracker.reset();
                rightkit_agent_engine_core::GoalGapObservation::Reset
            } else {
                let gap = rightkit_agent_engine_core::UnresolvedGoalGap::from_verifier_output(
                    &verification,
                );
                let mut goal = lock_recover(&session.goal_state, "session.goal_state");
                match goal.as_mut() {
                    Some(goal) => tracker.observe_for_goal(goal, &gap, false),
                    None => tracker.observe(&gap, false),
                }
            }
        };

        if !passed {
            {
                let mut evidence =
                    lock_recover(&session.completion_evidence, "session.completion_evidence");
                evidence.verification_present = true;
                evidence.verification_approved = false;
            }
            if let Some(goal) = lock_recover(&session.goal_state, "session.goal_state").as_mut() {
                goal.verification_present = true;
                goal.verification_approved = false;
                goal.verification_state = "failed".into();
            }
            push_artifact(
                &session,
                goal_verification_artifact(&vhash, false, "auto-jury", vec![], &verification),
            );
            if let rightkit_agent_engine_core::GoalGapObservation::Stalled { count } =
                gap_observation
            {
                // Repeated failure is the arbiter's escalation condition: the
                // goal is handed to the user as Blocked, so `Escalate` is
                // reachable (orchestration rule 11).
                let arb = stalled_verification_arbitration(gds);
                return (
                    arb.state,
                    format!(
                        "{}: goal paused after {count} consecutive independent verifications \
                 reported the same unresolved criterion/evidence set.\n{}",
                        arb.state.as_str(),
                        truncate(&verification, 400)
                    ),
                );
            }
            // Verification present but failed → arbiter bands by GDS.
            let arb = rightkit_agent_orchestration::coding::arbitrate(
                &rightkit_agent_orchestration::coding::CompletionInput {
                    gds,
                    verification_present: true,
                    jury_consensus: true,
                    verification_approved: false,
                    escalate: false,
                },
                &rightkit_agent_orchestration::coding::CompletionConfig::default(),
            );
            return (
                arb.state,
                format!(
                    "{} (GDS {gds:.2}): independent verification failed.\n{}",
                    arb.state.as_str(),
                    truncate(&verification, 400)
                ),
            );
        }

        // 2. Auto-jury. Jurors are distinct from the producer (signoff). They
        // inspect the workspace independently, so they run concurrently — serial
        // spawning only added latency (up to one full gate_timeout per juror).
        let juror_roles = ["challenger", "auditor"];
        let juror_prompt = |jr: &str| {
            format!(
                "Goal: {goal}\n\nAn independent verifier produced this goal_verification:\n\
             {verification}\n\nWorker results:\n{evidence}\n\nAs the '{jr}' juror, decide \
             whether the goal is truly complete and the verification is sound — inspect the \
             workspace yourself, do not rubber-stamp. Begin with exactly 'VERDICT: APPROVE' or \
             'VERDICT: REJECT', then 'CONFIDENCE: <0.0-1.0>', then your reasons."
            )
        };
        let jp_challenger = juror_prompt(juror_roles[0]);
        let jp_auditor = juror_prompt(juror_roles[1]);
        let (r0, r1) = tokio::join!(
            self.spawn_with_timeout(juror_roles[0], &jp_challenger, session_id, gate_timeout),
            self.spawn_with_timeout(juror_roles[1], &jp_auditor, session_id, gate_timeout),
        );
        let mut verdicts: Vec<JurorVerdict> = Vec::new();
        let mut missing_jurors: Vec<String> = Vec::new();
        for (jr, result) in juror_roles.iter().zip([r0, r1]) {
            match result {
                Ok(t) => {
                    let (approve, confidence) = parse_verdict(&t);
                    verdicts.push(JurorVerdict {
                        juror: jr.to_string(),
                        approve,
                        confidence,
                        reasoning: t,
                    });
                }
                Err(reason) => missing_jurors.push(juror_no_verdict_line(jr, &reason)),
            }
        }

        let mut decision = JurySystem::new(2).decide(&verdicts);
        // Jury tie-break: if the jury splits, the conductor queries an impartial
        // architect to adjudicate, then re-decides with the extra verdict.
        if matches!(decision, JuryDecision::NoConsensus) {
            let adj = format!(
                "Goal: {goal}\n\nThe jury split on whether the goal is genuinely complete. The \
             independent verification:\n{verification}\n\nAs an impartial adjudicator, inspect \
             the workspace and decide. Begin with exactly 'VERDICT: APPROVE' or \
             'VERDICT: REJECT', then 'CONFIDENCE: <0.0-1.0>'."
            );
            if let Ok((answer, conf)) = self
                .agent_query("conductor", "architect", &adj, session_id)
                .await
            {
                let (approve, _) = parse_verdict(&answer);
                verdicts.push(JurorVerdict {
                    juror: "architect".into(),
                    approve,
                    confidence: conf,
                    reasoning: answer,
                });
                decision = JurySystem::new(2).decide(&verdicts);
            }
        }
        let approved = matches!(decision, JuryDecision::Approved { .. });
        let consensus = !matches!(decision, JuryDecision::NoConsensus);
        let approvals: Vec<rightkit_agent_engine_protocol::ArtifactApproval> = verdicts
            .iter()
            .filter(|v| v.approve)
            .map(|v| rightkit_agent_engine_protocol::ArtifactApproval {
                approver: v.juror.clone(),
                bound_hash: vhash.clone(),
                created_at: now_iso(),
            })
            .collect();
        push_artifact(
            &session,
            goal_verification_artifact(&vhash, approved, "auto-jury", approvals, &verification),
        );

        // The arbiter maps GDS + the gate evidence to the terminal state.
        let arb = rightkit_agent_orchestration::coding::arbitrate(
            &rightkit_agent_orchestration::coding::CompletionInput {
                gds,
                verification_present: true,
                jury_consensus: consensus,
                verification_approved: approved,
                escalate: false,
            },
            &rightkit_agent_orchestration::coding::CompletionConfig::default(),
        );
        let approve_count = verdicts.iter().filter(|v| v.approve).count();
        // Orchestration rule 10: the arbiter's inputs and its answer are one
        // recorded event, not only text in the report.
        {
            let seq = conduit.next_seq();
            conduit.publish(completion_arbitrated_event(
                session_id,
                seq,
                gds,
                consensus,
                approved,
                arb.state.as_str(),
            ));
        }
        {
            let mut evidence =
                lock_recover(&session.completion_evidence, "session.completion_evidence");
            evidence.verification_present = true;
            evidence.verification_approved = approved;
        }
        if let Some(goal) = lock_recover(&session.goal_state, "session.goal_state").as_mut() {
            goal.verification_present = true;
            goal.verification_approved = approved;
            goal.verification_state = if approved
                && arb.state == rightkit_agent_orchestration::coding::CompletionState::Accepted
            {
                "verified".into()
            } else if approved {
                "pending".into()
            } else {
                "failed".into()
            };
        }
        let mut summary = format!(
            "{} (GDS {gds:.2}). Verification passed; auto-jury {approve_count}/{} approve, \
         decision {decision:?}. {}",
            arb.state.as_str(),
            verdicts.len(),
            arb.rationale,
        );

        for line in &missing_jurors {
            summary.push('\n');
            summary.push_str(line);
        }

        // Eval-hack advisory: inspect the run's diff for scoring exploitation
        // patterns before accepting a green verification.
        if workspace_config.orchestration.eval_hack_advisory() {
            let eval_findings =
                run_eval_hack_detection(&session.workspace_root, initial_git_ref, &outcome.results);
            if !eval_findings.is_empty() {
                let warn_lines: Vec<String> = eval_findings
                    .iter()
                    .map(|f| format!("[eval-hack:{}] {}", f.category, f.message))
                    .collect();
                summary.push_str(&format!(
                    "\n⚠ Eval-hack advisory ({} finding(s)): {}",
                    eval_findings.len(),
                    warn_lines.join("; ")
                ));
            }
        }

        (arb.state, summary)
    }
}
