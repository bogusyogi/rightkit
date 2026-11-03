//! `undo_edit` — restore a file to its state before the most recent edit.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_engine_history::{unified_diff, HistoryStore};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};

pub struct UndoEditTool;

#[async_trait::async_trait]
impl Tool for UndoEditTool {
    fn name(&self) -> &str {
        "undo_edit"
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::WriteFile)
    }

    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        crate::coding_tools::durable_write::path_arg_kind(args)
    }

    fn description(&self) -> &str {
        "Restore file to its snapshot before the most recent edit (edit_file/write_file/multi_edit). Fail: no edit history for this file, or (when target_receipt_id is given) a later conflicting effect has already landed on this path."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string", "description": "File to revert (relative to workspace or absolute)." },
                "target_receipt_id": {
                    "type": "integer",
                    "description": "Optional effect-receipt id (from a prior write_file/edit_file/multi_edit result) this undo is meant to unwind. When given, undo is rejected if a later mutation has touched this path since that receipt (ENG-18 bounded undo)."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required argument: path"),
        };

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

        // ENG-18: bounded undo — when the caller names the specific receipt
        // it wants unwound, refuse if some later mutation has already
        // touched this path. Without a target id (the common "just undo my
        // last edit" call) we skip this check: restoring the single most
        // recent snapshot can never itself skip over a later effect.
        if let Some(target_receipt_id) = args["target_receipt_id"].as_u64() {
            if crate::coding_tools::effect_receipt_store::has_conflicting_effect_after(
                &ctx.session_id,
                &path,
                target_receipt_id,
            ) {
                return ToolResult::error(format!(
                    "Refusing to undo '{}': a later mutation has already been applied to this \
                     path since receipt {target_receipt_id}. Re-read the file and use edit_file \
                     to make the change explicitly instead of undoing a stale receipt.",
                    path.display()
                ));
            }
        }

        let store = HistoryStore::new(&ctx.workspace_root, ".rightkit");
        match store.peek_latest(&path) {
            Err(e) => ToolResult::error(format!(
                "Failed to access history for '{}': {}",
                path.display(),
                e
            )),
            Ok(None) => ToolResult::error(format!(
                "No edit history for '{}'. Nothing to undo.",
                path.display()
            )),
            Ok(Some(snapshot)) => {
                let post_effect = match ctx.file_service.mutation_receipts() {
                    Ok(receipts) => receipts
                        .into_iter()
                        .rev()
                        .find(|receipt| receipt.path == path && receipt.destination.is_none()),
                    Err(error) => return ToolResult::error(error),
                };
                let expected = match post_effect.and_then(|receipt| receipt.after) {
                    Some(revision) => revision,
                    None => {
                        return ToolResult::error(format!(
                            "Refusing to undo '{}': no agent post-effect revision/receipt is available.",
                            path.display()
                        ));
                    }
                };

                let current_revision = match ctx.file_service.observe_preimage(&path) {
                    Ok(crate::coding_tools::file_service::ExpectedPreimage::Revision(revision)) => {
                        revision
                    }
                    Ok(crate::coding_tools::file_service::ExpectedPreimage::Missing) => {
                        return ToolResult::error(format!(
                            "Refusing to undo '{}': the agent post-effect file is missing or was externally recreated.",
                            path.display()
                        ));
                    }
                    Err(error) => return ToolResult::error(error.to_string()),
                };
                if current_revision != expected {
                    return ToolResult::error(format!(
                        "Refusing to undo '{}': file changed after the agent wrote it; newer bytes were preserved.",
                        path.display()
                    ));
                }

                let current = std::fs::read_to_string(&path).unwrap_or_default();
                let intent =
                    crate::coding_tools::file_service::MutationIntent::restore(&path, expected);
                match ctx
                    .file_service
                    .apply_byte_intent(intent, snapshot.content.as_bytes())
                {
                    Err(e) => {
                        ToolResult::error(format!("Failed to restore '{}': {}", path.display(), e))
                    }
                    Ok(_) => {
                        if let Err(error) = store.consume_snapshot(&snapshot) {
                            return ToolResult::error(format!(
                                "Restored '{}' but could not consume its history snapshot: {}",
                                path.display(),
                                error
                            ));
                        }
                        let diff = unified_diff(&current, &snapshot.content, path_str);
                        let mut msg = format!(
                            "Reverted '{}' to previous snapshot ({} bytes).",
                            path.display(),
                            snapshot.content.len()
                        );
                        if !diff.is_empty() {
                            msg.push_str(&format!("\n```diff\n{diff}```"));
                        }
                        crate::coding_tools::effect_receipt_store::record_effect(
                            &ctx.session_id,
                            vec![path.clone()],
                            diff,
                            crate::coding_tools::effect_receipt_store::EffectResult::Success,
                        );
                        crate::coding_tools::lsp_notify::document_mutated(
                            &ctx.workspace_root,
                            &path,
                        )
                        .await;
                        ToolResult::ok(msg)
                    }
                }
            }
        }
    }
}
