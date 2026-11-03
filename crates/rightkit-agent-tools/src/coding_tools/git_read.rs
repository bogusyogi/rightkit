//! `git_read` — read-only git status and log.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use git2::{BranchType, Repository};
use serde_json::{json, Value};

pub struct GitReadTool;

#[async_trait::async_trait]
impl Tool for GitReadTool {
    fn name(&self) -> &str {
        "git_read"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Typed, read-only Git status/log/show/blame/branch operations scoped to an exact repository, ref, and path."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["summary", "status", "log", "show", "blame", "branch", "discover", "remotes"], "default": "summary" },
                "repo_path": { "type": "string", "description": "Path to git repo (defaults to workspace root)" },
                "ref": { "type": "string", "description": "Exact revision/ref for log or show" },
                "path": { "type": "string", "description": "Repository-relative path for show or blame" },
                "max_count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 10 }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let repo = match open_workspace_repository(args["repo_path"].as_str(), ctx) {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };
        let operation = args["operation"].as_str().unwrap_or("summary");
        let max_count = args["max_count"].as_u64().unwrap_or(10).clamp(1, 100) as usize;

        let result = match operation {
            "summary" => summary(&repo, max_count),
            "status" => status(&repo),
            "log" => log(&repo, args["ref"].as_str(), max_count),
            "show" => show(&repo, args["ref"].as_str(), args["path"].as_str()),
            "blame" => blame(&repo, args["path"].as_str()),
            "branch" => branches(&repo),
            "discover" => discover(&repo),
            "remotes" => remotes(&repo),
            other => Err(format!("Unknown git_read operation '{other}'.")),
        };
        match result {
            Ok(output) => ToolResult::ok(output),
            Err(error) => ToolResult::error(error),
        }
    }
}

pub(crate) fn open_workspace_repository(
    requested: Option<&str>,
    ctx: &ToolContext,
) -> Result<Repository, String> {
    let repo_path = crate::coding_tools::read_file::resolve_in_workspace(
        requested.unwrap_or("."),
        &ctx.workspace_root,
    )?;
    let repo = Repository::discover(&repo_path)
        .map_err(|error| format!("Failed to open git repository: {error}"))?;
    let boundary = repo.workdir().unwrap_or_else(|| repo.path());
    let canonical_root = ctx
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| ctx.workspace_root.clone());
    let canonical_repo = boundary
        .canonicalize()
        .map_err(|error| format!("Resolve repository '{}': {error}", boundary.display()))?;
    if !canonical_repo.starts_with(&canonical_root) {
        return Err(format!(
            "Repository '{}' escapes workspace root '{}'.",
            canonical_repo.display(),
            canonical_root.display()
        ));
    }
    Ok(repo)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorkspaceSnapshot {
    pub branch: String,
    pub status_lines: Vec<String>,
    pub truncated: bool,
}

/// Keep engine-owned isolation checkouts out of user Git status without
/// changing tracked ignore files or hiding other workspace content.
pub fn ignore_runtime_worktrees(workspace: &std::path::Path) -> Result<(), String> {
    use std::io::Write;
    let repo = Repository::discover(workspace).map_err(|error| error.to_string())?;
    let workdir = repo.workdir().ok_or("workspace repository is bare")?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let workdir = workdir.canonicalize().map_err(|error| error.to_string())?;
    let relative = workspace
        .strip_prefix(&workdir)
        .map_err(|error| error.to_string())?;
    let runtime_path = relative.join(".rightkit/worktrees");
    let mut rule = String::from("/");
    for ch in runtime_path.to_string_lossy().replace('\\', "/").chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | ' ' | '#' | '!') {
            rule.push('\\');
        }
        rule.push(ch);
    }
    rule.push('/');
    let common = match std::fs::read_to_string(repo.path().join("commondir")) {
        Ok(path) => repo.path().join(path.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => repo.path().to_path_buf(),
        Err(error) => return Err(error.to_string()),
    };
    let exclude = common.join("info/exclude");
    std::fs::create_dir_all(exclude.parent().expect("info directory"))
        .map_err(|error| error.to_string())?;
    let existing = match std::fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string()),
    };
    if !existing.lines().any(|line| line == rule) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude)
            .map_err(|error| error.to_string())?;
        writeln!(file, "\n{rule}").map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub fn workspace_snapshot(
    workspace: &std::path::Path,
    max_status_lines: usize,
) -> Option<GitWorkspaceSnapshot> {
    let repo = Repository::discover(workspace).ok()?;
    let branch = repo
        .head()
        .ok()
        .and_then(|head| head.shorthand().ok().map(str::to_string))
        .unwrap_or_else(|| "HEAD".to_string());
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts)).ok()?;
    let truncated = statuses.len() > max_status_lines;
    let status_lines = statuses
        .iter()
        .take(max_status_lines)
        .map(|entry| {
            format!(
                "{} {}",
                short_status_code(entry.status()),
                entry.path().unwrap_or("?")
            )
        })
        .collect();
    Some(GitWorkspaceSnapshot {
        branch,
        status_lines,
        truncated,
    })
}

