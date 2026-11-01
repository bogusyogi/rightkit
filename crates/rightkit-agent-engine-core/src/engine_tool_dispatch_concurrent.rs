use super::*;
use std::{future::Future, pin::Pin};

// ENG-63/64 (durable cloud workspace + persistent run lifecycle) live in
// `rightkit_agent_tools::coding_tools::cloud_workspace`. That module's event log/cursor
// resumption (`resume_events_since`) is the primitive a reconnect-after-
// disconnect client calls; this file only dispatches *local* tool-call
// futures for the current turn and has no remote event-stream client to
// attach it to. Wiring a cloud-run reconnect path through this dispatcher
// (e.g. a `StreamingToolFuture` variant backed by
// `cloud_workspace::resume_events_since` instead of local tool execution)
// is a follow-up: `cloud_workspace` is not yet declared in
// `rightkit_agent_tools::coding_tools`'s `lib.rs`, so it cannot be `use`d from here without
// that out-of-lane wiring.

pub(in crate::engine) type StreamingToolFuture = Pin<
    Box<dyn Future<Output = (ToolCall, rightkit_agent_tools::coding_tools::ToolResult)> + Send>,
>;

impl SessionEngine {
    /// Build one governed concurrent execution future without borrowing the
    /// engine. The caller must apply the same 5a classifier used by the
    /// completed-response path before enqueueing this future.
    pub(in crate::engine) fn streaming_concurrent_tool_future(
        &self,
        call: ToolCall,
    ) -> StreamingToolFuture {
        let tools = self.tools.clone();
        let workspace_root = self.workspace_root.clone();
        let session_id = self.session_id.clone();
        let permission_mode = self.permission_mode;
        let workspace_trust = self.workspace_trust;
        let content_provenance = self.content_provenance.clone();
        let spawner = self.spawner.clone();
        let subagent_supervisor = self.subagent_supervisor.clone();
        let questioner = self.questioner.clone();
        #[cfg(feature = "host")]
        let background_shells = self.background_shells.clone();
        let file_service = self.file_service.clone();
        #[cfg(feature = "host")]
        let browser_sessions = self.browser_sessions.clone();
        let task_store = self.task_store.clone();
        let execution_backend = self.execution_backend.clone();
        let execution_workspace_id = self.execution_workspace_id.clone();
        let execution_workspace_path = self.execution_workspace_path.clone();
        let execution_network_profile = self.execution_network_profile.clone();
        let execution_resource_limits = self.execution_resource_limits.clone();
        #[cfg(feature = "host")]
        let execution_backend_registry = self.execution_backend_registry.clone();
        let mcp_channel = self.mcp_channel.clone();
        let toolset = self.toolset.clone();
        let plan_exit = self.plan_exit.clone();
        let message_dispatcher = self.message_dispatcher.clone();
        let team_directory = self.team_directory.clone();
        let web_search_config = self.web_search_config.clone();
        let image_provider_keys = self.image_provider_keys.clone();
        let cloudflare_config = self.cloudflare_config.clone();
        let github_token = self.github_token.clone();
        let await_deadline =
            crate::tool_await::non_provider_await_deadline(self.iteration_budget.max());
        let mcp = self.mcp.clone();
        let tool_result_dir = self.tool_result_dir.clone();
        let skill_dirs = self.skill_dirs.clone();
        let path_deny = self.deny_globs.clone();
        let hook_bus = self.hook_bus.clone();
        let repair_attribution = self.repair_attribution.clone();

        Box::pin(async move {
            let mut call = call;
            let (mut args, mut arg_parse_error) = parse_tool_arguments_attributed(
                &call.arguments_json,
                tools.get(&call.name).map(|t| t.input_schema()).as_ref(),
                Some(&call.name),
                repair_attribution.as_ref(),
            );
            let mut hook_denied = false;
            if arg_parse_error.is_none() {
                if let Some(bus) = &hook_bus {
                    match bus.run_pre_tool_use(&session_id, &call.name, &args).await {
                        rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Transform(
                            transformed,
                        ) => {
                            let serialized = serde_json::to_string(&transformed)
                                .expect("serde_json::Value always serializes into tool arguments");
                            let (validated, error) = parse_tool_arguments_attributed(
                                &serialized,
                                tools.get(&call.name).map(|t| t.input_schema()).as_ref(),
                                Some(&call.name),
                                repair_attribution.as_ref(),
                            );
                            args = validated;
                            arg_parse_error = error;
                            call.arguments_json = serde_json::to_string(&args)
                                .expect("serde_json::Value always serializes into tool arguments");
                        }
                        rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Deny(message) => {
                            hook_denied = true;
                            arg_parse_error = Some(message);
                        }
                        rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Ask {
                            reason, ..
                        } => {
                            hook_denied = true;
                            arg_parse_error =
                                Some(format!("PreToolUse hook requires approval: {reason}"));
                        }
                        rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Error(message) => {
                            eprintln!("[hook] PreToolUse error (non-blocking): {message}");
                        }
                        rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Proceed => {}
                    }
                }
            }
            let effect_denial =
                skill_effect_denial_for_tool_call(&skill_dirs, &workspace_root, &call);
            #[cfg(feature = "host")]
            let process_launch = match required_process_launch_authority(
                tools.as_ref(),
                &call.name,
                &args,
                &workspace_root,
            ) {
                Ok(process_launch) => process_launch,
                Err(result) => return (call, result),
            };
            let mut ctx = rightkit_agent_tools::coding_tools::ToolContext::new(
                &session_id,
                &workspace_root,
                permission_mode,
                workspace_trust,
            )
            .with_content_provenance(content_provenance.clone())
            .with_file_service(file_service)
            .with_mcp_channel(mcp_channel.clone())
            .with_tool_result_dir(tool_result_dir)
            .with_skill_dirs(skill_dirs)
            .with_path_deny(path_deny);
            for (provider, api_key) in &image_provider_keys {
                ctx = ctx.with_image_provider_key(provider.clone(), api_key.clone());
            }
            if let Some(ref toolset) = toolset {
                ctx = ctx.with_toolset(toolset.clone());
            }
            if let Some(ref spawner) = spawner {
                ctx = ctx.with_spawner(spawner.clone());
            }
            if let Some(ref supervisor) = subagent_supervisor {
                ctx = ctx.with_subagent_supervisor(supervisor.clone());
            }
            if let Some(ref questioner) = questioner {
                ctx = ctx.with_questioner(questioner.clone());
            }
            #[cfg(feature = "host")]
            {
                ctx = ctx.with_background_shells(background_shells.clone());
                ctx = ctx.with_browser_sessions(browser_sessions.clone());
            }
            ctx = ctx.with_task_store(task_store.clone());
            if let Some(ref backend) = execution_backend {
                ctx = ctx.with_execution_backend(backend.clone());
            }
            if let Some(ref workspace_id) = execution_workspace_id {
                ctx = ctx.with_execution_workspace_id(workspace_id.clone());
            }
            if let Some(ref workspace_path) = execution_workspace_path {
                ctx = ctx.with_execution_workspace_path(workspace_path.clone());
            }
            if let Some(ref profile) = execution_network_profile {
                ctx = ctx.with_execution_network_profile(profile.clone());
            }
            if let Some(ref limits) = execution_resource_limits {
                ctx = ctx.with_execution_resource_limits(limits.clone());
            }
            #[cfg(feature = "host")]
            if let Some(ref registry) = execution_backend_registry {
                ctx = ctx.with_execution_backend_registry(registry.clone());
            }
            if let Some(ref plan_exit) = plan_exit {
                ctx = ctx.with_plan_exit(plan_exit.clone());
            }
            if let Some(ref dispatcher) = message_dispatcher {
                ctx = ctx.with_message_dispatcher(dispatcher.clone());
            }
            if let Some(ref directory) = team_directory {
                ctx = ctx.with_team_directory(directory.clone());
            }
            if let Some(ref config) = web_search_config {
                ctx = ctx.with_web_search_config(config.clone());
            }
            if let Some(ref config) = cloudflare_config {
                ctx = ctx.with_cloudflare_config(config.clone());
            }
            if let Some(ref token) = github_token {
                ctx = ctx.with_github_token(token.clone());
            }
            if let Some(ref mcp) = mcp {
                ctx = ctx.with_mcp(mcp.clone());
            }

            #[cfg(feature = "host")]
            if let Some((adapter, launch_context)) = process_launch {
                ctx = ctx.with_process_launch(adapter, launch_context);
            }

            if !hook_denied && arg_parse_error.is_none() && effect_denial.is_none() {
                if let Some(bus) = &hook_bus {
                    bus.publish(rightkit_hooks::agent_hooks::HookEvent::PreToolUse {
                        session_id: session_id.clone(),
                        tool: call.name.clone(),
                    });
                }
            }
            let result = if let Some(error) = arg_parse_error {
                rightkit_agent_tools::coding_tools::ToolResult::error(error)
            } else if let Some(reason) = effect_denial {
                rightkit_agent_tools::coding_tools::ToolResult::error(reason)
            } else {
                match tools.get(&call.name) {
                    Some(tool) => {
                        crate::tool_await::supervise_tool_await(
                            &call.name,
                            await_deadline,
                            tool.execute(args, &ctx),
                        )
                        .await
                    }
                    None => rightkit_agent_tools::coding_tools::ToolResult::error(format!(
                        "unknown tool: {}",
                        call.name
                    )),
                }
            };
            if let Some(bus) = &hook_bus {
                bus.publish(rightkit_hooks::agent_hooks::HookEvent::PostToolUse {
                    session_id: session_id.clone(),
                    tool: call.name.clone(),
                    is_error: result.is_error,
                });
            }
            (call, result)
        })
    }

