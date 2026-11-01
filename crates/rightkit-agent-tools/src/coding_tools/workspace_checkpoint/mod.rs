//! `workspace_checkpoint` — whole-workspace snapshot/restore via a private
//! (unattached) git index, producing `write-tree`/`commit-tree` objects that
//! never touch the repository's real index, working `HEAD`, or refs.
//!
//! Donor lineage: cline `sdk/packages/core/src/hooks/checkpoint-hooks.ts` +
//! `sdk/packages/core/src/session/checkpoint-restore.ts` (VG-005). The donor
//! snapshots the whole workspace into a shadow git repository by staging into
//! a private index and calling `write-tree`/`commit-tree` without moving
//! `HEAD`, then restores transactionally: it resolves and validates the full
//! target tree in memory before touching a single file on disk, so a restore
//! either fully lands or leaves the workspace untouched.
//!
//! Rust adaptation notes:
//! - cline shells out to the `git` binary; this port uses `git2` (already a
//!   `host_tools` dependency) to call the equivalent plumbing
//!   (`Index::write_tree_to`, `Repository::commit` with `update_ref: None`)
//!   in-process, so the "private index" guarantee holds without a temp
//!   `GIT_INDEX_FILE` environment dance.
//! - Checkpoint metadata (commit oid, parent oid, label, timestamp) is
//!   persisted as one JSON file per checkpoint under
//!   `<workspace>/<state-dir>/checkpoints/<id>.json` rather than cline's
//!   session-scoped in-memory map, because the host app checkpoints must survive
//!   process restarts and be inspectable by the daemon.
//! - Transactional restore: the target tree is resolved and a full
//!   `git2::Index` is built from it in memory (`Index::read_tree`) before any
//!   `checkout_index` call touches the working directory. If tree resolution,
//!   corruption checks, or index construction fail, `execute` returns an
//!   error and the workspace is provably untouched (see
//!   `restore_is_transactional_on_missing_checkpoint` /
//!   `restore_never_touches_disk_when_tree_resolution_fails` tests). The
//!   actual filesystem mutation is the single `checkout_index` call, which is
//!   the smallest possible commit point, matching the donor's "validate then
//!   apply" contract.
//!
//! Not implemented (documented gap, no closure claim): true OS-level
//! filesystem transactionality (e.g. copy-on-write snapshot + atomic rename)
//! across the whole workdir. `checkout_index` is not atomic across many
//! files; a crash mid-checkout can leave a partially-restored tree. The donor
//! has the same limitation (it also drives a single `git checkout`). Callers
//! needing a hard atomicity guarantee should pair this with a
//! `WorkspaceCheckpointTool::create` snapshot immediately before restore so
//! recovery is always "restore forward to another checkpoint," never "repair
//! a half-restored tree by hand."

mod store;

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::file_service::WorkspaceRestoreProvenance;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use git2::{Index, IndexAddOption, ObjectType, Repository};
use rightkit_agent_policy::{CapabilityClass, PermissionDecision, PermissionEngine};
use serde_json::{json, Value};
use std::path::Path;

pub use store::{CheckpointRecord, CheckpointStore};

pub struct WorkspaceCheckpointTool;

