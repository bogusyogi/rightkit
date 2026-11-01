//! FM-161 — parallel-agent worktree enter/exit lifecycle.
//!
//! This host-only surface is omitted from iOS/mobile builds through
//! `host_orchestration`'s `host` feature.
//!
//! Canon `docs/canon/daemon-master-canon-atoms.md` (row `FM-161`, dispatch-plan
//! lane A15): retain the host app's run-level agent supervision, and add the
//! explicit *enter*/*exit* git-worktree lifecycle Claude Code uses for
//! parallel subagents. Claude Code's own `EnterWorktree`/`ExitWorktree`
//! tool pair is the non-Rust donor (per the Port-first rule, priority 2:
//! port faithfully into Rust, never redesign): enter creates (or reuses) an
//! isolated worktree and checks a branch out into it before the child
//! starts; exit either removes the worktree (when the caller asked for
//! cleanup) or leaves it in place for inspection, and never touches the
//! branch's commits either way — the child's own git history is the
//! deliverable, this lifecycle only owns the checkout directory.
//!
//! `SubagentWorktreeOptions { branch, path, cleanup }`
//! (`engine/crates/tools/src/context.rs`) and `spawn_subagent`'s worktree
//! schema already carry the *intent*; this module is the missing mechanism
//! that turns that intent into real `git worktree add` / `git worktree
//! remove` invocations, decoupled from git execution so the planning logic
//! is unit-testable without a repository on disk.
#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One agent's worktree request, mirroring
/// `rightkit_agent_tools::coding_tools::SubagentWorktreeOptions` at the orchestration seam
/// (this crate does not depend on `host_tools`, so the caller maps
/// fields across the boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEnterRequest {
    /// Repository root the worktree is created against.
    pub repo_root: PathBuf,
    /// Branch to check out into the new worktree. Created from `base_ref`
    /// when it does not already exist.
    pub branch: String,
    /// Base ref the branch is created from when it is new. Defaults to
    /// `HEAD` when `None`.
    pub base_ref: Option<String>,
    /// Explicit worktree directory. When `None`, a deterministic path is
    /// derived under the repo's `<state-dir>/worktrees/<branch>` so repeated
    /// enters for the same branch are idempotent.
    pub path: Option<PathBuf>,
}

/// Planned outcome of entering a worktree lifecycle. Pure — no filesystem
/// or git call has happened yet; `GitWorktreeExecutor::enter` carries it
/// out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeEnterPlan {
    pub worktree_path: PathBuf,
    pub branch: String,
    /// `git worktree add [-B <branch>] <path> [<base_ref>]` argv, ready to
    /// hand to `std::process::Command::args`.
    pub git_args: Vec<String>,
}

/// Planned outcome of exiting a worktree lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeExitPlan {
    pub worktree_path: PathBuf,
    /// `Some(argv)` when cleanup removes the worktree
    /// (`git worktree remove --force <path>`); `None` when the caller asked
    /// to keep the checkout (`cleanup == false`) and exit is a no-op at the
    /// git layer — the child's commits and the directory stay in place for
    /// inspection or handoff.
    pub git_args: Option<Vec<String>>,
}

/// Errors raised while planning an enter/exit. Planning never touches disk
/// or git; these are purely input-shape rejections.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorktreeLifecycleError {
    #[error("worktree branch is empty")]
    EmptyBranch,
    #[error("worktree repo_root is empty")]
    EmptyRepoRoot,
    #[error("worktree path escapes repo_root: {path}")]
    PathEscapesRepo { path: String },
}

