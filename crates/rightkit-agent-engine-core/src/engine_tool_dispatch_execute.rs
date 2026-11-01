use super::*;

// ENG-92: observe-only tool-call loop/repeat detection. Declared here (rather
// than in `engine_tool_dispatch.rs`, which is out of scope for this change)
// with an explicit `#[path]` — this file is itself pulled in via
// `#[path = "engine_tool_dispatch_execute.rs"] mod single_execute;`, so a
// bare `mod repeat_detector;` would resolve relative to a nonexistent
// `engine_tool_dispatch_execute/` subdirectory. The explicit path points
// straight at the sibling file in `crates/engine/src/`.
#[path = "repeat_detector.rs"]
mod repeat_detector;
use repeat_detector::RepeatDetector;

struct SessionCredentialRequester {
    session_id: String,
    conduit: Arc<crate::conduit::Conduit>,
    pending: Arc<Mutex<crate::credential_request::PendingCredentialRequests>>,
}

impl rightkit_agent_tools::coding_tools::CredentialRequester for SessionCredentialRequester {
    fn request(
        &self,
        tool_call_id: String,
        prompt: rightkit_agent_tools::coding_tools::CredentialPrompt,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Option<rightkit_agent_tools::coding_tools::SecretString>,
                        String,
                    >,
                > + Send,
        >,
    > {
        let session_id = self.session_id.clone();
        let conduit = self.conduit.clone();
        let pending = self.pending.clone();
        Box::pin(async move {
            use crate::credential_request::{CredentialKind, CredentialRequest, CredentialResume};
            let kind = match prompt.provider.as_str() {
                "cloudflare" => CredentialKind::Cloudflare,
                "flyio" => CredentialKind::FlyIo,
                _ => return Err("Unsupported credential provider.".to_string()),
            };
            let request = CredentialRequest::new(tool_call_id.clone(), kind, prompt.reason);
            let receiver = pending.lock().unwrap().insert(request.clone());
            conduit.emit(|seq| {
                rightkit_agent_engine_protocol::ProtocolEvent::CredentialRequested {
                    session_id: session_id.clone(),
                    seq,
                    created_at: now_iso(),
                    tool_call_id,
                    provider: request.kind.label().to_string(),
                }
            });
            match tokio::time::timeout(std::time::Duration::from_secs(300), receiver).await {
                Ok(Ok(CredentialResume::Supplied(value))) => {
                    Ok(Some(rightkit_agent_tools::coding_tools::SecretString::new(
                        value.expose().to_string(),
                    )))
                }
                Ok(Ok(CredentialResume::Declined)) => Ok(None),
                Ok(Err(_)) => Err("Credential request was closed.".to_string()),
                Err(_) => {
                    pending.lock().unwrap().remove(&request.tool_call_id);
                    conduit.emit(|seq| {
                        rightkit_agent_engine_protocol::ProtocolEvent::CredentialResolved {
                            session_id,
                            seq,
                            created_at: now_iso(),
                            tool_call_id: request.tool_call_id,
                            outcome: "timed_out".to_string(),
                        }
                    });
                    Err("Credential request timed out.".to_string())
                }
            }
        })
    }
}

struct CancelGuard {
    store: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
    tool_call_id: String,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        match self.store.lock() {
            Ok(mut map) => {
                map.remove(&self.tool_call_id);
            }
            Err(poisoned) => {
                eprintln!(
                    "[engine] cancel_tokens mutex poisoned while cleaning \
                     tool_call {}: {poisoned}; entry will leak",
                    self.tool_call_id
                );
            }
        }
    }
}

