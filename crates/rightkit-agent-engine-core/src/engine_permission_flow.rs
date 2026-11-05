use super::*;

impl SessionEngine {
    pub(super) fn permission_verdict_for(&self, call: &ToolCall) -> PermissionVerdict {
        // Prefer a tool-declared capability class; fall back to name inference
        // only when the tool doesn't declare one. This stops a renamed or newly
        // added tool from being silently mis-gated by the name table.
        let capability = self.capability_for_call(call);
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
        let mut base = engine.evaluate_with_request(
            Some(&request),
            capability,
            self.permission_mode,
            self.workspace_trust,
            path_arg.as_deref(),
            false,
        );
        // Test-file protection guards existing tests from being rewritten to pass. A test
        // file that does not exist yet weakens nothing, so creating one keeps the mode's
        // allow (the protection was the only escalation when it is the verdict's source).
        if base.policy_source == "test-file:protected" {
            let is_new_file = path_arg.as_deref().is_some_and(|path| {
                let path = std::path::Path::new(path);
                let full = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    self.workspace_root.join(path)
                };
                !full.exists()
            });
            if is_new_file {
                base = rightkit_agent_policy::PermissionVerdict::new(
                    PermissionDecision::Allow,
                    "a new test file weakens no existing test",
                    "test-file:new",
                );
            }
        }

        // If ABAC matched, `base` already carries `abac:<id>` and we must not
        // let the glob `RuleStore` overwrite it (ABAC first, glob second).
        let is_abac = base.policy_source.starts_with("abac:");

        let verdict = if is_hard_denial(&base) {
            base
        } else if is_abac {
            // Rule 51: an ABAC match keeps its own verdict, except that a persisted or session
            // Deny rule (including the fail-closed `*` deny of a corrupt rule file) still wins
            // over an ABAC Allow or Ask.
            let mut v = base;
            if v.decision != PermissionDecision::Deny {
                if let Some(rules) = &self.permission_rules {
                    let target = rule_target_for_call(call);
                    let denied = rules
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .effective_match_for(&target)
                        .filter(|rule| rule.effect == PermissionDecision::Deny);
                    if let Some(rule) = denied {
                        v = PermissionVerdict::new(
                            PermissionDecision::Deny,
                            format!(
                                "Deny by {} permission rule '{}' (outranks ABAC)",
                                rule_source_label(rule.source),
                                rule.pattern
                            ),
                            format!("rule:{}:{}", rule_source_label(rule.source), rule.pattern),
                        );
                    }
                }
            }
            v
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

        // Command classification is escalation-only. Windows shell calls use PowerShell, whose classifier
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
            let shell_verdict = engine.evaluate_shell_command_in(
                self.permission_mode,
                self.workspace_trust,
                &command,
                dialect,
                self.os_sandbox_enforced,
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
