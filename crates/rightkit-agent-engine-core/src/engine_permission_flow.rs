use super::*;

impl SessionEngine {
    pub(super) fn permission_verdict_for(&self, call: &ToolCall) -> PermissionVerdict {
        // Prefer a tool-declared capability class; fall back to name inference
        // only when the tool doesn't declare one. This stops a renamed or newly
        // added tool from being silently mis-gated by the name table.
        let capability = self.auto_approval_capability_for(call);
        // ABAC + engine evaluation: hard denials remain the backstop, but ABAC
        // is consulted before the `RuleStore` seam (FM-026).
        let path_arg = extract_path_arg(&call.arguments_json);
        let request = build_policy_request(call, self.permission_mode, self.workspace_trust);

        // Build an engine that knows about the attached PDP so `evaluate_with_request`
        // can surface `abac:<rule-id>` when the PDP matches. When no PDP is
        // attached, this degrades to the baseline `evaluate` path.
        let mut engine = match &self.deny_globs {
            Some(deny) => PermissionEngine::with_deny_globs((**deny).clone()),
            None => PermissionEngine::new(),
        };
        if let Some(pdp) = &self.abac_pdp {
            engine.set_pdp((**pdp).clone());
        }
        let base = engine.evaluate_with_request(
            Some(&request),
            capability,
            self.permission_mode,
            self.workspace_trust,
            path_arg.as_deref(),
            false,
        );

        // If ABAC matched, `base` already carries `abac:<id>` and we must not
        // let the glob `RuleStore` overwrite it (ABAC first, glob second).
        let is_abac = base.policy_source.starts_with("abac:");

        let verdict = if is_hard_denial(&base) || is_abac {
            base
        } else {
            let mut v = base;
            // A base Deny (mode/trust/capability) is never overridden by a
            // rule Allow/Ask (e.g. a session grant); a rule Deny is redundant.
            let base_denied = v.decision == PermissionDecision::Deny;
            if let (false, Some(rules)) = (base_denied, &self.permission_rules) {
                let target = rule_target_for_call(call);
                if let Some(rule) = rules
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .effective_match_for(&target)
                {
                    let source =
                        format!("rule:{}:{}", rule_source_label(rule.source), rule.pattern);
                    v = PermissionVerdict::new(
                        rule.effect,
                        format!(
                            "{:?} by {} permission rule '{}'",
                            rule.effect,
                            rule_source_label(rule.source),
                            rule.pattern
                        ),
                        source,
                    );
                }
            }
            v
        };

        let mut final_verdict = match self.tools.get(&call.name).and_then(|tool| {
            serde_json::from_str(&call.arguments_json)
                .ok()
                .and_then(|args| tool.durable_write_kind(&args))
        }) {
            Some(kind) => rightkit_agent_policy::enforce_durable_write_provenance(
                verdict,
                kind,
                &self.content_provenance,
            ),
            None => verdict,
        };

        // --- Adaptive Ask→Allow cache (FM-029, Goose discipline) ---
        // Bind every lookup/insertion to current call features and authority
        // context. Missing context skips cache reuse/insertion fail-closed.
        if final_verdict.decision == PermissionDecision::Ask {
            let features = self.auto_approval_features_for(call, capability);
            let context = self.auto_approval_context_for(capability);
            let cached = context.as_ref().is_some_and(|context| {
                self.is_auto_approved_cached(&features, &call.arguments_json, context)
            });
            if cached {
                tracing::info!(
                    tool = %call.name,
                    "auto-approval cache hit → Allow (Goose negative-only)"
                );
                final_verdict = PermissionVerdict::new(
                    PermissionDecision::Allow,
                    "auto-approval:cached allow (Goose: negative-only cache)",
                    "auto_approval:cached",
                );
            } else {
                let score = self.auto_approval_classifier.score(&features);
                let (tier, gray_zone) =
                    self.auto_approval_classifier.classify_reviewable(&features);
                // classify() is FM-029 decision seam; cache insertion is deferred
                // until dispatch has received final positive permission resolution.
                //
                // S:B4: a near-threshold result is never an auto-allow. Route
                // it through the existing approval path; unavailable review
                // times out as denial, so uncertainty cannot reach an effect.
                if tier == AutoApprovalTier::AutoEdit && !gray_zone {
                    tracing::info!(
                        tool = %call.name,
                        score = score,
                        gray_zone = gray_zone,
                        "auto-approval classify → Allow (cache deferred until positive resolution)"
                    );
                    final_verdict = PermissionVerdict::new(
                        PermissionDecision::Allow,
                        format!(
                            "auto-approval:allow score={:.2} (Goose: negative-only cache)",
                            score
                        ),
                        "auto_approval:allow",
                    );
                } else {
                    tracing::info!(
                        tool = %call.name,
                        score = score,
                        tier = ?tier,
                        gray_zone = gray_zone,
                        "auto-approval classify → Ask (uncertainty or gray-zone, not cached)"
                    );
                    if gray_zone {
                        final_verdict = PermissionVerdict::new(
                            PermissionDecision::Ask,
                            format!(
                                "auto-approval gray-zone score={:.2}; explicit review required",
                                score
                            ),
                            "auto_approval:gray-zone-review",
                        );
                    }
                }
            }
        }

        // Command classification is escalation-only & must also fence cached
        // adaptive allows. Windows shell calls use PowerShell, whose classifier
        // deliberately requires explicit approval until parsing is supported.
        if capability == rightkit_agent_policy::CapabilityClass::Shell
            && self.permission_mode == PermissionMode::AutoSafe
            && final_verdict.decision == PermissionDecision::Allow
        {
            let command = extract_command_arg(&call.arguments_json).unwrap_or_default();
            let dialect = if cfg!(windows) || call.name == "powershell" {
                rightkit_agent_policy::ShellDialect::PowerShell
            } else {
                rightkit_agent_policy::ShellDialect::Posix
            };
            let shell_verdict = engine.evaluate_shell_command(
                self.permission_mode,
                self.workspace_trust,
                &command,
                dialect,
            );
            if shell_verdict.decision != PermissionDecision::Allow {
                final_verdict = shell_verdict;
            }
        }

        if final_verdict.decision == PermissionDecision::Deny {
            if let Some(tracker) = &self.denial_tracker {
                let at = now_iso();
                let tool = call.name.clone();
                let reason = format!("{} ({})", final_verdict.reason, final_verdict.policy_source);
                let session = self.session_id.clone();
                let mut guard = tracker.lock().unwrap_or_else(|e| e.into_inner());
                // S:B5: bind the audit row to the exact tool call that
                // produced the decision prompt, so a repeated tool in one
                // session stays distinguishable after the fact.
                guard.record_correlated_denial_once(&tool, &reason, &session, &at, &call.id);
            }
        }
        final_verdict
    }

