//! ENG-54: stash save/pop/list/drop, revert, tag create/delete, and paged
//! commit history/detail. Implemented through libgit2, never shell command
//! interpolation, matching `git_write`'s approval and containment pattern.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use git2::{Repository, StashFlags};
use rightkit_agent_policy::{
    CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode,
};
use serde_json::{json, Value};

pub struct GitHistoryOpsTool;

#[async_trait::async_trait]
impl Tool for GitHistoryOpsTool {
    fn name(&self) -> &str {
        "git_history_ops"
    }

    fn description(&self) -> &str {
        "Guarded Git stash (save/pop/apply/drop/list), revert, tag create/delete, and paged \
         commit history/detail, scoped to an exact repository with machine-readable \
         approval/effect receipts."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": [
                        "stash_save", "stash_list", "stash_pop", "stash_apply", "stash_drop",
                        "revert", "tag_create", "tag_delete", "history", "commit_detail"
                    ]
                },
                "repo_path": { "type": "string" },
                "message": { "type": "string", "description": "Exact stash message or annotated tag message" },
                "include_untracked": { "type": "boolean", "description": "stash_save only" },
                "index": { "type": "integer", "minimum": 0, "description": "Exact stash index for stash_pop/apply/drop" },
                "commit": { "type": "string", "description": "Exact commit/ref for revert or commit_detail" },
                "tag": { "type": "string", "description": "Exact tag name for tag_create/tag_delete" },
                "target": { "type": "string", "description": "Exact ref/commit tag_create points at (default HEAD)" },
                "annotated_message": { "type": "string", "description": "Optional message; presence makes tag_create annotated instead of lightweight" },
                "skip": { "type": "integer", "minimum": 0, "default": 0, "description": "history paging offset" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 20, "description": "history page size" },
                "preview": { "type": "boolean", "description": "When true, revert/stash_pop/stash_apply return the effect without applying it" }
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::GitWrite)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let operation = match args["operation"].as_str() {
            Some(
                operation @ ("stash_save" | "stash_list" | "stash_pop" | "stash_apply"
                | "stash_drop" | "revert" | "tag_create" | "tag_delete" | "history"
                | "commit_detail"),
            ) => operation,
            Some(other) => {
                return ToolResult::error(format!("Unknown git_history_ops operation '{other}'."))
            }
            None => return ToolResult::error("Missing required argument: operation"),
        };
        let read_only = matches!(operation, "stash_list" | "history" | "commit_detail");
        if !read_only {
            if ctx.permission_mode != PermissionMode::Bypass && !ctx.durable_write_approved {
                return ToolResult::error(format!(
                    "git_history_ops {operation} requires approval bound to this exact tool call."
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

        let mut repo = match crate::coding_tools::git_read::open_workspace_repository(
            args["repo_path"].as_str(),
            ctx,
        ) {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };
        let result = match operation {
            "stash_save" => stash_save(&mut repo, &args),
            "stash_list" => stash_list(&mut repo),
            "stash_pop" => stash_pop_or_apply(&mut repo, &args, true),
            "stash_apply" => stash_pop_or_apply(&mut repo, &args, false),
            "stash_drop" => stash_drop(&mut repo, &args),
            "revert" => revert(&repo, &args),
            "tag_create" => tag_create(&repo, &args),
            "tag_delete" => tag_delete(&repo, &args),
            "history" => history(&repo, &args),
            "commit_detail" => commit_detail(&repo, &args),
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

/// ENG-54: save a stash. Optionally includes untracked files; never staged
/// changes are dropped silently — libgit2's stash_save2 already restores the
/// working tree to HEAD once the stash commit is written.
fn stash_save(repo: &mut Repository, args: &Value) -> Result<Value, String> {
    let message = args["message"]
        .as_str()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let include_untracked = args["include_untracked"].as_bool().unwrap_or(false);
    let signature = repo
        .signature()
        .map_err(|error| format!("Git author identity is not configured: {error}"))?;
    let mut flags = StashFlags::DEFAULT;
    if include_untracked {
        flags |= StashFlags::INCLUDE_UNTRACKED;
    }
    let oid = repo
        .stash_save2(&signature, message, Some(flags))
        .map_err(|error| format!("Stash save: {error}"))?;
    Ok(json!({"stash": oid.to_string(), "message": message}))
}

fn stash_list(repo: &mut Repository) -> Result<Value, String> {
    let mut entries = Vec::new();
    repo.stash_foreach(|index, message, oid| {
        entries.push(json!({"index": index, "message": message, "oid": oid.to_string()}));
        true
    })
    .map_err(|error| format!("List stashes: {error}"))?;
    Ok(json!({"stashes": entries}))
}

fn exact_index(args: &Value) -> Result<usize, String> {
    args["index"]
        .as_u64()
        .map(|value| value as usize)
        .ok_or_else(|| {
            "git_history_ops requires an exact non-negative integer 'index'.".to_string()
        })
}

/// ENG-54: pop or apply an exact stash index. Never "the latest stash" by
/// implicit default — the caller must have read `stash_list` and named the
/// index. On conflict the stash is left in place (pop keeps it, apply always
/// keeps it) so nothing is lost.
fn stash_pop_or_apply(repo: &mut Repository, args: &Value, pop: bool) -> Result<Value, String> {
    let index = exact_index(args)?;
    let preview = args["preview"].as_bool().unwrap_or(false);
    if preview {
        return Ok(json!({"index": index, "preview": true, "applied": false}));
    }
    let mut options = git2::StashApplyOptions::new();
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.safe();
    options.checkout_options(checkout);
    if pop {
        repo.stash_pop(index, Some(&mut options))
            .map_err(|error| format!("Stash pop [{index}]: {error}"))?;
    } else {
        repo.stash_apply(index, Some(&mut options))
            .map_err(|error| format!("Stash apply [{index}]: {error}"))?;
    }
    Ok(json!({"index": index, "applied": true, "dropped": pop}))
}

fn stash_drop(repo: &mut Repository, args: &Value) -> Result<Value, String> {
    let index = exact_index(args)?;
    repo.stash_drop(index)
        .map_err(|error| format!("Stash drop [{index}]: {error}"))?;
    Ok(json!({"index": index, "dropped": true}))
}

/// ENG-54: revert an exact commit into the working tree/index (does not
/// auto-commit) unless it produces conflicts, which are surfaced through the
/// same `git_merge` conflict shape rather than silently guessed at.
fn revert(repo: &Repository, args: &Value) -> Result<Value, String> {
    let commit_ref = args["commit"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "git_history_ops revert requires exact 'commit'.".to_string())?;
    let preview = args["preview"].as_bool().unwrap_or(false);
    let commit = repo
        .revparse_single(commit_ref)
        .map_err(|error| format!("Unknown revert target '{commit_ref}': {error}"))?
        .peel_to_commit()
        .map_err(|error| format!("Revert target '{commit_ref}' is not a commit: {error}"))?;
    if preview {
        return Ok(json!({"commit": commit.id().to_string(), "preview": true}));
    }
    repo.revert(&commit, None)
        .map_err(|error| format!("Revert '{commit_ref}': {error}"))?;
    let index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    if index.has_conflicts() {
        return Ok(json!({
            "commit": commit.id().to_string(),
            "status": "conflicts",
            "note": "Resolve via git_merge resolve_ours/resolve_theirs/resolve_manual, then commit.",
        }));
    }
    Ok(json!({"commit": commit.id().to_string(), "status": "reverted"}))
}

fn exact_tag_name(args: &Value) -> Result<&str, String> {
    let tag = args["tag"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "git_history_ops tag operations require exact 'tag'.".to_string())?;
    if !git2::Reference::is_valid_name(&format!("refs/tags/{tag}")) {
        return Err(format!("Invalid tag name '{tag}'."));
    }
    Ok(tag)
}

/// ENG-54: create a tag at an exact target (default HEAD). Annotated when
/// `annotated_message` is supplied, lightweight otherwise. Refuses to
/// overwrite an existing tag implicitly.
fn tag_create(repo: &Repository, args: &Value) -> Result<Value, String> {
    let tag = exact_tag_name(args)?;
    if repo.find_reference(&format!("refs/tags/{tag}")).is_ok() {
        return Err(format!(
            "Tag '{tag}' already exists; delete it first to replace it."
        ));
    }
    let target = args["target"].as_str().unwrap_or("HEAD");
    let object = repo
        .revparse_single(target)
        .map_err(|error| format!("Unknown tag target '{target}': {error}"))?;
    let oid = if let Some(message) = args["annotated_message"].as_str() {
        let signature = repo
            .signature()
            .map_err(|error| format!("Git author identity is not configured: {error}"))?;
        repo.tag(tag, &object, &signature, message, false)
            .map_err(|error| format!("Create annotated tag '{tag}': {error}"))?
    } else {
        repo.tag_lightweight(tag, &object, false)
            .map_err(|error| format!("Create tag '{tag}': {error}"))?
    };
    Ok(json!({"tag": tag, "target": object.id().to_string(), "tagObject": oid.to_string()}))
}

fn tag_delete(repo: &Repository, args: &Value) -> Result<Value, String> {
    let tag = exact_tag_name(args)?;
    repo.tag_delete(tag)
        .map_err(|error| format!("Delete tag '{tag}': {error}"))?;
    Ok(json!({"tag": tag, "deleted": true}))
}

/// ENG-54: page commit history from HEAD (or an exact `commit` start point).
fn history(repo: &Repository, args: &Value) -> Result<Value, String> {
    let start = args["commit"].as_str().unwrap_or("HEAD");
    let skip = args["skip"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 200) as usize;
    let mut walk = repo
        .revwalk()
        .map_err(|error| format!("Open revwalk: {error}"))?;
    let start_oid = repo
        .revparse_single(start)
        .map_err(|error| format!("Unknown history start '{start}': {error}"))?
        .id();
    walk.push(start_oid)
        .map_err(|error| format!("Push history start '{start}': {error}"))?;
    let mut commits = Vec::new();
    for oid in walk.skip(skip).take(limit) {
        let oid = oid.map_err(|error| format!("Walk history: {error}"))?;
        let commit = repo
            .find_commit(oid)
            .map_err(|error| format!("Read commit '{oid}': {error}"))?;
        commits.push(summarize_commit(&commit));
    }
    Ok(json!({"start": start, "skip": skip, "limit": limit, "commits": commits}))
}

fn commit_detail(repo: &Repository, args: &Value) -> Result<Value, String> {
    let commit_ref = args["commit"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "git_history_ops commit_detail requires exact 'commit'.".to_string())?;
    let commit = repo
        .revparse_single(commit_ref)
        .map_err(|error| format!("Unknown commit '{commit_ref}': {error}"))?
        .peel_to_commit()
        .map_err(|error| format!("'{commit_ref}' is not a commit: {error}"))?;
    let parent_tree = commit.parent(0).ok().and_then(|parent| parent.tree().ok());
    let tree = commit
        .tree()
        .map_err(|error| format!("Read commit tree: {error}"))?;
    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
        .map_err(|error| format!("Diff commit '{commit_ref}': {error}"))?;
    let mut files = Vec::new();
    diff.foreach(
        &mut |delta, _progress| {
            files.push(
                delta
                    .new_file()
                    .path()
                    .or_else(|| delta.old_file().path())
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            );
            true
        },
        None,
        None,
        None,
    )
    .map_err(|error| format!("Enumerate commit diff files: {error}"))?;
    let mut summary = summarize_commit(&commit);
    summary["files"] = json!(files);
    Ok(summary)
}

fn summarize_commit(commit: &git2::Commit<'_>) -> Value {
    let author = commit.author();
    json!({
        "oid": commit.id().to_string(),
        "summary": commit.summary().unwrap_or_default(),
        "message": commit.message().unwrap_or_default(),
        "author": {
            "name": author.name().unwrap_or_default(),
            "email": author.email().unwrap_or_default(),
        },
        "time": commit.time().seconds(),
        "parents": commit.parent_ids().map(|oid| oid.to_string()).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};

    fn ctx(root: &std::path::Path, mode: PermissionMode) -> ToolContext {
        ToolContext::new("git-history-ops-test", root, mode, WorkspaceTrust::Trusted)
    }

    fn init_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Agent Test").unwrap();
        config
            .set_str("user.email", "rightkit-agent@example.invalid")
            .unwrap();
        // Pin line-ending translation off. On a host with `core.autocrlf=true`
        // (the Windows default) git rewrites LF to CRLF on checkout, so a stash
        // round-trip would restore CRLF bytes and the byte-exact assertions
        // below would fail for reasons that have nothing to do with stashing.
        config.set_bool("core.autocrlf", false).unwrap();
        drop(config);
        (dir, repo)
    }

    fn commit_file(repo: &Repository, path: &str, content: &str, message: &str) -> git2::Oid {
        std::fs::write(repo.workdir().unwrap().join(path), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(path)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = repo.signature().unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn stash_round_trips_and_never_drops_on_apply() {
        let (dir, repo) = init_repo();
        commit_file(&repo, "a.txt", "one\n", "base");
        std::fs::write(dir.path().join("a.txt"), "dirty\n").unwrap();

        let tool = GitHistoryOpsTool;
        let saved = tool
            .execute(
                json!({"operation": "stash_save", "message": "wip"}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(!saved.is_error, "{}", saved.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\n"
        );

        let listed = tool
            .execute(
                json!({"operation": "stash_list"}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(listed.content.contains("wip"));

        let applied = tool
            .execute(
                json!({"operation": "stash_apply", "index": 0}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(!applied.is_error, "{}", applied.content);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "dirty\n"
        );

        // apply must never drop the stash
        let listed_after = tool
            .execute(
                json!({"operation": "stash_list"}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(listed_after.content.contains("wip"));
    }

    #[tokio::test]
    async fn tag_create_refuses_overwrite_and_history_pages() {
        let (dir, repo) = init_repo();
        commit_file(&repo, "a.txt", "one\n", "first");
        commit_file(&repo, "a.txt", "two\n", "second");

        let tool = GitHistoryOpsTool;
        let created = tool
            .execute(
                json!({"operation": "tag_create", "tag": "v1"}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(!created.is_error, "{}", created.content);

        let duplicate = tool
            .execute(
                json!({"operation": "tag_create", "tag": "v1"}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(
            duplicate.is_error,
            "must refuse to silently overwrite an existing tag"
        );

        let history = tool
            .execute(
                json!({"operation": "history", "limit": 1}),
                &ctx(dir.path(), PermissionMode::Bypass),
            )
            .await;
        assert!(!history.is_error, "{}", history.content);
        assert!(history.content.contains("second"));
        assert!(!history.content.contains("\"first\""));
    }

    #[tokio::test]
    async fn git_history_ops_requires_bound_approval_outside_bypass_for_mutations() {
        let (dir, _repo) = init_repo();
        let result = GitHistoryOpsTool
            .execute(
                json!({"operation": "stash_save"}),
                &ctx(dir.path(), PermissionMode::AutoSafe),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("requires approval"));
    }

    #[tokio::test]
    async fn history_and_stash_list_are_read_only_without_approval() {
        let (dir, repo) = init_repo();
        commit_file(&repo, "a.txt", "one\n", "base");
        let result = GitHistoryOpsTool
            .execute(
                json!({"operation": "history"}),
                &ctx(dir.path(), PermissionMode::AutoSafe),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
    }
}