impl SessionEngine {
    pub(in crate::engine) async fn execute_single_tool_call(
        &mut self,
        sid: &str,
        call: &ToolCall,
        pre_span_id: Option<&str>,
        user_approved: bool,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_tools::coding_tools::ToolResult,
        >,
    ) -> Option<rightkit_agent_tools::coding_tools::ToolResult> {
        let (args, arg_parse_error) = parse_tool_arguments_attributed(
            &call.arguments_json,
            self.tools
                .get(&call.name)
                .map(|t| t.input_schema())
                .as_ref(),
            Some(&call.name),
            self.repair_attribution.as_ref(),
        );

        // EFF-002: recompute the effect digest from the payload that is about
        // to execute and compare it to what the approval actually authorized.
        // Everything between approval and here - argument re-parse/repair, and
        // any future permitted transform - is inside this window, so a payload
        // that changed can no longer ride the old approval.
        if user_approved {
            let approved_digest = self
                .approved_effect_digests
                .lock()
                .unwrap()
                .get(&call.id)
                .cloned();
            let approval_required = self
                .approval_required_calls
                .lock()
                .unwrap()
                .contains(&call.id);
            if approval_required && approved_digest.is_none() {
                // An approval-required grant is only consumable with the digest
                // it was bound to. Missing (revoked, expired, replayed) denies.
                let summary = format!(
                    "Approval for `{}` carries no payload digest; the change must be re-approved before it can run.",
                    call.name
                );
                self.record_tool_runtime_span(
                    MiddlewarePhase::PostToolCall,
                    "denied",
                    call,
                    pre_span_id,
                    serde_json::json!({ "reason": "effect_digest_missing" }),
                );
                self.record_tool_error(sid, call, summary, turn_results);
                return None;
            }
            if let Some(approved_digest) = approved_digest {
                let current_digest =
                    crate::tool_dispatch_plumbing::effect_payload_digest(&call.name, &args);
                if current_digest != approved_digest {
                    let summary = format!(
                        "Approval no longer matches the executable payload for `{}` (approved {approved_digest}, now {current_digest}); the change must be re-evaluated before it can run.",
                        call.name
                    );
                    self.record_tool_runtime_span(
                        MiddlewarePhase::PostToolCall,
                        "denied",
                        call,
                        pre_span_id,
                        serde_json::json!({
                            "reason": "effect_digest_mismatch",
                            "approvedDigest": approved_digest,
                            "currentDigest": current_digest,
                        }),
                    );
                    self.approved_effect_digests
                        .lock()
                        .unwrap()
                        .remove(&call.id);
                    self.record_tool_error(sid, call, summary, turn_results);
                    return None;
                }
            }
        }
        // The grant is single-use for this dispatch; a later call must obtain
        // its own approval rather than inherit this one.
        self.approved_effect_digests
            .lock()
            .unwrap()
            .remove(&call.id);
        // The approval-required marker stays after consumption on purpose: a
        // replay of the same call id finds no digest and is denied above. It
        // is pruned when the turn ends (`prune_resolved_approval_markers`) or
        // on Deny/timeout/revoke.

        let turn_key = turn_results as *const _ as usize;
        let repeat_observation = RepeatDetector::observe(turn_key, sid, &call.name, &args);
        if repeat_observation.repeated {
            tracing::info!(
                session_id = %sid,
                tool = %call.name,
                tool_call_id = %call.id,
                occurrence = repeat_observation.occurrence,
                "ENG-92: repeated tool call detected within turn (observe-only, not blocked)"
            );
        }

        // KRN-008: resolve the one composition-owned process authority before
        // hooks, approvals, tool execution, or launch. Missing composition is
        // a typed failure; process-backed tools never receive an ungoverned adapter.
        #[cfg(feature = "host")]
        let process_launch = match required_process_launch_authority(
            self.tools.as_ref(),
            &call.name,
            &args,
            &self.workspace_root,
        ) {
            Ok(process_launch) => process_launch,
            Err(result) => return Some(*result),
        };

        if let Some(bus) = &self.hook_bus {
            if matches!(call.name.as_str(), "shell" | "powershell") {
                let command = args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if is_git_commit_command(command) {
                    let diff = staged_git_diff(&self.workspace_root).await;
                    if let rightkit_hooks::agent_hooks::HookOutcome::Deny(msg) =
                        bus.run_pre_commit(&self.session_id, &diff).await
                    {
                        let msg = format!("PreCommit gate denied the commit: {msg}");
                        self.record_tool_runtime_span(
                            MiddlewarePhase::PostToolCall,
                            "denied",
                            call,
                            pre_span_id,
                            serde_json::json!({ "reason": msg }),
                        );
                        self.record_tool_error(sid, call, msg, turn_results);
                        return None;
                    }
                }
            }
        }
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::PreToolUse {
            session_id: self.session_id.clone(),
            tool: call.name.clone(),
        });
        crate::tool_dispatch_plumbing::record_production_admission_stage(
            &call.id,
            crate::tool_dispatch_plumbing::EffectAdmissionStage::Execute,
        );
        let result = if let Some(err) = arg_parse_error {
            rightkit_agent_tools::coding_tools::ToolResult::error(err)
        } else {
            let registry = self.tools.clone();
            match registry.get(&call.name) {
                Some(tool) => {
                    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
                    self.cancel_tokens
                        .lock()
                        .unwrap()
                        .insert(call.id.clone(), cancel_tx);
                    let cancel_guard = CancelGuard {
                        store: self.cancel_tokens.clone(),
                        tool_call_id: call.id.clone(),
                    };
                    self.propagate_cancel_to_background_shells(&call.id, cancel_rx.clone());
                    let effective_mode = if user_approved {
                        rightkit_agent_policy::PermissionMode::Bypass
                    } else {
                        self.permission_mode
                    };
                    let mut ctx = ToolContext::new(
                        &self.session_id,
                        &self.workspace_root,
                        effective_mode,
                        self.workspace_trust,
                    )
                    .with_sandbox_permission_mode(self.permission_mode)
                    .with_tool_call_id(call.id.clone())
                    .with_credential_requester(Arc::new(SessionCredentialRequester {
                        session_id: self.session_id.clone(),
                        conduit: self.conduit.clone(),
                        pending: self.pending_credential_requests.clone(),
                    }))
                    .with_content_provenance(self.content_provenance.clone())
                    .with_file_service(self.file_service.clone())
                    .with_durable_write_approval(user_approved)
                    .with_mcp_channel(self.mcp_channel.clone())
                    .with_cancel_rx(cancel_rx.clone())
                    .with_tool_result_dir(self.tool_result_dir.clone())
                    .with_skill_dirs(self.skill_dirs.clone())
                    .with_path_deny(self.deny_globs.clone());
                    #[cfg(feature = "host")]
                    if let Some((approved_payload, grant)) =
                        self.sandbox_retry_grants.remove(&call.id)
                    {
                        let current_payload =
                            crate::tool_dispatch_plumbing::effect_payload_digest(&call.name, &args);
                        if approved_payload == current_payload {
                            ctx.sandbox_retry_grant = Some(grant);
                        }
                    }
                    let output_coalescer = crate::tool_output_stream::ToolOutputCoalescer::new(
                        self.conduit.clone(),
                        self.session_id.clone(),
                        call.id.clone(),
                    );
                    ctx = ctx.with_output_notifier(output_coalescer.notifier());
                    #[cfg(feature = "host")]
                    if let Some((adapter, launch_context)) = process_launch {
                        ctx = ctx.with_process_launch(adapter, launch_context);
                    }
                    for (provider, api_key) in &self.image_provider_keys {
                        ctx = ctx.with_image_provider_key(provider.clone(), api_key.clone());
                    }
                    if let Some(ref toolset) = self.toolset {
                        ctx = ctx.with_toolset(toolset.clone());
                    }
                    if let Some(ref spawner) = self.spawner {
                        ctx = ctx.with_spawner(spawner.clone());
                    }
                    if let Some(ref supervisor) = self.subagent_supervisor {
                        ctx = ctx.with_subagent_supervisor(supervisor.clone());
                    }
                    if let Some(ref questioner) = self.questioner {
                        ctx = ctx.with_questioner(questioner.clone());
                    }
                    #[cfg(feature = "host")]
                    {
                        ctx = ctx.with_background_shells(self.background_shells.clone());
                        ctx = ctx.with_browser_sessions(self.browser_sessions.clone());
                    }
                    ctx = ctx.with_task_store(self.task_store.clone());
                    if let Some(ref backend) = self.execution_backend {
                        ctx = ctx.with_execution_backend(backend.clone());
                    }
                    if let Some(ref workspace_id) = self.execution_workspace_id {
                        ctx = ctx.with_execution_workspace_id(workspace_id.clone());
                    }
                    if let Some(ref workspace_path) = self.execution_workspace_path {
                        ctx = ctx.with_execution_workspace_path(workspace_path.clone());
                    }
                    if let Some(ref profile) = self.execution_network_profile {
                        ctx = ctx.with_execution_network_profile(profile.clone());
                    }
                    if let Some(ref limits) = self.execution_resource_limits {
                        ctx = ctx.with_execution_resource_limits(limits.clone());
                    }
                    #[cfg(feature = "host")]
                    if let Some(ref reg) = self.execution_backend_registry {
                        ctx = ctx.with_execution_backend_registry(reg.clone());
                    }
                    if let Some(ref p) = self.plan_exit {
                        ctx = ctx.with_plan_exit(p.clone());
                    }
                    if let Some(ref dispatcher) = self.message_dispatcher {
                        ctx = ctx.with_message_dispatcher(dispatcher.clone());
                    }
                    if let Some(ref directory) = self.team_directory {
                        ctx = ctx.with_team_directory(directory.clone());
                    }
                    if let Some(ref config) = self.web_search_config {
                        ctx = ctx.with_web_search_config(config.clone());
                    }
                    if let Some(ref config) = self.cloudflare_config {
                        ctx = ctx.with_cloudflare_config(config.clone());
                    }
                    if let Some(ref token) = self.github_token {
                        ctx = ctx.with_github_token(token.clone());
                    }
                    if let Some(ref m) = self.mcp {
                        ctx = ctx.with_mcp(m.clone());
                    }
                    // Sessions rule 41: the engine's write path writes the restore
                    // point before a write tool runs. A call that cannot get one
                    // fails with `history_unavailable` and never edits.
                    let write_targets = tool.write_targets(&args, &ctx);
                    let history = (!write_targets.is_empty()).then(|| {
                        rightkit_agent_engine_history::HistoryStore::new(
                            &self.workspace_root,
                            ".rightkit",
                            &self.session_id,
                        )
                    });
                    let history_failure = history
                        .as_ref()
                        .and_then(|store| store.snapshot_targets(&write_targets).err());
                    let result = if let Some(failure) = history_failure {
                        rightkit_agent_tools::coding_tools::ToolResult::error(failure)
                    } else if call.name == "exec" {
                        crate::engine::composition::execute_exec_call(
                            self,
                            call,
                            &args,
                            cancel_rx.clone(),
                        )
                        .await
                    } else if call.name == "run_program" {
                        #[cfg(feature = "host")]
                        {
                            crate::engine::composition::execute_run_program_call(
                                self, call, &args, &ctx,
                            )
                            .await
                        }
                        #[cfg(not(feature = "host"))]
                        {
                            rightkit_agent_tools::coding_tools::ToolResult::error(
                                "run_program is unavailable in this runtime profile",
                            )
                        }
                    } else {
                        crate::tool_await::supervise_tool_await(
                            &call.name,
                            crate::tool_await::non_provider_await_deadline(
                                self.iteration_budget.max(),
                            ),
                            tool.execute(args, &ctx),
                        )
                        .await
                    };
                    if let Some(store) = history.as_ref() {
                        for path in store.record_after_targets(&write_targets) {
                            tracing::warn!(
                                session_id = %self.session_id,
                                path = %path.display(),
                                "history: the post-edit state was not recorded; a rewind will not verify this file"
                            );
                        }
                    }
                    output_coalescer.finish();
                    drop(cancel_guard);
                    result
                }
                None => rightkit_agent_tools::coding_tools::ToolResult::error(format!(
                    "unknown tool: {}",
                    call.name
                )),
            }
        };
        Some(result)
    }
}

