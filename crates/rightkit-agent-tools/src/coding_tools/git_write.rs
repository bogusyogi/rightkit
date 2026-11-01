//! Guarded Git mutations implemented through libgit2, never shell command
//! interpolation. Every operation is scoped to an exact repository and emits
//! a machine-readable approval/effect receipt.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use git2::{PushOptions, RemoteCallbacks, Repository};
use rightkit_agent_policy::{
    CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode,
};
use serde_json::{json, Value};

pub struct GitWriteTool;

#[async_trait::async_trait]
impl Tool for GitWriteTool {
    fn name(&self) -> &str {
        "git_write"
    }

    fn description(&self) -> &str {
        "Guarded Git stage, commit, and push with exact repository/path/ref/remote inputs and machine-readable approval/effect receipts. Force and wildcard pushes are refused."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": { "type": "string", "enum": ["stage", "unstage", "discard", "commit", "push", "init", "set_remote", "branch_create", "branch_switch", "branch_delete"] },
                "repo_path": { "type": "string" },
                "paths": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "message": { "type": "string" },
                "remote": { "type": "string" },
                "remote_url": { "type": "string", "description": "Exact URL for set_remote" },
                "refspec": { "type": "string", "description": "Exact non-force source:destination refspec" },
                "expected_remote_url": { "type": "string", "description": "Optional exact remote URL guard" },
                "branch": { "type": "string", "description": "Exact local branch name for branch_create/branch_switch/branch_delete" },
                "start_point": { "type": "string", "description": "Optional exact ref/commit branch_create starts from (default HEAD)" },
                "force": { "type": "boolean", "description": "branch_delete only: allow deleting an unmerged branch" },
                "preview": { "type": "boolean", "description": "When true, stage/unstage/discard return the effect without applying it" }
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::GitWrite)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let operation = match args["operation"].as_str() {
            Some(
                operation @ ("stage" | "unstage" | "discard" | "commit" | "push" | "init"
                | "set_remote" | "branch_create" | "branch_switch" | "branch_delete"),
            ) => operation,
            Some(other) => {
                return ToolResult::error(format!("Unknown git_write operation '{other}'."))
            }
            None => return ToolResult::error("Missing required argument: operation"),
        };
        if ctx.permission_mode != PermissionMode::Bypass && !ctx.durable_write_approved {
            return ToolResult::error(format!(
                "git_write {operation} requires approval bound to this exact tool call."
            ));
        }
        let verdict = PermissionEngine::new().evaluate(
            CapabilityClass::GitWrite,
            ctx.permission_mode,
            ctx.workspace_trust,
            None,
            operation == "push",
        );
        if verdict.decision == PermissionDecision::Deny {
            return ToolResult::error(format!(
                "Permission denied: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }
        if verdict.decision == PermissionDecision::Ask && !ctx.durable_write_approved {
            return ToolResult::error(format!(
                "Git operation requires approval: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        if operation == "init" {
            return match init_repository(args["repo_path"].as_str(), ctx) {
                Ok((repo, effect)) => ToolResult::ok(
                    json!({
                        "operation": operation,
                        "repository": repository_identity(&repo),
                        "approval": {
                            "permissionMode": format!("{:?}", ctx.permission_mode),
                            "explicit": ctx.durable_write_approved,
                            "policySource": verdict.policy_source
                        },
                        "effect": effect
                    })
                    .to_string(),
                ),
                Err(error) => ToolResult::error(error),
            };
        }

        let repo = match crate::coding_tools::git_read::open_workspace_repository(
            args["repo_path"].as_str(),
            ctx,
        ) {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };
        let result = match operation {
            "stage" => stage(&repo, &args),
            "unstage" => unstage(&repo, &args),
            "discard" => discard(&repo, &args),
            "commit" => commit(&repo, &args),
            "push" => push(&repo, &args, ctx.github_token.as_deref()),
            "set_remote" => set_remote(&repo, &args),
            "branch_create" => branch_create(&repo, &args),
            "branch_switch" => branch_switch(&repo, &args),
            "branch_delete" => branch_delete(&repo, &args),
            _ => unreachable!(),
        };
        match result {
            Ok(effect) => ToolResult::ok(
                json!({
                    "operation": operation,
                    "repository": repository_identity(&repo),
                    "approval": {
                        "permissionMode": format!("{:?}", ctx.permission_mode),
                        "explicit": ctx.durable_write_approved,
                        "policySource": verdict.policy_source
                    },
                    "effect": effect
                })
                .to_string(),
            ),
            Err(error) => ToolResult::error(error),
        }
    }
}

fn paths(args: &Value) -> Result<Vec<String>, String> {
    let values = args["paths"]
        .as_array()
        .ok_or_else(|| "git_write stage requires a non-empty 'paths' array.".to_string())?;
    if values.is_empty() {
        return Err("git_write stage requires a non-empty 'paths' array.".to_string());
    }
    let mut paths = Vec::with_capacity(values.len());
    for value in values {
        let path = value
            .as_str()
            .ok_or_else(|| "Every git_write path must be a string.".to_string())?;
        crate::coding_tools::git_read::validate_repo_relative_path(path)?;
        paths.push(path.to_string());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn stage(repo: &Repository, args: &Value) -> Result<Value, String> {
    let paths = paths(args)?;
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    for path in &paths {
        let workdir = repo
            .workdir()
            .ok_or_else(|| "Cannot stage paths in a bare repository.".to_string())?;
        let candidate = workdir.join(path);
        if candidate.exists() {
            index
                // `add_path` is intentionally exact. `add_all` interprets its
                // input as a pathspec, so a model-supplied `*.rs` could stage
                // siblings that were never named in the approval receipt.
                .add_path(std::path::Path::new(path))
                .map_err(|error| format!("Stage '{path}': {error}"))?;
        } else {
            index
                .remove_path(std::path::Path::new(path))
                .map_err(|error| format!("Stage deletion '{path}': {error}"))?;
        }
    }
    index
        .write()
        .map_err(|error| format!("Write index: {error}"))?;
    let diff = staged_diff(repo, &paths)?;
    Ok(json!({"paths": paths, "diff": diff}))
}

fn staged_diff(repo: &Repository, paths: &[String]) -> Result<String, String> {
    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let mut options = git2::DiffOptions::new();
    for path in paths {
        options.pathspec(path);
    }
    let diff = repo
        .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut options))
        .map_err(|error| format!("Read staged diff: {error}"))?;
    let mut output = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            output.push(line.origin() as u8);
        }
        output.extend_from_slice(line.content());
        output.len() <= 256 * 1024
    })
    .map_err(|error| format!("Format staged diff: {error}"))?;
    if output.len() > 256 * 1024 {
        output.truncate(256 * 1024);
        output.extend_from_slice(b"\n[staged diff truncated at 262144 bytes]");
    }
    Ok(crate::coding_tools::output_compaction::mask_secrets(
        &String::from_utf8_lossy(&output),
    ))
}

fn commit(repo: &Repository, args: &Value) -> Result<Value, String> {
    let message = args["message"]
        .as_str()
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .ok_or_else(|| "git_write commit requires a non-empty 'message'.".to_string())?;
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    if index.is_empty() {
        return Err("Refusing empty commit: the index has no entries.".to_string());
    }
    let tree_id = index
        .write_tree()
        .map_err(|error| format!("Write commit tree: {error}"))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| format!("Read commit tree: {error}"))?;
    let signature = repo
        .signature()
        .map_err(|error| format!("Git author identity is not configured: {error}"))?;
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    if parent.as_ref().map(|parent| parent.tree_id()) == Some(tree_id) {
        return Err("Refusing empty commit: index matches HEAD.".to_string());
    }
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .map_err(|error| format!("Create commit: {error}"))?;
    Ok(json!({
        "commit": oid.to_string(),
        "ref": repo.head().ok().and_then(|head| head.name().ok().map(str::to_string)),
        "message": message
    }))
}

