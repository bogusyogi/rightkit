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
use rightkit_agent_engine_history::unified_diff;
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

        // The restore point was written by the engine before this call ran
        // (`write_targets`, sessions rule 41).

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
    async fn edits_a_unique_match() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.rs");
        std::fs::write(&f, "fn main() {\n    let x = 1;\n}\n").unwrap();
        let ctx = ctx_with_read(dir.path(), &f);
        let r = EditFileTool
            .execute(
                json!({"path": f.to_str().unwrap(), "old_string": "let x = 1;", "new_string": "let x = 42;"}),
                &ctx,
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "fn main() {\n    let x = 42;\n}\n"
        );
    }

    /// The tolerant matching ladder is part of the live tool contract, not
    /// merely a helper contract: punctuation drift must still produce the
    /// same audited workspace mutation through `EditFileTool::execute`.
    #[tokio::test]
    async fn live_edit_tool_applies_unique_punctuation_drift() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "status: \u{201c}ready\u{201d}\n").unwrap();
        let ctx = ctx_with_read(dir.path(), &f);
        let r = EditFileTool
            .execute(
                json!({
                    "path": f.to_str().unwrap(),
                    "old_string": "status: \"ready\"",
                    "new_string": "status: \"done\""
                }),
                &ctx,
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "status: \"done\"\n");
    }

    #[tokio::test]
    async fn ambiguous_match_is_rejected() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x\nx\n").unwrap();
        let r = EditFileTool
            .execute(
                json!({"path": f.to_str().unwrap(), "old_string": "x", "new_string": "y"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("not unique"));
        // File untouched.
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "x\nx\n");
    }

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "x\nx\nx\n").unwrap();
        let ctx = ctx_with_read(dir.path(), &f);
        let r = EditFileTool
            .execute(
                json!({"path": f.to_str().unwrap(), "old_string": "x", "new_string": "y", "replace_all": true}),
                &ctx,
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "y\ny\ny\n");
    }

    #[tokio::test]
    async fn hashline_anchor_edits_matching_duplicate_line() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("dupes.txt");
        std::fs::write(&f, "same\nsame\n").unwrap();
        let ctx = ctx(dir.path());
        let second_line_anchor = read_hashline(&ctx, &f, 2).await;

        let r = EditFileTool
            .execute(
                json!({"path": f.to_str().unwrap(), "old_string": second_line_anchor, "new_string": "target"}),
                &ctx,
            )
            .await;

        assert!(!r.is_error, "{}", r.content);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "same\ntarget\n");
    }

    #[tokio::test]
    async fn hashline_anchor_stale_errors_with_fresh_anchors_without_relocating() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("stale.txt");
        std::fs::write(&f, "alpha\nbeta\ngamma\n").unwrap();
        let ctx = ctx(dir.path());
        let stale_anchor = read_hashline(&ctx, &f, 2).await;
        std::fs::write(&f, "alpha\nchanged\ngamma\nbeta\n").unwrap();

        let r = EditFileTool
            .execute(
                json!({"path": f.to_str().unwrap(), "old_string": stale_anchor, "new_string": "BETA"}),
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
    async fn missing_string_errors_clearly() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "hello\n").unwrap();
        let r = EditFileTool
            .execute(
                json!({"path": f.to_str().unwrap(), "old_string": "nope", "new_string": "x"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("not found"));
    }

    #[tokio::test]
    async fn missing_file_suggests_write_file() {
        let dir = tempdir().unwrap();
        let r = EditFileTool
            .execute(
                json!({"path": "ghost.txt", "old_string": "a", "new_string": "b"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("write_file"));
    }

    #[tokio::test]
    async fn rejects_workspace_escape() {
        let dir = tempdir().unwrap();
        let r = EditFileTool
            .execute(
                json!({"path": "../x.txt", "old_string": "a", "new_string": "b"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(r.is_error);
        assert!(r.content.contains("escapes the workspace"));
    }

    /// A successful semantic edit must emit one `Modified` mutation. The
    /// pre-existing file existed before the call, so `Created` is wrong even
    /// though that is what `write_file` reports for new files. This is the
    /// canonical case the post-edit guard depends on.
    #[tokio::test]
    async fn successful_edit_records_a_modified_mutation() {
        let dir = tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "alpha\nbeta\n").unwrap();
        let ctx = ctx_with_read(dir.path(), &f);
        let r = EditFileTool
            .execute(
                json!({"path": f.to_str().unwrap(), "old_string": "beta", "new_string": "BETA"}),
                &ctx,
            )
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert_eq!(r.mutations.len(), 1, "{:?}", r.mutations);
        assert_eq!(r.mutations[0].path, f);
        assert_eq!(r.mutations[0].kind, WorkspaceMutationKind::Modified);
    }

    /// A rejected edit (ambiguous match, missing string, …) never lands, so it
    /// must not emit a mutation. Symmetric with the `write_file` rule; the
    /// post-edit guard skips failed paths because they aren't real edits.
    #[tokio::test]
    async fn failed_edit_does_not_record_a_mutation() {
        let dir = tempdir().unwrap();
        let r = EditFileTool
            .execute(
                json!({"path": "ghost.txt", "old_string": "a", "new_string": "b"}),
                &ctx(dir.path()),
            )
            .await;
        assert!(r.is_error);
        assert!(r.mutations.is_empty(), "{:?}", r.mutations);
    }
}
