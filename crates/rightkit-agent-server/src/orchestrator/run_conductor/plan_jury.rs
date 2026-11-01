impl DaemonState {
    /// Plan-gate (the ArchitectPlan path's review, gated). Jury the **plan**
    /// before any code is written — "is this plan sound, complete, correctly
    /// scoped?", not "is it done?". Reuses the victory-gate juror mechanism
    /// (challenger + auditor → [`JurySystem`]) but over the plan text. Returns
    /// `true` to proceed to the build, `false` to block (a rejected plan).
    ///
    /// This is distinct from the end-audit: the existing victory gate (run after
    /// the build) is the end-audit; this runs before it, on the plan.
    async fn plan_jury(&self, session_id: &str, goal: &str, plan_text: &str) -> bool {
        let gate_timeout = Duration::from_secs(180);
        let juror_prompt = |jr: &str| {
            format!(
            "Goal: {goal}\n\nProposed plan (before any code is written):\n{plan_text}\n\nAs the \
                 '{jr}' juror, decide whether this PLAN is sound, complete, and correctly scoped — \
                 NOT whether it is done. Look for missing steps, wrong sequencing, unhandled \
                 failure modes, and over- or under-scoping. Begin with exactly 'VERDICT: APPROVE' \
                 or 'VERDICT: REJECT', then 'CONFIDENCE: <0.0-1.0>', then your reasons."
        )
        };
        let jp_challenger = juror_prompt("challenger");
        let jp_auditor = juror_prompt("auditor");
        let (r0, r1) = tokio::join!(
            self.spawn_with_timeout("challenger", &jp_challenger, session_id, gate_timeout),
            self.spawn_with_timeout("auditor", &jp_auditor, session_id, gate_timeout),
        );
        let mut verdicts: Vec<JurorVerdict> = Vec::new();
        for (jr, result) in ["challenger", "auditor"].iter().zip([r0, r1]) {
            if let Ok(t) = result {
                let (approve, confidence) = parse_verdict(&t);
                verdicts.push(JurorVerdict {
                    juror: jr.to_string(),
                    approve,
                    confidence,
                    reasoning: t,
                });
            }
        }
        // Fail-safe: no juror responded → do not block the build on infra failure.
        if verdicts.is_empty() {
            tracing::warn!(session_id = %session_id, "plan-jury produced no verdicts; proceeding without the plan gate");
            return true;
        }
        let mut decision = JurySystem::new(2).decide(&verdicts);
        // Match the end-audit jury: a 1-1 split gets an architect tie-break
        // instead of treating no-consensus as a hard plan rejection.
        if matches!(decision, JuryDecision::NoConsensus) {
            let adj = format!(
                "Goal: {goal}\n\nThe plan-jury split on whether this plan is sound, complete, \
                 and correctly scoped.\n\nProposed plan:\n{plan_text}\n\nAs an impartial \
                 architect adjudicator, decide whether to proceed with this plan. Begin with \
                 exactly 'VERDICT: APPROVE' or 'VERDICT: REJECT', then 'CONFIDENCE: <0.0-1.0>'."
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
        let approve_count = verdicts.iter().filter(|v| v.approve).count();
        if let Some(session) = self.get_session(session_id) {
            let summary = format!(
                "plan-jury {approve_count}/{} approve, decision {decision:?}",
                verdicts.len()
            );
            push_artifact(
                &session,
                ProtocolArtifact {
                    id: format!("art-{}", now_millis()),
                    path: String::new(),
                    kind: "plan_jury".into(),
                    content_hash: content_hash(plan_text),
                    shippable: false,
                    trust_label: "unreviewed".to_string(),
                    producer: "plan-jury".to_string(),
                    provenance: session_id.to_string(),
                    verdict: Some(rightkit_agent_engine_protocol::ArtifactVerdict {
                        passed: approved,
                        juror: "plan-jury".to_string(),
                        bound_hash: content_hash(plan_text),
                        summary: summary.clone(),
                    }),
                    approvals: vec![],
                    meta: summary,
                    created_at: now_iso(),
                },
            );
        }
        approved
    }
}
