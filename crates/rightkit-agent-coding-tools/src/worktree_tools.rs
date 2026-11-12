//! `worktree_enter` and `worktree_exit` — manage git worktrees for isolated work.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_agent_policy::CapabilityClass;
#[cfg(feature = "host")]
use rightkit_sandbox::execution::{CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext};
use serde_json::{json, Value};
#[cfg(feature = "host")]
use std::sync::Arc;
#[cfg(feature = "host")]
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

#[cfg(feature = "host")]
fn process_backend_error(ctx: &ToolContext, surface: &str) -> Option<String> {
    if ctx.process_launch_adapter.is_some() {
        None
    } else {
        ctx.nonlocal_execution_backend_error(surface)
    }
}

#[cfg(not(feature = "host"))]
fn process_backend_error(ctx: &ToolContext, surface: &str) -> Option<String> {
    ctx.nonlocal_execution_backend_error(surface)
}

#[cfg(not(feature = "host"))]
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

#[cfg(feature = "host")]
async fn run_git(
    ctx: &ToolContext,
    surface: &str,
    argv: Vec<&str>,
) -> Result<ManagedGitOutput, String> {
    let adapter: Arc<dyn ProcessLaunchAdapter> = ctx
        .process_launch_adapter
        .clone()
        .unwrap_or_else(|| Arc::new(crate::process_launch::LocalProcessLaunchAdapter));
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

#[cfg(feature = "host")]
struct ManagedGitOutput {
    status: bool,
    #[allow(dead_code)]
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

#[cfg(not(feature = "host"))]
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