/// Derive the enter plan. Pure & deterministic: the same request always
/// derives the same path and argv. The production executor recognizes an
/// already-registered checkout at that exact path and branch, making a
/// retried enter (for example after a daemon restart) idempotent.
pub fn plan_enter(
    request: &WorktreeEnterRequest,
) -> Result<WorktreeEnterPlan, WorktreeLifecycleError> {
    let branch = request.branch.trim();
    if branch.is_empty() {
        return Err(WorktreeLifecycleError::EmptyBranch);
    }
    if request.repo_root.as_os_str().is_empty() {
        return Err(WorktreeLifecycleError::EmptyRepoRoot);
    }
    let worktree_path = match &request.path {
        Some(explicit) => explicit.clone(),
        None => default_worktree_path(&request.repo_root, branch),
    };
    if let Some(explicit) = &request.path {
        if !path_is_contained(&request.repo_root, explicit) {
            return Err(WorktreeLifecycleError::PathEscapesRepo {
                path: explicit.display().to_string(),
            });
        }
    }
    let mut git_args = vec![
        "worktree".to_string(),
        "add".to_string(),
        "-B".to_string(),
        branch.to_string(),
        git_path_arg(&worktree_path),
    ];
    if let Some(base) = &request.base_ref {
        let base = base.trim();
        if !base.is_empty() {
            git_args.push(base.to_string());
        }
    }
    Ok(WorktreeEnterPlan {
        worktree_path,
        branch: branch.to_string(),
        git_args,
    })
}

/// Derive the exit plan. `cleanup == false` keeps the worktree (Claude
/// Code's own default when a subagent's work should stay inspectable after
/// exit); `cleanup == true` force-removes it so no dangling checkout
/// survives the parent turn.
pub fn plan_exit(worktree_path: &Path, cleanup: bool) -> WorktreeExitPlan {
    let git_args = cleanup.then(|| {
        vec![
            "worktree".to_string(),
            "remove".to_string(),
            "--force".to_string(),
            git_path_arg(worktree_path),
        ]
    });
    WorktreeExitPlan {
        worktree_path: worktree_path.to_path_buf(),
        git_args,
    }
}

fn default_worktree_path(repo_root: &Path, branch: &str) -> PathBuf {
    let safe_branch: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    repo_root
        .join(".rightkit")
        .join("worktrees")
        .join(safe_branch)
}

fn git_path_arg(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Lexical containment check reusing the same "no `..`, both slash styles"
/// contract `rightkit_workflow::control_plane::a2a::workspace_contains` already applies
/// at the A2A boundary, so an explicit `worktree.path` cannot be pointed
/// outside the repository it claims to belong to.
fn path_is_contained(repo_root: &Path, candidate: &Path) -> bool {
    let Ok(repo_root) = repo_root
        .canonicalize()
        .or_else(|_| Ok::<_, std::io::Error>(repo_root.to_path_buf()))
    else {
        return false;
    };
    let candidate_str = candidate.to_string_lossy().replace('\\', "/");
    if candidate_str.split('/').any(|segment| segment == "..") {
        return false;
    }
    // A relative candidate is always contained once `..` is ruled out; an
    // absolute candidate must share the repo root's prefix.
    if candidate.is_relative() {
        return true;
    }
    let repo_str = repo_root
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase();
    let candidate_str = candidate_str.to_lowercase();
    candidate_str == repo_str || candidate_str.starts_with(&(repo_str + "/"))
}

/// Executes the planned git worktree lifecycle. A trait so the daemon can
/// inject a real `std::process::Command`-backed executor while tests inject
/// a recording stub — the plan/execute split is what keeps `plan_enter` and
/// `plan_exit` unit-testable without a real repository on disk.
pub trait GitWorktreeExecutor: Send + Sync {
    fn run(&self, repo_root: &Path, args: &[String]) -> Result<(), String>;
}

/// Production executor: shells out to the system `git` binary.
pub struct SystemGitWorktreeExecutor;

impl GitWorktreeExecutor for SystemGitWorktreeExecutor {
    fn run(&self, repo_root: &Path, args: &[String]) -> Result<(), String> {
        let context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: repo_root.to_string_lossy().into_owned(),
            sandbox_profile: Some("git-worktree".into()),
            sandbox_required: true,
            network_required: false,
        };
        let output = run_managed_command("git", args, context)?;
        if output.0 == 0 {
            return Ok(());
        }
        if let Some((branch, path)) = worktree_add_target(args) {
            if registered_worktree_matches(repo_root, path, branch)? {
                return Ok(());
            }
        }
        Err(format!(
            "git {args:?} exited with {}: {}",
            output.0,
            String::from_utf8_lossy(&output.2)
        ))
    }
}

