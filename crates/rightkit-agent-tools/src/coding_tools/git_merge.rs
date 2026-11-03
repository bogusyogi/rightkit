//! ENG-53: conflict/merge/rebase recovery. Every merge/rebase operation
//! surfaces conflicts explicitly (file + stage) rather than auto-resolving —
//! resolution is always a caller-directed `resolve_ours`/`resolve_theirs`/
//! `resolve_manual` step against an already-listed conflict path, or an
//! explicit `continue`/`abort`. Implemented through libgit2, never shell.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use git2::{MergeOptions, Repository};
use rightkit_agent_policy::{
    CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode,
};
use serde_json::{json, Value};
use std::path::Path;

pub struct GitMergeTool;

#[async_trait::async_trait]
impl Tool for GitMergeTool {
    fn name(&self) -> &str {
        "git_merge"
    }

    fn description(&self) -> &str {
        "Guarded Git merge/rebase with explicit conflict surfacing — conflicts are always listed \
         by path and stage, never auto-resolved. Supports ours/theirs/manual resolution per file, \
         continue, and abort."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": [
                        "merge_start", "merge_conflicts", "merge_continue", "merge_abort",
                        "rebase_start", "rebase_conflicts", "rebase_continue", "rebase_abort",
                        "resolve_ours", "resolve_theirs", "resolve_manual"
                    ]
                },
                "repo_path": { "type": "string" },
                "source": { "type": "string", "description": "Exact ref/commit to merge or rebase onto for *_start" },
                "path": { "type": "string", "description": "Exact repo-relative conflicted path for resolve_*" },
                "content": { "type": "string", "description": "Exact replacement content for resolve_manual" },
                "message": { "type": "string", "description": "Optional exact commit message for merge_continue" },
                "preview": { "type": "boolean", "description": "When true, *_start returns the would-be conflict set without applying it" }
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::GitWrite)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let operation = match args["operation"].as_str() {
            Some(
                operation @ ("merge_start" | "merge_conflicts" | "merge_continue" | "merge_abort"
                | "rebase_start" | "rebase_conflicts" | "rebase_continue"
                | "rebase_abort" | "resolve_ours" | "resolve_theirs"
                | "resolve_manual"),
            ) => operation,
            Some(other) => {
                return ToolResult::error(format!("Unknown git_merge operation '{other}'."))
            }
            None => return ToolResult::error("Missing required argument: operation"),
        };
        let read_only = matches!(operation, "merge_conflicts" | "rebase_conflicts");
        if !read_only {
            if ctx.permission_mode != PermissionMode::Bypass && !ctx.durable_write_approved {
                return ToolResult::error(format!(
                    "git_merge {operation} requires approval bound to this exact tool call."
                ));
            }
            let verdict = PermissionEngine::new().evaluate(
                CapabilityClass::GitWrite,
                ctx.permission_mode,
                ctx.workspace_trust,
                None,
                false,
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
        }

        let repo = match crate::coding_tools::git_read::open_workspace_repository(
            args["repo_path"].as_str(),
            ctx,
        ) {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };
        let result = match operation {
            "merge_start" => merge_start(&repo, &args),
            "merge_conflicts" => list_conflicts(&repo),
            "merge_continue" => merge_continue(&repo, &args),
            "merge_abort" => merge_abort(&repo),
            "rebase_start" => rebase_start(&repo, &args),
            "rebase_conflicts" => list_conflicts(&repo),
            "rebase_continue" => rebase_continue(&repo),
            "rebase_abort" => rebase_abort(&repo),
            "resolve_ours" => resolve_side(&repo, &args, true),
            "resolve_theirs" => resolve_side(&repo, &args, false),
            "resolve_manual" => resolve_manual(&repo, &args),
            _ => unreachable!(),
        };
        match result {
            Ok(effect) => {
                ToolResult::ok(json!({ "operation": operation, "effect": effect }).to_string())
            }
            Err(error) => ToolResult::error(error),
        }
    }
}

/// Every conflicted path in the index, with the stages present for it
/// (`ours`/`theirs`/`ancestor`). Never resolved implicitly — this is the
/// explicit surface a caller must read before choosing a resolution.
fn list_conflicts(repo: &Repository) -> Result<Value, String> {
    let index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    if !index.has_conflicts() {
        return Ok(json!({"conflicts": [], "hasConflicts": false}));
    }
    let mut conflicts = Vec::new();
    for conflict in index
        .conflicts()
        .map_err(|error| format!("Read conflicts: {error}"))?
    {
        let conflict = conflict.map_err(|error| format!("Read conflict entry: {error}"))?;
        let path = conflict
            .our
            .as_ref()
            .or(conflict.their.as_ref())
            .or(conflict.ancestor.as_ref())
            .map(|entry| String::from_utf8_lossy(&entry.path).to_string())
            .unwrap_or_default();
        conflicts.push(json!({
            "path": path,
            "stages": {
                "ancestor": conflict.ancestor.is_some(),
                "ours": conflict.our.is_some(),
                "theirs": conflict.their.is_some(),
            }
        }));
    }
    Ok(json!({"conflicts": conflicts, "hasConflicts": !conflicts.is_empty()}))
}