fn push(repo: &Repository, args: &Value, github_token: Option<&str>) -> Result<Value, String> {
    let remote_name = args["remote"]
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "git_write push requires exact 'remote'.".to_string())?;
    let refspec = args["refspec"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "git_write push requires exact 'refspec'.".to_string())?;
    if refspec.starts_with('+') || refspec.starts_with(':') || refspec.contains('*') {
        return Err("Force, deletion, and wildcard push refspecs are refused.".to_string());
    }
    let (source, destination) = refspec
        .split_once(':')
        .ok_or_else(|| "Push refspec must name exact source:destination refs.".to_string())?;
    if !source.starts_with("refs/") || !destination.starts_with("refs/") {
        return Err(
            "Push refspec source and destination must both start with 'refs/'.".to_string(),
        );
    }
    let object = repo
        .revparse_single(source)
        .map_err(|error| format!("Unknown push source ref '{source}': {error}"))?;
    let mut remote = repo
        .find_remote(remote_name)
        .map_err(|error| format!("Unknown remote '{remote_name}': {error}"))?;
    let remote_url = remote
        .url()
        .map_err(|error| format!("Read remote '{remote_name}' URL: {error}"))?
        .to_string();
    if let Some(expected) = args["expected_remote_url"].as_str() {
        if expected != remote_url {
            return Err(format!(
                "Remote URL changed: expected '{expected}', current '{}'.",
                crate::coding_tools::output_compaction::mask_secrets(&remote_url)
            ));
        }
    }
    // A GitHub token is scoped to GitHub. Never offer it to an arbitrary URL
    // supplied by repository configuration (credential exfiltration sink).
    let github_token = github_token.filter(|_| {
        remote_url.starts_with("https://github.com/")
            || remote_url.starts_with("https://api.github.com/")
    });
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, username, allowed| {
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            if let Some(token) = github_token {
                return git2::Cred::userpass_plaintext("x-access-token", token);
            }
        }
        if allowed.contains(git2::CredentialType::SSH_KEY) {
            return username
                .map(git2::Cred::ssh_key_from_agent)
                .unwrap_or_else(|| {
                    Err(git2::Error::from_str("remote did not provide SSH username"))
                });
        }
        git2::Cred::default()
    });
    let mut options = PushOptions::new();
    options.remote_callbacks(callbacks);
    remote
        .push(&[refspec], Some(&mut options))
        .map_err(|error| format!("Push to '{remote_name}' failed: {error}"))?;
    Ok(json!({
        "remote": remote_name,
        "remoteUrl": crate::coding_tools::output_compaction::mask_secrets(&remote_url),
        "refspec": refspec,
        "sourceCommit": object.id().to_string()
    }))
}