fn run_managed_command(
    program: &str,
    args: &[String],
    context: rightkit_sandbox::execution::ProcessLaunchContext,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    let adapter: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter> =
        Arc::new(rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter);
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: program.to_string(),
        argv: args.to_vec(),
        cwd: context.workspace_root.clone(),
        context,
        env: Vec::new(),
    };
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|error| format!("managed command runtime: {error}"))?;
        runtime.block_on(async move {
            use tokio::io::AsyncReadExt;
            let mut child = adapter
                .spawn(spec)
                .await
                .map_err(|error| format!("managed command launch: {error:?}"))?;
            let mut stdout = child.take_stdout();
            let mut stderr = child.take_stderr();
            let stdout_task = async {
                let mut bytes = Vec::new();
                if let Some(stream) = stdout.as_mut() {
                    let _ = stream.read_to_end(&mut bytes).await;
                }
                bytes
            };
            let stderr_task = async {
                let mut bytes = Vec::new();
                if let Some(stream) = stderr.as_mut() {
                    let _ = stream.read_to_end(&mut bytes).await;
                }
                bytes
            };
            let (status, out, err) = tokio::join!(child.wait(), stdout_task, stderr_task);
            let status = status.map_err(|error| format!("managed command wait: {error:?}"))?;
            Ok((status, out, err))
        })
    })
    .join()
    .map_err(|_| "managed command worker panicked".to_string())?
}

fn worktree_add_target(args: &[String]) -> Option<(&str, &Path)> {
    match args {
        [worktree, add, force_branch, branch, path, ..]
            if worktree == "worktree" && add == "add" && force_branch == "-B" =>
        {
            Some((branch, Path::new(path)))
        }
        _ => None,
    }
}

fn registered_worktree_matches(
    repo_root: &Path,
    requested_path: &Path,
    requested_branch: &str,
) -> Result<bool, String> {
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: repo_root.to_string_lossy().into_owned(),
        sandbox_profile: Some("git-worktree".into()),
        sandbox_required: true,
        network_required: false,
    };
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    let prepared = rightkit_sandbox::execution::CommandSandboxAdapter::prepare(
        &adapter,
        "git",
        &["worktree".into(), "list".into(), "--porcelain".into()],
        &context,
    )
    .map_err(|error| format!("git worktree list launch rejected: {error}"))?;
    let output = run_managed_command(&prepared.program, &prepared.argv, context)?;
    if output.0 != 0 {
        return Err(format!(
            "git worktree list exited with {}: {}",
            output.0,
            String::from_utf8_lossy(&output.2)
        ));
    }

    let requested_path = requested_path
        .canonicalize()
        .unwrap_or_else(|_| requested_path.to_path_buf());
    let requested_branch = format!("refs/heads/{requested_branch}");
    Ok(String::from_utf8_lossy(&output.1)
        .split("\n\n")
        .any(|entry| {
            let path_matches = entry
                .lines()
                .find_map(|line| line.strip_prefix("worktree "))
                .map(Path::new)
                .map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
                .is_some_and(|path| path == requested_path);
            let branch_matches = entry
                .lines()
                .any(|line| line == format!("branch {requested_branch}"));
            path_matches && branch_matches
        }))
}

/// Enter a worktree lifecycle end-to-end: plan then execute.
pub fn enter_worktree(
    request: &WorktreeEnterRequest,
    executor: &dyn GitWorktreeExecutor,
) -> Result<WorktreeEnterPlan, String> {
    let plan = plan_enter(request).map_err(|error| error.to_string())?;
    executor.run(&request.repo_root, &plan.git_args)?;
    Ok(plan)
}