fn exact_source<'a>(args: &'a Value, field: &str) -> Result<&'a str, String> {
    args[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("git_merge requires exact '{field}'."))
}

/// ENG-53: start a merge. Never fast-forwards silently past a caller's
/// expectation and never auto-resolves — on conflict it writes the
/// conflicted index and stops, leaving `merge_conflicts` to enumerate it.
fn merge_start(repo: &Repository, args: &Value) -> Result<Value, String> {
    let source = exact_source(args, "source")?;
    let preview = args["preview"].as_bool().unwrap_or(false);
    let annotated = repo
        .find_annotated_commit(
            repo.revparse_single(source)
                .map_err(|error| format!("Unknown merge source '{source}': {error}"))?
                .id(),
        )
        .map_err(|error| format!("Resolve merge source '{source}': {error}"))?;
    let (analysis, _preference) = repo
        .merge_analysis(&[&annotated])
        .map_err(|error| format!("Merge analysis: {error}"))?;
    if analysis.is_up_to_date() {
        return Ok(json!({"status": "up_to_date", "conflicts": []}));
    }
    if preview {
        return Ok(json!({"status": "preview", "fastForward": analysis.is_fast_forward()}));
    }
    let mut options = MergeOptions::new();
    repo.merge(&[&annotated], Some(&mut options), None)
        .map_err(|error| format!("Merge '{source}': {error}"))?;
    let conflicts = list_conflicts(repo)?;
    let has_conflicts = conflicts["hasConflicts"].as_bool().unwrap_or(false);
    Ok(json!({
        "status": if has_conflicts { "conflicts" } else { "merged" },
        "conflicts": conflicts["conflicts"],
    }))
}

fn merge_continue(repo: &Repository, args: &Value) -> Result<Value, String> {
    let conflicts = list_conflicts(repo)?;
    if conflicts["hasConflicts"].as_bool().unwrap_or(false) {
        return Err(
            "Refusing merge_continue: unresolved conflicts remain. Resolve every path first."
                .to_string(),
        );
    }
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    let tree_id = index
        .write_tree()
        .map_err(|error| format!("Write merge tree: {error}"))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| format!("Read merge tree: {error}"))?;
    let signature = repo
        .signature()
        .map_err(|error| format!("Git author identity is not configured: {error}"))?;
    let head_commit = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| format!("Read HEAD commit: {error}"))?;
    let merge_head = repo
        .find_reference("MERGE_HEAD")
        .and_then(|reference| reference.peel_to_commit())
        .map_err(|error| format!("Read MERGE_HEAD: {error}"))?;
    let message = args["message"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Merge");
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &[&head_commit, &merge_head],
        )
        .map_err(|error| format!("Create merge commit: {error}"))?;
    repo.cleanup_state()
        .map_err(|error| format!("Clear merge state: {error}"))?;
    Ok(json!({"commit": oid.to_string()}))
}

/// Aborts a merge in progress by resetting the working tree/index to HEAD
/// and clearing repository merge state. Never touches history.
fn merge_abort(repo: &Repository) -> Result<Value, String> {
    let head_commit = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| format!("Read HEAD commit: {error}"))?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.reset(
        head_commit.as_object(),
        git2::ResetType::Hard,
        Some(&mut checkout),
    )
    .map_err(|error| format!("Reset to HEAD: {error}"))?;
    repo.cleanup_state()
        .map_err(|error| format!("Clear merge state: {error}"))?;
    Ok(json!({"aborted": true, "head": head_commit.id().to_string()}))
}

/// ENG-53: rebase using libgit2's native rebase machinery, one step at a
/// time, so a conflict at any step surfaces before the next step runs.
fn rebase_start(repo: &Repository, args: &Value) -> Result<Value, String> {
    let source = exact_source(args, "source")?;
    let preview = args["preview"].as_bool().unwrap_or(false);
    if preview {
        return Ok(json!({"status": "preview", "onto": source}));
    }
    let branch_annotated = repo
        .reference_to_annotated_commit(&repo.head().map_err(|error| format!("Read HEAD: {error}"))?)
        .map_err(|error| format!("Resolve HEAD annotated commit: {error}"))?;
    let onto_annotated = repo
        .find_annotated_commit(
            repo.revparse_single(source)
                .map_err(|error| format!("Unknown rebase target '{source}': {error}"))?
                .id(),
        )
        .map_err(|error| format!("Resolve rebase target '{source}': {error}"))?;
    let mut rebase = repo
        // `upstream` bounds the replayed set (commits in HEAD that are not in
        // `source`); with `upstream = None` libgit2 would replay every
        // reachable commit, re-applying already-shared history.
        .rebase(Some(&branch_annotated), Some(&onto_annotated), None, None)
        .map_err(|error| format!("Start rebase onto '{source}': {error}"))?;
    advance_rebase(repo, &mut rebase)
}