#[async_trait::async_trait]
impl Tool for WorkspaceCheckpointTool {
    fn name(&self) -> &str {
        "workspace_checkpoint"
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::WriteFile)
    }

    fn capability_class_for(&self, args: &Value) -> Option<CapabilityClass> {
        match args.get("operation").and_then(Value::as_str) {
            Some("list") => Some(CapabilityClass::ReadFile),
            _ => Some(CapabilityClass::WriteFile),
        }
    }

    fn description(&self) -> &str {
        "Whole-workspace snapshot/restore via a private git index: create(label?) snapshots every tracked+untracked file into a write-tree/commit-tree object without moving HEAD; restore(checkpoint_id) transactionally re-applies a prior snapshot (validated fully in memory before any file is touched); list() enumerates snapshots. Fail: not a git repo, unknown checkpoint_id, corrupt tree."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["create", "restore", "restore_turn", "list"],
                    "description": "Operation to perform. restore_turn re-applies the automatic per-turn snapshot of session_id (latest, or the given turn); use it after a fork or resume."
                },
                "session_id": {
                    "type": "string",
                    "description": "restore_turn: session whose per-turn snapshot to restore (the source session after a fork)"
                },
                "turn": {
                    "type": "integer",
                    "description": "restore_turn: 1-based turn number; omit for the session's latest snapshot"
                },
                "repo_path": {
                    "type": "string",
                    "description": "Repository path (defaults to workspace root)"
                },
                "label": {
                    "type": "string",
                    "description": "Optional human-readable label for create"
                },
                "checkpoint_id": {
                    "type": "string",
                    "description": "Checkpoint id to restore"
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let operation = match args["operation"].as_str() {
            Some(op) => op,
            None => return ToolResult::error("Missing required argument: operation"),
        };

        let read_only = operation == "list";
        if let Err(error) = authorize(ctx, read_only) {
            return ToolResult::error(error);
        }

        let repo = match crate::coding_tools::git_read::open_workspace_repository(
            args["repo_path"].as_str(),
            ctx,
        ) {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };
        let workdir = match repo.workdir() {
            Some(dir) => dir.to_path_buf(),
            None => return ToolResult::error("Repository has no working directory (bare repo)"),
        };
        let store = CheckpointStore::new(&ctx.workspace_root);

        match operation {
            "create" => create_checkpoint(&repo, &store, args["label"].as_str()),
            "restore" => {
                let checkpoint_id = match args["checkpoint_id"].as_str() {
                    Some(id) => id,
                    None => return ToolResult::error("Missing required argument: checkpoint_id"),
                };
                restore_checkpoint(&repo, &workdir, &store, checkpoint_id, ctx)
            }
            "restore_turn" => {
                let session_id = match args["session_id"].as_str() {
                    Some(id) if !id.trim().is_empty() => id,
                    _ => return ToolResult::error("Missing required argument: session_id"),
                };
                let turn = args["turn"].as_u64();
                match find_turn_checkpoint(&store, session_id, turn) {
                    Ok(Some(record)) => {
                        restore_checkpoint(&repo, &workdir, &store, &record.id, ctx)
                    }
                    Ok(None) => ToolResult::error(match turn {
                        Some(turn) => format!(
                            "No per-turn checkpoint recorded for session {session_id} turn {turn}"
                        ),
                        None => format!("No per-turn checkpoint recorded for session {session_id}"),
                    }),
                    Err(error) => ToolResult::error(error),
                }
            }
            "list" => list_checkpoints(&store),
            other => ToolResult::error(format!("Unknown operation: {other}")),
        }
    }
}