/// Exit a worktree lifecycle end-to-end: plan then execute (a no-op when
/// `cleanup == false`).
pub fn exit_worktree(
    repo_root: &Path,
    worktree_path: &Path,
    cleanup: bool,
    executor: &dyn GitWorktreeExecutor,
) -> Result<WorktreeExitPlan, String> {
    let plan = plan_exit(worktree_path, cleanup);
    if let Some(args) = &plan.git_args {
        executor.run(repo_root, args)?;
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn plan_enter_derives_deterministic_path_when_unset() {
        let request = WorktreeEnterRequest {
            repo_root: PathBuf::from("/repo"),
            branch: "codex/fix-bug".to_string(),
            base_ref: None,
            path: None,
        };
        let plan = plan_enter(&request).expect("plan");
        assert_eq!(
            plan.worktree_path,
            PathBuf::from("/repo/.rightkit/worktrees/codex-fix-bug")
        );
        assert_eq!(plan.branch, "codex/fix-bug");
        assert_eq!(
            plan.git_args,
            vec![
                "worktree",
                "add",
                "-B",
                "codex/fix-bug",
                "/repo/.rightkit/worktrees/codex-fix-bug"
            ]
        );
    }

    #[test]
    fn plan_enter_appends_base_ref_when_present() {
        let request = WorktreeEnterRequest {
            repo_root: PathBuf::from("/repo"),
            branch: "feature".to_string(),
            base_ref: Some("origin/main".to_string()),
            path: Some(PathBuf::from("wt")),
        };
        let plan = plan_enter(&request).expect("plan");
        assert_eq!(plan.git_args.last().unwrap(), "origin/main");
    }

    #[test]
    fn plan_enter_rejects_empty_branch() {
        let request = WorktreeEnterRequest {
            repo_root: PathBuf::from("/repo"),
            branch: "  ".to_string(),
            base_ref: None,
            path: None,
        };
        assert_eq!(
            plan_enter(&request),
            Err(WorktreeLifecycleError::EmptyBranch)
        );
    }

    #[test]
    fn plan_enter_rejects_path_traversal_out_of_repo() {
        let request = WorktreeEnterRequest {
            repo_root: PathBuf::from("/repo"),
            branch: "feature".to_string(),
            base_ref: None,
            path: Some(PathBuf::from("../outside")),
        };
        assert!(matches!(
            plan_enter(&request),
            Err(WorktreeLifecycleError::PathEscapesRepo { .. })
        ));
    }

    #[test]
    fn plan_exit_is_noop_git_args_when_cleanup_false() {
        let plan = plan_exit(Path::new("/repo/.rightkit/worktrees/x"), false);
        assert!(plan.git_args.is_none());
    }

    #[test]
    fn plan_exit_force_removes_when_cleanup_true() {
        let plan = plan_exit(Path::new("/repo/.rightkit/worktrees/x"), true);
        assert_eq!(
            plan.git_args.unwrap(),
            vec![
                "worktree",
                "remove",
                "--force",
                "/repo/.rightkit/worktrees/x"
            ]
        );
    }

    struct RecordingExecutor {
        calls: Mutex<Vec<(PathBuf, Vec<String>)>>,
    }

    impl GitWorktreeExecutor for RecordingExecutor {
        fn run(&self, repo_root: &Path, args: &[String]) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push((repo_root.to_path_buf(), args.to_vec()));
            Ok(())
        }
    }

    #[test]
    fn enter_then_exit_round_trip_invokes_executor_exactly_once_each() {
        let executor = RecordingExecutor {
            calls: Mutex::new(Vec::new()),
        };
        let request = WorktreeEnterRequest {
            repo_root: PathBuf::from("/repo"),
            branch: "codex/round-trip".to_string(),
            base_ref: None,
            path: None,
        };
        let entered = enter_worktree(&request, &executor).expect("enter");
        let exited = exit_worktree(&request.repo_root, &entered.worktree_path, true, &executor)
            .expect("exit");
        assert!(exited.git_args.is_some());
        let calls = executor.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1[1], "add");
        assert_eq!(calls[1].1[1], "remove");
    }

    #[test]
    fn exit_without_cleanup_never_calls_executor() {
        let executor = RecordingExecutor {
            calls: Mutex::new(Vec::new()),
        };
        exit_worktree(
            Path::new("/repo"),
            Path::new("/repo/.rightkit/worktrees/keep"),
            false,
            &executor,
        )
        .expect("exit without cleanup");
        assert!(executor.calls.lock().unwrap().is_empty());
    }
}
