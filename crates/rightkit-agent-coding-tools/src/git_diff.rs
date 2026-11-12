//! `git_diff` — show diff of working tree vs HEAD.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use git2::{DiffFormat, DiffOptions};
use serde_json::{json, Value};

pub struct GitDiffTool;

const MAX_DIFF_BYTES: usize = 100 * 1024;

#[async_trait::async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Unified diff of working tree (or staged with staged=true) vs HEAD (max 100KB). Fail: not a git repo."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": { "type": "string" },
                "base": { "type": "string", "description": "Base ref to diff against (default: HEAD)" },
                "staged": { "type": "boolean", "description": "Show staged changes instead of worktree changes (default: false)" },
                "paths": { "type": "array", "items": { "type": "string" }, "description": "Exact repository-relative path filters" }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let repo = match crate::git_read::open_workspace_repository(args["repo_path"].as_str(), ctx)
        {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };

        let staged = args["staged"].as_bool().unwrap_or(false);
        let base = args["base"].as_str().unwrap_or("HEAD");
        let head_tree = match repo
            .revparse_single(base)
            .and_then(|object| object.peel_to_tree())
        {
            Ok(tree) => Some(tree),
            Err(_error) if base == "HEAD" && repo.is_empty().unwrap_or(false) => None,
            Err(error) => {
                return ToolResult::error(format!("Failed to resolve base ref '{base}': {error}"))
            }
        };
        let mut opts = DiffOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        if let Some(paths) = args["paths"].as_array() {
            if paths.is_empty() {
                return ToolResult::error("'paths' must not be empty when supplied.");
            }
            for value in paths {
                let Some(path) = value.as_str() else {
                    return ToolResult::error("Every git_diff path must be a string.");
                };
                if let Err(error) = crate::git_read::validate_repo_relative_path(path) {
                    return ToolResult::error(error);
                }
                opts.pathspec(path);
            }
        }

        let diff = if staged {
            match repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts)) {
                Ok(d) => d,
                Err(e) => return ToolResult::error(format!("Failed to diff staged: {}", e)),
            }
        } else {
            // Full worktree (index + unstaged) against the exact base.
            match repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts)) {
                Ok(d) => d,
                Err(e) => return ToolResult::error(format!("Failed to diff workdir: {}", e)),
            }
        };

        let mut output = String::new();
        // Track the true total size even after we stop appending, so a
        // truncation marker can state the real original size (7b) instead of
        // just "this is where we capped it".
        let mut total_bytes: usize = 0;
        let result = diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' | '-' | ' ' => line.origin().to_string(),
                _ => String::new(),
            };
            let content = String::from_utf8_lossy(line.content());
            total_bytes += prefix.len() + content.len();
            if output.len() < MAX_DIFF_BYTES {
                output.push_str(&prefix);
                output.push_str(&content);
            }
            true
        });

        if let Err(e) = result {
            return ToolResult::error(format!("Failed to format diff: {}", e));
        }

        // 7b: mask secrets (e.g. an accidentally-diffed `.env` line) before
        // the model sees the diff.
        let mut output = crate::output_compaction::mask_secrets(&output);

        if total_bytes > MAX_DIFF_BYTES {
            output.push_str(&format!(
                "\n[{}]",
                crate::output_compaction::truncation_marker(total_bytes, MAX_DIFF_BYTES)
            ));
        }

        if output.is_empty() {
            ToolResult::ok("(no changes)")
        } else {
            ToolResult::ok(output)
        }
    }
}
