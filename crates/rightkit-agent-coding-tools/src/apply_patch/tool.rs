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
        if crate::tool_options::apply_patch_newer() {
            return "Apply a custom patch to files. Format: lines starting with '*** ' set the target file, '--- ' remove a line, '+++ ' add a line, and bare lines are context that must match. A Codex patch (`*** Begin Patch` with `*** Update File: path` / `*** Add File: path` headers, `@@` markers, and ` `/`-`/`+` prefixed lines) is also accepted.";
        }
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
        // A Codex envelope names its files differently; read the targets from its translation.
        let translated = if crate::tool_options::apply_patch_newer() {
            match translate_codex_envelope(patch) {
                Some(Ok(translated)) => Some(translated),
                // The call is refused, so no file is written and none needs a restore point.
                Some(Err(_)) => return Vec::new(),
                None => None,
            }
        } else {
            None
        };
        for line in parse_patch(translated.as_ref().map_or(patch, |t| t.native.as_str())) {
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

        let newer = crate::tool_options::apply_patch_newer();
        let discipline = crate::tool_options::edit_read_discipline();
        // A Codex `*** Begin Patch` envelope is translated to the native format first.
        let translated = if newer {
            match translate_codex_envelope(patch_str) {
                Some(Ok(native)) => Some(native),
                Some(Err(error)) => return ToolResult::error(error),
                None => None,
            }
        } else {
            None
        };
        let added_files: &[String] = translated.as_ref().map_or(&[], |t| &t.added_files);
        let patch_str = translated.as_ref().map_or(patch_str, |t| t.native.as_str());

        let patch_lines = parse_patch(patch_str);

        // Split into per-file hunks. With host option `apply_patch_newer`, fail closed on anything
        // that used to be dropped silently: body lines before the first header, a header with no
        // body, and Codex envelope markers such as `*** Begin Patch`, which would otherwise become
        // a bogus target path.
        let mut hunks: Vec<(PathBuf, Vec<PatchLine>, bool)> = Vec::new();
        let mut current_file: Option<(String, PathBuf)> = None;
        let mut current_lines: Vec<PatchLine> = Vec::new();

        for (index, line) in patch_lines.iter().enumerate() {
            match line {
                PatchLine::FilePath(path_str) => {
                    if newer {
                        let marker = path_str.trim();
                        if matches!(marker, "Begin Patch" | "End Patch" | "End of File")
                            || ["Update File:", "Add File:", "Delete File:", "Move to:"]
                                .iter()
                                .any(|prefix| marker.starts_with(prefix))
                        {
                            return ToolResult::error(format!(
                                "Patch line {}: '*** {marker}' is a Codex patch envelope marker, not a file path. This tool's format: '*** path' sets the file, '--- ' removes a line, '+++ ' adds a line, and bare lines are context.",
                                index + 1
                            ));
                        }
                    }
                    if let Some((name, file)) = current_file.take() {
                        if current_lines.is_empty() {
                            if newer {
                                return ToolResult::error(format!(
                                    "No patch lines for '{name}'. Give each '*** path' header at least one '--- ', '+++ ' or context line (use write_file to create an empty file)."
                                ));
                            }
                        } else {
                            let is_added = added_files.contains(&name);
                            hunks.push((file, std::mem::take(&mut current_lines), is_added));
                        }
                    }
                    current_file =
                        match crate::read_file::resolve_in_workspace(path_str, &ctx.workspace_root)
                        {
                            Ok(path) => Some((path_str.clone(), path)),
                            Err(error) => return ToolResult::error(error),
                        };
                }
                other => {
                    if current_file.is_some() {
                        current_lines.push(other.clone());
                    } else if newer
                        && !matches!(other, PatchLine::Context(text) if text.trim().is_empty())
                    {
                        return ToolResult::error(format!(
                            "Patch line {} appears before the first '*** path' header: {:?}. Start the patch with '*** path/to/file'.",
                            index + 1,
                            patch_str.lines().nth(index).unwrap_or_default()
                        ));
                    }
                }
            }
        }
        if let Some((name, file)) = current_file.take() {
            if current_lines.is_empty() {
                if newer {
                    return ToolResult::error(format!(
                        "No patch lines for '{name}'. Give each '*** path' header at least one '--- ', '+++ ' or context line (use write_file to create an empty file)."
                    ));
                }
            } else {
                let is_added = added_files.contains(&name);
                hunks.push((file, current_lines, is_added));
            }
        }

        if hunks.is_empty() {
            return ToolResult::error(
                "No file paths found in patch (lines must start with '*** ')",
            );
        }

        // Preflight every target before writing any hunk so a later sensitive
        // target cannot leave an earlier non-sensitive hunk partially applied.
        for (path, _, is_added) in &hunks {
            if *is_added && path.exists() {
                return ToolResult::error(format!(
                    "'{}' already exists; an '*** Add File:' section creates a new file. Use '*** Update File:' to change it, or write_file to replace it.",
                    path.display()
                ));
            }
            if newer {
                // The patch holds the whole file and edits copies of it in memory.
                if let Some(refusal) = crate::edit_helpers::oversize_file_refusal(path) {
                    return ToolResult::error(refusal);
                }
            }
            if let Err(error) = crate::durable_write::require_path_authorization(path, ctx) {
                return ToolResult::error(error);
            }
            for access in [
                crate::read_file::PathAccess::Write,
                crate::read_file::PathAccess::Read,
            ] {
                if let Err(error) = crate::read_file::enforce_path_deny(ctx, path, access) {
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
            /// A line of the patch only fit through the whitespace/punctuation tolerance.
            inexact: bool,
        }

        // Prepare every file completely before the FileService batch is
        // journaled or any ordinary workspace byte is mutated. Repeated hunks
        // for the same file stage against the prior staged result.
        let mut prepared: Vec<PreparedPatch> = Vec::new();
        for (path, lines, _) in &hunks {
            // Evaluate the canonical workspace-relative target, like the other edit tools, so a
            // symlink alias cannot hide a protected (e.g. test) file from the policy rules.
            let policy_path = crate::read_file::policy_target_path(path, &ctx.workspace_root);
            let verdict = engine.evaluate(
                CapabilityClass::WriteFile,
                ctx.permission_mode,
                ctx.workspace_trust,
                Some(&policy_path),
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
                if discipline {
                    existing.inexact |= !patch_old_lines_are_exact(&existing.modified, lines);
                }
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
            let intent = if existed && discipline {
                // The current revision is observed (before the content is read), not recorded as
                // a read the model never made.
                let revision = match ctx.file_service.observe_preimage(path) {
                    Ok(crate::file_service::ExpectedPreimage::Revision(revision)) => revision,
                    Ok(crate::file_service::ExpectedPreimage::Missing) => {
                        return ToolResult::error(format!(
                            "'{}' disappeared before the patch",
                            path.display()
                        ))
                    }
                    Err(error) => return ToolResult::error(error.to_string()),
                };
                crate::file_service::MutationIntent::update(path, revision)
            } else if existed {
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
            let inexact = discipline && existed && !patch_old_lines_are_exact(&original, lines);
            prepared.push(PreparedPatch {
                path: path.clone(),
                existed,
                original,
                modified,
                intent,
                inexact,
            });
        }

        // A patch whose lines only matched through the tolerance needs the whole file read first,
        // as an inexact `edit_file` does; the read must still be current.
        for item in prepared
            .iter_mut()
            .filter(|item| item.inexact && item.existed)
        {
            match ctx.file_service.require_read_receipt(&item.path) {
                Ok(revision) => {
                    item.intent = crate::file_service::MutationIntent::update(&item.path, revision)
                }
                Err(error) => {
                    return ToolResult::error(format!(
                        "Patch for '{}' matched only through whitespace or punctuation tolerance, so the file must be read first: {error}",
                        item.path.display()
                    ))
                }
            }
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
            crate::lsp_notify::document_mutated(&ctx.workspace_root, &item.path).await;
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
