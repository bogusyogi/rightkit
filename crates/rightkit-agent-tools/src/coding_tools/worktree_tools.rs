//! `worktree_enter` and `worktree_exit` — manage git worktrees for isolated work.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::CapabilityClass;
#[cfg(feature = "coding_tools-host")]
use rightkit_sandbox::execution::{CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext};
use serde_json::{json, Value};
#[cfg(feature = "coding_tools-host")]
use std::sync::Arc;
#[cfg(feature = "coding_tools-host")]
use tokio::io::AsyncReadExt;

pub struct EnterWorktreeTool;
pub struct ExitWorktreeTool;

#[async_trait::async_trait]
impl Tool for EnterWorktreeTool {
    fn name(&self) -> &str {
        "worktree_enter"
    }

    // Audit A-4 (2026-07-04): worktree_enter/exit had no explicit capability
    // class and weren't in the name-inference table, so they fell to
    // CapabilityClass::Unknown — which AutoSafe mode auto-allows (unlike
    // Destructive/ProcessControl/Browser). GitWrite is the correct class: it
    // is a filesystem-mutating git operation, so it gets the deny-glob write
    // backstop and the same gating as WriteFile across modes, rather than
    // silently bypassing policy entirely. Worktrees are legitimately created
    // OUTSIDE the workspace root by design (sibling directories for parallel
    // branches), so this is deliberately NOT a workspace-boundary check like
    // read_file's — that would break the intended usage.
    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::GitWrite)
    }

    fn description(&self) -> &str {
        "Create a git worktree for a branch, enabling isolated work without switching branches. \
         Runs `git worktree add {path} {branch}`. Uses a temp dir if no path given. \
         Returns the worktree path."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["branch"],
            "properties": {
                "branch": {
                    "type": "string",
                    "description": "Git branch to check out in the worktree."
                },
                "path": {
                    "type": "string",
                    "description": "Directory path for the worktree. Defaults to a temp dir."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(error) = process_backend_error(ctx, "worktree_enter") {
            return ToolResult::error(error);
        }
        let branch = match args["branch"].as_str() {
            Some(b) => b,
            None => return ToolResult::error("worktree_enter: 'branch' is required"),
        };

        let worktree_path = match args["path"].as_str() {
            Some(p) => p.to_string(),
            None => {
                let tmp = std::env::temp_dir();
                let safe: String = branch
                    .chars()
                    .map(|c| {
                        if c.is_alphanumeric() || c == '-' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                tmp.join(format!("rightkit-agent-wt-{safe}"))
                    .to_string_lossy()
                    .into_owned()
            }
        };

        let output = run_git(
            ctx,
            "worktree_enter",
            vec!["worktree", "add", &worktree_path, branch],
        )
        .await;

        match output {
            Ok(out) if out.status => {
                ToolResult::ok(format!("Worktree created at: {worktree_path}"))
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                ToolResult::error(format!("worktree_enter: git error: {stderr}"))
            }
            Err(e) => ToolResult::error(format!("worktree_enter: failed to run git: {e}")),
        }
    }
}

#[async_trait::async_trait]
impl Tool for ExitWorktreeTool {
    fn name(&self) -> &str {
        "worktree_exit"
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::GitWrite)
    }

    fn description(&self) -> &str {
        "Remove a git worktree at the given path. \
         Runs `git worktree remove {path}`. \
         Returns 'Worktree removed: {path}' on success."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path of the worktree to remove."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if let Some(error) = process_backend_error(ctx, "worktree_exit") {
            return ToolResult::error(error);
        }
        let path = match args["path"].as_str() {
            Some(p) => p,
            None => return ToolResult::error("worktree_exit: 'path' is required"),
        };

        let output = run_git(ctx, "worktree_exit", vec!["worktree", "remove", path]).await;

        match output {
            Ok(out) if out.status => ToolResult::ok(format!("Worktree removed: {path}")),
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                ToolResult::error(format!("worktree_exit: git error: {stderr}"))
            }
            Err(e) => ToolResult::error(format!("worktree_exit: failed to run git: {e}")),
        }
    }
}

#[cfg(feature = "coding_tools-host")]
fn process_backend_error(ctx: &ToolContext, surface: &str) -> Option<String> {
    if ctx.process_launch_adapter.is_some() {
        None
    } else {
        ctx.nonlocal_execution_backend_error(surface)
    }
}