    /// Record result only after every earlier queued call has been recorded.
    /// This is shared by streaming drain & existing batch semantics.
    pub(in crate::engine) async fn record_streaming_concurrent_result(
        &mut self,
        call: ToolCall,
        result: rightkit_agent_tools::coding_tools::ToolResult,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) {
        let sid = self.session_id.clone();
        let mut result = result;
        if let Err(error) = self.record_assurance_receipt(&call, &result) {
            result.is_error = true;
            result.content = format!(
                "{}\n\n[assurance receipt failed: {error}; completion is blocked until this is repaired]",
                result.content
            );
        }
        turn_results.insert(call.id.clone(), result.clone());
        if !result.is_error {
            self.add_typed_mutations(result.mutations.len() as u32);
        }
        self.observe_tool_result_provenance(&call.name);
        if !result.is_error {
            self.pending_view_images
                .extend(extract_view_images(&call.id, &result.content));
        }
        self.record_tool_runtime_span(
            MiddlewarePhase::PostToolCall,
            if result.is_error {
                "error"
            } else {
                "completed"
            },
            &call,
            None,
            serde_json::json!({
                "dispatchMode": "streamingConcurrent",
                "isError": result.is_error,
                "contentBytes": result.content.len(),
            }),
        );
        let (summary, output, store_failed) =
            self.store_tool_output(&call.name, &result.content, result.media_type.as_deref());
        // A failed archive is a typed failure, not a clean observation.
        let is_error = result.is_error || store_failed;
        self.transcript
            .push_tool_result_with_output(&call.id, &summary, output.clone(), is_error);
        let result_msg = rightkit_agent_engine_protocol::TranscriptMessage::Tool {
            id: "tr".into(),
            created_at: now_iso(),
            tool_call_id: call.id.clone(),
            summary: summary.clone(),
            output,
            trust: Some(tool_result_trust(&call.name, &summary)),
            is_error,
            pinned: false,
            process_id: result.process_id.clone(),
        };
        self.emit(|seq| ProtocolEvent::ToolResultAdded {
            session_id: sid,
            seq,
            created_at: now_iso(),
            result: result_msg,
        });
        if let Err(repair) = self.checkpoint_sentinel(&call, &result).await {
            self.transcript
                .push_user(format!("[Sentinel repair] {repair}"));
        }
    }