fn short_status_code(status: git2::Status) -> String {
    if status.contains(git2::Status::WT_NEW) && !status.contains(git2::Status::INDEX_NEW) {
        return "??".to_string();
    }
    let index = if status.contains(git2::Status::INDEX_NEW) {
        'A'
    } else if status.contains(git2::Status::INDEX_MODIFIED) {
        'M'
    } else if status.contains(git2::Status::INDEX_DELETED) {
        'D'
    } else if status.contains(git2::Status::INDEX_RENAMED) {
        'R'
    } else if status.contains(git2::Status::INDEX_TYPECHANGE) {
        'T'
    } else {
        ' '
    };
    let worktree = if status.contains(git2::Status::WT_MODIFIED) {
        'M'
    } else if status.contains(git2::Status::WT_DELETED) {
        'D'
    } else if status.contains(git2::Status::WT_RENAMED) {
        'R'
    } else if status.contains(git2::Status::WT_TYPECHANGE) {
        'T'
    } else {
        ' '
    };
    format!("{index}{worktree}")
}

fn summary(repo: &Repository, max_count: usize) -> Result<String, String> {
    let mut output = String::new();

    // Current branch
    match repo.head() {
        Ok(head) => {
            let branch = head.shorthand().unwrap_or("(detached HEAD)");
            output.push_str(&format!("Branch: {}\n\n", branch));
        }
        Err(e) => output.push_str(&format!("Branch: (error: {})\n\n", e)),
    }

    // Recent commits
    output.push_str("Recent commits:\n");
    if let Ok(mut revwalk) = repo.revwalk() {
        let _ = revwalk.push_head();
        revwalk.set_sorting(git2::Sort::TIME).ok();
        for oid in revwalk.take(max_count).flatten() {
            if let Ok(commit) = repo.find_commit(oid) {
                let short_id = &oid.to_string()[..7];
                let author = commit.author().name().unwrap_or("?").to_string();
                let subject = commit.summary().ok().flatten().unwrap_or("").to_string();
                output.push_str(&format!("  {} {} {}\n", short_id, author, subject));
            }
        }
    }
    output.push('\n');

    // Working tree status
    output.push_str("Status:\n");
    let mut status_opts = git2::StatusOptions::new();
    status_opts
        .include_untracked(true)
        .recurse_untracked_dirs(false);
    match repo.statuses(Some(&mut status_opts)) {
        Ok(statuses) => {
            if statuses.is_empty() {
                output.push_str("  (clean)\n");
            } else {
                for entry in statuses.iter().take(50) {
                    let status = entry.status();
                    let flag = status_flag(status);
                    let path = entry.path().unwrap_or("?");
                    output.push_str(&format!("  {} {}\n", flag, path));
                }
                if statuses.len() > 50 {
                    output.push_str(&format!("  ... and {} more\n", statuses.len() - 50));
                }
            }
        }
        Err(e) => output.push_str(&format!("  (error reading status: {})\n", e)),
    }

    Ok(output)
}

fn status(repo: &Repository) -> Result<String, String> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo
        .statuses(Some(&mut opts))
        .map_err(|error| format!("Failed to read status: {error}"))?;
    let entries: Vec<Value> = statuses
        .iter()
        .map(|entry| {
            json!({
                "status": status_flag(entry.status()),
                "path": entry.path().unwrap_or("?")
            })
        })
        .collect();
    Ok(json!({"entries": entries}).to_string())
}