/// ENG-45: `git init` a new repository inside the app-owned workspace root.
/// Refuses to escape the workspace the same way every other operation does,
/// by resolving through `resolve_in_workspace` before calling `git2::init`.
fn init_repository(
    requested: Option<&str>,
    ctx: &ToolContext,
) -> Result<(Repository, Value), String> {
    let target = crate::coding_tools::read_file::resolve_in_workspace(
        requested.unwrap_or("."),
        &ctx.workspace_root,
    )?;
    let canonical_root = ctx
        .workspace_root
        .canonicalize()
        .unwrap_or_else(|_| ctx.workspace_root.clone());
    // `resolve_in_workspace` already refuses `..` escapes; re-check containment
    // once the directory exists, matching the guard every other operation uses.
    std::fs::create_dir_all(&target)
        .map_err(|error| format!("Create '{}': {error}", target.display()))?;
    let canonical_target = target
        .canonicalize()
        .map_err(|error| format!("Resolve '{}': {error}", target.display()))?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(format!(
            "Init target '{}' escapes workspace root '{}'.",
            canonical_target.display(),
            canonical_root.display()
        ));
    }
    let repo = Repository::init(&target).map_err(|error| format!("git init: {error}"))?;
    let identity = crate::coding_tools::git_read::repository_identity_path(&repo);
    Ok((repo, json!({ "path": identity })))
}

/// ENG-45: set (or replace) a remote's URL explicitly. Never force, never
/// implicit — the caller must name both the remote and the exact URL.
fn set_remote(repo: &Repository, args: &Value) -> Result<Value, String> {
    let name = args["remote"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "git_write set_remote requires exact 'remote'.".to_string())?;
    let url = args["remote_url"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "git_write set_remote requires exact 'remote_url'.".to_string())?;
    if repo.find_remote(name).is_ok() {
        repo.remote_set_url(name, url)
            .map_err(|error| format!("Set remote '{name}' URL: {error}"))?;
    } else {
        repo.remote(name, url)
            .map_err(|error| format!("Create remote '{name}': {error}"))?;
    }
    Ok(json!({"remote": name, "url": crate::coding_tools::output_compaction::mask_secrets(url)}))
}