/// Advances an in-progress rebase step-by-step until it finishes or a step
/// produces conflicts, which stop the rebase in place (never skipped, never
/// auto-resolved) so `rebase_conflicts` can enumerate them.
fn advance_rebase(repo: &Repository, rebase: &mut git2::Rebase<'_>) -> Result<Value, String> {
    let signature = repo
        .signature()
        .map_err(|error| format!("Git author identity is not configured: {error}"))?;
    loop {
        match rebase.next() {
            None => {
                rebase
                    .finish(Some(&signature))
                    .map_err(|error| format!("Finish rebase: {error}"))?;
                return Ok(json!({"status": "completed"}));
            }
            Some(Err(error)) => return Err(format!("Rebase step: {error}")),
            Some(Ok(_operation)) => {
                let conflicts = list_conflicts(repo)?;
                if conflicts["hasConflicts"].as_bool().unwrap_or(false) {
                    return Ok(json!({
                        "status": "conflicts",
                        "conflicts": conflicts["conflicts"],
                    }));
                }
                rebase
                    .commit(None, &signature, None)
                    .map_err(|error| format!("Commit rebase step: {error}"))?;
            }
        }
    }
}

fn rebase_continue(repo: &Repository) -> Result<Value, String> {
    let conflicts = list_conflicts(repo)?;
    if conflicts["hasConflicts"].as_bool().unwrap_or(false) {
        return Err(
            "Refusing rebase_continue: unresolved conflicts remain. Resolve every path first."
                .to_string(),
        );
    }
    let mut rebase = repo
        .open_rebase(None)
        .map_err(|error| format!("Open in-progress rebase: {error}"))?;
    let signature = repo
        .signature()
        .map_err(|error| format!("Git author identity is not configured: {error}"))?;
    rebase
        .commit(None, &signature, None)
        .map_err(|error| format!("Commit rebase step: {error}"))?;
    advance_rebase(repo, &mut rebase)
}

fn rebase_abort(repo: &Repository) -> Result<Value, String> {
    let mut rebase = repo
        .open_rebase(None)
        .map_err(|error| format!("Open in-progress rebase: {error}"))?;
    rebase
        .abort()
        .map_err(|error| format!("Abort rebase: {error}"))?;
    Ok(json!({"aborted": true}))
}

/// ENG-53: resolve one conflicted path by taking the `ours` or `theirs`
/// side wholesale. Requires the exact path to already be a listed conflict —
/// never a pathspec, never "resolve everything".
fn resolve_side(repo: &Repository, args: &Value, ours: bool) -> Result<Value, String> {
    let path = exact_source(args, "path")?;
    crate::coding_tools::git_read::validate_repo_relative_path(path)?;
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    let conflict = index
        .conflict_get(Path::new(path))
        .map_err(|error| format!("Read conflict '{path}': {error}"))?;
    let (chosen, entry) = if ours {
        ("ours", conflict.our)
    } else {
        ("theirs", conflict.their)
    };
    let entry = entry.ok_or_else(|| {
        format!("Conflict '{path}' has no '{chosen}' side to resolve to (likely add/add or delete/modify).")
    })?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| "Cannot resolve conflicts in a bare repository.".to_string())?;
    let blob = repo
        .find_blob(entry.id)
        .map_err(|error| format!("Read '{chosen}' blob for '{path}': {error}"))?;
    std::fs::write(workdir.join(path), blob.content())
        .map_err(|error| format!("Write resolved '{path}': {error}"))?;
    index
        .remove_path(std::path::Path::new(path))
        .map_err(|error| format!("Clear conflict stages for '{path}': {error}"))?;
    index
        .add_path(std::path::Path::new(path))
        .map_err(|error| format!("Stage resolved '{path}': {error}"))?;
    index
        .write()
        .map_err(|error| format!("Write index: {error}"))?;
    Ok(json!({"path": path, "resolvedWith": chosen}))
}

/// ENG-53: resolve one conflicted path with caller-supplied exact content —
/// the manual-resolution path, still one named file at a time.
fn resolve_manual(repo: &Repository, args: &Value) -> Result<Value, String> {
    let path = exact_source(args, "path")?;
    crate::coding_tools::git_read::validate_repo_relative_path(path)?;
    let content = args["content"]
        .as_str()
        .ok_or_else(|| "git_merge resolve_manual requires exact 'content'.".to_string())?;
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    if index.conflict_get(Path::new(path)).is_err() {
        return Err(format!("'{path}' is not a listed conflict."));
    }
    let workdir = repo
        .workdir()
        .ok_or_else(|| "Cannot resolve conflicts in a bare repository.".to_string())?;
    std::fs::write(workdir.join(path), content)
        .map_err(|error| format!("Write resolved '{path}': {error}"))?;
    index
        .remove_path(std::path::Path::new(path))
        .map_err(|error| format!("Clear conflict stages for '{path}': {error}"))?;
    index
        .add_path(std::path::Path::new(path))
        .map_err(|error| format!("Stage resolved '{path}': {error}"))?;
    index
        .write()
        .map_err(|error| format!("Write index: {error}"))?;
    Ok(json!({"path": path, "resolvedWith": "manual"}))
}
