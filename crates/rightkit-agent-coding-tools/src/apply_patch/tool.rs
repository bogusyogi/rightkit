#[async_trait::async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }
    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::WriteFile)
    }
    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        crate::durable_write::patch_arg_kind(args)
    }

    fn description(&self) -> &str {
        "Apply a custom patch to files. Format: lines starting with '*** ' set the target file, '--- ' remove a line, '+++ ' add a line, and bare lines are context that must match."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["patch"],
            "properties": {
                "patch": {
                    "type": "string",
                    "description": "The patch to apply in the custom format"
                }
            }
        })
    }

    fn write_targets(&self, args: &Value, ctx: &ToolContext) -> Vec<PathBuf> {
        let Some(patch) = args["patch"].as_str() else {
            return Vec::new();
        };
        let mut targets: Vec<PathBuf> = Vec::new();
        for line in parse_patch(patch) {
            if let PatchLine::FilePath(path) = line {
                if let Ok(resolved) =
                    crate::read_file::resolve_in_workspace(&path, &ctx.workspace_root)
                {
                    if !targets.contains(&resolved) {
                        targets.push(resolved);
                    }
                }
            }
        }
        targets
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let patch_str = match args["patch"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required argument: patch"),
        };

        let patch_lines = parse_patch(patch_str);

        // Split into per-file hunks
        let mut hunks: Vec<(PathBuf, Vec<PatchLine>)> = Vec::new();
        let mut current_file: Option<PathBuf> = None;
        let mut current_lines: Vec<PatchLine> = Vec::new();

        for line in &patch_lines {
            match line {
                PatchLine::FilePath(path_str) => {
                    if let Some(file) = current_file.take() {
                        if !current_lines.is_empty() {
                            hunks.push((file, std::mem::take(&mut current_lines)));
                        }
                    }
                    current_file = match crate::read_file::resolve_in_workspace(
                        path_str,
                        &ctx.workspace_root,
                    ) {
                        Ok(path) => Some(path),
                        Err(error) => return ToolResult::error(error),
                    };
                }
                other => {
                    if current_file.is_some() {
                        current_lines.push(other.clone());
                    }
                }
            }
        }
        if let Some(file) = current_file.take() {
            if !current_lines.is_empty() {
                hunks.push((file, current_lines));
            }
        }

        if hunks.is_empty() {
            return ToolResult::error(
                "No file paths found in patch (lines must start with '*** ')",
            );
        }

        // Preflight every target before writing any hunk so a later sensitive
        // target cannot leave an earlier non-sensitive hunk partially applied.
        for (path, _) in &hunks {
            if let Err(error) =
                crate::durable_write::require_path_authorization(path, ctx)
            {
                return ToolResult::error(error);
            }
            for access in [
                crate::read_file::PathAccess::Write,
                crate::read_file::PathAccess::Read,
            ] {
                if let Err(error) =
                    crate::read_file::enforce_path_deny(ctx, path, access)
                {
                    return ToolResult::error(error);
                }
            }
        }

        let engine = PermissionEngine::new();

        struct PreparedPatch {
            path: PathBuf,
            existed: bool,
            original: String,
            modified: String,
            intent: crate::file_service::MutationIntent,
        }

        // Prepare every file completely before the FileService batch is
        // journaled or any ordinary workspace byte is mutated. Repeated hunks
        // for the same file stage against the prior staged result.
        let mut prepared: Vec<PreparedPatch> = Vec::new();
        for (path, lines) in &hunks {
            let path_str_for_check = path.to_string_lossy();
            let verdict = engine.evaluate(
                CapabilityClass::WriteFile,
                ctx.permission_mode,
                ctx.workspace_trust,
                Some(&path_str_for_check),
                false,
            );
            if verdict.decision == PermissionDecision::Deny {
                return ToolResult::error(format!(
                    "Permission denied for '{}': {} (policy: {})",
                    path.display(),
                    verdict.reason,
                    verdict.policy_source
                ));
            }
            if verdict.decision == PermissionDecision::Ask {
                return ToolResult::error(format!(
                    "Tool requires approval for '{}': {}",
                    path.display(),
                    verdict.reason
                ));
            }

            if let Some(existing) = prepared.iter_mut().find(|item| item.path == *path) {
                existing.modified = match apply_to_content(&existing.modified, lines) {
                    Ok(modified) => modified,
                    Err(error) => {
                        return ToolResult::error(format!(
                            "Failed to prepare patch for '{}': {error}",
                            path.display()
                        ));
                    }
                };
                continue;
            }

            let existed = path.exists();
            let intent = if existed {
                let receipt = match ctx.file_service.record_read(path, true) {
                    Ok(receipt) => receipt,
                    Err(error) => return ToolResult::error(error),
                };
                crate::file_service::MutationIntent::update(path, receipt.revision)
            } else {
                crate::file_service::MutationIntent::create(path)
            };
            let original = match std::fs::read_to_string(path) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && !existed => {
                    String::new()
                }
                Err(error) => {
                    return ToolResult::error(format!(
                        "Failed to read '{}': {error}",
                        path.display()
                    ));
                }
            };
            let modified = match apply_to_content(&original, lines) {
                Ok(modified) => modified,
                Err(error) => {
                    return ToolResult::error(format!(
                        "Failed to prepare patch for '{}': {error}",
                        path.display()
                    ));
                }
            };
            prepared.push(PreparedPatch {
                path: path.clone(),
                existed,
                original,
                modified,
                intent,
            });
        }

        let entries = prepared
            .iter()
            .map(|item| {
                crate::file_service::MutationBatchEntry::write(
                    item.intent.clone(),
                    item.modified.as_bytes().to_vec(),
                )
            })
            .collect::<Vec<_>>();

        let (batch_receipt, batch_error) = match ctx
            .file_service
            .write_batch_with_receipts(&ctx.workspace_root, entries)
        {
            Ok(receipt) => (receipt, None),
            Err(failure) => {
                let message = failure.to_string();
                (failure.receipt, Some(message))
            }
        };

        let mut applied = Vec::new();
        let mut mutations = Vec::new();
        for entry in &batch_receipt.entries {
            if entry.outcome != crate::file_service::BatchEntryOutcome::Committed {
                continue;
            }
            let item = &prepared[entry.index];
            crate::lsp_notify::document_mutated(&ctx.workspace_root, &item.path)
                .await;
            applied.push(item.path.display().to_string());
            mutations.push(WorkspaceMutation {
                path: item.path.clone(),
                kind: if item.existed {
                    WorkspaceMutationKind::Modified
                } else {
                    WorkspaceMutationKind::Created
                },
                previous_content: item.existed.then(|| item.original.clone()),
            });
        }

        if let Some(error) = batch_error {
            let exact = serde_json::to_string(&batch_receipt)
                .unwrap_or_else(|_| "<receipt serialization failed>".to_string());
            return ToolResult::error(format!(
                "Patch batch did not complete: {error}. Exact mutation receipt: {exact}"
            ))
            .with_mutations(mutations);
        }

        ToolResult::ok(format!("Patch applied to: {}", applied.join(", ")))
            .with_mutations(mutations)
    }
}