fn authorize(ctx: &ToolContext, read_only: bool) -> Result<(), String> {
    let class = if read_only {
        CapabilityClass::ReadFile
    } else {
        CapabilityClass::WriteFile
    };
    let verdict = PermissionEngine::new().evaluate(
        class,
        ctx.permission_mode,
        ctx.workspace_trust,
        None,
        false,
    );
    match verdict.decision {
        PermissionDecision::Allow => Ok(()),
        PermissionDecision::Ask => Err(format!(
            "Tool requires approval before proceeding: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
        PermissionDecision::Deny => Err(format!(
            "Permission denied: {} (policy: {})",
            verdict.reason, verdict.policy_source
        )),
    }
}

/// Snapshot the entire workspace (tracked + untracked, respecting
/// `.gitignore`) into a `write-tree`/`commit-tree` object using a private,
/// unattached `git2::Index` so the repository's real index and `HEAD` are
/// Make checkpoint create/restore byte-exact: pin line-ending conversion off for
/// this repository handle only (an in-memory, highest-priority config layer; the
/// repository's own config on disk is never modified). Without this a global
/// `core.autocrlf=true` normalizes CRLF to LF on snapshot and re-expands on
/// restore, drifting file bytes.
fn pin_exact_bytes(repo: &Repository) {
    let path = repo.path().join("rightkit-agent-checkpoint.gitconfig");
    let body = "[core]
	autocrlf = false
	safecrlf = false
	eol = lf
";
    if std::fs::write(&path, body).is_err() {
        return;
    }
    if let Ok(mut config) = repo.config() {
        let _ = config.add_file(&path, git2::ConfigLevel::App, true);
        let _ = repo.set_config(&config);
    }
}

/// Hand the repository back its on-disk index after a private-index scan.
///
/// Best-effort by design: the real `.git/index` is never written during a
/// checkpoint, so even if re-attaching fails the user's staged state on disk
/// is intact — only this in-memory `Repository` handle would still point at
/// the throwaway index.
fn restore_real_index(repo: &Repository) {
    if let Ok(mut real_index) = Index::open(&repo.path().join("index")) {
        let _ = repo.set_index(&mut real_index);
    }
}

/// never touched. Mirrors cline `checkpoint-hooks.ts`'s shadow-commit shape.
fn create_checkpoint(
    repo: &Repository,
    store: &CheckpointStore,
    label: Option<&str>,
) -> ToolResult {
    // The "private index" the donor's shadow-repo trick relies on: writing to
    // it must not disturb whatever the user has staged in the real index.
    //
    // `Index::new()` alone cannot do this: an unattached index has no owning
    // repository, and `add_all` must walk a worktree, so libgit2 rejects it as
    // a bare-repo operation. Open a throwaway index file and attach it for the
    // scan instead, then hand the repository its real index back. The real
    // `.git/index` is never written, so the user's staged state is untouched
    // either way.
    pin_exact_bytes(repo);
    let private_index_path = repo.path().join("rightkit-agent-checkpoint.index");
    let _ = std::fs::remove_file(&private_index_path);
    let mut index = match Index::open(&private_index_path) {
        Ok(index) => index,
        Err(error) => return ToolResult::error(format!("Failed to create private index: {error}")),
    };
    if let Err(error) = repo.set_index(&mut index) {
        return ToolResult::error(format!("Failed to attach private index: {error}"));
    }

    // Reconcile the private index in both directions before write-tree:
    // `add_all` alone records additions/updates but leaves deleted tracked
    // paths behind, which would make a checkpoint resurrect files removed
    // from the workspace. `update_all` removes those absent paths while
    // leaving the repository's real index untouched.
    let tree_result = index
        .update_all(["*"].iter(), None)
        .and_then(|()| index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None))
        .and_then(|()| index.write_tree_to(repo));
    restore_real_index(repo);
    let _ = std::fs::remove_file(&private_index_path);

    let tree_oid = match tree_result {
        Ok(oid) => oid,
        Err(error) => {
            return ToolResult::error(format!(
                "Failed to stage workspace into private index: {error}"
            ))
        }
    };
    let tree = match repo.find_tree(tree_oid) {
        Ok(tree) => tree,
        Err(error) => return ToolResult::error(format!("Failed to resolve written tree: {error}")),
    };

    let parent_oid = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .map(|oid| oid.to_string());
    let parents: Vec<git2::Commit> =
        match repo.head().ok().and_then(|head| head.peel_to_commit().ok()) {
            Some(commit) => vec![commit],
            None => Vec::new(),
        };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let signature = match repo.signature().or_else(|_| {
        git2::Signature::now(
            "rightkit-agent-checkpoint",
            "checkpoint@rightkit-agent.local",
        )
    }) {
        Ok(signature) => signature,
        Err(error) => {
            return ToolResult::error(format!("Failed to build commit signature: {error}"))
        }
    };

    let message = match label {
        Some(label) => format!("checkpoint: {label}"),
        None => "checkpoint".to_string(),
    };

    // `update_ref: None` is the load-bearing detail: this commit object is
    // written to the object database but no branch/ref is moved, so HEAD and
    // the user's real branch state are untouched — a pure snapshot.
    let commit_oid = match repo.commit(None, &signature, &signature, &message, &tree, &parent_refs)
    {
        Ok(oid) => oid,
        Err(error) => return ToolResult::error(format!("commit-tree failed: {error}")),
    };

    let record = CheckpointRecord {
        id: commit_oid.to_string(),
        commit_oid: commit_oid.to_string(),
        parent_oid,
        label: label.map(str::to_string),
        created_at_unix: current_unix_time(),
    };

    if let Err(error) = store.append(&record) {
        return ToolResult::error(format!("Failed to persist checkpoint record: {error}"));
    }

    ToolResult::ok(format!(
        "Checkpoint created: {} (tree {})",
        record.id, tree_oid
    ))
}

/// Transactionally restore a prior checkpoint: fully resolve and validate
/// the target commit/tree and build the in-memory index from it *before*
/// performing the single `checkout_index` call that touches the working
/// directory. Any failure before that point leaves the workspace untouched.
fn restore_checkpoint(
    repo: &Repository,
    workdir: &Path,
    store: &CheckpointStore,
    checkpoint_id: &str,
    ctx: &ToolContext,
) -> ToolResult {
    let record = match store.find(checkpoint_id) {
        Ok(Some(record)) => record,
        Ok(None) => {
            return ToolResult::error(format!("Unknown checkpoint_id: {checkpoint_id}"));
        }
        Err(error) => {
            return ToolResult::error(format!("Failed to read checkpoint store: {error}"))
        }
    };

    let commit_oid = match git2::Oid::from_str(&record.commit_oid) {
        Ok(oid) => oid,
        Err(error) => {
            return ToolResult::error(format!("Corrupt checkpoint record (bad oid): {error}"))
        }
    };

    pin_exact_bytes(repo);
    // --- validation phase: no filesystem mutation below this point until
    // the checkout_index call. ---
    let commit = match repo.find_commit(commit_oid) {
        Ok(commit) => commit,
        Err(error) => {
            return ToolResult::error(format!(
                "Checkpoint commit missing from object database: {error}"
            ))
        }
    };
    let tree = match commit.tree() {
        Ok(tree) => tree,
        Err(error) => {
            return ToolResult::error(format!("Checkpoint tree missing or corrupt: {error}"))
        }
    };
    if let Err(error) = validate_tree_readable(repo, &tree) {
        return ToolResult::error(format!("Checkpoint tree failed validation: {error}"));
    }
    let mut index = match Index::new() {
        Ok(index) => index,
        Err(error) => return ToolResult::error(format!("Failed to create private index: {error}")),
    };
    if let Err(error) = index.read_tree(&tree) {
        return ToolResult::error(format!("Failed to build restore index from tree: {error}"));
    }

    // --- apply phase: the single point that can mutate the working
    // directory. It executes under FileService's exclusive workspace lease,
    // so ordinary write/edit/patch/move/delete work cannot interleave with
    // this privileged Git checkout.
    let provenance = WorkspaceRestoreProvenance {
        checkpoint_id: record.id.clone(),
        commit_oid: record.commit_oid.clone(),
    };
    match ctx.file_service.with_exclusive_workspace_restore(
        &ctx.workspace_root,
        provenance,
        || {
            let mut checkout = git2::build::CheckoutBuilder::new();
            checkout.force().remove_untracked(true).target_dir(workdir);
            repo.checkout_index(Some(&mut index), Some(&mut checkout))
                .map_err(|error| format!(
                    "Restore failed during checkout (workspace may be partially restored ? restore forward to another checkpoint): {error}"
                ))
        },
    ) {
        Ok(((), receipt)) => ToolResult::ok(
            json!({
                "message": format!("Restored checkpoint {} (commit {})", record.id, record.commit_oid),
                "restoreReceipt": receipt,
            })
            .to_string(),
        ),
        Err(failure) => {
            let exact = serde_json::to_string(&failure.receipt)
                .unwrap_or_else(|_| "<restore receipt serialization failed>".to_string());
            ToolResult::error(format!("{failure}. Exact restore receipt: {exact}"))
        }
    }
}

/// Walk every blob in the tree to confirm the object database actually has
/// the content before committing to a filesystem checkout. This is the
/// "resolve fully in memory before touching disk" half of the transactional
/// contract.
fn validate_tree_readable(repo: &Repository, tree: &git2::Tree) -> Result<(), git2::Error> {
    let mut result = Ok(());
    tree.walk(git2::TreeWalkMode::PreOrder, |_root, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            if let Some(oid) = entry.id().into() {
                if repo.find_blob(oid).is_err() {
                    result = Err(git2::Error::from_str(
                        "missing blob referenced by checkpoint tree",
                    ));
                    return git2::TreeWalkResult::Abort;
                }
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    result
}

/// Label of the automatic snapshot taken at the start of `turn` (1-based) of
/// `session_id`. The label is the durable link between a session turn and its
/// workspace snapshot, so a fork or a resume can find it by session + turn.
pub fn turn_checkpoint_label(session_id: &str, turn: u64) -> String {
    format!("turn:{session_id}:{turn}")
}

/// Take the automatic per-turn snapshot (same private-index mechanism as the
/// `create` operation: HEAD, the real index and refs are never touched).
///
/// Returns `Ok(None)` when `workspace_root` is not a non-bare git repository
/// with a working directory, so callers can enable the feature unconditionally.
/// Blocking; call from `spawn_blocking`.
pub fn snapshot_turn(
    workspace_root: &Path,
    session_id: &str,
    turn: u64,
) -> Result<Option<String>, String> {
    let repo = match Repository::open(workspace_root) {
        Ok(repo) => repo,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to open repository: {error}")),
    };
    if repo.is_bare() || repo.workdir().is_none() {
        return Ok(None);
    }
    let store = CheckpointStore::new(workspace_root);
    let label = turn_checkpoint_label(session_id, turn);
    let result = create_checkpoint(&repo, &store, Some(&label));
    if result.is_error {
        return Err(result.content);
    }
    // "Checkpoint created: <id> (tree <oid>)"
    Ok(result.content.split_whitespace().nth(2).map(str::to_string))
}

/// Find the per-turn snapshot of `session_id`: the exact `turn` when given,
/// otherwise the highest-numbered turn recorded for that session.
pub fn find_turn_checkpoint(
    store: &CheckpointStore,
    session_id: &str,
    turn: Option<u64>,
) -> Result<Option<CheckpointRecord>, String> {
    let prefix = format!("turn:{session_id}:");
    let mut best: Option<(u64, CheckpointRecord)> = None;
    for record in store.list()? {
        let Some(number) = record
            .label
            .as_deref()
            .and_then(|label| label.strip_prefix(&prefix))
            .and_then(|rest| rest.parse::<u64>().ok())
        else {
            continue;
        };
        if turn.is_some_and(|wanted| wanted != number) {
            continue;
        }
        // `list()` is oldest-first, so `>=` lets a later retake of the same
        // turn number win.
        if best
            .as_ref()
            .map_or(true, |(current, _)| number >= *current)
        {
            best = Some((number, record));
        }
    }
    Ok(best.map(|(_, record)| record))
}

/// Highest turn number already snapshotted for `session_id` (0 when none), so
/// a resumed session continues numbering instead of reusing turn 1.
pub fn latest_turn_number(workspace_root: &Path, session_id: &str) -> u64 {
    let prefix = format!("turn:{session_id}:");
    find_turn_checkpoint(&CheckpointStore::new(workspace_root), session_id, None)
        .ok()
        .flatten()
        .and_then(|record| {
            record
                .label
                .as_deref()
                .and_then(|label| label.strip_prefix(&prefix))
                .and_then(|rest| rest.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

/// Opt-in "auto-commit on exit": commit every change in the workspace to the
/// current branch.
///
/// Unlike a checkpoint this DOES move HEAD and write the real index, so it is
/// deliberately conservative and returns `Ok(None)` (no commit, no error)
/// when there is nothing to commit, the path is not a non-bare repository, or
/// HEAD is unborn; it returns `Err` and changes nothing when HEAD is detached
/// or a merge/rebase/cherry-pick/revert/bisect is in progress. Returns the
/// new commit id on success. Blocking; call from `spawn_blocking`.
pub fn auto_commit_workspace(
    workspace_root: &Path,
    message: &str,
) -> Result<Option<String>, String> {
    let repo = match Repository::open(workspace_root) {
        Ok(repo) => repo,
        Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(error) => return Err(format!("Failed to open repository: {error}")),
    };
    if repo.is_bare() || repo.workdir().is_none() {
        return Ok(None);
    }
    if repo.state() != git2::RepositoryState::Clean {
        return Err(format!(
            "repository has an operation in progress ({:?}); not auto-committing",
            repo.state()
        ));
    }
    if repo.head_detached().unwrap_or(false) {
        return Err("HEAD is detached; not auto-committing".to_string());
    }
    let parent = match repo.head().and_then(|head| head.peel_to_commit()) {
        Ok(commit) => commit,
        // Unborn HEAD: never create a repository's first commit implicitly.
        Err(_) => return Ok(None),
    };

    let mut index = repo
        .index()
        .map_err(|error| format!("Failed to open index: {error}"))?;
    index
        .update_all(["*"].iter(), None)
        .and_then(|()| index.add_all(["*"].iter(), IndexAddOption::DEFAULT, None))
        .and_then(|()| index.write())
        .map_err(|error| format!("Failed to stage workspace: {error}"))?;
    let tree_oid = index
        .write_tree()
        .map_err(|error| format!("Failed to write tree: {error}"))?;
    if parent.tree_id() == tree_oid {
        return Ok(None);
    }
    let tree = repo
        .find_tree(tree_oid)
        .map_err(|error| format!("Failed to resolve written tree: {error}"))?;
    let signature = repo
        .signature()
        .or_else(|_| git2::Signature::now("rightkit-agent", "auto-commit@rightkit-agent.local"))
        .map_err(|error| format!("Failed to build commit signature: {error}"))?;
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &[&parent],
        )
        .map_err(|error| format!("auto-commit failed: {error}"))?;
    Ok(Some(oid.to_string()))
}

fn list_checkpoints(store: &CheckpointStore) -> ToolResult {
    match store.list() {
        Ok(records) => {
            let payload = json!(records);
            ToolResult::ok(payload.to_string())
        }
        Err(error) => ToolResult::error(format!("Failed to list checkpoints: {error}")),
    }
}

fn current_unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::TempDir;

    fn init_repo(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "Test").unwrap();
            config.set_str("user.email", "test@example.com").unwrap();
        }
        repo
    }

    fn ctx_for(workspace: &Path) -> ToolContext {
        ToolContext::new(
            "test",
            workspace.to_path_buf(),
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
        )
    }

    #[tokio::test]
    async fn create_then_restore_round_trips_file_content() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        init_repo(workspace);
        std::fs::write(workspace.join("a.txt"), "v1").unwrap();

        let tool = WorkspaceCheckpointTool;
        let ctx = ctx_for(workspace);

        let create_result = tool
            .execute(json!({"operation": "create", "label": "before-edit"}), &ctx)
            .await;
        assert!(!create_result.is_error, "{}", create_result.content);
        let checkpoint_id = create_result
            .content
            .split_whitespace()
            .nth(2)
            .unwrap()
            .to_string();

        std::fs::write(workspace.join("a.txt"), "v2 - mutated after checkpoint").unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "v2 - mutated after checkpoint"
        );

        let restore_result = tool
            .execute(
                json!({"operation": "restore", "checkpoint_id": checkpoint_id}),
                &ctx,
            )
            .await;
        assert!(!restore_result.is_error, "{}", restore_result.content);
        assert_eq!(
            std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "v1"
        );
    }

    #[tokio::test]
    async fn create_does_not_move_head_or_current_branch() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        let repo = init_repo(workspace);
        std::fs::write(workspace.join("a.txt"), "v1").unwrap();
        // Establish a real HEAD commit via the real index, independent of
        // the tool's private index, so we can prove HEAD never moves.
        {
            let mut real_index = repo.index().unwrap();
            real_index.add_path(Path::new("a.txt")).unwrap();
            real_index.write().unwrap();
            let tree_oid = real_index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        let head_before = repo.head().unwrap().target().unwrap();

        let tool = WorkspaceCheckpointTool;
        let ctx = ctx_for(workspace);
        std::fs::write(workspace.join("b.txt"), "untracked-new-file").unwrap();
        let result = tool.execute(json!({"operation": "create"}), &ctx).await;
        assert!(!result.is_error, "{}", result.content);

        let repo_after = Repository::open(workspace).unwrap();
        let head_after = repo_after.head().unwrap().target().unwrap();
        assert_eq!(
            head_before, head_after,
            "checkpoint create must not move HEAD"
        );
    }

    #[tokio::test]
    async fn restore_rejects_unknown_checkpoint_id_without_touching_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        init_repo(workspace);
        std::fs::write(workspace.join("a.txt"), "untouched").unwrap();

        let tool = WorkspaceCheckpointTool;
        let ctx = ctx_for(workspace);
        let result = tool
            .execute(
                json!({"operation": "restore", "checkpoint_id": "deadbeef"}),
                &ctx,
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("Unknown checkpoint_id"));
        assert_eq!(
            std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "untouched"
        );
    }

    #[tokio::test]
    async fn list_reports_created_checkpoints() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        init_repo(workspace);
        std::fs::write(workspace.join("a.txt"), "v1").unwrap();

        let tool = WorkspaceCheckpointTool;
        let ctx = ctx_for(workspace);
        tool.execute(json!({"operation": "create", "label": "one"}), &ctx)
            .await;
        tool.execute(json!({"operation": "create", "label": "two"}), &ctx)
            .await;

        let result = tool.execute(json!({"operation": "list"}), &ctx).await;
        assert!(!result.is_error);
        assert!(result.content.contains("\"one\""));
        assert!(result.content.contains("\"two\""));
    }

    #[tokio::test]
    async fn restore_emits_provenance_and_workspace_generation_receipt() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path();
        init_repo(workspace);
        std::fs::write(workspace.join("a.txt"), "v1").unwrap();

        let tool = WorkspaceCheckpointTool;
        let ctx = ctx_for(workspace);
        let create_result = tool
            .execute(
                json!({"operation": "create", "label": "receipt-proof"}),
                &ctx,
            )
            .await;
        assert!(!create_result.is_error, "{}", create_result.content);
        let checkpoint_id = create_result
            .content
            .split_whitespace()
            .nth(2)
            .unwrap()
            .to_string();

        std::fs::write(workspace.join("a.txt"), "v2").unwrap();
        let before = ctx.file_service.workspace_generation_revision(workspace);
        assert!(before.observation_error.is_none());

        let restore_result = tool
            .execute(
                json!({"operation": "restore", "checkpoint_id": checkpoint_id}),
                &ctx,
            )
            .await;
        assert!(!restore_result.is_error, "{}", restore_result.content);
        let payload: Value = serde_json::from_str(&restore_result.content).unwrap();
        let receipt = &payload["restoreReceipt"];
        assert_eq!(receipt["provenance"]["checkpoint_id"], checkpoint_id);
        assert_eq!(receipt["provenance"]["commit_oid"], checkpoint_id);
        assert_eq!(receipt["succeeded"], true);
        let receipt_before_generation = receipt["before"]["generation"].as_u64().unwrap();
        let receipt_after_generation = receipt["after"]["generation"].as_u64().unwrap();
        assert!(receipt_before_generation >= before.generation);
        assert_eq!(receipt_after_generation, receipt_before_generation + 1);
        assert_ne!(receipt["before"]["revision"], receipt["after"]["revision"]);
        assert!(receipt["before"]["observation_error"].is_null());
        assert!(receipt["after"]["observation_error"].is_null());
        assert_eq!(
            std::fs::read_to_string(workspace.join("a.txt")).unwrap(),
            "v1"
        );
    }

    #[test]
    fn checkpoint_capability_is_operation_aware() {
        let tool = WorkspaceCheckpointTool;
        assert_eq!(
            tool.capability_class_for(&json!({"operation": "list"})),
            Some(CapabilityClass::ReadFile)
        );
        for operation in ["create", "restore"] {
            assert_eq!(
                tool.capability_class_for(&json!({"operation": operation})),
                Some(CapabilityClass::WriteFile)
            );
        }
    }
}

