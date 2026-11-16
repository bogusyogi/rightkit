//! `edit_file` — semantic find-and-replace edit (Claude-Code `FileEdit` parity).
//!
//! Replaces an exact `old_string` with `new_string` in a file. Unlike a blind
//! overwrite (`write_file`) or a positional patch (`apply_patch`), this is the
//! reliable agent edit primitive: the match must be **unique** (or `replace_all`
//! must be set), so an edit can never silently land in the wrong place, and the
//! tool fails loudly with an actionable message when the target is missing or
//! ambiguous. Line endings and surrounding bytes are preserved verbatim.

use crate::context::ToolContext;
use crate::edit_helpers::{apply_single_edit, apply_single_edit_counted};
use crate::tool_trait::{Tool, ToolResult, WorkspaceMutation, WorkspaceMutationKind};
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
        crate::durable_write::path_arg_kind(args)
    }

    fn description(&self) -> &str {
        if crate::tool_options::edit_file_newer() && crate::tool_options::edit_read_discipline() {
            return "Replace exact substring in a file. An exact old_string match needs no prior read. An inexact (tolerant) match needs the whole file read with read_file first. Hashline-prefixed old_string values from read_file are validated against their original line anchors and stale anchors are rejected with fresh anchors. Plain old_string must be unique unless replace_all=true. With no exact match, a whitespace- or punctuation-tolerant match is used and the result says so. Newlines in new_string become CRLF where the matched text or the whole file uses CRLF; otherwise new_string is written verbatim.";
        }
        if crate::tool_options::edit_file_newer() {
            return "Replace exact substring in a file. Hashline-prefixed old_string values from read_file are validated against their original line anchors and stale anchors are rejected with fresh anchors. Plain old_string must be unique unless replace_all=true. With no exact match, a whitespace- or punctuation-tolerant match is used and the result says so. Newlines in new_string become CRLF where the matched text or the whole file uses CRLF; otherwise new_string is written verbatim.";
        }
        if crate::tool_options::edit_read_discipline() {
            return "Replace exact substring in a file. An exact old_string match needs no prior read. An inexact (tolerant) match needs the whole file read with read_file first. Hashline-prefixed old_string values from read_file are validated against their original line anchors and stale anchors are rejected with fresh anchors. Plain old_string must be unique unless replace_all=true.";
        }
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
            .and_then(|path| crate::read_file::resolve_in_workspace(path, &ctx.workspace_root).ok())
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

        let path = match crate::read_file::resolve_in_workspace(path_str, &ctx.workspace_root) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };
        if let Err(error) = crate::durable_write::require_path_authorization(&path, ctx) {
            return ToolResult::error(error);
        }
        if let Err(error) =
            crate::read_file::enforce_path_deny(ctx, &path, crate::read_file::PathAccess::Write)
        {
            return ToolResult::error(error);
        }
        if let Err(error) =
            crate::read_file::enforce_path_deny(ctx, &path, crate::read_file::PathAccess::Read)
        {
            return ToolResult::error(error);
        }

        // Permission check (same as write_file: this mutates the workspace). Evaluate the
        // resolved, canonical workspace-relative target, not the raw caller string, so a symlink
        // alias or a `..` spelling cannot hide a protected (e.g. test) file from the policy rules.
        let policy_path = crate::read_file::policy_target_path(&path, &ctx.workspace_root);
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
        // Test-file protection guards existing test text; with host option
        // `edit_test_file_additions` an insert-only edit keeps all of it.
        let test_addition = crate::tool_options::edit_test_file_additions()
            && verdict.policy_source == "test-file:protected"
            && crate::edit_helpers::edit_only_inserts(old_string, new_string, replace_all);
        if verdict.decision == PermissionDecision::Ask && !test_addition {
            return ToolResult::error(format!(
                "Tool requires approval before proceeding: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }
        let newer = crate::tool_options::edit_file_newer();
        let discipline = crate::tool_options::edit_read_discipline();
        if let Some(refusal) = crate::edit_helpers::oversize_refusal(&path) {
            return ToolResult::error(refusal);
        }

        // With no read receipt, the revision is taken BEFORE the content is read, so the write is
        // refused if the file changed in between.
        let observed = if discipline {
            ctx.file_service.observe_when_unread(&path)
        } else {
            None
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if newer => {
                let hint = match e.kind() {
                    std::io::ErrorKind::NotFound => " (use write_file to create a new file)",
                    std::io::ErrorKind::InvalidData => {
                        " (the file is not UTF-8 text, which edit_file cannot edit; to replace it whole, read_file it, then write_file; read_file shows such a file lossily, so rewriting it from what was read replaces the original bytes)"
                    }
                    _ => "",
                };
                return ToolResult::error(format!("Cannot read '{}': {e}{hint}", path.display()));
            }
            Err(e) => {
                return ToolResult::error(format!(
                    "Cannot read '{}': {} (use write_file to create a new file)",
                    path.display(),
                    e
                ))
            }
        };

        // `anchored`: the edit was located in the current text by a HashLine anchor or, with host
        // option `edit_read_discipline`, by an exact old_string match. `exact_text`: the plain text
        // path matched exactly (not a HashLine block and not a tolerant match).
        let (updated, matches, tolerant_rung, anchored, exact_text) =
            match crate::read_file::resolve_hashline_block(old_string, &content) {
                Ok(Some(anchor)) => (
                    crate::read_file::replace_hashline_block(&content, &anchor, new_string),
                    1,
                    None,
                    true,
                    false,
                ),
                Ok(None) => {
                    let (updated, replaced, rung) = match apply_single_edit_counted(
                        &content,
                        old_string,
                        new_string,
                        replace_all,
                        &format!("'{}'", path.display()),
                    ) {
                        Ok(applied) => applied,
                        Err(e) => return ToolResult::error(e),
                    };
                    // The older message counts the exact matches of the text, 0 for a tolerant one.
                    let matches = if newer {
                        replaced
                    } else {
                        content.matches(old_string).count()
                    };
                    (
                        updated,
                        matches,
                        rung,
                        discipline && rung.is_none(),
                        rung.is_none(),
                    )
                }
                Err(e) => return ToolResult::error(e),
            };

        // An insert-only edit of a test file keeps the existing text only when the match was exact;
        // a tolerant or HashLine-anchored one rewrites the matched span.
        if verdict.decision == PermissionDecision::Ask && !exact_text {
            return ToolResult::error(format!(
                "Tool requires approval before proceeding: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        // The restore point was written by the engine before this call ran
        // (`write_targets`, sessions rule 41).

        match ctx.file_service.write_with_receipt_intent_from(
            crate::file_service::MutationIntentKind::Update,
            &path,
            updated.as_bytes(),
            anchored,
            observed.as_ref(),
        ) {
            Ok(_) => {
                let diff = unified_diff(&content, &updated, path_str);
                let mut msg = format!(
                    "Edited '{}': replaced {} occurrence{} ({} → {} bytes).",
                    path.display(),
                    if newer || replace_all { matches } else { 1 },
                    if (newer || replace_all) && matches != 1 {
                        "s"
                    } else {
                        ""
                    },
                    content.len(),
                    updated.len()
                );
                if let Some(rung) = tolerant_rung.filter(|_| newer) {
                    msg.push_str(&format!(
                        " No exact match for old_string: a {rung} match was replaced; check the diff."
                    ));
                }
                if !diff.is_empty() {
                    msg.push_str(&format!("\n```diff\n{diff}```"));
                }
                let mutation = WorkspaceMutation {
                    path: path.clone(),
                    kind: WorkspaceMutationKind::Modified,
                    previous_content: Some(content),
                };
                crate::effect_receipt_store::record_effect(
                    &ctx.session_id,
                    vec![path.clone()],
                    diff,
                    crate::effect_receipt_store::EffectResult::Success,
                );
                crate::lsp_notify::document_mutated(&ctx.workspace_root, &path).await;
                ToolResult::ok(msg).with_mutations(vec![mutation])
            }
            Err(e) => {
                crate::effect_receipt_store::record_effect(
                    &ctx.session_id,
                    vec![path.clone()],
                    format!("edit_file failed: {e}"),
                    crate::effect_receipt_store::EffectResult::Error(e.to_string()),
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
        let path = crate::read_file::resolve_in_workspace(path_str, &ctx.workspace_root).ok()?;
        let content = std::fs::read_to_string(&path).ok()?;

        let updated = match crate::read_file::resolve_hashline_block(old_string_raw, &content)
            .ok()?
        {
            Some(anchor) => crate::read_file::replace_hashline_block(&content, &anchor, new_string),
            None => apply_single_edit(&content, old_string_raw, new_string, replace_all, path_str)
                .ok()?,
        };
        let diff = unified_diff(&content, &updated, path_str);
        (!diff.is_empty()).then_some(diff)
    }
}
