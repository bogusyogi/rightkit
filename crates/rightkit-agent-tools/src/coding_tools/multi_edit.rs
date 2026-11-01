//! `multi_edit` — apply several `edit_file`-style edits to one file atomically
//! (Claude-Code `MultiEdit` parity).
//!
//! Edits are applied in order to an in-memory copy; each `old_string` must match
//! the *current* (post-previous-edits) content, be unique unless `replace_all`,
//! and the whole batch is written back only if every edit succeeds. Either the
//! file ends up with all edits applied, or it is left untouched with an error —
//! never a partial write.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::edit_helpers::apply_single_edit;
use crate::coding_tools::tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
use rightkit_agent_engine_history::unified_diff;
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};

pub struct MultiEditTool;

#[async_trait::async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "multi_edit"
    }
    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::WriteFile)
    }
    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        crate::coding_tools::durable_write::path_arg_kind(args)
    }

    fn description(&self) -> &str {
        "Apply multiple edits atomically to one file. Edits array: each {old_string, new_string, replace_all?} runs on prior result. Hashline-prefixed old_string values from read_file are validated against their line anchors. All succeed or none written."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path", "edits"],
            "properties": {
                "path": { "type": "string", "description": "File to edit (relative to workspace or absolute)" },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["old_string", "new_string"],
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" },
                            "replace_all": { "type": "boolean", "description": "Replace every occurrence (default false)" }
                        }
                    }
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
        let edits = match args["edits"].as_array() {
            Some(e) if !e.is_empty() => e,
            _ => return ToolResult::error("`edits` must be a non-empty array."),
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

        let original = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error(format!(
                    "Cannot read '{}': {} (use write_file to create a new file)",
                    path.display(),
                    e
                ))
            }
        };

        let mut content = original.clone();
        let mut hashline_validated = false;
        for (i, edit) in edits.iter().enumerate() {
            let old_string = edit["old_string"].as_str().unwrap_or("");
            let new_string = edit["new_string"].as_str().unwrap_or("");
            let replace_all = edit["replace_all"].as_bool().unwrap_or(false);

            if old_string.is_empty() {
                return ToolResult::error(format!("edits[{i}].old_string is empty."));
            }
            if old_string == new_string {
                return ToolResult::error(format!(
                    "edits[{i}].old_string and new_string are identical."
                ));
            }
            content = match crate::coding_tools::read_file::resolve_hashline_block(
                old_string, &content,
            ) {
                Ok(Some(anchor)) => {
                    hashline_validated = true;
                    crate::coding_tools::read_file::replace_hashline_block(
                        &content, &anchor, new_string,
                    )
                }
                Ok(None) => match apply_single_edit(
                    &content,
                    old_string,
                    new_string,
                    replace_all,
                    &format!("edits[{i}] (after applying {i} earlier edit(s)); no changes written"),
                ) {
                    Ok(c) => c,
                    Err(e) => return ToolResult::error(e),
                },
                Err(e) => return ToolResult::error(format!("edits[{i}]: {e}")),
            };
        }

        // The restore point was written by the engine before this call ran
        // (`write_targets`, sessions rule 41).

        match ctx.file_service.write_with_receipt_intent(
            crate::coding_tools::file_service::MutationIntentKind::Update,
            &path,
            content.as_bytes(),
            hashline_validated,
        ) {
            Ok(_) => {
                let diff = unified_diff(&original, &content, path_str);
                let mut msg = format!(
                    "Edited '{}': applied {} edit(s) ({} → {} bytes).",
                    path.display(),
                    edits.len(),
                    original.len(),
                    content.len()
                );
                if !diff.is_empty() {
                    msg.push_str(&format!("\n```diff\n{diff}```"));
                }
                let mutation = WorkspaceMutation {
                    path: path.clone(),
                    kind: WorkspaceMutationKind::Modified,
                    previous_content: Some(original),
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
                    format!("multi_edit failed: {e}"),
                    crate::coding_tools::effect_receipt_store::EffectResult::Error(e.to_string()),
                );
                ToolResult::error(format!("Failed to write '{}': {}", path.display(), e))
            }
        }
    }

    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Option<String> {
        let path_str = args["path"].as_str()?;
        let edits = args["edits"].as_array().filter(|e| !e.is_empty())?;
        let path =
            crate::coding_tools::read_file::resolve_in_workspace(path_str, &ctx.workspace_root)
                .ok()?;
        let original = std::fs::read_to_string(&path).ok()?;
        let mut content = original.clone();
        for edit in edits {
            let old_string = edit["old_string"].as_str().unwrap_or("");
            let new_string = edit["new_string"].as_str().unwrap_or("");
            let replace_all = edit["replace_all"].as_bool().unwrap_or(false);
            content =
                match crate::coding_tools::read_file::resolve_hashline_block(old_string, &content)
                    .ok()?
                {
                    Some(anchor) => crate::coding_tools::read_file::replace_hashline_block(
                        &content, &anchor, new_string,
                    ),
                    None => {
                        apply_single_edit(&content, old_string, new_string, replace_all, "preview")
                            .ok()?
                    }
                };
        }
        let diff = unified_diff(&original, &content, path_str);
        (!diff.is_empty()).then_some(diff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::read_file::ReadFileTool;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::tempdir;

    fn ctx(root: &std::path::Path) -> ToolContext {
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
    }

    fn ctx_with_read(root: &std::path::Path, file: &std::path::Path) -> ToolContext {
        let ctx = ctx(root);
        ctx.file_service.record_read(file, true).unwrap();
        ctx
    }

    async fn read_hashline(ctx: &ToolContext, file: &std::path::Path, line: usize) -> String {
        let result = ReadFileTool
            .execute(json!({"path": file.to_str().unwrap()}), ctx)
            .await;
        assert!(!result.is_error, "{}", result.content);
        result
            .content
            .lines()
            .nth(line - 1)
            .expect("hashline exists")
            .to_string()
    }

    #[tokio::test]
    async fn applies_sequential_edits() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "let a = 1;\nlet b = 2;\n").unwrap();
        let ctx = ctx_with_read(dir.path(), &f);
        let r = MultiEditTool
            .execute(
                json!({"path": f.to_str().unwrap(), "edits": [
                    {"old_string": "let a = 1;", "new_string": "let a = 10;"},
                    {"old_string": "let b = 2;", "new_string": "let b = 20;"}
                ]}),
                &ctx,
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "let a = 10;\nlet b = 20;\n"
        );
    }

    #[tokio::test]
    async fn atomic_on_failure() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "keep\n").unwrap();
        let r = MultiEditTool
            .execute(
                json!({"path": f.to_str().unwrap(), "edits": [
                    {"old_string": "keep", "new_string": "changed"},
                    {"old_string": "does-not-exist", "new_string": "x"}
                ]}),
                &ctx(dir.path()),
            )
            .await;
        assert!(r.is_error);
        // First edit must NOT have been written.
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "keep\n");
    }

    #[tokio::test]
    async fn second_edit_can_match_first_result() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "alpha\n").unwrap();
        let ctx = ctx_with_read(dir.path(), &f);
        let r = MultiEditTool
            .execute(
                json!({"path": f.to_str().unwrap(), "edits": [
                    {"old_string": "alpha", "new_string": "beta"},
                    {"old_string": "beta", "new_string": "gamma"}
                ]}),
                &ctx,
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "gamma\n");
    }

    #[tokio::test]
    async fn hashline_anchor_edits_matching_duplicate_line() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("dupes.txt");
        std::fs::write(&f, "same\nsame\n").unwrap();
        let ctx = ctx(dir.path());
        let second_line_anchor = read_hashline(&ctx, &f, 2).await;

        let r = MultiEditTool
            .execute(
                json!({"path": f.to_str().unwrap(), "edits": [
                    {"old_string": second_line_anchor, "new_string": "target"}
                ]}),
                &ctx,
            )
            .await;

        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "same\ntarget\n");
    }

    #[tokio::test]
    async fn hashline_anchor_stale_errors_with_fresh_anchors_without_writing() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("stale.txt");
        std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
        let ctx = ctx(dir.path());
        let stale_anchor = read_hashline(&ctx, &f, 2).await;
        std::fs::write(&f, "alpha\nchanged\ngamma\nbeta\n").unwrap();

        let r = MultiEditTool
            .execute(
                json!({"path": f.to_str().unwrap(), "edits": [
                    {"old_string": stale_anchor, "new_string": "BETA"}
                ]}),
                &ctx,
            )
            .await;

        assert!(r.is_error);
        assert!(r.content.contains("Stale hashline anchor"), "{}", r.content);
        assert!(r.content.contains("Fresh anchors"), "{}", r.content);
        assert!(
            r.content
                .contains("Nearest-content suggestion: lines 4-4 (similarity "),
            "{}",
            r.content
        );
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "alpha\nchanged\ngamma\nbeta\n"
        );
    }

    #[tokio::test]
    async fn rejects_empty_edits() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x\n").unwrap();
        let r = MultiEditTool
            .execute(
                json!({"path": f.to_str().unwrap(), "edits": []}),
                &ctx(dir.path()),
            )
            .await;
        assert!(r.is_error);
    }

    /// Atomic batch succeeds → records one `Modified` mutation, even when the
    /// file existed before. The post-edit guard uses this single mutation to
    /// re-parse the file once and surface every landed edit at the same
    /// severity.
    #[tokio::test]
    async fn successful_multi_edit_records_a_modified_mutation() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("multi.txt");
        std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
        let ctx = ctx_with_read(dir.path(), &f);
        let r = MultiEditTool
            .execute(
                json!({"path": f.to_str().unwrap(), "edits": [
                    {"old_string": "alpha", "new_string": "ALPHA"},
                    {"old_string": "gamma", "new_string": "GAMMA"},
                ]}),
                &ctx,
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(r.mutations.len(), 1);
        assert_eq!(r.mutations[0].path, f);
        assert_eq!(r.mutations[0].kind, WorkspaceMutationKind::Modified);
    }

    /// An aborted atomic batch never reaches disk: no mutations, even though
    /// the tool did read the file. Symmetric with the single-edit failure
    /// shape — the guard has no path to act on.
    #[tokio::test]
    async fn aborted_multi_edit_does_not_record_a_mutation() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("multi.txt");
        std::fs::write(&f, "alpha\nbeta\n").unwrap();
        let r = MultiEditTool
            .execute(
                json!({"path": f.to_str().unwrap(), "edits": [
                    {"old_string": "alpha", "new_string": "ALPHA"},
                    {"old_string": "missing", "new_string": "X"},
                ]}),
                &ctx(dir.path()),
            )
            .await;
        assert!(r.is_error);
        assert!(r.mutations.is_empty(), "{:?}", r.mutations);
    }
}
