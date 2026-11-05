use super::*;

impl SessionEngine {
    pub(super) fn record_session_end(
        &self,
        parent_id: Option<&str>,
        status: &str,
        reason: Option<&str>,
    ) {
        // The engine's session boundary is the run span, so SessionEnd fires
        // here, once per completed run.
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::SessionEnd {
            session_id: self.session_id.clone(),
        });
        // Configured SessionEnd hooks are fire-and-forget (cannot block).
        if let (Some(bus), Ok(handle)) =
            (self.hook_bus.clone(), tokio::runtime::Handle::try_current())
        {
            let session_id = self.session_id.clone();
            let status = status.to_string();
            handle.spawn(async move {
                bus.run_event_hooks(
                    "SessionEnd",
                    &session_id,
                    None,
                    None,
                    &[("RIGHTKIT_HOOK_STATUS", status)],
                )
                .await;
            });
        }
        self.record_runtime_span(
            MiddlewarePhase::SessionEnd,
            status,
            None,
            parent_id,
            serde_json::json!({
                "reason": reason,
                "promptTokens": self.usage.prompt_tokens,
                "completionTokens": self.usage.completion_tokens,
                "costUsd": self.usage.cost_usd,
            }),
        );
    }

    pub(super) fn enabled_runtime_middleware(
        &self,
        phase: MiddlewarePhase,
    ) -> Vec<MiddlewareDefinition> {
        self.runtime_middleware
            .as_ref()
            .map(|registry| {
                registry
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .enabled_for_phase(phase)
            })
            .unwrap_or_default()
    }

    pub(super) fn record_tool_runtime_span(
        &self,
        phase: MiddlewarePhase,
        status: &str,
        call: &ToolCall,
        parent_id: Option<&str>,
        attributes: serde_json::Value,
    ) -> Option<String> {
        self.record_runtime_span(
            phase,
            status,
            Some(call.id.as_str()),
            parent_id,
            merge_runtime_attributes(
                attributes,
                serde_json::json!({
                    "tool": call.name,
                    "toolCallId": call.id,
                }),
            ),
        )
    }

    pub(super) fn record_runtime_span(
        &self,
        phase: MiddlewarePhase,
        status: &str,
        turn_id: Option<&str>,
        parent_id: Option<&str>,
        attributes: serde_json::Value,
    ) -> Option<String> {
        let registry = self.runtime_middleware.as_ref()?;
        let definitions = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .enabled_for_phase(phase);
        if definitions.is_empty() {
            return None;
        }
        let middleware_ids = definitions
            .iter()
            .map(|definition| definition.id.clone())
            .collect::<Vec<_>>();
        let now = now_iso();
        let id = format!(
            "runtime-{}-{}-{}",
            self.session_id,
            middleware_phase_key(phase),
            now_nanos()
        );
        let span = RuntimeSpan {
            id: id.clone(),
            parent_id: parent_id.map(str::to_string),
            session_id: self.session_id.clone(),
            turn_id: turn_id.map(str::to_string),
            phase,
            status: status.to_string(),
            started_at: now.clone(),
            ended_at: Some(now),
            attributes: merge_runtime_attributes(
                attributes,
                serde_json::json!({
                    "middlewareIds": middleware_ids,
                    "middlewareCount": definitions.len(),
                    "channel": self.mcp_channel,
                    "executionBackend": self.execution_backend,
                    "executionWorkspaceId": self.execution_workspace_id,
                    "executionNetworkProfile": self.execution_network_profile,
                    "workspaceTrust": format!("{:?}", self.workspace_trust),
                }),
            ),
        };
        registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record_span(span);
        Some(id)
    }

    pub(super) fn record_runtime_observation(
        &self,
        span_id: &str,
        severity: &str,
        message: String,
        payload: serde_json::Value,
    ) {
        let Some(registry) = &self.runtime_middleware else {
            return;
        };
        let observation = RuntimeObservation {
            id: format!("runtime-observation-{}-{}", self.session_id, now_nanos()),
            span_id: span_id.to_string(),
            session_id: self.session_id.clone(),
            severity: severity.to_string(),
            message,
            payload,
            created_at: now_iso(),
        };
        registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .record_observation(observation);
    }

    pub(super) fn approval_middleware_decisions(
        &self,
        call: &ToolCall,
        verdict: &PermissionVerdict,
        guardrail: Option<&ToolGuardrailDecision>,
        applied: &[AppliedMiddlewareDecision],
    ) -> Vec<MiddlewareDecisionEvidence> {
        let has_registry = self.runtime_middleware.is_some();
        let mut decisions = Vec::new();

        for decision in applied {
            if matches!(decision.action, AppliedMiddlewareAction::Continue) {
                continue;
            }
            decisions.push(MiddlewareDecisionEvidence {
                middleware_id: decision.middleware_id.clone(),
                title: decision.title.clone(),
                phase: middleware_phase_key(MiddlewarePhase::PreToolCall).to_string(),
                effect: decision.effect.clone(),
                decision: decision.action.as_str().to_string(),
                reason: decision.reason.clone(),
            });
        }

        let pre_definitions = self.enabled_runtime_middleware(MiddlewarePhase::PreToolCall);
        if !has_registry || !pre_definitions.is_empty() {
            let pre_decisions = if has_registry {
                pre_definitions
            } else {
                vec![synthetic_runtime_middleware(
                    "builtin.pre-tool-policy",
                    "Pre-tool policy",
                    MiddlewarePhase::PreToolCall,
                    MiddlewareEffect::RequireApproval,
                )]
            };
            for definition in pre_decisions {
                decisions.push(MiddlewareDecisionEvidence {
                    middleware_id: definition.id,
                    title: definition.title,
                    phase: middleware_phase_key(definition.phase).to_string(),
                    effect: middleware_effect_key(definition.effect).to_string(),
                    decision: "requireApproval".to_string(),
                    reason: pre_tool_approval_reason(verdict, guardrail),
                });
            }
        }

        let approval_definitions =
            self.enabled_runtime_middleware(MiddlewarePhase::ApprovalRequested);
        if !has_registry || !approval_definitions.is_empty() {
            let approval_decisions = if has_registry {
                approval_definitions
            } else {
                vec![synthetic_runtime_middleware(
                    "builtin.observe-approvals",
                    "Observe approvals",
                    MiddlewarePhase::ApprovalRequested,
                    MiddlewareEffect::Observe,
                )]
            };
            for definition in approval_decisions {
                decisions.push(MiddlewareDecisionEvidence {
                    middleware_id: definition.id,
                    title: definition.title,
                    phase: middleware_phase_key(definition.phase).to_string(),
                    effect: middleware_effect_key(definition.effect).to_string(),
                    decision: "emitObservation".to_string(),
                    reason: format!(
                        "Approval requested for tool call {} ({})",
                        call.id, call.name
                    ),
                });
            }
        }

        decisions
    }

    #[cfg(test)]
    pub(super) fn apply_pre_tool_middleware(
        &self,
        call: &mut ToolCall,
    ) -> Vec<AppliedMiddlewareDecision> {
        let mut decisions = self.apply_pre_tool_arg_transforms(call);
        decisions.extend(self.evaluate_pre_tool_policy(call));
        decisions
    }

    pub(super) fn apply_pre_tool_arg_transforms(
        &self,
        call: &mut ToolCall,
    ) -> Vec<AppliedMiddlewareDecision> {
        let mut decisions = Vec::new();
        let definitions = self.enabled_runtime_middleware(MiddlewarePhase::PreToolCall);
        if definitions.is_empty() {
            return decisions;
        }
        let mut args = serde_json::from_str::<serde_json::Value>(&call.arguments_json)
            .unwrap_or(serde_json::Value::Null);
        let mut mutated = false;
        for definition in definitions {
            if definition.source
                != rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin
            {
                continue;
            }
            if !middleware_matches_call(&definition, call, &args) {
                continue;
            }
            let reason = definition.reason.clone().unwrap_or_else(|| {
                format!(
                    "{} applied to tool call {} ({})",
                    definition.title, call.id, call.name
                )
            });
            match definition.effect {
                MiddlewareEffect::Observe
                | MiddlewareEffect::RequireApproval
                | MiddlewareEffect::Deny => continue,
                MiddlewareEffect::MutateArgs => {
                    let mutated_arg_keys = apply_middleware_arg_patch(&mut args, &definition);
                    if !mutated_arg_keys.is_empty() {
                        mutated = true;
                    }
                    decisions.push(AppliedMiddlewareDecision {
                        middleware_id: definition.id,
                        title: definition.title,
                        effect: "mutateArgs".to_string(),
                        action: AppliedMiddlewareAction::MutateArgs,
                        reason,
                        mutated_arg_keys,
                    });
                }
            }
        }
        if mutated {
            if let Ok(arguments_json) = serde_json::to_string(&args) {
                call.arguments_json = arguments_json;
            }
        }
        decisions
    }

    pub(super) fn evaluate_pre_tool_policy(
        &self,
        call: &ToolCall,
    ) -> Vec<AppliedMiddlewareDecision> {
        let mut decisions = Vec::new();
        let args = serde_json::from_str::<serde_json::Value>(&call.arguments_json)
            .unwrap_or(serde_json::Value::Null);
        for definition in self.enabled_runtime_middleware(MiddlewarePhase::PreToolCall) {
            if definition.source
                != rightkit_agent_engine_runtime_middleware::MiddlewareSource::LocalPlugin
                || definition.effect == MiddlewareEffect::MutateArgs
                || !middleware_matches_call(&definition, call, &args)
            {
                continue;
            }
            let reason = definition.reason.clone().unwrap_or_else(|| {
                format!(
                    "{} applied to tool call {} ({})",
                    definition.title, call.id, call.name
                )
            });
            let (effect, action) = match definition.effect {
                MiddlewareEffect::Observe => ("observe", AppliedMiddlewareAction::Continue),
                MiddlewareEffect::RequireApproval => {
                    ("requireApproval", AppliedMiddlewareAction::RequireApproval)
                }
                MiddlewareEffect::Deny => ("deny", AppliedMiddlewareAction::Deny),
                MiddlewareEffect::MutateArgs => unreachable!(),
            };
            decisions.push(AppliedMiddlewareDecision {
                middleware_id: definition.id,
                title: definition.title,
                effect: effect.to_string(),
                action,
                reason,
                mutated_arg_keys: Vec::new(),
            });
        }
        decisions
    }
}
