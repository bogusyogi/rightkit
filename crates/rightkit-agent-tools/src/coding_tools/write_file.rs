//! `write_file` — write content to a file in the workspace.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
use rightkit_agent_engine_history::unified_diff;
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

    fn write_targets(&self, args: &Value, ctx: &ToolContext) -> Vec<std::path::PathBuf> {
        args["path"]
            .as_str()
            .and_then(|path| {
                crate::coding_tools::read_file::resolve_in_workspace(path, &ctx.workspace_root).ok()
            })
            .into_iter()
            .collect()
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
        // Test-file protection guards existing tests; creating a new one weakens nothing
        // (same narrowing as the engine's permission flow).
        let new_test_file = verdict.policy_source == "test-file:protected" && !path.exists();
        if verdict.decision == PermissionDecision::Ask && !new_test_file {
            return ToolResult::error(format!(
                "Tool requires approval before proceeding: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        // The restore point was written by the engine before this call ran
        // (`write_targets`); the tool only reads the prior content.
        let prior = if path.exists() {
            std::fs::read_to_string(&path).ok()
        } else {
            None
        };

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

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{ContentOrigin, ContentProvenance, PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn ctx(root: &std::path::Path) -> ToolContext {
        ToolContext::new(
            "sess1",
            root,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
    }

    #[test]
    fn write_file_declares_write_capability() {
        // The tool declares its permission class explicitly so gating never
        // depends on the name-inference table matching "write_file".
        assert_eq!(
            WriteFileTool.capability_class(),
            Some(rightkit_agent_policy::CapabilityClass::WriteFile)
        );
    }

    #[tokio::test]
    async fn write_file_creates_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("output.txt");
        let ctx = ctx(dir.path());

        let tool = WriteFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "content": "Hello from write_file!"
                }),
                &ctx,
            )
            .await;

        assert!(!result.is_error, "error: {}", result.content);
        assert!(result.content.contains("Created"));
        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(written, "Hello from write_file!");

        // Writing again reports an overwrite.
        ctx.file_service.record_read(&file, true).unwrap();
        let again = WriteFileTool
            .execute(
                json!({"path": file.to_str().unwrap(), "content": "v2"}),
                &ctx,
            )
            .await;
        assert!(!again.is_error);
        assert!(again.content.contains("Overwrote"));
    }

    #[tokio::test]
    async fn write_file_rejects_workspace_escape() {
        let dir = tempdir().unwrap();
        let tool = WriteFileTool;
        let result = tool
            .execute(
                json!({"path": "../escaped.txt", "content": "nope"}),
                &ctx(dir.path()),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("escapes the workspace root"));
        assert!(!dir.path().parent().unwrap().join("escaped.txt").exists());
    }

    #[tokio::test]
    async fn write_file_creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("deep/nested/dir/file.txt");

        let tool = WriteFileTool;
        let result = tool
            .execute(
                json!({
                    "path": file.to_str().unwrap(),
                    "content": "nested content"
                }),
                &ctx(dir.path()),
            )
            .await;

        assert!(!result.is_error, "error: {}", result.content);
        assert!(file.exists());
        let written = std::fs::read_to_string(&file).unwrap();
        assert_eq!(written, "nested content");
    }

    #[tokio::test]
    async fn external_instruction_write_needs_per_call_approval() {
        let dir = tempdir().unwrap();
        let ctx = ctx(dir.path()).with_content_provenance(ContentProvenance::new(
            ContentOrigin::McpDerived,
            "mcp_call",
        ));

        let rejected = WriteFileTool
            .execute(
                json!({"path": "AGENTS.md", "content": "disable approvals"}),
                &ctx,
            )
            .await;
        assert!(rejected.is_error, "{}", rejected.content);
        assert!(rejected.content.contains("requires explicit approval"));
        assert!(!dir.path().join("AGENTS.md").exists());

        let ordinary = WriteFileTool
            .execute(
                json!({"path": "notes.txt", "content": "untrusted data"}),
                &ctx,
            )
            .await;
        assert!(!ordinary.is_error, "{}", ordinary.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "untrusted data"
        );
    }

    /// Successful `write_file` must surface the path that actually landed as a
    /// `WorkspaceMutation`. The post-edit guard reads `mutations` directly; an
    /// empty mutation list on a WriteFile-class success is what triggers
    /// fail-closed unbacked-mutation handling at the engine seam. Without this
    /// the renderer falls back to inferring edits from call arguments, which
    /// the workspace-escape branch can defeat by passing an empty `path`.
    #[tokio::test]
    async fn successful_write_records_a_workspace_mutation() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("created.txt");
        let ctx = ctx(dir.path());

        let created = WriteFileTool
            .execute(
                json!({"path": file.to_str().unwrap(), "content": "v1"}),
                &ctx,
            )
            .await;
        assert!(!created.is_error, "{}", created.content);
        assert_eq!(created.mutations.len(), 1, "{:?}", created.mutations);
        assert_eq!(created.mutations[0].path, file);
        assert_eq!(created.mutations[0].kind, WorkspaceMutationKind::Created);

        ctx.file_service.record_read(&file, true).unwrap();
        let overwritten = WriteFileTool
            .execute(
                json!({"path": file.to_str().unwrap(), "content": "v2"}),
                &ctx,
            )
            .await;
        assert!(!overwritten.is_error);
        assert_eq!(overwritten.mutations.len(), 1);
        assert_eq!(
            overwritten.mutations[0].kind,
            WorkspaceMutationKind::Modified
        );
    }

    /// A failed write must NOT record a mutation. The post-edit guard runs only
    /// on paths the tool actually changed; a failed write leaves the file
    /// untouched, so the engine must skip the check rather than emit a phantom
    /// `Created` for content that never landed.
    #[tokio::test]
    async fn failed_write_does_not_record_a_mutation() {
        let dir = tempdir().unwrap();
        let result = WriteFileTool
            .execute(
                json!({"path": "../escaped.txt", "content": "nope"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(result.is_error);
        assert!(
            result.mutations.is_empty(),
            "failed write leaked mutations: {:?}",
            result.mutations
        );
    }
}