    pub(in crate::engine) async fn dispatch_concurrent_batch(
        &mut self,
        calls: &[&ToolCall],
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) {
        // Hook-bearing calls must use the serialized authority.  The caller's
        // classifier already excludes them, but keep this boundary defensive
        // so a direct internal consumer cannot publish an origin for the
        // proposal before its hook transform and final policy evaluation.
        if self.hook_bus.is_some() {
            for call in calls {
                self.dispatch_tool_call(call, turn_results).await;
            }
            return;
        }
        let sid = self.session_id.clone();

        // Govern every proposal before any concurrent future starts. This
        // keeps validation, policy, approval, origin, and resource admission
        // ahead of ToolCallStarted while leaving read-only execution overlap
        // intact.
        let mut admitted_concurrent_ids = HashSet::new();
        for call in calls {
            let schema = self.tools.get(&call.name).map(|tool| tool.input_schema());
            let (_, argument_error) = parse_tool_arguments_attributed(
                &call.arguments_json,
                schema.as_ref(),
                Some(&call.name),
                self.repair_attribution.as_ref(),
            );
            let mut transformed_call = (*call).clone();
            let transform_requires_serial = if argument_error.is_none() {
                let (arguments, _) = parse_tool_arguments_attributed(
                    &call.arguments_json,
                    schema.as_ref(),
                    Some(&call.name),
                    self.repair_attribution.as_ref(),
                );
                transformed_call.arguments_json = serde_json::to_string(&arguments)
                    .expect("serde_json::Value always serializes into tool arguments");
                self.apply_pre_tool_arg_transforms(&mut transformed_call);
                transformed_call.arguments_json != call.arguments_json
            } else {
                false
            };
            let guardrail = evaluate_tool_guardrails(&transformed_call);
            let policy_requires_serial = self
                .evaluate_pre_tool_policy(&transformed_call)
                .iter()
                .any(|decision| {
                    matches!(
                        decision.action,
                        AppliedMiddlewareAction::Deny | AppliedMiddlewareAction::RequireApproval
                    )
                });
            let concurrency_denied = !self.can_dispatch_readonly_fast(call);
            // A configured PreToolUse hook can mutate the payload, ask for
            // approval, or contribute model context; all three need the
            // serial authority (approval flow, digest binding, transcript).
            let hook_requires_serial = self
                .hook_bus
                .as_ref()
                .is_some_and(|bus| bus.has_pre_tool_use_hooks());
            let governed_serial = transform_requires_serial
                || policy_requires_serial
                || guardrail.is_some()
                || hook_requires_serial;
            if argument_error.is_some() || concurrency_denied {
                let reason = if argument_error.is_some() {
                    "tool arguments failed canonical validation".to_string()
                } else {
                    "readonly fast-path admission denied; dispatch must remain governed".to_string()
                };
                for stage in [
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Validate,
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Transform,
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Normalize,
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Policy,
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Approval,
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Verify,
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Settle,
                ] {
                    crate::tool_dispatch_plumbing::record_production_admission_stage(
                        &call.id, stage,
                    );
                }
                self.record_tool_error(&sid, call, reason, turn_results);
                continue;
            }
            if governed_serial {
                // The serial authority owns transformed payloads, approval
                // grants, guardrails, and EFF-002 digest binding. Do not
                // manufacture a concurrent result or run hooks twice here.
                self.dispatch_tool_call(call, turn_results).await;
                continue;
            }
            crate::tool_dispatch_plumbing::record_production_admission_stage(
                &call.id,
                crate::tool_dispatch_plumbing::EffectAdmissionStage::Validate,
            );
            crate::tool_dispatch_plumbing::record_production_admission_stage(
                &call.id,
                crate::tool_dispatch_plumbing::EffectAdmissionStage::Transform,
            );
            crate::tool_dispatch_plumbing::record_production_admission_stage(
                &call.id,
                crate::tool_dispatch_plumbing::EffectAdmissionStage::Normalize,
            );
            crate::tool_dispatch_plumbing::record_production_admission_stage(
                &call.id,
                crate::tool_dispatch_plumbing::EffectAdmissionStage::Policy,
            );
            crate::tool_dispatch_plumbing::record_production_admission_stage(
                &call.id,
                crate::tool_dispatch_plumbing::EffectAdmissionStage::Approval,
            );
            let origin_seq = self.conduit.next_seq();
            let origin_event = ProtocolEvent::ToolCallStarted {
                session_id: sid.clone(),
                seq: origin_seq,
                created_at: now_iso(),
                call: (*call).clone(),
            };
            let origin_outcome = self.record_run_origin_event(&origin_event).await;
            if !matches!(
                crate::run_origin::decide_after_origin(&origin_outcome),
                crate::run_origin::PreEffectDecision::Execute
            ) {
                let reason = format!("pre-effect origin seam refused the call: {origin_outcome}");
                self.record_tool_error(&sid, call, reason, turn_results);
                for stage in [
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Verify,
                    crate::tool_dispatch_plumbing::EffectAdmissionStage::Settle,
                ] {
                    crate::tool_dispatch_plumbing::record_production_admission_stage(
                        &call.id, stage,
                    );
                }
                continue;
            }
            self.conduit.publish(origin_event);
            crate::tool_dispatch_plumbing::record_production_admission_stage(
                &call.id,
                crate::tool_dispatch_plumbing::EffectAdmissionStage::Resource,
            );
            self.record_tool_runtime_span(
                MiddlewarePhase::PreToolCall,
                "completed",
                call,
                None,
                serde_json::json!({
                    "path": extract_path_arg(&call.arguments_json),
                    "hasCommand": extract_command_arg(&call.arguments_json).is_some(),
                    "commandBytes": extract_command_arg(&call.arguments_json).map(|value| value.len()),
                    "dispatchMode": "concurrentBatch",
                }),
            );
            let path_arg = extract_path_arg(&call.arguments_json);
            let command_arg = extract_command_arg(&call.arguments_json);
            self.add_triggered_skill_hint("PreToolUse", |ctx| {
                ctx.set("tool", &call.name);
                if let Some(path) = &path_arg {
                    ctx.set("file_path", path);
                }
                if let Some(command) = &command_arg {
                    ctx.set("command", command);
                }
                ctx.trigger_type = Some("tool_call".into());
                ctx.trigger_payload = Some(call.name.clone());
            });
            admitted_concurrent_ids.insert(call.id.clone());
        }

        // Snapshot everything needed for execution (all owned/Arc — no &mut borrow).
        let tools = self.tools.clone();
        let workspace_root = self.workspace_root.clone();
        let session_id = self.session_id.clone();
        let permission_mode = self.permission_mode;
        let workspace_trust = self.workspace_trust;
        let content_provenance = self.content_provenance.clone();
        let spawner = self.spawner.clone();
        let subagent_supervisor = self.subagent_supervisor.clone();
        let questioner = self.questioner.clone();
        #[cfg(feature = "host")]
        let background_shells = self.background_shells.clone();
        let file_service = self.file_service.clone();
        #[cfg(feature = "host")]
        let browser_sessions = self.browser_sessions.clone();
        let task_store = self.task_store.clone();
        let execution_backend = self.execution_backend.clone();
        let execution_workspace_id = self.execution_workspace_id.clone();
        let execution_workspace_path = self.execution_workspace_path.clone();
        let execution_network_profile = self.execution_network_profile.clone();
        let execution_resource_limits = self.execution_resource_limits.clone();
        #[cfg(feature = "host")]
        let execution_backend_registry = self.execution_backend_registry.clone();
        let mcp_channel = self.mcp_channel.clone();
        let toolset = self.toolset.clone();
        let plan_exit = self.plan_exit.clone();
        let message_dispatcher = self.message_dispatcher.clone();
        let team_directory = self.team_directory.clone();
        let web_search_config = self.web_search_config.clone();
        let image_provider_keys = self.image_provider_keys.clone();
        let cloudflare_config = self.cloudflare_config.clone();
        let github_token = self.github_token.clone();
        let await_deadline =
            crate::tool_await::non_provider_await_deadline(self.iteration_budget.max());
        let mcp = self.mcp.clone();
        let tool_result_dir = self.tool_result_dir.clone();
        let skill_dirs = self.skill_dirs.clone();
        let path_deny = self.deny_globs.clone();
        let owned_calls: Vec<ToolCall> = calls
            .iter()
            .filter(|call| admitted_concurrent_ids.contains(&call.id))
            .map(|c| (*c).clone())
            .collect();
        let available_tool_names: HashSet<String> = self
            .tools
            .all()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        let skill_ceiling_observations = calls
            .iter()
            .flat_map(|call| {
                skill_toolset_ceiling_observations(
                    &self.skill_dirs,
                    &self.workspace_root,
                    call,
                    &available_tool_names,
                )
            })
            .collect::<Vec<_>>();
        for observation in skill_ceiling_observations {
            let session_id = self.session_id.clone();
            self.emit(|seq| ProtocolEvent::OrchestrationObservationRecorded {
                session_id: session_id.clone(),
                seq,
                created_at: now_iso(),
                observation: rightkit_agent_engine_protocol::OrchestrationObservation {
                    id: format!("skill-toolset-ceiling-{seq}"),
                    kind: "skill_toolset_ceiling".into(),
                    subject: session_id.clone(),
                    summary: format!(
                        "skill '{}' requested tool(s) outside the active session ceiling: {}",
                        observation.skill,
                        observation.requested_tools_outside_ceiling.join(",")
                    ),
                    fields: observation.fields(),
                    created_at: now_iso(),
                },
            });
        }
        // EFF-001: this path has no approval stage, because it is documented
        // to receive only concurrency-safe, side-effect-free calls. That was a
        // CALLER-enforced invariant: a mis-routed mutating call would have
        // executed here with no approval at all. Re-verify it inside the
        // authority so the guarantee does not depend on every caller getting
        // the routing right.
        // Keep the eligibility guard separate from skill policy.  A hook may
        // replace arguments, so skill policy must be recomputed below from
        // the effective call rather than captured from this proposal.
        let concurrency_denials: HashMap<String, String> = calls
            .iter()
            .filter(|&call| !self.can_dispatch_readonly_fast(call) ).map(|call| (
                        call.id.clone(),
                        format!(
                            "`{}` is not concurrency-safe and must not run in a concurrent batch; effect-producing calls execute sequentially through the approval path",
                            call.name
                        ),
                    ))
            .collect();

        // Build one future per call; these run without holding any &mut self borrow.
        let hook_bus = self.hook_bus.clone();
        let repair_attribution = self.repair_attribution.clone();
        let futs: Vec<_> = owned_calls
            .into_iter()
            .map(|call| {
                let repair_attribution = repair_attribution.clone();
                let tools = tools.clone();
                let workspace_root = workspace_root.clone();
                let session_id = session_id.clone();
                let content_provenance = content_provenance.clone();
                let spawner = spawner.clone();
                let subagent_supervisor = subagent_supervisor.clone();
                let questioner = questioner.clone();
                #[cfg(feature = "host")]
                let background_shells = background_shells.clone();
                let file_service = file_service.clone();
                #[cfg(feature = "host")]
                let browser_sessions = browser_sessions.clone();
                let task_store = task_store.clone();
                let execution_backend = execution_backend.clone();
                let execution_workspace_id = execution_workspace_id.clone();
                let execution_workspace_path = execution_workspace_path.clone();
                let execution_network_profile = execution_network_profile.clone();
                let execution_resource_limits = execution_resource_limits.clone();
                #[cfg(feature = "host")]
                let execution_backend_registry = execution_backend_registry.clone();
                let mcp_channel = mcp_channel.clone();
                let toolset = toolset.clone();
                let plan_exit = plan_exit.clone();
                let message_dispatcher = message_dispatcher.clone();
                let team_directory = team_directory.clone();
                let web_search_config = web_search_config.clone();
                let image_provider_keys = image_provider_keys.clone();
                let cloudflare_config = cloudflare_config.clone();
                let github_token = github_token.clone();
                let mcp = mcp.clone();
                let tool_result_dir = tool_result_dir.clone();
                let skill_dirs = skill_dirs.clone();
                let path_deny = path_deny.clone();
                let hook_bus = hook_bus.clone();
                let concurrency_denial = concurrency_denials.get(&call.id).cloned();
                async move {
                    let mut call = call;
                    let (mut args, mut arg_parse_error) = parse_tool_arguments_attributed(
                        &call.arguments_json,
                        tools.get(&call.name).map(|t| t.input_schema()).as_ref(),
                        Some(&call.name),
                        repair_attribution.as_ref(),
                    );
                    let mut hook_denied = false;
                    if arg_parse_error.is_none() {
                        if let Some(bus) = &hook_bus {
                            match bus.run_pre_tool_use(&session_id, &call.name, &args).await {
                                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Transform(
                                    transformed,
                                ) => {
                                    let serialized = serde_json::to_string(&transformed).expect(
                                        "serde_json::Value always serializes into tool arguments",
                                    );
                                    let (validated, error) = parse_tool_arguments_attributed(
                                        &serialized,
                                        tools.get(&call.name).map(|t| t.input_schema()).as_ref(),
                                        Some(&call.name),
                                        repair_attribution.as_ref(),
                                    );
                                    args = validated;
                                    arg_parse_error = error;
                                    call.arguments_json = serde_json::to_string(&args).expect(
                                        "serde_json::Value always serializes into tool arguments",
                                    );
                                }
                                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Deny(
                                    message,
                                ) => {
                                    hook_denied = true;
                                    arg_parse_error = Some(message);
                                }
                                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Ask {
                                    reason,
                                    ..
                                } => {
                                    hook_denied = true;
                                    arg_parse_error = Some(format!(
                                        "PreToolUse hook requires approval: {reason}"
                                    ));
                                }
                                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Error(
                                    message,
                                ) => {
                                    eprintln!("[hook] PreToolUse error (non-blocking): {message}");
                                }
                                rightkit_hooks::agent_hooks::bus::PreToolUseOutcome::Proceed => {}
                            }
                        }
                    }
                    let effect_denial =
                        skill_effect_denial_for_tool_call(&skill_dirs, &workspace_root, &call);
                    #[cfg(feature = "host")]
                    let process_launch = match required_process_launch_authority(
                        tools.as_ref(),
                        &call.name,
                        &args,
                        &workspace_root,
                    ) {
                        Ok(process_launch) => process_launch,
                        Err(result) => return (call, result),
                    };
                    let mut ctx = rightkit_agent_tools::coding_tools::ToolContext::new(
                        &session_id,
                        &workspace_root,
                        permission_mode,
                        workspace_trust,
                    )
                    .with_content_provenance(content_provenance.clone())
                    .with_file_service(file_service)
                    .with_mcp_channel(mcp_channel.clone())
                    .with_tool_result_dir(tool_result_dir)
                    .with_skill_dirs(skill_dirs)
                    .with_path_deny(path_deny);
                    for (provider, api_key) in &image_provider_keys {
                        ctx = ctx.with_image_provider_key(provider.clone(), api_key.clone());
                    }
                    if let Some(ref toolset) = toolset {
                        ctx = ctx.with_toolset(toolset.clone());
                    }
                    if let Some(ref s) = spawner {
                        ctx = ctx.with_spawner(s.clone());
                    }
                    if let Some(ref supervisor) = subagent_supervisor {
                        ctx = ctx.with_subagent_supervisor(supervisor.clone());
                    }
                    if let Some(ref q) = questioner {
                        ctx = ctx.with_questioner(q.clone());
                    }
                    #[cfg(feature = "host")]
                    {
                        ctx = ctx.with_background_shells(background_shells.clone());
                        ctx = ctx.with_browser_sessions(browser_sessions.clone());
                    }
                    ctx = ctx.with_task_store(task_store.clone());
                    if let Some(ref backend) = execution_backend {
                        ctx = ctx.with_execution_backend(backend.clone());
                    }
                    if let Some(ref workspace_id) = execution_workspace_id {
                        ctx = ctx.with_execution_workspace_id(workspace_id.clone());
                    }
                    if let Some(ref workspace_path) = execution_workspace_path {
                        ctx = ctx.with_execution_workspace_path(workspace_path.clone());
                    }
                    if let Some(ref profile) = execution_network_profile {
                        ctx = ctx.with_execution_network_profile(profile.clone());
                    }
                    if let Some(ref limits) = execution_resource_limits {
                        ctx = ctx.with_execution_resource_limits(limits.clone());
                    }
                    #[cfg(feature = "host")]
                    if let Some(ref reg) = execution_backend_registry {
                        ctx = ctx.with_execution_backend_registry(reg.clone());
                    }
                    if let Some(ref p) = plan_exit {
                        ctx = ctx.with_plan_exit(p.clone());
                    }
                    if let Some(ref dispatcher) = message_dispatcher {
                        ctx = ctx.with_message_dispatcher(dispatcher.clone());
                    }
                    if let Some(ref directory) = team_directory {
                        ctx = ctx.with_team_directory(directory.clone());
                    }
                    if let Some(ref config) = web_search_config {
                        ctx = ctx.with_web_search_config(config.clone());
                    }
                    if let Some(ref config) = cloudflare_config {
                        ctx = ctx.with_cloudflare_config(config.clone());
                    }
                    if let Some(ref token) = github_token {
                        ctx = ctx.with_github_token(token.clone());
                    }
                    if let Some(ref m) = mcp {
                        ctx = ctx.with_mcp(m.clone());
                    }
                    #[cfg(feature = "host")]
                    if let Some((adapter, launch_context)) = process_launch {
                        ctx = ctx.with_process_launch(adapter, launch_context);
                    }
                    if !hook_denied && arg_parse_error.is_none() && effect_denial.is_none() {
                        if let Some(bus) = &hook_bus {
                            bus.publish(rightkit_hooks::agent_hooks::HookEvent::PreToolUse {
                                session_id: session_id.clone(),
                                tool: call.name.clone(),
                            });
                        }
                    }
                    let result = if let Some(err) = arg_parse_error {
                        rightkit_agent_tools::coding_tools::ToolResult::error(err)
                    } else if let Some(reason) = effect_denial {
                        rightkit_agent_tools::coding_tools::ToolResult::error(reason)
                    } else if let Some(reason) = concurrency_denial {
                        rightkit_agent_tools::coding_tools::ToolResult::error(reason)
                    } else {
                        crate::tool_dispatch_plumbing::record_production_admission_stage(
                            &call.id,
                            crate::tool_dispatch_plumbing::EffectAdmissionStage::Execute,
                        );
                        match tools.get(&call.name) {
                            Some(tool) => {
                                crate::tool_await::supervise_tool_await(
                                    &call.name,
                                    await_deadline,
                                    tool.execute(args, &ctx),
                                )
                                .await
                            }
                            None => rightkit_agent_tools::coding_tools::ToolResult::error(format!(
                                "unknown tool: {}",
                                call.name
                            )),
                        }
                    };
                    if let Some(bus) = &hook_bus {
                        bus.publish(rightkit_hooks::agent_hooks::HookEvent::PostToolUse {
                            session_id: session_id.clone(),
                            tool: call.name.clone(),
                            is_error: result.is_error,
                        });
                    }
                    (call, result)
                }
            })
            .collect();

        let results = futures::future::join_all(futs).await;

        // Record all results sequentially (requires &mut self for transcript + emit).
        for (call, result) in results {
            // Record the post-execution mutation list before the engine has
            // any chance to forget. The post-edit guard and the unbacked-
            // mutation detector both read from `turn_results`. The
            // concurrent path can produce partial deltas (one call lands,
            // another fails), and the engine must observe the union, not
            // whichever call returned last.
            let mut result = result;
            if let Err(error) = self.record_assurance_receipt(&call, &result) {
                result.is_error = true;
                result.content = format!(
                    "{}\n\n[assurance receipt failed: {error}; completion is blocked until this is repaired]",
                    result.content
                );
            }
            turn_results.insert(call.id.clone(), result.clone());
            if !result.is_error {
                self.add_typed_mutations(result.mutations.len() as u32);
            }
            self.observe_tool_result_provenance(&call.name);
            if !result.is_error {
                self.pending_view_images
                    .extend(extract_view_images(&call.id, &result.content));
            }
            self.record_tool_runtime_span(
                MiddlewarePhase::PostToolCall,
                if result.is_error {
                    "error"
                } else {
                    "completed"
                },
                &call,
                None,
                serde_json::json!({
                    "dispatchMode": "concurrentBatch",
                    "isError": result.is_error,
                    "contentBytes": result.content.len(),
                }),
            );
            let (summary, output, store_failed) =
                self.store_tool_output(&call.name, &result.content, result.media_type.as_deref());
            // A failed archive is a typed failure, not a clean observation.
            let is_error = result.is_error || store_failed;
            self.transcript.push_tool_result_with_output(
                &call.id,
                &summary,
                output.clone(),
                is_error,
            );
            let result_msg = rightkit_agent_engine_protocol::TranscriptMessage::Tool {
                id: "tr".into(),
                created_at: now_iso(),
                tool_call_id: call.id.clone(),
                summary: summary.clone(),
                output,
                trust: Some(tool_result_trust(&call.name, &summary)),
                is_error,
                pinned: false,
                process_id: result.process_id.clone(),
            };
            self.emit(|seq| ProtocolEvent::ToolResultAdded {
                session_id: sid.clone(),
                seq,
                created_at: now_iso(),
                result: result_msg,
            });
            if let Err(repair) = self.checkpoint_sentinel(&call, &result).await {
                self.transcript
                    .push_user(format!("[Sentinel repair] {repair}"));
            }
            crate::tool_dispatch_plumbing::record_production_admission_stage(
                &call.id,
                crate::tool_dispatch_plumbing::EffectAdmissionStage::Verify,
            );
            crate::tool_dispatch_plumbing::record_production_admission_stage(
                &call.id,
                crate::tool_dispatch_plumbing::EffectAdmissionStage::Settle,
            );
        }

        // Mirror the latest todo_write call into the task graph so the UI and any
        // persistence layer reflect the agent's current plan.
        if let Some(call) = calls.iter().rev().find(|c| c.name == "todo_write") {
            let args: serde_json::Value =
                serde_json::from_str(&call.arguments_json).unwrap_or(serde_json::Value::Null);
            let tasks =
                rightkit_agent_tools::coding_tools::todo_write::task_nodes_from_args(&args, "t");
            if !tasks.is_empty() {
                let summary = format!("{} todos", tasks.len());
                self.emit(|seq| ProtocolEvent::TaskGraphUpdated {
                    session_id: sid.clone(),
                    seq,
                    created_at: now_iso(),
                    summary,
                    tasks,
                });
            }
        }
    }
}