#[cfg(test)]
mod turn_checkpoint_tests {
    use super::*;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use tempfile::TempDir;

    fn init_repo_with_head(dir: &Path) -> Repository {
        let repo = Repository::init(dir).unwrap();
        {
            let mut config = repo.config().unwrap();
            config.set_str("user.name", "Test").unwrap();
            config.set_str("user.email", "test@example.com").unwrap();
        }
        std::fs::write(dir.join("a.txt"), "v0").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("a.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
                .unwrap();
        }
        repo
    }

    fn ctx_for(workspace: &Path) -> ToolContext {
        ToolContext::new(
            "test",
            workspace.to_path_buf(),
            PermissionMode::AcceptEdits,
            WorkspaceTrust::Trusted,
        )
    }

    #[test]
    fn snapshot_turn_outside_a_git_repo_is_a_no_op() {
        let temp = TempDir::new().unwrap();
        assert_eq!(snapshot_turn(temp.path(), "s1", 1).unwrap(), None);
    }

    #[tokio::test]
    async fn per_turn_snapshots_restore_by_session_and_turn_and_do_not_move_head() {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();
        let repo = init_repo_with_head(ws);
        let head_before = repo.head().unwrap().target().unwrap();

        std::fs::write(ws.join("a.txt"), "turn-1 state").unwrap();
        let first = snapshot_turn(ws, "sess", 1).unwrap().expect("snapshot 1");
        std::fs::write(ws.join("a.txt"), "turn-2 state").unwrap();
        let second = snapshot_turn(ws, "sess", 2).unwrap().expect("snapshot 2");
        assert_ne!(first, second);
        std::fs::write(ws.join("a.txt"), "scratch after turn 2").unwrap();
        // A different session's snapshot must never be picked up.
        std::fs::write(ws.join("a.txt"), "other session").unwrap();
        snapshot_turn(ws, "other", 7)
            .unwrap()
            .expect("other snapshot");
        std::fs::write(ws.join("a.txt"), "scratch after turn 2").unwrap();

        assert_eq!(repo.head().unwrap().target().unwrap(), head_before);

        let tool = WorkspaceCheckpointTool;
        let ctx = ctx_for(ws);

        // Latest snapshot of the session (a resumed session restores this).
        let latest = tool
            .execute(
                json!({"operation": "restore_turn", "session_id": "sess"}),
                &ctx,
            )
            .await;
        assert!(!latest.is_error, "{}", latest.content);
        assert_eq!(
            std::fs::read_to_string(ws.join("a.txt")).unwrap(),
            "turn-2 state"
        );

        // An explicit earlier turn, from a forked session naming its source.
        let earlier = tool
            .execute(
                json!({"operation": "restore_turn", "session_id": "sess", "turn": 1}),
                &ctx,
            )
            .await;
        assert!(!earlier.is_error, "{}", earlier.content);
        assert_eq!(
            std::fs::read_to_string(ws.join("a.txt")).unwrap(),
            "turn-1 state"
        );

        // Records survive a restore to an earlier turn (they are ignored, so
        // the restore neither captured nor deleted them).
        let again = tool
            .execute(
                json!({"operation": "restore_turn", "session_id": "sess", "turn": 2}),
                &ctx,
            )
            .await;
        assert!(!again.is_error, "{}", again.content);
        assert_eq!(
            std::fs::read_to_string(ws.join("a.txt")).unwrap(),
            "turn-2 state"
        );
        assert_eq!(repo.head().unwrap().target().unwrap(), head_before);
    }