/// ENG-50: unstage exact paths — restores the index entry to match HEAD (or
/// removes it entirely for a path that was newly added), never a blanket reset.
fn unstage(repo: &Repository, args: &Value) -> Result<Value, String> {
    let requested = paths(args)?;
    let preview = args["preview"].as_bool().unwrap_or(false);
    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    if preview {
        return Ok(json!({"paths": requested, "preview": true, "applied": false}));
    }
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    for path in &requested {
        let path_ref = std::path::Path::new(path);
        match head_tree
            .as_ref()
            .and_then(|tree| tree.get_path(path_ref).ok())
        {
            Some(entry) => {
                index
                    .add(&git2::IndexEntry {
                        ctime: git2::IndexTime::new(0, 0),
                        mtime: git2::IndexTime::new(0, 0),
                        dev: 0,
                        ino: 0,
                        mode: entry.filemode() as u32,
                        uid: 0,
                        gid: 0,
                        file_size: 0,
                        id: entry.id(),
                        flags: 0,
                        flags_extended: 0,
                        path: path.as_bytes().to_vec(),
                    })
                    .map_err(|error| format!("Unstage '{path}': {error}"))?;
            }
            None => {
                index
                    .remove_path(path_ref)
                    .map_err(|error| format!("Unstage '{path}': {error}"))?;
            }
        }
    }
    index
        .write()
        .map_err(|error| format!("Write index: {error}"))?;
    Ok(json!({"paths": requested, "applied": true}))
}

/// ENG-50: discard exact working-tree paths back to the index (staged) content.
/// A path with no index entry is simply left alone rather than guessed at.
fn discard(repo: &Repository, args: &Value) -> Result<Value, String> {
    let requested = paths(args)?;
    let preview = args["preview"].as_bool().unwrap_or(false);
    if preview {
        return Ok(json!({"paths": requested, "preview": true, "applied": false}));
    }
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    for path in &requested {
        checkout.path(path.as_str());
    }
    repo.checkout_index(None, Some(&mut checkout))
        .map_err(|error| format!("Discard: {error}"))?;
    Ok(json!({"paths": requested, "applied": true}))
}

fn is_dirty(repo: &Repository) -> Result<bool, String> {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    Ok(!repo
        .statuses(Some(&mut options))
        .map_err(|error| format!("Read status: {error}"))?
        .is_empty())
}

fn exact_branch_name(args: &Value) -> Result<&str, String> {
    let branch = args["branch"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "git_write branch operations require exact 'branch'.".to_string())?;
    if !git2::Reference::is_valid_name(&format!("refs/heads/{branch}")) {
        return Err(format!("Invalid branch name '{branch}'."));
    }
    Ok(branch)
}

/// ENG-48: create a local branch at HEAD (or an explicit `start_point`)
/// without switching to it and without touching the working tree.
fn branch_create(repo: &Repository, args: &Value) -> Result<Value, String> {
    let branch = exact_branch_name(args)?;
    let start_point = args["start_point"].as_str().unwrap_or("HEAD");
    let target = repo
        .revparse_single(start_point)
        .map_err(|error| format!("Unknown start point '{start_point}': {error}"))?
        .peel_to_commit()
        .map_err(|error| format!("Start point '{start_point}' is not a commit: {error}"))?;
    repo.branch(branch, &target, false)
        .map_err(|error| format!("Create branch '{branch}': {error}"))?;
    Ok(json!({"branch": branch, "target": target.id().to_string()}))
}

/// ENG-48: switch HEAD to an existing local branch. Refuses on a dirty
/// working tree so an in-progress edit is never silently overwritten by a
/// safe checkout, and refuses when the checkout itself would hit a conflict.
fn branch_switch(repo: &Repository, args: &Value) -> Result<Value, String> {
    let branch = exact_branch_name(args)?;
    if is_dirty(repo)? {
        return Err("Refusing branch switch: working tree is dirty.".to_string());
    }
    repo.find_branch(branch, git2::BranchType::Local)
        .map_err(|error| format!("Unknown local branch '{branch}': {error}"))?;
    let refname = format!("refs/heads/{branch}");
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    repo.set_head(&refname)
        .map_err(|error| format!("Set HEAD to '{branch}': {error}"))?;
    repo.checkout_head(Some(&mut checkout)).map_err(|error| {
        format!("Checkout '{branch}' failed (likely conflict), HEAD may be inconsistent: {error}")
    })?;
    Ok(json!({"branch": branch}))
}