fn log(repo: &Repository, reference: Option<&str>, max_count: usize) -> Result<String, String> {
    let mut walk = repo
        .revwalk()
        .map_err(|error| format!("Create revision walk: {error}"))?;
    if let Some(reference) = reference {
        let object = repo
            .revparse_single(reference)
            .map_err(|error| format!("Unknown ref '{reference}': {error}"))?;
        walk.push(object.id())
            .map_err(|error| format!("Walk ref '{reference}': {error}"))?;
    } else {
        walk.push_head()
            .map_err(|error| format!("Walk HEAD: {error}"))?;
    }
    let _ = walk.set_sorting(git2::Sort::TIME);
    let mut commits = Vec::new();
    for oid in walk.take(max_count) {
        let oid = oid.map_err(|error| format!("Read revision: {error}"))?;
        let commit = repo
            .find_commit(oid)
            .map_err(|error| format!("Read commit {oid}: {error}"))?;
        commits.push(json!({
            "id": oid.to_string(),
            "author": commit.author().name().unwrap_or("?"),
            "summary": commit.summary().ok().flatten().unwrap_or(""),
            "time": commit.time().seconds()
        }));
    }
    Ok(json!({"commits": commits}).to_string())
}

fn show(repo: &Repository, reference: Option<&str>, path: Option<&str>) -> Result<String, String> {
    let reference = reference.unwrap_or("HEAD");
    let object = repo
        .revparse_single(reference)
        .map_err(|error| format!("Unknown ref '{reference}': {error}"))?;
    let commit = object
        .peel_to_commit()
        .map_err(|error| format!("Ref '{reference}' is not a commit: {error}"))?;
    if let Some(path) = path {
        validate_repo_relative_path(path)?;
        let tree = commit
            .tree()
            .map_err(|error| format!("Read tree for '{reference}': {error}"))?;
        let entry = tree
            .get_path(std::path::Path::new(path))
            .map_err(|error| format!("Path '{path}' not found at '{reference}': {error}"))?;
        let blob = repo
            .find_blob(entry.id())
            .map_err(|error| format!("Path '{path}' is not a blob: {error}"))?;
        const MAX_SHOW_BYTES: usize = 256 * 1024;
        if blob.content().len() > MAX_SHOW_BYTES {
            return Err(format!(
                "Blob '{path}' is {} bytes; typed show limit is {MAX_SHOW_BYTES} bytes.",
                blob.content().len()
            ));
        }
        return Ok(String::from_utf8_lossy(blob.content()).into_owned());
    }
    Ok(json!({
        "id": commit.id().to_string(),
        "author": commit.author().name().unwrap_or("?"),
        "summary": commit.summary().ok().flatten().unwrap_or(""),
        "message": commit.message().unwrap_or(""),
        "parents": commit.parent_ids().map(|id| id.to_string()).collect::<Vec<_>>()
    })
    .to_string())
}

fn blame(repo: &Repository, path: Option<&str>) -> Result<String, String> {
    let path = path.ok_or_else(|| "git_read blame requires 'path'.".to_string())?;
    validate_repo_relative_path(path)?;
    let blame = repo
        .blame_file(std::path::Path::new(path), None)
        .map_err(|error| format!("Blame '{path}': {error}"))?;
    let hunks: Vec<Value> = blame
        .iter()
        .map(|hunk| {
            json!({
                "commit": hunk.final_commit_id().to_string(),
                "startLine": hunk.final_start_line(),
                "lines": hunk.lines_in_hunk()
            })
        })
        .collect();
    Ok(json!({"path": path, "hunks": hunks}).to_string())
}

fn branches(repo: &Repository) -> Result<String, String> {
    let mut rows = Vec::new();
    for branch in repo
        .branches(None)
        .map_err(|error| format!("List branches: {error}"))?
    {
        let (branch, kind) = branch.map_err(|error| format!("Read branch: {error}"))?;
        let target = branch.get().target();
        let (ahead, behind) = if kind == BranchType::Local {
            ahead_behind_upstream(repo, &branch)
        } else {
            (None, None)
        };
        rows.push(json!({
            "name": branch.name().ok().flatten().unwrap_or("?"),
            "kind": if kind == BranchType::Local { "local" } else { "remote" },
            "head": branch.is_head(),
            "target": target.map(|oid| oid.to_string()),
            "ahead": ahead,
            "behind": behind
        }));
    }
    Ok(json!({"branches": rows}).to_string())
}