    #[tokio::test]
    async fn restore_turn_reports_unknown_session_and_turn_without_touching_files() {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();
        init_repo_with_head(ws);
        std::fs::write(ws.join("a.txt"), "keep me").unwrap();
        snapshot_turn(ws, "sess", 1).unwrap().expect("snapshot");

        let tool = WorkspaceCheckpointTool;
        let ctx = ctx_for(ws);
        let unknown_session = tool
            .execute(
                json!({"operation": "restore_turn", "session_id": "nope"}),
                &ctx,
            )
            .await;
        assert!(unknown_session.is_error);
        let unknown_turn = tool
            .execute(
                json!({"operation": "restore_turn", "session_id": "sess", "turn": 9}),
                &ctx,
            )
            .await;
        assert!(unknown_turn.is_error);
        let missing_session = tool
            .execute(json!({"operation": "restore_turn"}), &ctx)
            .await;
        assert!(missing_session.is_error);
        assert_eq!(
            std::fs::read_to_string(ws.join("a.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn latest_turn_number_continues_numbering_for_a_resumed_session() {
        let temp = TempDir::new().unwrap();
        assert_eq!(latest_turn_number(temp.path(), "s"), 0);
        let store = CheckpointStore::new(temp.path());
        for (id, label) in [("b1", "turn:s:4"), ("b2", "turn:s:12"), ("b3", "turn:t:50")] {
            store
                .append(&CheckpointRecord {
                    id: id.to_string(),
                    commit_oid: id.to_string(),
                    parent_oid: None,
                    label: Some(label.to_string()),
                    created_at_unix: 1,
                })
                .unwrap();
        }
        assert_eq!(latest_turn_number(temp.path(), "s"), 12);
        assert_eq!(latest_turn_number(temp.path(), "t"), 50);
    }

    #[test]
    fn find_turn_checkpoint_prefers_highest_turn_and_ignores_other_labels() {
        let temp = TempDir::new().unwrap();
        let store = CheckpointStore::new(temp.path());
        for (id, label, at) in [
            ("a1", Some("turn:s:2"), 10),
            ("a2", Some("turn:s:10"), 11),
            ("a3", Some("turn:s:3"), 12),
            ("a4", Some("manual"), 13),
            ("a5", Some("turn:t:99"), 14),
            ("a6", None, 15),
        ] {
            store
                .append(&CheckpointRecord {
                    id: id.to_string(),
                    commit_oid: id.to_string(),
                    parent_oid: None,
                    label: label.map(str::to_string),
                    created_at_unix: at,
                })
                .unwrap();
        }
        let latest = find_turn_checkpoint(&store, "s", None).unwrap().unwrap();
        assert_eq!(latest.id, "a2", "numeric, not lexicographic, turn order");
        let exact = find_turn_checkpoint(&store, "s", Some(3)).unwrap().unwrap();
        assert_eq!(exact.id, "a3");
        assert!(find_turn_checkpoint(&store, "s", Some(4))
            .unwrap()
            .is_none());
        assert!(find_turn_checkpoint(&store, "nobody", None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn auto_commit_commits_changes_once_and_is_a_no_op_when_clean() {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();
        let repo = init_repo_with_head(ws);
        let before = repo.head().unwrap().target().unwrap();

        // Nothing changed: no commit.
        assert_eq!(auto_commit_workspace(ws, "auto").unwrap(), None);
        assert_eq!(repo.head().unwrap().target().unwrap(), before);

        std::fs::write(ws.join("a.txt"), "changed").unwrap();
        std::fs::write(ws.join("new.txt"), "added").unwrap();
        let oid = auto_commit_workspace(ws, "rightkit-agent: auto-commit on exit")
            .unwrap()
            .expect("a commit");
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.id().to_string(), oid);
        assert_eq!(head.parent_id(0).unwrap(), before);
        assert_eq!(
            head.message().unwrap(),
            "rightkit-agent: auto-commit on exit"
        );
        let tree = head.tree().unwrap();
        assert!(tree.get_name("new.txt").is_some());

        // Idempotent: the second call finds nothing new.
        assert_eq!(auto_commit_workspace(ws, "auto").unwrap(), None);
    }

    #[test]
    fn auto_commit_refuses_detached_head_and_skips_non_repos() {
        let plain = TempDir::new().unwrap();
        assert_eq!(auto_commit_workspace(plain.path(), "auto").unwrap(), None);

        let temp = TempDir::new().unwrap();
        let ws = temp.path();
        let repo = init_repo_with_head(ws);
        let head = repo.head().unwrap().target().unwrap();
        repo.set_head_detached(head).unwrap();
        std::fs::write(ws.join("a.txt"), "changed").unwrap();
        assert!(auto_commit_workspace(ws, "auto").is_err());
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
    }

    #[test]
    fn auto_commit_skips_an_unborn_head() {
        let temp = TempDir::new().unwrap();
        let ws = temp.path();
        Repository::init(ws).unwrap();
        std::fs::write(ws.join("a.txt"), "x").unwrap();
        assert_eq!(auto_commit_workspace(ws, "auto").unwrap(), None);
    }
}
