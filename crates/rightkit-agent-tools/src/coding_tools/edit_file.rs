//! `edit_file` — semantic find-and-replace edit (Claude-Code `FileEdit` parity).
//!
//! Replaces an exact `old_string` with `new_string` in a file. Unlike a blind
//! overwrite (`write_file`) or a positional patch (`apply_patch`), this is the
//! reliable agent edit primitive: the match must be **unique** (or `replace_all`
//! must be set), so an edit can never silently land in the wrong place, and the
//! tool fails loudly with an actionable message when the target is missing or
//! ambiguous. Line endings and surrounding bytes are preserved verbatim.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::edit_helpers::apply_single_edit;
use crate::coding_tools::tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
use rightkit_agent_engine_history::{unified_diff, HistoryStore};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};

pub struct EditFileTool;

#[async_trait::async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::WriteFile)
    }
    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        crate::coding_tools::durable_write::path_arg_kind(args)
    }

    fn description(&self) -> &str {
        "Replace exact substring in a file. Hashline-prefixed old_string values from read_file are validated against their original line anchors and stale anchors are rejected with fresh anchors. Plain old_string must be unique unless replace_all=true."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "old_string", "new_string"],
            "properties": {
                "path": { "type": "string", "description": "File to edit (relative to workspace or absolute)" },
                "old_string": { "type": "string", "description": "Exact text to replace (unique unless replace_all)" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required argument: path"),
        };
        let old_string = match args["old_string"].as_str() {
            Some(s) => s,
            None => return ToolResult::error("Missing required argument: old_string"),
        };
        let new_string = match args["new_string"].as_str() {
            Some(s) => s,
            None => return ToolResult::error("Missing required argument: new_string"),
        };
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);

        if old_string.is_empty() {
            return ToolResult::error(
                "old_string is empty. To create a new file use write_file; edit_file changes existing text.",
            );
        }
        if old_string == new_string {
            return ToolResult::error(
                "old_string and new_string are identical — nothing to change.",
            );
        }

        let path = match crate::coding_tools::read_file::resolve_in_workspace(
            path_str,
            &ctx.workspace_root,
        ) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        if let Err(error) =
            crate::coding_tools::durable_write::require_path_authorization(&path, ctx)
        {
            return ToolResult::error(error);
        }
        if let Err(error) = crate::coding_tools::read_file::enforce_path_deny(
            ctx,
            &path,
            crate::coding_tools::read_file::PathAccess::Write,
        ) {
            return ToolResult::error(error);
        }
        if let Err(error) = crate::coding_tools::read_file::enforce_path_deny(
            ctx,
            &path,
            crate::coding_tools::read_file::PathAccess::Read,
        ) {
            return ToolResult::error(error);
        }

        // Permission check (same as write_file: this mutates the workspace).
        let engine = PermissionEngine::new();
        let verdict = engine.evaluate(
            CapabilityClass::WriteFile,
            ctx.permission_mode,
            ctx.workspace_trust,
            Some(path_str),
            false,
        );
        if verdict.decision == PermissionDecision::Deny {
            return ToolResult::error(format!(
                "Permission denied: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }
        if verdict.decision == PermissionDecision::Ask {
            return ToolResult::error(format!(
                "Tool requires approval before proceeding: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error(format!(
                    "Cannot read '{}': {} (use write_file to create a new file)",
                    path.display(),
                    e
                ))
            }
        };

        let (updated, matches, hashline_validated) =
            match crate::coding_tools::read_file::resolve_hashline_block(old_string, &content) {
                Ok(Some(anchor)) => (
                    crate::coding_tools::read_file::replace_hashline_block(
                        &content, &anchor, new_string,
                    ),
                    1,
                    true,
                ),
                Ok(None) => {
                    let updated = match apply_single_edit(
                        &content,
                        old_string,
                        new_string,
                        replace_all,
                        &format!("'{}'", path.display()),
                    ) {
                        Ok(u) => u,
                        Err(e) => return ToolResult::error(e),
                    };
                    let matches = content.matches(old_string).count();
                    (updated, matches, false)
                }
                Err(e) => return ToolResult::error(e),
            };

        // Snapshot prior content before writing.
        let store = HistoryStore::new(&ctx.workspace_root, ".rightkit");
        let _ = store.snapshot(&path, &content);

        match ctx.file_service.write_with_receipt_intent(
            crate::coding_tools::file_service::MutationIntentKind::Update,
            &path,
            updated.as_bytes(),
            hashline_validated,
        ) {
            Ok(_) => {
                let diff = unified_diff(&content, &updated, path_str);
                let mut msg = format!(
                    "Edited '{}': replaced {} occurrence{} ({} → {} bytes).",
                    path.display(),
                    if replace_all { matches } else { 1 },
                    if replace_all && matches != 1 { "s" } else { "" },
                    content.len(),
                    updated.len()
                );
                if !diff.is_empty() {
                    msg.push_str(&format!("\n```diff\n{diff}```"));
                }
                let mutation = WorkspaceMutation {
                    path: path.clone(),
                    kind: WorkspaceMutationKind::Modified,
                    previous_content: Some(content),
                };
                crate::coding_tools::effect_receipt_store::record_effect(
                    &ctx.session_id,
                    vec![path.clone()],
                    diff,
                    crate::coding_tools::effect_receipt_store::EffectResult::Success,
                );
                crate::coding_tools::lsp_notify::document_mutated(&ctx.workspace_root, &path).await;
                ToolResult::ok(msg).with_mutations(vec![mutation])
            }
            Err(e) => {
                crate::coding_tools::effect_receipt_store::record_effect(
                    &ctx.session_id,
                    vec![path.clone()],
                    format!("edit_file failed: {e}"),
                    crate::coding_tools::effect_receipt_store::EffectResult::Error(e.to_string()),
                );
                ToolResult::error(format!("Failed to write '{}': {}", path.display(), e))
            }
        }
    }

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Option<String> {
        let path_str = args["path"].as_str()?;
        let old_string_raw = args["old_string"].as_str()?;
        let new_string = args["new_string"].as_str()?;
        let replace_all = args["replace_all"].as_bool().unwrap_or(false);
        if old_string_raw.is_empty() || old_string_raw == new_string {
            return None;
        }
        let path =
            crate::coding_tools::read_file::resolve_in_workspace(path_str, &ctx.workspace_root)
                .ok()?;
        let content = std::fs::read_to_string(&path).ok()?;

        let updated =
            match crate::coding_tools::read_file::resolve_hashline_block(old_string_raw, &content)
                .ok()?
            {
                Some(anchor) => crate::coding_tools::read_file::replace_hashline_block(
                    &content, &anchor, new_string,
                ),
                None => {
                    apply_single_edit(&content, old_string_raw, new_string, replace_all, path_str)
                        .ok()?
                }
            };
        let diff = unified_diff(&content, &updated, path_str);
        (!diff.is_empty()).then_some(diff)
    }
}
