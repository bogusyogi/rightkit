//! `write_file` — write content to a file in the workspace.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
use rightkit_agent_engine_history::{unified_diff, HistoryStore};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};

pub struct WriteFileTool;

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::WriteFile)
    }
    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        crate::coding_tools::durable_write::path_arg_kind(args)
    }

    fn description(&self) -> &str {
        "Write content to a file (creates or overwrites). Auto-creates parent directories. Fail if path escapes workspace or permission denied."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (relative to workspace root or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path_str = match args["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("Missing required argument: path"),
        };
        let content = match args["content"].as_str() {
            Some(c) => c,
            None => return ToolResult::error("Missing required argument: content"),
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
        // Mirror read_file: gate the deny-glob backstop on the resolved, `..`-free
        // workspace-relative path, not the raw caller string (a raw `sub/../.env`
        // is boundary-allowed by resolve_in_workspace but its literal `..` defeats
        // the `**/.env` / `.git/**` write-deny glob).
        let policy_path =
            crate::coding_tools::read_file::policy_target_path(&path, &ctx.workspace_root);

        // Permission check
        let engine = PermissionEngine::new();
        let verdict = engine.evaluate(
            CapabilityClass::WriteFile,
            ctx.permission_mode,
            ctx.workspace_trust,
            Some(&policy_path),
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

        // Snapshot prior content if the file exists (skip for new files).
        let prior = if path.exists() {
            std::fs::read_to_string(&path).ok()
        } else {
            None
        };
        if let Some(ref prior_content) = prior {
            let store = HistoryStore::new(&ctx.workspace_root, ".rightkit");
            let _ = store.snapshot(&path, prior_content);
        }

        // ENG-17: preserve the file's existing line-ending convention on a
        // full overwrite rather than letting caller-supplied content
        // silently flip CRLF <-> LF for the whole file.
        let content_to_write = match prior {
            Some(ref old) => {
                crate::coding_tools::observed_version::preserve_line_endings(old, content)
            }
            None => content.to_string(),
        };

        // Write file under its receipt and per-path mutation lock.
        let existed = prior.is_some();
        match ctx.file_service.write_with_receipt_intent(
            if existed {
                crate::coding_tools::file_service::MutationIntentKind::Replace
            } else {
                crate::coding_tools::file_service::MutationIntentKind::Create
            },
            &path,
            content_to_write.as_bytes(),
            false,
        ) {
            Ok(_) => {
                let diff = if let Some(ref old) = prior {
                    unified_diff(old, &content_to_write, path_str)
                } else {
                    String::new()
                };
                let mut msg = format!(
                    "{} '{}' ({} bytes, {} lines).",
                    if existed { "Overwrote" } else { "Created" },
                    path.display(),
                    content_to_write.len(),
                    content_to_write.lines().count()
                );
                if !diff.is_empty() {
                    msg.push_str(&format!("\n```diff\n{diff}```"));
                }
                let mutation = WorkspaceMutation {
                    path: path.clone(),
                    kind: if existed {
                        WorkspaceMutationKind::Modified
                    } else {
                        WorkspaceMutationKind::Created
                    },
                    previous_content: prior,
                };
                crate::coding_tools::effect_receipt_store::record_effect(
                    &ctx.session_id,
                    vec![path.clone()],
                    diff.clone(),
                    crate::coding_tools::effect_receipt_store::EffectResult::Success,
                );
                crate::coding_tools::lsp_notify::document_mutated(&ctx.workspace_root, &path).await;
                ToolResult::ok(msg).with_mutations(vec![mutation])
            }
            Err(e) => {
                crate::coding_tools::effect_receipt_store::record_effect(
                    &ctx.session_id,
                    vec![path.clone()],
                    format!("write_file failed: {e}"),
                    crate::coding_tools::effect_receipt_store::EffectResult::Error(e.to_string()),
                );
                ToolResult::error(format!("Failed to write file '{}': {}", path.display(), e))
            }
        }
    }

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Option<String> {
        let path_str = args["path"].as_str()?;
        let content = args["content"].as_str()?;
        let path =
            crate::coding_tools::read_file::resolve_in_workspace(path_str, &ctx.workspace_root)
                .ok()?;
        match std::fs::read_to_string(&path) {
            Ok(old) => {
                let diff = unified_diff(&old, content, path_str);
                (!diff.is_empty()).then_some(diff)
            }
            // New file: preview the whole content as additions.
            Err(_) => {
                let diff = unified_diff("", content, path_str);
                Some(diff)
            }
        }
    }
}
