impl DaemonState {
    /// Conductor trigger-routing (gated). Match the goal against the agent-catalog
    /// descriptions via the mandatory trigger rule, parse the announced
    /// [`RoutePath`] (fail-safe → `ArchitectPlan` on any ambiguity), record the
    /// decision as a `routing_decision` session artifact, and return the path.
    async fn decide_route(
        &self,
        session_id: &str,
        registry: &AgentRegistry,
        llm: &dyn LlmClient,
        goal: &str,
    ) -> RoutePath {
        let catalog = build_catalog(&roster_entries(registry), &[], CATALOG_BUDGET_CHARS);
        let prompt = format!(
            "Agent catalog:\n{catalog}\n\n{TRIGGER_RULE}\n\nTask: {goal}\n\nAnnounce your PATH line now."
        );
        let msg = TranscriptMessage::User {
            id: format!("routing-{}", now_millis()),
            created_at: now_iso(),
            content: prompt,
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        };
        let announced = match llm.complete(&[msg]).await {
            Ok(resp) => resp.text,
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "routing decision call failed; defaulting to ArchitectPlan");
                String::new()
            }
        };
        // Fail-safe: an unannounced/garbled path defaults to the gated path.
        let path = parse_announced_path(&announced).unwrap_or(RoutePath::ArchitectPlan);
        tracing::info!(session_id = %session_id, path = path.as_token(), "conductor routing decision");
        if let Some(session) = self.get_session(session_id) {
            push_artifact(
                &session,
                ProtocolArtifact {
                    id: format!("art-{}", now_millis()),
                    path: String::new(),
                    kind: "routing_decision".into(),
                    content_hash: content_hash(&format!("PATH: {}", path.as_token())),
                    shippable: false,
                    trust_label: "unreviewed".to_string(),
                    producer: "conductor".to_string(),
                    provenance: session_id.to_string(),
                    verdict: None,
                    approvals: vec![],
                    meta: truncate(announced.trim(), 400),
                    created_at: now_iso(),
                },
            );
        }
        path
    }

    /// The `DirectEdit` lean arm: one coder + ponytail, then the deterministic
    /// gate, then a thin diff review. No plan-jury, no full victory gate.
    ///
    /// Returns `Ok(Some(outcome))` when the change landed and both the gate and
    /// the thin-diff reviewer approve (the goal is finalized `Accepted` by the
    /// caller — the thin-diff reviewer is this lane's review gate). Returns
    /// `Ok(None)` to **escalate**: the gate failed or the reviewer rejected, so
    /// the caller falls through to the `ArchitectPlan` path (the v1 one-hop
    /// escalation — see [`rightkit_agent_orchestration::coding::escalation::should_escalate`]).
    async fn run_direct_edit(
        &self,
        session_id: &str,
        goal: &str,
        workspace_config: &rightkit_agent_config::HostConfig,
    ) -> Result<Option<RunOutcome>, String> {
        let session = self
            .get_session(session_id)
            .ok_or_else(|| format!("session not found: {session_id}"))?;
        let conduit = session.conduit.clone();

        let mechanism = resolve_conductor_mechanism(session_id, goal).await;
        let mechanism_receipt = serde_json::to_string(&mechanism)
            .map_err(|error| format!("serialize conductor mechanism receipt: {error}"))?;
        push_artifact(
            &session,
            ProtocolArtifact {
                id: format!("art-{}", now_millis()),
                path: String::new(),
                kind: "mechanism_decision".into(),
                content_hash: content_hash(&mechanism_receipt),
                shippable: false,
                trust_label: "unreviewed".to_string(),
                producer: "mechanism_resolver".to_string(),
                provenance: session_id.to_string(),
                verdict: None,
                approvals: vec![],
                meta: truncate(&mechanism_receipt, 400),
                created_at: now_iso(),
            },
        );
        if !mechanism.needs_model() {
            return Err(format!(
                "direct conductor mechanism resolution ended terminally: {}",
                mechanism.reason
            ));
        }

        // 1. One coder, ponytail-scoped.
        let prompt = format!(
        "{goal}\n\n[ponytail] Make the smallest change that works. Read before writing, keep the \
             diff minimal and focused, leave the build green. If this turns out larger or more \
             cross-cutting than a one-or-two-file change, say so plainly and stop rather than \
             sprawling — the conductor will escalate to a planned build."
    );
        let coder_out = self
            .spawn_with_timeout("coder", &prompt, session_id, Duration::from_secs(600))
            .await?;

        // 2. Deterministic gate (post-edit): the build/test/fmt checks must pass.
        let specs =
            crate::pre_gate::specs_from_config(&workspace_config.orchestration.pre_gate_checks);
        let gate_ok =
            if specs.is_empty() {
                true
            } else {
                let ctx = rightkit_agent_coding_tools::ToolContext::new(
                    session_id,
                    session.workspace_root.trim(),
                    rightkit_agent_policy::PermissionMode::Bypass,
                    session.workspace_trust,
                );
                !matches!(
                crate::pre_gate::run_pre_gate(&conduit, session_id, &ctx, &specs).await,
                rightkit_agent_coding_tools::check_runner::PreGateDecision::SkipJuryFailed(_)
            )
            };

        // 3. Thin diff review (the DirectEdit lane's review gate). Skip straight
        // to escalation if the gate already failed — no point reviewing a red build.
        let review_ok = if gate_ok {
            let review_prompt = format!(
                "A coder just made a small, localized change for this task:\n{goal}\n\nReview the \
                 resulting git diff (run `git diff`) as a thin, CodeRabbit-style pass: correctness, \
                 obvious defects, and whether the change is actually scoped to the task. Begin your \
                 reply with exactly 'VERDICT: APPROVE' or 'VERDICT: REJECT', then your concerns."
            );
            match self
                .spawn_with_timeout(
                    "code-reviewer",
                    &review_prompt,
                    session_id,
                    Duration::from_secs(180),
                )
                .await
            {
                Ok(t) => parse_verdict(&t).0,
                // A reviewer that cannot run is not an approval — escalate.
                Err(e) => {
                    tracing::warn!(session_id = %session_id, error = %e, "thin diff reviewer could not run; escalating");
                    false
                }
            }
        } else {
            false
        };

        let landed = gate_ok && review_ok;
        let result_hash = content_hash(&coder_out);
        push_artifact(
            &session,
            ProtocolArtifact {
                id: format!("art-{}", now_millis()),
                path: String::new(),
                kind: "direct_edit_result".into(),
                content_hash: result_hash.clone(),
                shippable: landed,
                trust_label: if landed { "reviewed" } else { "unreviewed" }.to_string(),
                producer: "coder".to_string(),
                provenance: session_id.to_string(),
                verdict: Some(rightkit_agent_engine_protocol::ArtifactVerdict {
                    passed: landed,
                    juror: "code-reviewer".to_string(),
                    bound_hash: result_hash,
                    summary: format!("gate_ok={gate_ok} review_ok={review_ok}"),
                }),
                approvals: vec![],
                meta: format!("gate_ok={gate_ok} review_ok={review_ok}"),
                created_at: now_iso(),
            },
        );

        if landed {
            Ok(Some(RunOutcome {
                completed: true,
                results: vec![],
                failed: vec![],
                stall_nudge: None,
            }))
        } else {
            // v1 escalation backstop: route the failed-direct-edit signal
            // through `should_escalate_direct_edit()` so the same one-hop rule governs both
            // the worker-self escalate tool and the post-gate auto-escalation.
            // `already_escalated` is false on the first hop; a retry of the
            // DirectEdit path within the same run would set it true to block
            // a second hop (today that loop is in caller, not here).
            let escalate =
                rightkit_agent_orchestration::coding::escalation::should_escalate_direct_edit(
                    gate_ok, review_ok, false,
                );
            if escalate {
                // Escalate: caller falls through to the ArchitectPlan path.
                Ok(None)
            } else {
                // Escalation suppressed — treat as completed-but-failed so the run
                // terminates cleanly with the gate's failure surface visible.
                Ok(Some(RunOutcome {
                    completed: true,
                    results: vec![],
                    failed: vec![],
                    stall_nudge: None,
                }))
            }
        }
    }
}