/// Ahead/behind counts of a local branch relative to its configured upstream,
/// if any. Never touches the working tree or network — purely local ref math.
fn ahead_behind_upstream(
    repo: &Repository,
    branch: &git2::Branch<'_>,
) -> (Option<usize>, Option<usize>) {
    let local_target = match branch.get().target() {
        Some(target) => target,
        None => return (None, None),
    };
    let upstream = match branch.upstream() {
        Ok(upstream) => upstream,
        Err(_) => return (None, None),
    };
    let upstream_target = match upstream.get().target() {
        Some(target) => target,
        None => return (None, None),
    };
    match repo.graph_ahead_behind(local_target, upstream_target) {
        Ok((ahead, behind)) => (Some(ahead), Some(behind)),
        Err(_) => (None, None),
    }
}

/// ENG-45: repository discovery — identity, default branch, & remote list, all
/// read-only. Never mutates config or refs.
fn discover(repo: &Repository) -> Result<String, String> {
    let identity = repository_identity_path(repo);
    let head_branch = repo
        .head()
        .ok()
        .and_then(|head| head.shorthand().ok().map(str::to_string));
    let default_branch = default_branch_name(repo);
    let remotes = remote_rows(repo)?;
    Ok(json!({
        "path": identity,
        "isBare": repo.is_bare(),
        "headBranch": head_branch,
        "defaultBranch": default_branch,
        "remotes": remotes
    })
    .to_string())
}

fn remotes(repo: &Repository) -> Result<String, String> {
    Ok(json!({"remotes": remote_rows(repo)?}).to_string())
}

fn remote_rows(repo: &Repository) -> Result<Vec<Value>, String> {
    let names = repo
        .remotes()
        .map_err(|error| format!("List remotes: {error}"))?;
    let mut rows = Vec::new();
    for name in names.iter().flatten().flatten() {
        let remote = repo
            .find_remote(name)
            .map_err(|error| format!("Read remote '{name}': {error}"))?;
        rows.push(json!({
            "name": name,
            "url": remote
                .url()
                .ok()
                .map(crate::coding_tools::output_compaction::mask_secrets),
            "pushUrl": remote
                .pushurl()
                .ok()
                .flatten()
                .map(crate::coding_tools::output_compaction::mask_secrets)
        }));
    }
    Ok(rows)
}

/// Best-effort default branch: the remote HEAD symbolic ref for `origin` when
/// present, else the repository's own HEAD shorthand.
fn default_branch_name(repo: &Repository) -> Option<String> {
    if let Ok(reference) = repo.find_reference("refs/remotes/origin/HEAD") {
        if let Ok(Some(target)) = reference.symbolic_target() {
            return target
                .strip_prefix("refs/remotes/origin/")
                .map(str::to_string);
        }
    }
    repo.head()
        .ok()
        .and_then(|head| head.shorthand().ok().map(str::to_string))
}

pub(crate) fn repository_identity_path(repo: &Repository) -> String {
    repo.workdir()
        .unwrap_or_else(|| repo.path())
        .canonicalize()
        .unwrap_or_else(|_| repo.path().to_path_buf())
        .display()
        .to_string()
}

pub(crate) fn validate_repo_relative_path(path: &str) -> Result<(), String> {
    let path = std::path::Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.to_str().is_some_and(|path| {
            path.starts_with(':')
                || path
                    .chars()
                    .any(|character| matches!(character, '*' | '?' | '[' | ']'))
        })
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || path
            .components()
            .any(|component| component.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return Err(format!(
            "Git path '{path_display}' must be an exact repository-relative path without pathspec metacharacters and may not target .git.",
            path_display = path.display()
        ));
    }
    Ok(())
}

fn status_flag(status: git2::Status) -> &'static str {
    if status.contains(git2::Status::INDEX_NEW) || status.contains(git2::Status::WT_NEW) {
        "?"
    } else if status.contains(git2::Status::INDEX_MODIFIED)
        || status.contains(git2::Status::WT_MODIFIED)
    {
        "M"
    } else if status.contains(git2::Status::INDEX_DELETED)
        || status.contains(git2::Status::WT_DELETED)
    {
        "D"
    } else if status.contains(git2::Status::INDEX_RENAMED)
        || status.contains(git2::Status::WT_RENAMED)
    {
        "R"
    } else {
        " "
    }
}
