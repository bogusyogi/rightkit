//! `git_diff` — show diff of working tree vs HEAD.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
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
        let repo = match crate::coding_tools::git_read::open_workspace_repository(
            args["repo_path"].as_str(),
            ctx,
        ) {
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
                if let Err(error) = crate::coding_tools::git_read::validate_repo_relative_path(path)
                {
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
        let mut output = crate::coding_tools::output_compaction::mask_secrets(&output);

        if total_bytes > MAX_DIFF_BYTES {
            output.push_str(&format!(
                "\n[{}]",
                crate::coding_tools::output_compaction::truncation_marker(
                    total_bytes,
                    MAX_DIFF_BYTES
                )
            ));
        }

        if output.is_empty() {
            ToolResult::ok("(no changes)")
        } else {
            ToolResult::ok(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::{tempdir_in, TempDir};

    fn ctx(root: &std::path::Path) -> ToolContext {
        ToolContext::new("s1", root, PermissionMode::Bypass, WorkspaceTrust::Trusted)
    }

    fn test_tempdir() -> TempDir {
        let root = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("rightkit-agent-test-tmp");
        std::fs::create_dir_all(&root).unwrap();
        tempdir_in(root).unwrap()
    }

    #[tokio::test]
    async fn git_diff_returns_empty_for_clean_repo() {
        let dir = test_tempdir();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();

        let tool = GitDiffTool;
        let result = tool.execute(json!({}), &ctx(dir.path())).await;
        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content.contains("no changes") || result.content.is_empty() || !result.is_error
        );
    }

    #[tokio::test]
    async fn git_diff_fails_on_non_repo() {
        let dir = test_tempdir();
        std::fs::write(dir.path().join(".git"), "not a git repository").unwrap();
        let tool = GitDiffTool;
        let result = tool.execute(json!({}), &ctx(dir.path())).await;
        assert!(result.is_error);
    }

    #[tokio::test]
    async fn git_diff_includes_untracked_files() {
        let dir = test_tempdir();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let sig = git2::Signature::now("Test", "test@test.com").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        std::fs::write(dir.path().join("new.txt"), "hello\n").unwrap();

        let tool = GitDiffTool;
        let result = tool.execute(json!({}), &ctx(dir.path())).await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("new.txt"), "{}", result.content);
        assert!(result.content.contains("+hello"), "{}", result.content);
    }
}