/// ENG-48: delete a local branch. Refuses an unmerged branch relative to HEAD
/// unless `force` is explicitly set, and refuses deleting the current branch.
fn branch_delete(repo: &Repository, args: &Value) -> Result<Value, String> {
    let branch = exact_branch_name(args)?;
    let force = args["force"].as_bool().unwrap_or(false);
    let mut target = repo
        .find_branch(branch, git2::BranchType::Local)
        .map_err(|error| format!("Unknown local branch '{branch}': {error}"))?;
    if target.is_head() {
        return Err(format!("Refusing to delete the current branch '{branch}'."));
    }
    if !force {
        if let (Some(branch_oid), Ok(head)) = (target.get().target(), repo.head()) {
            if let Some(head_oid) = head.target() {
                let merged = repo
                    .graph_descendant_of(head_oid, branch_oid)
                    .unwrap_or(false)
                    || head_oid == branch_oid;
                if !merged {
                    return Err(format!(
                        "Refusing to delete unmerged branch '{branch}' without force."
                    ));
                }
            }
        }
    }
    target
        .delete()
        .map_err(|error| format!("Delete branch '{branch}': {error}"))?;
    Ok(json!({"branch": branch, "deleted": true}))
}

fn repository_identity(repo: &Repository) -> String {
    repo.workdir()
        .unwrap_or_else(|| repo.path())
        .canonicalize()
        .unwrap_or_else(|_| repo.path().to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};

    fn init_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Agent Test").unwrap();
        config
            .set_str("user.email", "rightkit-agent@example.invalid")
            .unwrap();
        drop(config);
        (dir, repo)
    }

    fn ctx(root: &std::path::Path, mode: PermissionMode) -> ToolContext {
        ToolContext::new("git-write-test", root, mode, WorkspaceTrust::Trusted)
    }

    #[tokio::test]
    async fn stage_and_commit_emit_exact_receipts() {
        let (dir, _repo) = init_repo();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let tool = GitWriteTool;
        let staged = tool
            .execute(
                json!({"operation":"stage", "paths":["a.txt"]}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(!staged.is_error, "{}", staged.content);
        assert!(staged.content.contains("a.txt"));
        let committed = tool
            .execute(
                json!({"operation":"commit", "message":"test commit"}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(!committed.is_error, "{}", committed.content);
        assert!(committed.content.contains("test commit"));
    }

    #[tokio::test]
    async fn git_write_requires_bound_approval_outside_bypass() {
        let (dir, _repo) = init_repo();
        let result = GitWriteTool
            .execute(
                json!({"operation":"stage", "paths":["a.txt"]}),
                &ctx(dir.path(), PermissionMode::AutoSafe),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("requires approval"));
    }

    #[tokio::test]
    async fn push_rejects_force_and_wildcard_refspecs() {
        let (dir, _repo) = init_repo();
        for refspec in [
            "+refs/heads/main:refs/heads/main",
            "refs/heads/*:refs/heads/*",
        ] {
            let result = GitWriteTool
                .execute(
                    json!({"operation":"push", "remote":"origin", "refspec":refspec}),
                    &ctx(dir.path(), PermissionMode::Bypass),
                )
                .await;
            assert!(result.is_error);
            assert!(result.content.contains("refused"));
        }
    }

    #[tokio::test]
    async fn stage_treats_wildcard_as_literal_and_never_stages_siblings() {
        let (dir, repo) = init_repo();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();

        let wildcard = GitWriteTool
            .execute(
                json!({"operation":"stage", "paths":["*.txt"]}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(
            wildcard.is_error,
            "a nonexistent literal wildcard must fail"
        );
        assert_eq!(
            repo.index().unwrap().len(),
            0,
            "siblings must stay unstaged"
        );

        let exact = GitWriteTool
            .execute(
                json!({"operation":"stage", "paths":["a.txt"]}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(!exact.is_error, "{}", exact.content);
        let reopened = Repository::open(dir.path()).unwrap();
        let index = reopened.index().unwrap();
        assert!(index.get_path(std::path::Path::new("a.txt"), 0).is_some());
        assert!(index.get_path(std::path::Path::new("b.txt"), 0).is_none());
    }
}