    pub(super) fn record_correlated_permission_denial(&self, call: &ToolCall, reason: &str) {
        let Some(tracker) = &self.denial_tracker else {
            return;
        };
        let at = now_iso();
        tracker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .record_correlated_denial_once(&call.name, reason, &self.session_id, &at, &call.id);
    }
}

fn is_hard_denial(verdict: &PermissionVerdict) -> bool {
    verdict.decision == PermissionDecision::Deny
        && (verdict.policy_source.starts_with("deny-glob:")
            || verdict.policy_source == "trust:locked")
}

fn rule_source_label(source: RuleSource) -> &'static str {
    match source {
        RuleSource::Builtin => "builtin",
        RuleSource::Project => "project",
        RuleSource::User => "user",
        RuleSource::Session => "session",
    }
}

/// Build an ABAC [`PolicyRequest`] from a tool call. The request carries the
/// tool name and structured attributes (`command`, `path`, `mode`, `trust`)
/// so YAML-like `from_yaml_like` rules can match on `attribute: env op: eq
/// value: prod`-style conditions. This is the adaptor that lets
/// `PolicyDecisionPoint` be consulted before `RuleStore` at the
/// `permission_rules` seam.
fn build_policy_request(
    call: &ToolCall,
    mode: PermissionMode,
    trust: WorkspaceTrust,
) -> rightkit_agent_policy::PolicyRequest {
    use rightkit_agent_policy::{Attribute, PolicyRequest};
    let mut req = PolicyRequest::new(&call.name);
    if let Some(cmd) = extract_command_arg(&call.arguments_json) {
        req = req.with_command(cmd.clone());
        req = req.with_attribute("command", cmd);
    }
    if let Some(path) = extract_path_arg(&call.arguments_json) {
        req = req.with_attribute("path", path.clone());
        // also expose as `file` for ergonomic rules
        req = req.with_attribute("file", path);
    }
    req = req.with_attribute("tool", call.name.clone());
    req = req.with_attribute("mode", format!("{:?}", mode));
    req = req.with_attribute("trust", format!("{:?}", trust));
    // Collect any extra JSON attributes as generic ABAC attributes so
    // callers can author rules on `attribute: env value: prod` etc.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&call.arguments_json) {
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                if k == "command" || k == "cmd" || k == "path" || k == "tool" {
                    continue;
                }
                let s = match val {
                    serde_json::Value::String(s) => s.clone(),
                    _ => val.to_string(),
                };
                // Avoid duplicating mode/trust/tool
                if k == "mode" || k == "trust" {
                    continue;
                }
                req.attributes.push(Attribute::new(k.clone(), s));
            }
        }
    }
    req
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_protocol::ToolCall;
    use std::sync::{Arc, Mutex};

    #[test]
    fn session_allow_rule_cannot_override_a_mode_deny() {
        let mut rules = rightkit_agent_policy::RuleStore::new();
        rules.add(rightkit_agent_policy::Rule::new(
            "write_file",
            rightkit_agent_policy::PermissionDecision::Allow,
            rightkit_agent_policy::RuleSource::Session,
        ));
        let cfg = SessionEngineConfig {
            session_id: "sess-deny-wins".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Plan,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            permission_rules: Some(Arc::new(Mutex::new(rules))),
            ..Default::default()
        };
        let engine = SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new()),
            Arc::new(Conduit::new(16)),
            cfg,
        );
        let call = ToolCall {
            id: "tc-dw".into(),
            name: "write_file".into(),
            arguments_json: serde_json::json!({"path": "src/main.rs", "content": "x"}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Deny
        );
        assert!(!verdict.policy_source.starts_with("rule:session"));
    }

    #[test]
    fn deny_records_to_denial_tracker() {
        let tracker = Arc::new(Mutex::new(rightkit_agent_policy::DenialTracker::new()));
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(Conduit::new(16));
        let cfg = SessionEngineConfig {
            session_id: "sess-123".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Plan,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            denial_tracker: Some(tracker.clone()),
            ..Default::default()
        };
        // Plan mode denies writes (already set)
        let engine = SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        let call = ToolCall {
            id: "tc1".into(),
            name: "write_file".into(),
            arguments_json: serde_json::json!({"path": "src/main.rs", "content": "hi"}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Deny
        );
        let guard = tracker.lock().unwrap();
        let (slice, total) = guard.query(Some("sess-123"), Some("write_file"), 0, 10);
        assert_eq!(total, 1);
        assert_eq!(slice[0].tool, "write_file");
        assert_eq!(
            slice[0].correlation,
            rightkit_agent_policy::denial_tracking::correlation_token("sess-123", "tc1")
        );
        assert!(slice[0].reason.contains("mode:plan") || slice[0].reason.contains("denied"));
    }

    #[test]
    fn auto_approval_cache_upgrades_ask_to_allow() {
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-aaa".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Ask,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        // Edit to a non-test file under Ask mode would normally be Ask; classifier
        // should promote to Allow and cache it (file_tool + low destructiveness).
        let call = ToolCall {
            id: "tc2".into(),
            name: "Edit".into(),
            arguments_json: serde_json::json!({"path": "src/main.rs", "edits": []}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Allow,
            "classifier should upgrade Ask→Allow for benign Edit"
        );
        assert!(
            verdict.policy_source.starts_with("auto_approval"),
            "upgraded verdict must be tagged auto_approval, got {}",
            verdict.policy_source
        );
        // Runtime dispatch inserts only after final positive resolution.
        assert!(engine.cache_auto_approval_after_positive_resolution(&call));
        // Second call with same (tool,args) should hit context-bound cache.
        let verdict2 = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict2.decision,
            rightkit_agent_policy::PermissionDecision::Allow
        );
        assert_eq!(verdict2.policy_source, "auto_approval:cached");
    }

    #[test]
    fn gray_zone_auto_approval_routes_to_explicit_review() {
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-gray".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Ask,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            ..Default::default()
        };
        let mut engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        let call = ToolCall {
            id: "tc-gray".into(),
            name: "Edit".into(),
            arguments_json: serde_json::json!({"path": "src/main.rs", "edits": []}).to_string(),
        };
        let capability = rightkit_agent_policy::parse_capability(Some(&call.name));
        let features = engine.auto_approval_features_for(&call, capability);
        for _ in 0..6 {
            engine.auto_approval_classifier.train(&features, false);
        }
        assert!(engine.auto_approval_classifier.is_gray_zone(&features));

        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Ask
        );
        assert_eq!(verdict.policy_source, "auto_approval:gray-zone-review");
        assert!(verdict.reason.contains("explicit review required"));
    }

    #[test]
    fn auto_approval_cache_negative_only_uncertainty_stays_ask() {
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-bbb".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Ask,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        // Destructive Bash should stay Ask (AlwaysAsk, score <= -0.2) and never be cached
        let call = ToolCall {
            id: "tc3".into(),
            name: "Bash".into(),
            arguments_json: serde_json::json!({"command": "rm -rf /tmp/foo"}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Ask
        );
        // No context-bound cache entry for this destructive call
        let capability = rightkit_agent_policy::parse_capability(Some(&call.name));
        let features = engine.auto_approval_features_for(&call, capability);
        let context = engine
            .auto_approval_context_for(capability)
            .expect("temporary workspace should provide cache authority context");
        let cache = engine.auto_approval_cache();
        let guard = cache.lock().unwrap();
        assert!(
            !guard.contains_call(&features, &call.arguments_json, &context),
            "destructive Ask must not be cached (negative-only)"
        );
    }

    #[test]
    fn auto_approval_cache_key_normalized() {
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-ccc".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Ask,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        let call_a = ToolCall {
            id: "tc4".into(),
            name: "Edit".into(),
            arguments_json: r#"{"path":"src/a.rs","edits":[]}"#.to_string(),
        };
        let call_b = ToolCall {
            id: "tc5".into(),
            name: "edit".into(),
            arguments_json: r#"{"edits":[],"path":"src/a.rs"}"#.to_string(),
        };
        // Both should be auto-allowed and share the same cache key (normalized)
        let v_a = engine.permission_verdict_for(&call_a);
        assert_eq!(
            v_a.decision,
            rightkit_agent_policy::PermissionDecision::Allow
        );
        assert!(engine.cache_auto_approval_after_positive_resolution(&call_a));
        // call_b should hit the cache populated by call_a (same normalized args, case-insensitive tool)
        let v_b = engine.permission_verdict_for(&call_b);
        assert_eq!(
            v_b.decision,
            rightkit_agent_policy::PermissionDecision::Allow
        );
        assert_eq!(v_b.policy_source, "auto_approval:cached");
    }

    #[test]
    fn auto_approval_cache_is_bound_to_live_authority_context() {
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-context".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Ask,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        let call = ToolCall {
            id: "tc-context".into(),
            name: "Edit".into(),
            arguments_json: serde_json::json!({"path": "src/main.rs", "edits": []}).to_string(),
        };
        let capability = rightkit_agent_policy::parse_capability(Some(&call.name));
        let features = engine.auto_approval_features_for(&call, capability);
        let context = engine
            .auto_approval_context_for(capability)
            .expect("temporary workspace should provide cache authority context");
        assert!(engine.cache_auto_approval(&features, &call.arguments_json, &context));
        assert!(engine.is_auto_approved_cached(&features, &call.arguments_json, &context));

        let changed_context = rightkit_agent_policy::AutoApprovalContext::new(
            context.policy_revision.clone(),
            context.rules_revision.clone(),
            context.workspace_identity.clone(),
            "session=sess-context;mode=FullAccess;trust=Trusted",
        );
        assert!(!engine.is_auto_approved_cached(&features, &call.arguments_json, &changed_context));
    }

    #[test]
    fn auto_approval_rule_revision_is_unambiguous_for_delimiter_bearing_rules() {
        let mut first = rightkit_agent_policy::RuleStore::new();
        first.add(rightkit_agent_policy::Rule::new(
            "tool|one",
            rightkit_agent_policy::PermissionDecision::Allow,
            rightkit_agent_policy::RuleSource::Project,
        ));
        first.add(rightkit_agent_policy::Rule::new(
            "tool-two",
            rightkit_agent_policy::PermissionDecision::Deny,
            rightkit_agent_policy::RuleSource::User,
        ));

        let mut second = rightkit_agent_policy::RuleStore::new();
        second.add(rightkit_agent_policy::Rule::new(
            "tool",
            rightkit_agent_policy::PermissionDecision::Allow,
            rightkit_agent_policy::RuleSource::Project,
        ));
        second.add(rightkit_agent_policy::Rule::new(
            "one|tool-two",
            rightkit_agent_policy::PermissionDecision::Deny,
            rightkit_agent_policy::RuleSource::User,
        ));

        assert_ne!(
            canonical_rule_revision(&first),
            canonical_rule_revision(&second)
        );
    }

    #[test]
    fn auto_approval_does_not_upgrade_hard_denial() {
        let tracker = Arc::new(Mutex::new(rightkit_agent_policy::DenialTracker::new()));
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-hard".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Bypass,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Locked,
            workspace_root: std::env::temp_dir(),
            denial_tracker: Some(tracker.clone()),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        let call = ToolCall {
            id: "tc-hard".into(),
            name: "Edit".into(),
            arguments_json: serde_json::json!({"path": "src/main.rs", "edits": []}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Deny
        );
        assert_eq!(verdict.policy_source, "trust:locked");
    }

    // ----- FM-026 ABAC dispatch tests -----

    fn abac_pdp_from_yaml(yaml: &str) -> Arc<rightkit_agent_policy::PolicyDecisionPoint> {
        Arc::new(
            rightkit_agent_policy::PolicyDecisionPoint::from_yaml_like(yaml).expect("yaml parse"),
        )
    }

    #[test]
    fn abac_precedes_rule_store() {
        // RuleStore says deny shell, but ABAC allow should win (ABAC first, glob second)
        let yaml = r#"
rules:
  - id: allow-shell-abac
    effect: allow
    tool: shell
"#;
        let pdp = abac_pdp_from_yaml(yaml);
        let mut rules = rightkit_agent_policy::RuleStore::new();
        rules.add(rightkit_agent_policy::Rule::new(
            "shell",
            rightkit_agent_policy::PermissionDecision::Deny,
            rightkit_agent_policy::RuleSource::Session,
        ));
        let rules_arc = Arc::new(Mutex::new(rules));
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-abac-1".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Ask,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            permission_rules: Some(rules_arc),
            abac_pdp: Some(pdp),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        let call = ToolCall {
            id: "tc-abac-1".into(),
            name: "shell".into(),
            arguments_json: serde_json::json!({"command": "echo hi"}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Allow
        );
        assert_eq!(verdict.policy_source, "abac:allow-shell-abac");
    }

    #[test]
    fn abac_fallback_to_rule_store_when_no_match() {
        let yaml = r#"
rules:
  - id: allow-read-only
    effect: allow
    tool: read_file
"#;
        let pdp = abac_pdp_from_yaml(yaml);
        let mut rules = rightkit_agent_policy::RuleStore::new();
        rules.add(rightkit_agent_policy::Rule::new(
            "shell",
            rightkit_agent_policy::PermissionDecision::Deny,
            rightkit_agent_policy::RuleSource::Session,
        ));
        let rules_arc = Arc::new(Mutex::new(rules));
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-abac-2".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Ask,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            permission_rules: Some(rules_arc),
            abac_pdp: Some(pdp),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        // shell does not match ABAC (only read_file), so falls through to RuleStore deny
        let call = ToolCall {
            id: "tc-abac-2".into(),
            name: "shell".into(),
            arguments_json: serde_json::json!({"command": "echo hi"}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Deny
        );
        assert!(
            verdict.policy_source.starts_with("rule:"),
            "expected RuleStore source, got {}",
            verdict.policy_source
        );
    }

    #[test]
    fn abac_first_match_wins_via_dispatch() {
        let yaml = r#"
rules:
  - id: deny-first
    effect: deny
    tool: shell
  - id: allow-second
    effect: allow
    tool: shell
"#;
        let pdp = abac_pdp_from_yaml(yaml);
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-abac-3".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Bypass,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Trusted,
            workspace_root: std::env::temp_dir(),
            abac_pdp: Some(pdp),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        let call = ToolCall {
            id: "tc-abac-3".into(),
            name: "shell".into(),
            arguments_json: serde_json::json!({"command": "echo hi"}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Deny
        );
        assert_eq!(verdict.policy_source, "abac:deny-first");
    }

    #[test]
    fn abac_hard_denial_not_overridden() {
        let yaml = r#"
rules:
  - id: allow-all
    effect: allow
    tool: "*"
"#;
        let pdp = abac_pdp_from_yaml(yaml);
        let tools = Arc::new(rightkit_agent_tools::coding_tools::ToolRegistry::new());
        let conduit = Arc::new(crate::conduit::Conduit::new(16));
        let cfg = crate::engine::SessionEngineConfig {
            session_id: "sess-abac-hard".into(),
            permission_mode: rightkit_agent_policy::PermissionMode::Bypass,
            workspace_trust: rightkit_agent_policy::WorkspaceTrust::Locked,
            workspace_root: std::env::temp_dir(),
            abac_pdp: Some(pdp),
            ..Default::default()
        };
        let engine = crate::engine::SessionEngine::new(
            Arc::new(crate::llm::ScriptedLlm::new(vec![])),
            tools,
            conduit,
            cfg,
        );
        let call = ToolCall {
            id: "tc-abac-hard".into(),
            name: "read_file".into(),
            arguments_json: serde_json::json!({"path": "src/main.rs"}).to_string(),
        };
        let verdict = engine.permission_verdict_for(&call);
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Deny
        );
        assert_eq!(verdict.policy_source, "trust:locked");
    }
}
