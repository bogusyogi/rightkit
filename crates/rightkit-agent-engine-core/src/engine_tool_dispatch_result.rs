use super::*;

/// Longest the language server may take, across all files of one edit, before its rows are dropped.
#[cfg(feature = "host")]
const INLINE_DIAGNOSTICS_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(feature = "host")]
const INLINE_DIAGNOSTICS_MAX_FILES: usize = 5;
#[cfg(feature = "host")]
const INLINE_DIAGNOSTICS_MAX_ROWS: usize = 20;

impl SessionEngine {
    pub(in crate::engine) async fn checkpoint_sentinel(
        &mut self,
        call: &ToolCall,
        result: &rightkit_agent_coding_tools::ToolResult,
    ) -> Result<(), String> {
        let (Some(client), Some(run_id)) = (&self.sentinel_client, &self.sentinel_run_id) else {
            return Ok(());
        };
        let receipt = self
            .assurance_broker
            .as_ref()
            .and_then(|broker| broker.lock().ok())
            .and_then(|broker| broker.receipt(&call.id).cloned());
        let receipt_json = receipt
            .as_ref()
            .and_then(|receipt| serde_json::to_string(receipt).ok());
        let checks = receipt.as_ref().map(|receipt| {
            vec![serde_json::json!({
                "kind": receipt.tool_class,
                "specification": receipt.operation,
                "criterion_ids": receipt.criterion_ids,
                "exit_code": receipt.exit_status,
                "receipt": receipt_json.clone().unwrap_or_default(),
            })]
        });
        let evidence = receipt.as_ref().map(|receipt| {
            let value = receipt_json.clone().unwrap_or_default();
            vec![
                serde_json::json!({"kind":"repo","receipt":value.clone(),"criterion_ids":receipt.criterion_ids}),
                serde_json::json!({"kind":"test","receipt":value,"criterion_ids":receipt.criterion_ids}),
            ]
        });
        let arguments = serde_json::json!({
            "operation": "checkpoint",
            "run_id": run_id,
            "session_id": self.session_id,
            "task_id": self.assurance_broker.as_ref().and_then(|broker| broker.lock().ok().and_then(|broker| broker.contract().map(|contract| contract.task_id.clone()))),
            "summary": format!("host receipt: {} ({})", call.name, if result.is_error { "failed" } else { "passed" }),
            "checks": checks,
            "evidence": evidence,
        });
        let value = client
            .lock()
            .await
            .beacon(arguments)
            .await
            .map_err(|error| error.to_string())?;
        let decision = value
            .get("decision")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("blocked");
        if matches!(decision, "blocked" | "escalate") {
            let repair = value
                .get("next_actions")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    "Sentinel checkpoint is blocked; obtain valid evidence before continuing."
                        .into()
                });
            return Err(repair);
        }
        Ok(())
    }

    pub(in crate::engine) fn assurance_scope_for_call(&self, call: &ToolCall) -> Vec<String> {
        let args = serde_json::from_str::<serde_json::Value>(&call.arguments_json).ok();
        let path = args.as_ref().and_then(|value| {
            ["path", "file_path", "filePath", "cwd"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(|item| item.as_str()))
        });
        let resolved = path
            .map(std::path::PathBuf::from)
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    self.workspace_root.join(path)
                }
            })
            .unwrap_or_else(|| self.workspace_root.clone());
        // Paths outside the workspace are not a task-scope decision: each tool
        // owns the workspace-boundary check (typed "escapes the workspace
        // root" error) and the sandbox/EFF-001 external grant flow. Gating
        // them here would pre-empt both with an opaque assurance denial.
        let resolved = if rightkit_agent_coding_tools::read_file::path_is_within(
            &resolved,
            &self.workspace_root,
        ) {
            resolved
        } else {
            self.workspace_root.clone()
        };
        vec![resolved.to_string_lossy().into_owned()]
    }

    pub(in crate::engine) fn record_assurance_receipt(
        &mut self,
        call: &ToolCall,
        result: &rightkit_agent_coding_tools::ToolResult,
    ) -> Result<(), String> {
        let Some(broker) = &self.assurance_broker else {
            return Ok(());
        };
        let mut broker = broker
            .lock()
            .map_err(|_| "assurance broker lock poisoned".to_string())?;
        let operation = if matches!(call.name.as_str(), "shell" | "powershell") {
            let command = serde_json::from_str::<serde_json::Value>(&call.arguments_json)
                .ok()
                .and_then(|value| {
                    value
                        .get("command")
                        .and_then(|item| item.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            if command.contains("test") || command.contains("check") {
                "test"
            } else {
                "shell"
            }
        } else {
            "tool"
        };
        broker
            .issue_receipt(crate::assurance_broker::ReceiptRequest {
                turn_id: self.active_run_id.clone(),
                tool_call_id: call.id.clone(),
                tool_class: call.name.clone(),
                operation: operation.into(),
                scope: self.assurance_scope_for_call(call),
                input: call.arguments_json.as_bytes(),
                output: result.content.as_bytes(),
                exit_status: if result.is_error { 1 } else { 0 },
                source_generation_id: None,
                criterion_ids: Vec::new(),
            })
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// `<file_diagnostics>` rows for the code files `result` mutated, from the language server's
    /// `diagnostics` operation (the one `post_edit_diagnostics` and the verify fan-out use), under
    /// [`INLINE_DIAGNOSTICS_BUDGET`] in all. `None` when the setting or the host option is off, no file is a code
    /// file, no server is configured or reachable in time, or the server reports nothing: a
    /// missing server drops the block, it never adds a note. The end-of-turn gate still runs.
    #[cfg(feature = "host")]
    pub(in crate::engine) async fn inline_file_diagnostics(
        &self,
        result: &rightkit_agent_coding_tools::ToolResult,
    ) -> Option<String> {
        use rightkit_agent_coding_tools::lsp_tool::{LspTool, DIAGNOSTICS_PREFIX};
        use rightkit_agent_coding_tools::Tool as _;
        if !self.auto_post_edit_diagnostics || !crate::host_options::inline_file_diagnostics() {
            return None;
        }
        let started = std::time::Instant::now();
        let root_uri = format!("file://{}/", self.workspace_root.display());
        let mut blocks = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for mutation in &result.mutations {
            if matches!(
                mutation.kind,
                rightkit_agent_coding_tools::WorkspaceMutationKind::Deleted
            ) || !crate::verify_fanout::verifiers::is_code_ext(&mutation.path)
                || !seen.insert(mutation.path.clone())
                || seen.len() > INLINE_DIAGNOSTICS_MAX_FILES
            {
                continue;
            }
            let remaining = INLINE_DIAGNOSTICS_BUDGET.checked_sub(started.elapsed())?;
            let lsp = LspTool;
            let args = serde_json::json!({
                "operation": "diagnostics",
                "path": mutation.path.to_string_lossy(),
            });
            let ctx = crate::verify_fanout::verifiers::with_lsp_process_launcher(
                rightkit_agent_coding_tools::ToolContext::new(
                    self.session_id.clone(),
                    self.workspace_root.clone(),
                    self.permission_mode,
                    self.workspace_trust,
                ),
                &lsp,
                &args,
            );
            let Ok(answer) = tokio::time::timeout(remaining, lsp.execute(args, &ctx)).await else {
                return None;
            };
            if answer.is_error {
                continue;
            }
            if let Some(body) = answer.content.strip_prefix(DIAGNOSTICS_PREFIX) {
                for line in body.lines().take(INLINE_DIAGNOSTICS_MAX_ROWS) {
                    blocks.push(line.replace(&root_uri, ""));
                }
            }
        }
        (!blocks.is_empty()).then(|| {
            format!(
                "<file_diagnostics>\n{}\n</file_diagnostics>",
                blocks.join("\n")
            )
        })
    }

    pub(in crate::engine) fn record_single_tool_result(
        &mut self,
        sid: &str,
        call: &ToolCall,
        pre_span_id: Option<&str>,
        mut result: rightkit_agent_coding_tools::ToolResult,
        turn_results: &mut std::collections::HashMap<
            String,
            rightkit_agent_coding_tools::ToolResult,
        >,
    ) {
        // The post-edit guard reads `turn_results` to derive typed
        // `WorkspaceMutation`s for the whole turn. Single-call dispatch must
        // contribute to the same map the concurrent path uses, so the
        // unbacked-mutation detector sees every call's outcome in one shape.
        // Also bump the engine's typed-mutation counter so the unbacked-victory
        // gate in `finalize_goal` sees the same evidence without re-parsing
        // `turn_results` (which is per-turn local state).
        if let Some(note) = self.file_collision_note(call, &result) {
            result.content = format!("{}\n\n{note}", result.content);
        }
        // A config file the edit left unparseable is reported in this very result, so the model can repair
        // it on its next step instead of waiting for the end-of-turn gate.
        if !result.is_error {
            for mutation in &result.mutations {
                if let Some(error) = crate::post_edit_diagnostics::config_parse_error(
                    &self.workspace_root,
                    &mutation.path,
                ) {
                    // A host that sets `config_error_predating_edit_not_reported` does not report
                    // an error that was already in the file before this edit: not this edit's fault.
                    if crate::host_options::config_error_predating_edit_not_reported()
                        && mutation
                            .previous_content
                            .as_deref()
                            .is_some_and(|previous| {
                                crate::post_edit_diagnostics::config_error_predates_edit(
                                    &mutation.path,
                                    previous,
                                    &error,
                                )
                            })
                    {
                        continue;
                    }
                    result.content = format!(
                        "{}\n\nThis file no longer parses: {}: {error}",
                        result.content,
                        mutation.path.display()
                    );
                }
            }
        }
        if !result.mutations.is_empty() {
            if let Err(error) = self.record_run_changes(call, &result) {
                // The edit already landed, so never hide it. Turn the result
                // into a visible failure and retain its typed mutations so the
                // post-edit guard can still inspect the affected paths.
                result.is_error = true;
                result.content = format!(
                    "{}\n\n[workspace-change capture failed: {error}; review is unavailable until this is repaired]",
                    result.content
                );
            }
        }
        if let Err(error) = self.record_assurance_receipt(call, &result) {
            result.is_error = true;
            result.content = format!(
                "{}\n\n[assurance receipt failed: {error}; completion is blocked until this is repaired]",
                result.content
            );
        }
        if !result.is_error {
            self.add_typed_mutations(result.mutations.len() as u32);
        }
        turn_results.insert(call.id.clone(), result.clone());
        self.observe_tool_result_provenance(&call.name);
        self.publish_hook(rightkit_hooks::agent_hooks::HookEvent::PostToolUse {
            session_id: self.session_id.clone(),
            tool: call.name.clone(),
            is_error: result.is_error,
        });
        self.record_tool_runtime_span(
            MiddlewarePhase::PostToolCall,
            if result.is_error {
                "error"
            } else {
                "completed"
            },
            call,
            pre_span_id,
            serde_json::json!({
                "isError": result.is_error,
                "contentBytes": result.content.len(),
            }),
        );

        let (mut summary, mut output, mut store_failed) =
            self.store_tool_output(&call.name, &result.content, result.media_type.as_deref());
        if !result.is_error {
            self.pending_view_images
                .extend(extract_view_images(&call.id, &result.content));
        }

        // Apply read-dedup cache for read_file tool.
        if call.name == "read_file"
            && !result.is_error
            && !self.internal_composition_call_ids.contains(&call.id)
        {
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.arguments_json) {
                if let Some(path) = args.get("path").and_then(|p| p.as_str()) {
                    if !self.read_cache.record(path, &result.content) {
                        // Identical file already sent earlier; elide to short reference.
                        summary = format!("[file '{path}' unchanged since earlier read this session — contents elided to save context]");
                        let (elided_summary, elided_output, elided_failed) = self
                            .store_tool_output(
                                "read_file",
                                &summary,
                                Some("text/plain; charset=utf-8"),
                            );
                        summary = elided_summary;
                        output = elided_output;
                        store_failed = elided_failed;
                    }
                }
            }
        }

        if self.internal_composition_call_ids.contains(&call.id) {
            if let Some(output) = output.as_mut() {
                output.kind = rightkit_agent_engine_protocol::TOOL_OUTPUT_INTERNAL_KIND.into();
            }
        }

        // A failed archive (safety limit or disk write) is a typed failure,
        // not a clean observation — flag it like the decay pass does.
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
            session_id: sid.to_string(),
            seq,
            created_at: now_iso(),
            result: result_msg,
        });

        // A lone todo_write also updates the task graph (the concurrent batch path does the same), so
        // the daemon's task list and a rebuilt session show the agent's plan.
        if call.name == "todo_write" && !is_error {
            let args: serde_json::Value =
                serde_json::from_str(&call.arguments_json).unwrap_or(serde_json::Value::Null);
            let tasks = rightkit_agent_coding_tools::todo_write::task_nodes_from_args(&args, "t");
            if !tasks.is_empty() {
                let summary = format!("{} todos", tasks.len());
                self.emit(|seq| ProtocolEvent::TaskGraphUpdated {
                    session_id: sid.to_string(),
                    seq,
                    created_at: now_iso(),
                    summary,
                    tasks,
                });
            }
        }
    }
}