#[cfg(not(feature = "coding_tools-host"))]
fn process_backend_error(ctx: &ToolContext, surface: &str) -> Option<String> {
    ctx.nonlocal_execution_backend_error(surface)
}

#[cfg(not(feature = "coding_tools-host"))]
async fn run_git(
    ctx: &ToolContext,
    surface: &str,
    argv: Vec<&str>,
) -> Result<ManagedGitOutput, String> {
    rightkit_child_process::tokio_support::command("git")
        .args(argv)
        .current_dir(&ctx.workspace_root)
        .output()
        .await
        .map(|output| ManagedGitOutput {
            status: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
        .map_err(|error| format!("{surface}: failed to run git: {error}"))
}

#[cfg(feature = "coding_tools-host")]
async fn run_git(
    ctx: &ToolContext,
    surface: &str,
    argv: Vec<&str>,
) -> Result<ManagedGitOutput, String> {
    let adapter: Arc<dyn ProcessLaunchAdapter> =
        ctx.process_launch_adapter.clone().unwrap_or_else(|| {
            Arc::new(crate::coding_tools::process_launch::LocalProcessLaunchAdapter)
        });
    let launch_context = ProcessLaunchContext {
        workspace_root: ctx.workspace_root.to_string_lossy().into_owned(),
        ..ctx.process_launch_context.clone()
    };
    let spec = CommandLaunchSpec {
        program: "git".to_string(),
        argv: argv.into_iter().map(str::to_owned).collect(),
        cwd: ctx.workspace_root.to_string_lossy().into_owned(),
        context: launch_context,
        env: Vec::new(),
    };
    let mut child = adapter
        .spawn(spec)
        .await
        .map_err(|error| format!("{surface}: failed to run git: {error}"))?;
    let mut stdout = child.take_stdout();
    let mut stderr = child.take_stderr();
    let (status, out, err) = tokio::join!(
        child.wait(),
        async {
            let mut bytes = Vec::new();
            if let Some(stream) = stdout.as_mut() {
                let _ = stream.read_to_end(&mut bytes).await;
            }
            bytes
        },
        async {
            let mut bytes = Vec::new();
            if let Some(stream) = stderr.as_mut() {
                let _ = stream.read_to_end(&mut bytes).await;
            }
            bytes
        }
    );
    Ok(ManagedGitOutput {
        status: status.map_err(|error| format!("{surface}: git wait failed: {error}"))? == 0,
        stdout: out,
        stderr: err,
    })
}

#[cfg(feature = "coding_tools-host")]
struct ManagedGitOutput {
    status: bool,
    #[allow(dead_code)]
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[cfg(not(feature = "coding_tools-host"))]
struct ManagedGitOutput {
    status: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

// Audit A-4 (2026-07-04), repro step 1e: are worktree paths lexically
// normalized + boundary-checked like read_file's, and do they carry a
// CapabilityClass? BEFORE this fix, neither Tool impl declared a
// capability_class(), so the policy engine's name-based fallback
// (`parse_capability`) fell through to `CapabilityClass::Unknown` — which
// AutoSafe mode auto-allows regardless of the target path (only
// Destructive/ProcessControl/Browser are gated there). The fix declares
// `GitWrite` (worktree add/remove genuinely mutates the filesystem via git),
// which gets the deny-glob write backstop and the same mode-gating as
// WriteFile. This is deliberately NOT a workspace-boundary check like
// read_file's resolve_in_workspace: worktrees are legitimately created
// OUTSIDE the workspace root by design (sibling directories for parallel
// branches — this very audit lane runs from one), so rejecting out-of-root
// paths would break the intended usage.
#[cfg(test)]
mod worktree_boundary_tests {
    use super::*;
    use crate::coding_tools::tool_trait::Tool;
    use rightkit_agent_policy::{
        parse_capability, CapabilityClass, PermissionMode, WorkspaceTrust,
    };

    // 1e-i: both tools now declare GitWrite explicitly, so classification
    // never depends on the name-inference table falling through to Unknown.
    #[test]
    fn worktree_tools_declare_gitwrite_capability_class() {
        assert_eq!(
            EnterWorktreeTool.capability_class(),
            Some(CapabilityClass::GitWrite)
        );
        assert_eq!(
            ExitWorktreeTool.capability_class(),
            Some(CapabilityClass::GitWrite)
        );
    }

    // 1e-ii: the name-inference table lookup (the fallback path exercised
    // when a tool does NOT declare a class) still has no entries for these
    // names and reasonably falls to Unknown. This test just documents that
    // fact so a future removal of the explicit capability_class() overrides
    // above is caught immediately by 1e-i, not silently reintroducing the gap.
    #[test]
    fn worktree_tool_names_alone_still_fall_to_unknown_in_name_table() {
        assert_eq!(
            parse_capability(Some("worktree_enter")),
            CapabilityClass::Unknown
        );
        assert_eq!(
            parse_capability(Some("worktree_exit")),
            CapabilityClass::Unknown
        );
    }

    // 1e-iii: end-to-end policy behavior with the fix in place — GitWrite is
    // treated as is_file_write, so in AutoSafe mode a worktree operation is
    // gated the same way a file write would be (auto-allow unless it hits a
    // deny-glob or a destructive escalation), rather than bypassing policy
    // entirely via Unknown. This locks that worktree_enter/exit are no longer
    // invisible to the deny-glob backstop.
    #[test]
    fn autosafe_mode_gates_worktree_gitwrite_like_a_file_write() {
        let engine = rightkit_agent_policy::PermissionEngine::new();
        let cls = EnterWorktreeTool.capability_class().unwrap();
        assert!(rightkit_agent_policy::is_file_write(cls));

        let verdict = engine.evaluate(
            cls,
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted,
            Some("../../outside-the-workspace"),
            false,
        );
        // Worktrees outside the root are the intended use case, so this
        // specific path is still Allow — the fix is that it now goes through
        // the SAME gate as any other file write (deny-glob backstop applies)
        // instead of being invisible to policy via Unknown.
        assert_eq!(
            verdict.decision,
            rightkit_agent_policy::PermissionDecision::Allow
        );
    }

    // 1e-iii: the tool implementation itself performs no lexical normalization
    // or boundary check on `path` before handing it to `git worktree add` —
    // confirm this at the Tool level (not just the policy level) using a
    // real temp git repo, so the finding is anchored to actual behavior, not
    // only the classification table.
    #[tokio::test]
    async fn worktree_enter_passes_absolute_outside_path_to_git_unchecked() {
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_target = outside.path().join("escaped-worktree");

        let init = rightkit_child_process::tokio_support::command("git")
            .args(["init", "-q"])
            .current_dir(repo.path())
            .output()
            .await
            .unwrap();
        assert!(init.status.success(), "git init failed: {init:?}");
        std::fs::write(repo.path().join("a.txt"), "x").unwrap();
        let config_name = rightkit_child_process::tokio_support::command("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo.path())
            .output()
            .await
            .unwrap();
        assert!(config_name.status.success());
        let config_user = rightkit_child_process::tokio_support::command("git")
            .args(["config", "user.name", "Test"])
            .current_dir(repo.path())
            .output()
            .await
            .unwrap();
        assert!(config_user.status.success());
        let add = rightkit_child_process::tokio_support::command("git")
            .args(["add", "a.txt"])
            .current_dir(repo.path())
            .output()
            .await
            .unwrap();
        assert!(add.status.success());
        let commit = rightkit_child_process::tokio_support::command("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(repo.path())
            .output()
            .await
            .unwrap();
        assert!(commit.status.success(), "git commit failed: {commit:?}");

        let ctx = ToolContext::new(
            "s1",
            repo.path(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        );
        let tool = EnterWorktreeTool;
        let result = tool
            .execute(
                json!({
                    "branch": "HEAD",
                    "path": outside_target.to_string_lossy(),
                }),
                &ctx,
            )
            .await;

        // The tool has no boundary check of its own; whether this succeeds is
        // entirely up to git. What matters for the audit is that the tool
        // layer never rejected the escaping path before invoking git — if it
        // had a boundary check, this call would fail with a workspace-escape
        // error message instead of a git error (or success).
        assert!(
            !result.content.to_lowercase().contains("workspace"),
            "worktree_enter has no workspace-boundary check on `path` (confirmed): {}",
            result.content
        );

        let _ = std::fs::remove_dir_all(&outside_target);
    }
}