fn is_git_commit_command(command: &str) -> bool {
    let mut saw_git = false;
    for word in command.split_whitespace() {
        if word == "git" {
            saw_git = true;
        } else if saw_git && word == "commit" {
            return true;
        } else if word == "&&" || word == ";" || word == "|" {
            saw_git = false;
        }
    }
    false
}

async fn staged_git_diff(workspace_root: &std::path::Path) -> String {
    #[cfg(feature = "host")]
    {
        let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
        let context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: workspace_root.display().to_string(),
            sandbox_profile: Some("git-precommit-diff".into()),
            sandbox_required: true,
            network_required: false,
        };
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: "git".into(),
            argv: vec!["diff".into(), "--cached".into()],
            cwd: context.workspace_root.clone(),
            context,
            env: Vec::new(),
        };
        let Ok(mut child) =
            rightkit_agent_tools::coding_tools::process_resource_authority::spawn_with_process_authority(
                &adapter,
                spec,
                rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::ChildProcess,
                true,
            )
            .await
        else {
            return String::new();
        };
        let Some(mut stdout) = child.take_stdout() else {
            let _ = child.cancel().await;
            return String::new();
        };
        let mut bytes = Vec::new();
        if tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut bytes)
            .await
            .is_err()
        {
            let _ = child.cancel().await;
            return String::new();
        }
        if child.wait().await.ok() == Some(0) {
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            String::new()
        }
    }
    #[cfg(not(feature = "host"))]
    {
        let _ = workspace_root;
        String::new()
    }
}

#[cfg(test)]
mod pre_commit_gate_tests {
    use super::{is_git_commit_command, staged_git_diff};

    #[test]
    fn detects_plain_and_compound_git_commit() {
        assert!(is_git_commit_command("git commit -m msg"));
        assert!(is_git_commit_command("git add -A && git commit -m msg"));
        assert!(is_git_commit_command("cd x; git commit --amend"));
    }

    #[test]
    fn ignores_non_commit_git_and_lookalikes() {
        assert!(!is_git_commit_command("git status"));
        assert!(!is_git_commit_command("git add commit.txt"));
        assert!(!is_git_commit_command("echo git && echo commit"));
        assert!(!is_git_commit_command("cargo commit"));
    }

    #[tokio::test]
    async fn staged_diff_is_empty_outside_a_repo() {
        let dir = std::env::temp_dir().join("cr-precommit-norepo-test");
        let _ = std::fs::create_dir_all(&dir);
        assert_eq!(staged_git_diff(&dir).await, "");
    }
}
