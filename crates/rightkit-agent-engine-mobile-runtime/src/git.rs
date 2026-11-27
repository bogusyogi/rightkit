//! Typed Git transport for iOS host UI. No shell, URL credentials, force
//! operations, or wildcard pathspecs. Merges and rebases run only through the
//! explicit `merge` and `rebase` operations (with their continue and abort
//! pairs), never implicitly and never forced.

use git2::build::CheckoutBuilder;
use git2::{
    build::RepoBuilder, BranchType, FetchOptions, PushOptions, RemoteCallbacks, Repository,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use url::Url;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRequest {
    pub operation: String,
    pub workspace_root: PathBuf,
    pub remote_url: Option<String>,
    pub token: Option<String>,
    pub paths: Option<Vec<String>>,
    pub message: Option<String>,
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub branch: Option<String>,
    pub remote: Option<String>,
    pub force: Option<bool>,
    /// ENG-55: hard byte ceiling for `lfsFetch`. Required for that operation;
    /// there is no default, so a caller can never trigger an unbounded LFS
    /// download by omission.
    pub max_bytes: Option<u64>,
    /// Candidate threshold for `lfsCandidates`. Defaults to 5 MiB.
    pub min_bytes: Option<u64>,
    /// Commit id for `revert`, `commitDetail` and `tagCreate`.
    pub sha: Option<String>,
    /// Tag name for the tag operations.
    pub tag: Option<String>,
    /// Commits to skip for `log`.
    pub skip: Option<usize>,
    /// Commits to return for `log` (clamped to 1..=100, default 30).
    pub limit: Option<usize>,
    /// `ours`, `theirs` or `manual` for `resolve`.
    pub resolution: Option<String>,
    /// Replacement text for a `manual` `resolve`.
    pub content: Option<String>,
    /// Stash entry for `stashApply` and `stashDrop`.
    pub stash_index: Option<usize>,
    /// Unified diff for `stageHunk`.
    pub patch: Option<String>,
    /// Branch to rebase onto.
    pub onto: Option<String>,
    /// Whether `stashCreate` also stashes untracked files.
    pub include_untracked: Option<bool>,
    /// Caller-chosen id that lets `progress` and `cancel` address a running
    /// `clone`, `fetch`, `pull` or `push` from a second call.
    pub operation_id: Option<String>,
    /// Simulator fixture only: `http://127.0.0.1:<port>/` replaces the
    /// `https://github.com/` network endpoint. Anything else is refused.
    pub loopback_base: Option<String>,
}

const OPERATION_PHASE_RECEIVING: u8 = 1;
const OPERATION_PHASE_RESOLVING: u8 = 2;

/// Live state of one network operation, shared between the thread running it
/// and the non-blocking `progress` and `cancel` calls.
#[derive(Default)]
struct OperationState {
    cancel: AtomicBool,
    received: AtomicU64,
    total: AtomicU64,
    bytes: AtomicU64,
    phase: AtomicU8,
}

static OPERATIONS: OnceLock<Mutex<HashMap<String, Arc<OperationState>>>> = OnceLock::new();

fn operations() -> std::sync::MutexGuard<'static, HashMap<String, Arc<OperationState>>> {
    OPERATIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Removes the operation from the registry on every exit path.
struct OperationGuard {
    id: String,
    state: Arc<OperationState>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        operations().remove(&self.id);
    }
}

fn register_operation(raw: Option<&str>) -> Result<Option<OperationGuard>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let id = raw.trim();
    if id.is_empty() || id.len() > 128 {
        return Err("Invalid operationId.".into());
    }
    let state = Arc::new(OperationState::default());
    let mut registry = operations();
    if registry.contains_key(id) {
        return Err("Operation id is already in use.".into());
    }
    registry.insert(id.to_owned(), state.clone());
    Ok(Some(OperationGuard {
        id: id.to_owned(),
        state,
    }))
}

fn operation_progress(raw: Option<&str>) -> Result<Value, String> {
    let id = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "progress requires operationId.".to_string())?;
    let state = operations().get(id).cloned();
    Ok(match state {
        Some(state) => json!({
            "phase": match state.phase.load(Ordering::SeqCst) {
                OPERATION_PHASE_RECEIVING => "receiving",
                OPERATION_PHASE_RESOLVING => "resolving",
                _ => "idle",
            },
            "received": state.received.load(Ordering::SeqCst),
            "total": state.total.load(Ordering::SeqCst),
            "bytes": state.bytes.load(Ordering::SeqCst)
        }),
        None => json!({"phase": "idle", "received": 0, "total": 0, "bytes": 0}),
    })
}

fn operation_cancel(raw: Option<&str>) -> Result<Value, String> {
    let id = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "cancel requires operationId.".to_string())?;
    let state = operations().get(id).cloned();
    Ok(match state {
        Some(state) => {
            state.cancel.store(true, Ordering::SeqCst);
            json!({"cancelled": true})
        }
        None => json!({"cancelled": false}),
    })
}

/// Maps a libgit2 failure to a message. A callback that returned `false` makes
/// libgit2 abort, and the cancel flag is the authority on why it did.
fn transfer_error(guard: Option<&OperationGuard>, label: &str, error: git2::Error) -> String {
    let cancelled = error.code() == git2::ErrorCode::User
        || guard.is_some_and(|guard| guard.state.cancel.load(Ordering::SeqCst));
    if cancelled {
        "Cancelled by user.".to_string()
    } else {
        format!("{label}: {error}")
    }
}

/// Store fetch progress and return `!cancel` from the callback so libgit2 aborts.
fn attach_progress(callbacks: &mut RemoteCallbacks<'static>, state: Option<&Arc<OperationState>>) {
    let Some(state) = state else {
        return;
    };
    let state = state.clone();
    callbacks.transfer_progress(move |progress| {
        state
            .received
            .store(progress.received_objects() as u64, Ordering::SeqCst);
        state
            .total
            .store(progress.total_objects() as u64, Ordering::SeqCst);
        state
            .bytes
            .store(progress.received_bytes() as u64, Ordering::SeqCst);
        state.phase.store(
            if progress.indexed_deltas() > 0 {
                OPERATION_PHASE_RESOLVING
            } else {
                OPERATION_PHASE_RECEIVING
            },
            Ordering::SeqCst,
        );
        !state.cancel.load(Ordering::SeqCst)
    });
}

/// Push progress and rejection capture. `push_update_reference` reports a
/// server refusal per reference, which libgit2 otherwise returns as success.
fn attach_push_progress(
    callbacks: &mut RemoteCallbacks<'static>,
    state: Option<&Arc<OperationState>>,
    rejection: Arc<Mutex<Option<String>>>,
) {
    if let Some(state) = state {
        let progress_state = state.clone();
        callbacks.push_transfer_progress(move |current, total, bytes| {
            progress_state
                .received
                .store(current as u64, Ordering::SeqCst);
            progress_state.total.store(total as u64, Ordering::SeqCst);
            progress_state.bytes.store(bytes as u64, Ordering::SeqCst);
            progress_state
                .phase
                .store(OPERATION_PHASE_RECEIVING, Ordering::SeqCst);
        });
        let negotiation_state = state.clone();
        callbacks.push_negotiation(move |_| {
            if negotiation_state.cancel.load(Ordering::SeqCst) {
                Err(git2::Error::from_str("Cancelled by user."))
            } else {
                Ok(())
            }
        });
    }
    let reference_state = state.cloned();
    callbacks.push_update_reference(move |_, status| {
        if reference_state
            .as_ref()
            .is_some_and(|state| state.cancel.load(Ordering::SeqCst))
        {
            return Err(git2::Error::from_str("Cancelled by user."));
        }
        if let Some(reason) = status {
            let mut slot = rejection
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if slot.is_none() {
                *slot = Some(reason.to_owned());
            }
        }
        Ok(())
    });
}

/// `http://127.0.0.1:<port>/` exactly, or an error. Nothing else is ever
/// honoured, and nothing at all unless the host turned on
/// [`crate::host_options::set_git_loopback_fixture`], so this can never widen where a
/// credential may be sent.
fn validated_loopback(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if !crate::host_options::git_loopback_fixture() {
        return Err("Loopback git fixtures are not enabled in this build.".into());
    }
    let invalid = || "Invalid loopback base.".to_string();
    let parsed = Url::parse(raw).map_err(|_| invalid())?;
    let port = parsed.port().ok_or_else(invalid)?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || raw != format!("http://127.0.0.1:{port}/")
    {
        return Err(invalid());
    }
    Ok(Some(raw.to_owned()))
}

/// The URL libgit2 connects to: the loopback base plus the path for a GitHub
/// URL when a loopback is set, otherwise `url` unchanged.
fn network_url(url: &str, loopback: Option<&str>) -> String {
    match (url.strip_prefix("https://github.com/"), loopback) {
        (Some(path), Some(base)) => format!("{base}{path}"),
        _ => url.to_owned(),
    }
}

pub fn execute(raw: &str) -> Value {
    let result = serde_json::from_str::<GitRequest>(raw)
        .map_err(|error| format!("Invalid Git request: {error}"))
        .and_then(execute_request);
    match result {
        Ok(value) => json!({"ok": true, "value": value}),
        Err(error) => json!({"ok": false, "error": error}),
    }
}

fn execute_request(request: GitRequest) -> Result<Value, String> {
    require_absolute_workspace(&request.workspace_root)?;
    let loopback = validated_loopback(request.loopback_base.as_deref())?;
    match request.operation.as_str() {
        "clone" => clone_repo(&request, loopback.as_deref()),
        "status" => status(&open_repo(&request.workspace_root)?),
        "diff" => diff(&open_repo(&request.workspace_root)?),
        "pull" => {
            let guard = register_operation(request.operation_id.as_deref())?;
            pull(
                &open_repo(&request.workspace_root)?,
                request.token.as_deref(),
                loopback.as_deref(),
                guard.as_ref(),
            )
        }
        "stage" => stage(
            &open_repo(&request.workspace_root)?,
            request.paths.as_deref(),
        ),
        "commit" => commit(&open_repo(&request.workspace_root)?, &request),
        "push" => {
            let guard = register_operation(request.operation_id.as_deref())?;
            push(
                &open_repo(&request.workspace_root)?,
                request.token.as_deref(),
                loopback.as_deref(),
                guard.as_ref(),
            )
        }
        "createBranch" => create_branch(
            &open_repo(&request.workspace_root)?,
            request.branch.as_deref(),
        ),
        "switchBranch" => switch_branch(
            &open_repo(&request.workspace_root)?,
            request.branch.as_deref(),
        ),
        "deleteBranch" => delete_branch(
            &open_repo(&request.workspace_root)?,
            request.branch.as_deref(),
            request.force.unwrap_or(false),
        ),
        "fetch" => {
            let guard = register_operation(request.operation_id.as_deref())?;
            fetch(
                &open_repo(&request.workspace_root)?,
                request.remote.as_deref(),
                request.token.as_deref(),
                loopback.as_deref(),
                guard.as_ref(),
            )
        }
        "discover" => discover(&open_repo(&request.workspace_root)?),
        "unstage" => unstage(
            &open_repo(&request.workspace_root)?,
            request.paths.as_deref(),
        ),
        "discard" => discard(
            &open_repo(&request.workspace_root)?,
            request.paths.as_deref(),
        ),
        "lfsScan" => lfs_scan(&open_repo(&request.workspace_root)?),
        "lfsCandidates" => lfs_candidates(
            &open_repo(&request.workspace_root)?,
            request.min_bytes.unwrap_or(5 * 1024 * 1024),
        ),
        "lfsTrack" => lfs_track(&open_repo(&request.workspace_root)?, &request),
        "lfsFetch" => {
            let repo = open_repo(&request.workspace_root)?;
            let plan = resolve_lfs_fetch_plan(&repo, &request)?;
            drop(repo);
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("Create LFS transfer runtime: {error}"))?;
            runtime.block_on(lfs_fetch(plan, request.token.as_deref()))
        }
        "progress" => operation_progress(request.operation_id.as_deref()),
        "cancel" => operation_cancel(request.operation_id.as_deref()),
        "stageHunk" => stage_hunk(
            &open_repo(&request.workspace_root)?,
            request.paths.as_deref(),
            request.patch.as_deref(),
        ),
        "merge" => merge(&open_repo(&request.workspace_root)?, &request),
        "conflicts" => conflicts(&open_repo(&request.workspace_root)?),
        "resolve" => resolve(&open_repo(&request.workspace_root)?, &request),
        "mergeContinue" => {
            let mut repo = open_repo(&request.workspace_root)?;
            merge_continue(&mut repo, &request)
        }
        "mergeAbort" => merge_abort(&open_repo(&request.workspace_root)?),
        "rebase" => rebase_start(&open_repo(&request.workspace_root)?, &request),
        "rebaseContinue" => rebase_continue(&open_repo(&request.workspace_root)?, &request),
        "rebaseAbort" => rebase_abort(&open_repo(&request.workspace_root)?),
        "stashList" => {
            let mut repo = open_repo(&request.workspace_root)?;
            stash_list(&mut repo)
        }
        "stashCreate" => {
            let mut repo = open_repo(&request.workspace_root)?;
            stash_create(&mut repo, &request)
        }
        "stashApply" => {
            let mut repo = open_repo(&request.workspace_root)?;
            stash_apply(&mut repo, request.stash_index)
        }
        "stashDrop" => {
            let mut repo = open_repo(&request.workspace_root)?;
            stash_drop(&mut repo, request.stash_index)
        }
        "revert" => revert(&open_repo(&request.workspace_root)?, &request),
        "tagList" => tag_list(&open_repo(&request.workspace_root)?),
        "tagCreate" => tag_create(&open_repo(&request.workspace_root)?, &request),
        "tagDelete" => tag_delete(&open_repo(&request.workspace_root)?, request.tag.as_deref()),
        "tagPush" => {
            let guard = register_operation(request.operation_id.as_deref())?;
            tag_push(
                &open_repo(&request.workspace_root)?,
                request.tag.as_deref(),
                request.token.as_deref(),
                loopback.as_deref(),
                guard.as_ref(),
            )
        }
        "log" => log(&open_repo(&request.workspace_root)?, &request),
        "commitDetail" => {
            commit_detail(&open_repo(&request.workspace_root)?, request.sha.as_deref())
        }
        other => Err(format!("Unsupported Git operation '{other}'.")),
    }
}

fn require_absolute_workspace(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("workspaceRoot must be absolute.".into());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("workspaceRoot may not contain '..'.".into());
    }
    Ok(())
}

fn open_repo(path: &Path) -> Result<Repository, String> {
    let repo = Repository::open(path).map_err(|error| format!("Open repository: {error}"))?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| "Bare repositories are unsupported on iPhone.".to_string())?;
    let canonical_root = path
        .canonicalize()
        .map_err(|error| format!("Resolve workspace: {error}"))?;
    let canonical_repo = workdir
        .canonicalize()
        .map_err(|error| format!("Resolve repository: {error}"))?;
    if canonical_repo != canonical_root {
        return Err("Repository root must equal selected workspace root.".into());
    }
    Ok(repo)
}

fn validated_github_url(raw: &str) -> Result<String, String> {
    let parsed = Url::parse(raw).map_err(|error| format!("Invalid remote URL: {error}"))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("github.com") {
        return Err("On-device Git currently accepts HTTPS github.com remotes only.".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Remote URL must not contain credentials.".into());
    }
    let mut normalized = parsed;
    normalized.set_query(None);
    normalized.set_fragment(None);
    Ok(normalized.to_string())
}

fn callbacks(token: Option<&str>, loopback: Option<&str>) -> RemoteCallbacks<'static> {
    let token = token.map(str::to_owned);
    let loopback = loopback.map(str::to_owned);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, _, allowed| {
        let loopback_target = loopback
            .as_deref()
            .is_some_and(|base| url.starts_with(base));
        if !url.starts_with("https://github.com/") && !loopback_target {
            return Err(git2::Error::from_str(
                "Refusing credentials for non-GitHub remote",
            ));
        }
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            if let Some(token) = token.as_deref() {
                return git2::Cred::userpass_plaintext("x-access-token", token);
            }
        }
        git2::Cred::default()
    });
    callbacks
}

/// Remember `message` as the first failure, keeping an earlier one.
fn note_failure(slot: &mut Option<String>, message: String) {
    if slot.is_none() {
        *slot = Some(message);
    }
}

/// Delete every child of a clone destination, never the destination itself and
/// never anything outside it, so a cancelled or failed clone leaves it empty.
/// Returns the first thing that could not be removed or read.
fn clear_clone_destination(root: &Path) -> Result<(), String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("resolve destination: {error}"))?;
    if canonical_root.parent().is_none() {
        return Err("refusing to clear a filesystem root".into());
    }
    let entries =
        std::fs::read_dir(&canonical_root).map_err(|error| format!("read destination: {error}"))?;
    let mut first_failure: Option<String> = None;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                note_failure(&mut first_failure, format!("read destination: {error}"));
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                note_failure(
                    &mut first_failure,
                    format!("inspect {}: {error}", path.display()),
                );
                continue;
            }
        };
        let removal = if file_type.is_symlink() || file_type.is_file() {
            // Removes the link or file itself, never what a link points to.
            std::fs::remove_file(&path)
        } else if file_type.is_dir()
            && path.canonicalize().is_ok_and(|resolved| {
                resolved.starts_with(&canonical_root) && resolved != canonical_root
            })
        {
            std::fs::remove_dir_all(&path)
        } else {
            Ok(())
        };
        if let Err(error) = removal {
            note_failure(
                &mut first_failure,
                format!("remove {}: {error}", path.display()),
            );
        }
    }
    first_failure.map_or(Ok(()), Err)
}

fn clone_repo(request: &GitRequest, loopback: Option<&str>) -> Result<Value, String> {
    let guard = register_operation(request.operation_id.as_deref())?;
    let raw = request
        .remote_url
        .as_deref()
        .ok_or_else(|| "clone requires remoteUrl.".to_string())?;
    let remote_url = validated_github_url(raw)?;
    if request.workspace_root.exists()
        && request
            .workspace_root
            .read_dir()
            .map_err(|error| format!("Read clone destination: {error}"))?
            .next()
            .is_some()
    {
        return Err("Clone destination must be empty.".into());
    }
    std::fs::create_dir_all(&request.workspace_root)
        .map_err(|error| format!("Create clone destination: {error}"))?;
    let outcome = (|| {
        let mut fetch = FetchOptions::new();
        let mut remote_callbacks = callbacks(request.token.as_deref(), loopback);
        attach_progress(
            &mut remote_callbacks,
            guard.as_ref().map(|guard| &guard.state),
        );
        fetch.remote_callbacks(remote_callbacks);
        let mut builder = RepoBuilder::new();
        builder.fetch_options(fetch);
        let repo = builder
            .clone(&network_url(&remote_url, loopback), &request.workspace_root)
            .map_err(|error| transfer_error(guard.as_ref(), "Clone failed", error))?;
        // The repository's origin stays the GitHub URL even when the bytes
        // came from a loopback fixture.
        repo.remote_set_url("origin", &remote_url)
            .map_err(|error| format!("Set origin: {error}"))?;
        status(&repo)
    })();
    match outcome {
        Ok(value) => Ok(value),
        Err(message) => match clear_clone_destination(&request.workspace_root) {
            Ok(()) => Err(message),
            Err(cleanup) => Err(format!("{message} (cleanup failed: {cleanup})")),
        },
    }
}

fn status(repo: &Repository) -> Result<Value, String> {
    let mut options = git2::StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    let statuses = repo
        .statuses(Some(&mut options))
        .map_err(|error| format!("Read status: {error}"))?;
    let entries = statuses
        .iter()
        .map(|entry| {
            let status = entry.status();
            json!({
                "path": entry.path().unwrap_or("?"),
                "index": index_label(status),
                "worktree": worktree_label(status)
            })
        })
        .collect::<Vec<_>>();
    let branches = repo
        .branches(Some(BranchType::Local))
        .map_err(|error| format!("List branches: {error}"))?
        .filter_map(Result::ok)
        .filter_map(|(branch, _)| branch.name().ok().flatten().map(str::to_owned))
        .collect::<Vec<_>>();
    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|remote| remote.url().ok().map(str::to_owned));
    Ok(json!({
        "branch": repo.head().ok().and_then(|head| head.shorthand().ok().map(str::to_owned)),
        "remoteUrl": remote_url,
        "clean": entries.is_empty(),
        "entries": entries,
        "branches": branches,
        "repoState": match repo.state() {
            git2::RepositoryState::Merge => "merge",
            git2::RepositoryState::Rebase
            | git2::RepositoryState::RebaseInteractive
            | git2::RepositoryState::RebaseMerge => "rebase",
            git2::RepositoryState::Revert | git2::RepositoryState::RevertSequence => "revert",
            git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence => {
                "cherryPick"
            }
            _ => "clean",
        }
    }))
}

fn diff(repo: &Repository) -> Result<Value, String> {
    let mut options = git2::DiffOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let diff = repo
        .diff_tree_to_workdir_with_index(None, Some(&mut options))
        .map_err(|error| format!("Read diff: {error}"))?;
    let mut bytes = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            bytes.push(line.origin() as u8);
        }
        bytes.extend_from_slice(line.content());
        bytes.len() <= 512 * 1024
    })
    .map_err(|error| format!("Format diff: {error}"))?;
    if bytes.len() > 512 * 1024 {
        bytes.truncate(512 * 1024);
        bytes.extend_from_slice(b"\n[diff truncated at 512 KiB]");
    }
    Ok(json!({"diff": String::from_utf8_lossy(&bytes)}))
}

fn index_label(status: git2::Status) -> Option<&'static str> {
    if status.intersects(git2::Status::INDEX_NEW) {
        Some("added")
    } else if status.intersects(git2::Status::INDEX_MODIFIED) {
        Some("modified")
    } else if status.intersects(git2::Status::INDEX_DELETED) {
        Some("deleted")
    } else if status.intersects(git2::Status::INDEX_RENAMED) {
        Some("renamed")
    } else {
        None
    }
}

fn worktree_label(status: git2::Status) -> Option<&'static str> {
    if status.intersects(git2::Status::WT_NEW) {
        Some("untracked")
    } else if status.intersects(git2::Status::WT_MODIFIED) {
        Some("modified")
    } else if status.intersects(git2::Status::WT_DELETED) {
        Some("deleted")
    } else if status.intersects(git2::Status::WT_RENAMED) {
        Some("renamed")
    } else {
        None
    }
}

fn has_changes(repo: &Repository) -> Result<bool, String> {
    let mut options = git2::StatusOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    Ok(repo
        .statuses(Some(&mut options))
        .map_err(|error| format!("Read status: {error}"))?
        .iter()
        .any(|entry| {
            // Journals belong to the local runtime, not the user's worktree.
            // Tracked journal edits still count; checkout's safe mode also
            // protects against an untracked file colliding with the target.
            !(entry.status() == git2::Status::WT_NEW
                && entry.path().is_ok_and(is_runtime_state_path))
        }))
}

/// Fetch `remote_name`, through the loopback fixture when one is set. Returns
/// the validated GitHub URL and the received and total object counts.
fn fetch_remote(
    repo: &Repository,
    remote_name: &str,
    token: Option<&str>,
    loopback: Option<&str>,
    guard: Option<&OperationGuard>,
) -> Result<(String, usize, usize), String> {
    let mut named = repo
        .find_remote(remote_name)
        .map_err(|error| format!("Find remote '{remote_name}': {error}"))?;
    let remote_url = named.url().map_err(|error| error.to_string())?.to_owned();
    validated_github_url(&remote_url)?;
    let mut options = FetchOptions::new();
    let mut remote_callbacks = callbacks(token, loopback);
    attach_progress(&mut remote_callbacks, guard.map(|guard| &guard.state));
    options.remote_callbacks(remote_callbacks);
    let (received, total) = match loopback {
        Some(base) => {
            let refspec = format!("+refs/heads/*:refs/remotes/{remote_name}/*");
            let mut anonymous = repo
                .remote_anonymous(&network_url(&remote_url, Some(base)))
                .map_err(|error| format!("Open remote: {error}"))?;
            anonymous
                .fetch(&[refspec.as_str()], Some(&mut options), None)
                .map_err(|error| transfer_error(guard, "Fetch failed", error))?;
            let stats = anonymous.stats();
            (stats.received_objects(), stats.total_objects())
        }
        None => {
            named
                .fetch(&[] as &[&str], Some(&mut options), None)
                .map_err(|error| transfer_error(guard, "Fetch failed", error))?;
            let stats = named.stats();
            (stats.received_objects(), stats.total_objects())
        }
    };
    Ok((remote_url, received, total))
}

fn pull(
    repo: &Repository,
    token: Option<&str>,
    loopback: Option<&str>,
    guard: Option<&OperationGuard>,
) -> Result<Value, String> {
    if has_changes(repo)? {
        return Err("Pull requires a clean working tree.".into());
    }
    fetch_remote(repo, "origin", token, loopback, guard)?;
    let fetch_head = repo
        .find_reference("FETCH_HEAD")
        .map_err(|error| format!("Read FETCH_HEAD: {error}"))?;
    let annotated = repo
        .reference_to_annotated_commit(&fetch_head)
        .map_err(|error| format!("Read fetched commit: {error}"))?;
    let (analysis, _) = repo
        .merge_analysis(&[&annotated])
        .map_err(|error| format!("Plan pull: {error}"))?;
    if analysis.is_up_to_date() {
        return Ok(json!({"result": "upToDate"}));
    }
    if !analysis.is_fast_forward() {
        return Err(
            "Pull is not a fast-forward; resolve merge or rebase on cloud/computer.".into(),
        );
    }
    let head = repo.head().map_err(|error| format!("Read HEAD: {error}"))?;
    let reference_name = head
        .name()
        .map_err(|_| "Detached HEAD cannot fast-forward.".to_string())?
        .to_owned();
    drop(head);
    let mut reference = repo
        .find_reference(&reference_name)
        .map_err(|error| format!("Open branch: {error}"))?;
    reference
        .set_target(
            annotated.id(),
            &format!("{} fast-forward pull", rightkit_agent_config::app_name()),
        )
        .map_err(|error| format!("Fast-forward branch: {error}"))?;
    repo.set_head(&reference_name)
        .map_err(|error| format!("Set HEAD: {error}"))?;
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().safe()))
        .map_err(|error| format!("Check out fast-forward: {error}"))?;
    Ok(json!({"result": "fastForward", "commit": annotated.id().to_string()}))
}

fn exact_paths(paths: Option<&[String]>) -> Result<Vec<&Path>, String> {
    let values = paths.ok_or_else(|| "stage requires paths.".to_string())?;
    if values.is_empty() {
        return Err("stage requires at least one path.".into());
    }
    values
        .iter()
        .map(|value| {
            let path = Path::new(value);
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
                || value
                    .chars()
                    .any(|character| matches!(character, '*' | '?' | '[' | ']'))
                || path
                    .components()
                    .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"))
            {
                Err(format!("Invalid exact repository path '{value}'."))
            } else {
                Ok(path)
            }
        })
        .collect()
}

fn stage(repo: &Repository, paths: Option<&[String]>) -> Result<Value, String> {
    let paths = exact_paths(paths)?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| "Bare repository.".to_string())?;
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    for path in &paths {
        if workdir.join(path).exists() {
            index
                .add_path(path)
                .map_err(|error| format!("Stage path: {error}"))?;
        } else {
            index
                .remove_path(path)
                .map_err(|error| format!("Stage deletion: {error}"))?;
        }
    }
    index
        .write()
        .map_err(|error| format!("Write index: {error}"))?;
    Ok(json!({
        "paths": paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>()
    }))
}

fn commit(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let message = request
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "commit requires message.".to_string())?;
    let name = request
        .author_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "commit requires authorName.".to_string())?;
    let email = request
        .author_email
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "commit requires authorEmail.".to_string())?;
    let signature =
        git2::Signature::now(name, email).map_err(|error| format!("Author identity: {error}"))?;
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    let tree_id = index
        .write_tree()
        .map_err(|error| format!("Write tree: {error}"))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| format!("Read tree: {error}"))?;
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    if parent.as_ref().map(|value| value.tree_id()) == Some(tree_id) {
        return Err("Refusing empty commit.".into());
    }
    let parents = parent.iter().collect::<Vec<_>>();
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .map_err(|error| format!("Commit failed: {error}"))?;
    Ok(json!({"commit": oid.to_string(), "message": message}))
}

/// Push one explicit refspec to `origin`, through the loopback fixture when
/// one is set. Returns the validated GitHub URL. A reference the server
/// refuses is an error, never a silent success.
fn push_refspec(
    repo: &Repository,
    refspec: &str,
    token: Option<&str>,
    loopback: Option<&str>,
    guard: Option<&OperationGuard>,
) -> Result<String, String> {
    let named = repo
        .find_remote("origin")
        .map_err(|error| format!("Find origin: {error}"))?;
    let remote_url = named.url().map_err(|error| error.to_string())?.to_owned();
    validated_github_url(&remote_url)?;
    let rejection = Arc::new(Mutex::new(None::<String>));
    let mut remote_callbacks = callbacks(token, loopback);
    attach_push_progress(
        &mut remote_callbacks,
        guard.map(|guard| &guard.state),
        rejection.clone(),
    );
    let mut options = PushOptions::new();
    options.remote_callbacks(remote_callbacks);
    let mut remote = match loopback {
        Some(base) => repo
            .remote_anonymous(&network_url(&remote_url, Some(base)))
            .map_err(|error| format!("Open remote: {error}"))?,
        None => named,
    };
    remote
        .push(&[refspec], Some(&mut options))
        .map_err(|error| transfer_error(guard, "Push failed", error))?;
    let rejected = rejection
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(reason) = rejected {
        return Err(format!("Push rejected: {reason}"));
    }
    Ok(remote_url)
}

fn push(
    repo: &Repository,
    token: Option<&str>,
    loopback: Option<&str>,
    guard: Option<&OperationGuard>,
) -> Result<Value, String> {
    let head = repo.head().map_err(|error| format!("Read HEAD: {error}"))?;
    let branch = head
        .shorthand()
        .map_err(|_| "Detached HEAD cannot push.".to_string())?;
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let remote_url = push_refspec(repo, &refspec, token, loopback, guard)?;
    Ok(json!({"branch": branch, "remoteUrl": remote_url}))
}

fn valid_branch(raw: Option<&str>) -> Result<&str, String> {
    let branch = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "branch is required.".to_string())?;
    if !git2::Reference::is_valid_name(&format!("refs/heads/{branch}")) {
        return Err("Invalid branch name.".into());
    }
    Ok(branch)
}

fn create_branch(repo: &Repository, raw: Option<&str>) -> Result<Value, String> {
    let branch = valid_branch(raw)?;
    if has_changes(repo)? {
        return Err("Create and switch branch requires a clean working tree.".into());
    }
    let commit = repo
        .head()
        .map_err(|error| error.to_string())?
        .peel_to_commit()
        .map_err(|error| error.to_string())?;
    repo.branch(branch, &commit, false)
        .map_err(|error| format!("Create branch: {error}"))?;
    switch_branch(repo, Some(branch))
}

fn switch_branch(repo: &Repository, raw: Option<&str>) -> Result<Value, String> {
    let branch = valid_branch(raw)?;
    if has_changes(repo)? {
        return Err("Switch branch requires a clean working tree.".into());
    }
    repo.find_branch(branch, BranchType::Local)
        .map_err(|error| format!("Find branch: {error}"))?;
    repo.set_head(&format!("refs/heads/{branch}"))
        .map_err(|error| format!("Set HEAD: {error}"))?;
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().safe()))
        .map_err(|error| format!("Check out branch: {error}"))?;
    Ok(json!({"branch": branch}))
}

/// ENG-46: authenticated, cancellable-by-caller fetch that updates only
/// `refs/remotes/<remote>/*`. Never calls `checkout_head`, `set_head`, or
/// touches the index — that boundary is the whole point of `fetch` versus
/// `pull` above, which is explicitly a fast-forward-or-refuse operation.
fn fetch(
    repo: &Repository,
    remote: Option<&str>,
    token: Option<&str>,
    loopback: Option<&str>,
    guard: Option<&OperationGuard>,
) -> Result<Value, String> {
    let remote_name = remote.unwrap_or("origin");
    let (remote_url, received, total) = fetch_remote(repo, remote_name, token, loopback, guard)?;
    Ok(json!({
        "remote": remote_name,
        "remoteUrl": remote_url,
        "receivedObjects": received,
        "totalObjects": total
    }))
}

/// ENG-45: repository discovery — identity, current branch, default branch,
/// and remote list. Read-only.
fn discover(repo: &Repository) -> Result<Value, String> {
    let path = repo
        .workdir()
        .unwrap_or_else(|| repo.path())
        .canonicalize()
        .unwrap_or_else(|_| repo.path().to_path_buf())
        .display()
        .to_string();
    let head_branch = repo
        .head()
        .ok()
        .and_then(|head| head.shorthand().ok().map(str::to_owned));
    let default_branch = repo
        .find_reference("refs/remotes/origin/HEAD")
        .ok()
        .and_then(|reference| {
            reference
                .symbolic_target()
                .ok()
                .flatten()
                .and_then(|target| target.strip_prefix("refs/remotes/origin/"))
                .map(str::to_owned)
        })
        .or_else(|| head_branch.clone());
    let remotes = repo
        .remotes()
        .map_err(|error| format!("List remotes: {error}"))?
        .iter()
        .flatten()
        .flatten()
        .filter_map(|name| {
            repo.find_remote(name)
                .ok()
                .map(|remote| json!({"name": name, "url": remote.url().ok()}))
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "path": path,
        "headBranch": head_branch,
        "defaultBranch": default_branch,
        "remotes": remotes
    }))
}

/// ENG-48: delete a local branch. Refuses the current branch outright and
/// refuses an unmerged branch unless the caller explicitly sets `force`.
fn delete_branch(repo: &Repository, raw: Option<&str>, force: bool) -> Result<Value, String> {
    let branch = valid_branch(raw)?;
    let mut target = repo
        .find_branch(branch, BranchType::Local)
        .map_err(|error| format!("Find branch: {error}"))?;
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
        .map_err(|error| format!("Delete branch: {error}"))?;
    Ok(json!({"branch": branch, "deleted": true}))
}

/// ENG-50: unstage exact paths only — restore the index entry to HEAD, or
/// remove it if the path was newly added. Never a blanket reset.
fn unstage(repo: &Repository, paths: Option<&[String]>) -> Result<Value, String> {
    let paths = exact_paths(paths)?;
    let head_tree = repo.head().ok().and_then(|head| head.peel_to_tree().ok());
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    for path in &paths {
        match head_tree.as_ref().and_then(|tree| tree.get_path(path).ok()) {
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
                        path: path.to_string_lossy().into_owned().into_bytes(),
                    })
                    .map_err(|error| format!("Unstage: {error}"))?;
            }
            None => {
                index
                    .remove_path(path)
                    .map_err(|error| format!("Unstage: {error}"))?;
            }
        }
    }
    index
        .write()
        .map_err(|error| format!("Write index: {error}"))?;
    Ok(json!({
        "paths": paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>()
    }))
}

/// ENG-50: discard exact working-tree paths back to the staged/index content.
fn discard(repo: &Repository, paths: Option<&[String]>) -> Result<Value, String> {
    let paths = exact_paths(paths)?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    for path in &paths {
        checkout.path(*path);
    }
    repo.checkout_index(None, Some(&mut checkout))
        .map_err(|error| format!("Discard: {error}"))?;
    Ok(json!({
        "paths": paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>()
    }))
}

/// ENG-50: stage one hunk. The patch must touch exactly the selected path and
/// nothing else; it is applied to the index only, never the working tree.
fn stage_hunk(
    repo: &Repository,
    paths: Option<&[String]>,
    patch: Option<&str>,
) -> Result<Value, String> {
    let paths = exact_paths(paths)?;
    if paths.len() != 1 {
        return Err("stageHunk requires exactly one path.".into());
    }
    let selected = paths[0];
    let patch = patch
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "stageHunk requires patch.".to_string())?;
    let diff = git2::Diff::from_buffer(patch.as_bytes())
        .map_err(|error| format!("Parse patch: {error}"))?;
    let mismatch = || "Patch does not match the selected path.".to_string();
    if diff.deltas().count() != 1 {
        return Err(mismatch());
    }
    for delta in diff.deltas() {
        let mut matched = false;
        for file in [delta.old_file(), delta.new_file()] {
            if let Some(path) = file.path() {
                if path != selected {
                    return Err(mismatch());
                }
                matched = true;
            }
        }
        if !matched {
            return Err(mismatch());
        }
    }
    repo.apply(&diff, git2::ApplyLocation::Index, None)
        .map_err(|error| format!("Stage hunk: {error}"))?;
    Ok(json!({"paths": [selected.display().to_string()]}))
}

fn required_text<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{field} is required."))
}

fn author_signature(request: &GitRequest) -> Result<git2::Signature<'static>, String> {
    let name = required_text(request.author_name.as_deref(), "authorName")?;
    let email = required_text(request.author_email.as_deref(), "authorEmail")?;
    git2::Signature::now(name, email).map_err(|error| format!("Author identity: {error}"))
}

fn require_idle(repo: &Repository) -> Result<(), String> {
    if repo.state() != git2::RepositoryState::Clean {
        return Err("Finish or abort the Git operation in progress first.".into());
    }
    Ok(())
}

fn index_conflict_path(conflict: &git2::IndexConflict) -> Option<String> {
    conflict
        .our
        .as_ref()
        .or(conflict.their.as_ref())
        .or(conflict.ancestor.as_ref())
        .map(|entry| String::from_utf8_lossy(&entry.path).into_owned())
}

fn conflict_paths(index: &git2::Index) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for conflict in index
        .conflicts()
        .map_err(|error| format!("Read conflicts: {error}"))?
    {
        let conflict = conflict.map_err(|error| format!("Read conflict: {error}"))?;
        if let Some(path) = index_conflict_path(&conflict) {
            paths.push(path);
        }
    }
    Ok(paths)
}

/// Per-side conflict text capped at 256 KiB; a binary side has no text.
const CONFLICT_TEXT_LIMIT: usize = 262_144;

fn conflict_side(
    repo: &Repository,
    entry: Option<&git2::IndexEntry>,
) -> Result<(Option<String>, bool), String> {
    let Some(entry) = entry else {
        return Ok((None, false));
    };
    let blob = repo
        .find_blob(entry.id)
        .map_err(|error| format!("Read conflict side: {error}"))?;
    if blob.is_binary() {
        return Ok((None, true));
    }
    let content = blob.content();
    let capped = &content[..content.len().min(CONFLICT_TEXT_LIMIT)];
    Ok((Some(String::from_utf8_lossy(capped).into_owned()), false))
}

fn read_conflicts(repo: &Repository, index: &git2::Index) -> Result<Vec<Value>, String> {
    let mut records = Vec::new();
    for conflict in index
        .conflicts()
        .map_err(|error| format!("Read conflicts: {error}"))?
    {
        let conflict = conflict.map_err(|error| format!("Read conflict: {error}"))?;
        let Some(path) = index_conflict_path(&conflict) else {
            continue;
        };
        let (base, base_binary) = conflict_side(repo, conflict.ancestor.as_ref())?;
        let (ours, ours_binary) = conflict_side(repo, conflict.our.as_ref())?;
        let (theirs, theirs_binary) = conflict_side(repo, conflict.their.as_ref())?;
        let binary = base_binary || ours_binary || theirs_binary;
        records.push(json!({
            "path": path,
            "base": if binary { None } else { base },
            "ours": if binary { None } else { ours },
            "theirs": if binary { None } else { theirs },
            "binary": binary
        }));
    }
    Ok(records)
}

/// ENG-53: merge `branch` into the current branch. Starts only on a clean
/// tree, records `refs/<host slug>/premerge` first, and leaves conflicts in the
/// index for the explicit `conflicts`, `resolve` and `mergeContinue` calls.
fn merge(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let branch = valid_branch(request.branch.as_deref())?;
    let signature = author_signature(request)?;
    require_idle(repo)?;
    if has_changes(repo)? {
        return Err("Working tree must be clean to merge.".into());
    }
    let target = repo
        .find_branch(branch, BranchType::Local)
        .map_err(|error| format!("Find branch: {error}"))?;
    let annotated = repo
        .reference_to_annotated_commit(target.get())
        .map_err(|error| format!("Read branch commit: {error}"))?;
    let (analysis, _) = repo
        .merge_analysis(&[&annotated])
        .map_err(|error| format!("Plan merge: {error}"))?;
    if analysis.is_up_to_date() {
        return Ok(json!({"state": "upToDate"}));
    }
    let head = repo.head().map_err(|error| format!("Read HEAD: {error}"))?;
    if !head.is_branch() {
        return Err("Detached HEAD cannot merge.".into());
    }
    let head_name = head
        .name()
        .map_err(|_| "Current branch name is not valid UTF-8.".to_string())?
        .to_owned();
    let head_commit = head
        .peel_to_commit()
        .map_err(|error| format!("Read HEAD commit: {error}"))?;
    drop(head);
    if analysis.is_fast_forward() {
        let commit = repo
            .find_commit(annotated.id())
            .map_err(|error| format!("Read branch commit: {error}"))?;
        // Update the files first: the checkout baseline is HEAD, so moving the
        // branch first would leave nothing to check out.
        repo.checkout_tree(commit.as_object(), Some(CheckoutBuilder::new().safe()))
            .map_err(|error| format!("Check out fast-forward: {error}"))?;
        let mut reference = repo
            .find_reference(&head_name)
            .map_err(|error| format!("Open branch: {error}"))?;
        reference
            .set_target(
                annotated.id(),
                &format!("{} fast-forward merge", rightkit_agent_config::app_name()),
            )
            .map_err(|error| format!("Fast-forward branch: {error}"))?;
        return Ok(json!({"state": "fastForward"}));
    }
    if !analysis.is_normal() {
        return Err("These branches cannot be merged.".into());
    }
    repo.reference(
        &format!("refs/{}/premerge", rightkit_agent_config::host_slug()),
        head_commit.id(),
        true,
        &format!("{} pre-merge HEAD", rightkit_agent_config::app_name()),
    )
    .map_err(|error| format!("Record pre-merge HEAD: {error}"))?;
    let mut merge_options = git2::MergeOptions::new();
    let mut checkout = CheckoutBuilder::new();
    checkout
        .safe()
        .allow_conflicts(true)
        .conflict_style_merge(true);
    repo.merge(&[&annotated], Some(&mut merge_options), Some(&mut checkout))
        .map_err(|error| format!("Merge failed: {error}"))?;
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    index
        .read(true)
        .map_err(|error| format!("Reload index: {error}"))?;
    if index.has_conflicts() {
        return Ok(json!({"state": "conflicted", "conflicts": conflict_paths(&index)?}));
    }
    let tree_id = index
        .write_tree()
        .map_err(|error| format!("Write tree: {error}"))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| format!("Read tree: {error}"))?;
    let their_commit = repo
        .find_commit(annotated.id())
        .map_err(|error| format!("Read branch commit: {error}"))?;
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            &format!("Merge branch '{branch}'"),
            &tree,
            &[&head_commit, &their_commit],
        )
        .map_err(|error| format!("Commit merge: {error}"))?;
    repo.cleanup_state()
        .map_err(|error| format!("Clean up merge state: {error}"))?;
    Ok(json!({"state": "merged", "sha": oid.to_string()}))
}

/// ENG-53: the conflicted files of the merge or rebase in progress, with each
/// side's text.
fn conflicts(repo: &Repository) -> Result<Value, String> {
    let state = match repo.state() {
        git2::RepositoryState::Merge => "merge",
        git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseInteractive
        | git2::RepositoryState::RebaseMerge => "rebase",
        _ => "none",
    };
    let index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    Ok(json!({"state": state, "conflicts": read_conflicts(repo, &index)?}))
}

/// The working-tree file for an exact repository path, refused when it would
/// land outside the repository or replace a symlink.
fn workdir_target(repo: &Repository, path: &Path) -> Result<PathBuf, String> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| "Bare repository.".to_string())?
        .canonicalize()
        .map_err(|error| format!("Resolve repository: {error}"))?;
    let target = workdir.join(path);
    let mut probe = target.parent();
    while let Some(ancestor) = probe {
        if ancestor.exists() {
            let resolved = ancestor
                .canonicalize()
                .map_err(|error| format!("Resolve path: {error}"))?;
            if !resolved.starts_with(&workdir) {
                return Err("Path resolves outside the repository.".into());
            }
            break;
        }
        probe = ancestor.parent();
    }
    if std::fs::symlink_metadata(&target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err("Refusing to resolve through a symbolic link.".into());
    }
    Ok(target)
}

/// ENG-53: resolve one currently conflicted path with `ours`, `theirs` or
/// `manual` content, then stage it, which clears its conflict entry.
fn resolve(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let paths = exact_paths(request.paths.as_deref())?;
    if paths.len() != 1 {
        return Err("resolve requires exactly one path.".into());
    }
    let path = paths[0];
    let resolution = request.resolution.as_deref().unwrap_or_default();
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    let conflict = index
        .conflict_get(path)
        .map_err(|_| "Path is not conflicted.".to_string())?;
    let target = workdir_target(repo, path)?;
    let side = match resolution {
        "ours" => Some(conflict.our.as_ref()),
        "theirs" => Some(conflict.their.as_ref()),
        "manual" => None,
        _ => return Err("resolution must be ours, theirs, or manual.".into()),
    };
    // `Some(None)` is a side that deleted the file.
    let bytes: Option<Vec<u8>> = match side {
        Some(Some(entry)) => Some(
            repo.find_blob(entry.id)
                .map_err(|error| format!("Read conflict side: {error}"))?
                .content()
                .to_vec(),
        ),
        Some(None) => None,
        None => Some(
            request
                .content
                .as_deref()
                .ok_or_else(|| "manual resolution requires content.".to_string())?
                .as_bytes()
                .to_vec(),
        ),
    };
    match bytes {
        Some(bytes) => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("Create directory: {error}"))?;
            }
            std::fs::write(&target, bytes)
                .map_err(|error| format!("Write resolved file: {error}"))?;
            index
                .add_path(path)
                .map_err(|error| format!("Stage resolved file: {error}"))?;
        }
        None => {
            if target.exists() {
                std::fs::remove_file(&target)
                    .map_err(|error| format!("Remove resolved file: {error}"))?;
            }
            index
                .remove_path(path)
                .map_err(|error| format!("Stage resolved deletion: {error}"))?;
        }
    }
    index
        .write()
        .map_err(|error| format!("Write index: {error}"))?;
    Ok(json!({"paths": [path.display().to_string()]}))
}

/// ENG-53: commit the merge once every conflict is resolved.
fn merge_continue(repo: &mut Repository, request: &GitRequest) -> Result<Value, String> {
    if repo.state() != git2::RepositoryState::Merge {
        return Err("No merge is in progress.".into());
    }
    let signature = author_signature(request)?;
    let mut index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    if index.has_conflicts() {
        return Err("Resolve all conflicts first.".into());
    }
    let mut merge_heads = Vec::new();
    repo.mergehead_foreach(|oid| {
        merge_heads.push(*oid);
        true
    })
    .map_err(|error| format!("Read MERGE_HEAD: {error}"))?;
    if merge_heads.is_empty() {
        return Err("MERGE_HEAD is missing.".into());
    }
    let message = request
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            repo.message()
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "Merge commit".to_string());
    let tree_id = index
        .write_tree()
        .map_err(|error| format!("Write tree: {error}"))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| format!("Read tree: {error}"))?;
    let mut parents = vec![repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| format!("Read HEAD commit: {error}"))?];
    for oid in merge_heads {
        parents.push(
            repo.find_commit(oid)
                .map_err(|error| format!("Read merge parent: {error}"))?,
        );
    }
    let parent_refs = parents.iter().collect::<Vec<_>>();
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &parent_refs,
        )
        .map_err(|error| format!("Commit merge: {error}"))?;
    repo.cleanup_state()
        .map_err(|error| format!("Clean up merge state: {error}"))?;
    Ok(json!({"state": "merged", "sha": oid.to_string()}))
}

/// ENG-53: abandon a merge in progress and return to its starting commit.
fn merge_abort(repo: &Repository) -> Result<Value, String> {
    if repo.state() != git2::RepositoryState::Merge {
        return Err("No merge is in progress.".into());
    }
    let head = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| format!("Read HEAD commit: {error}"))?;
    let mut checkout = CheckoutBuilder::new();
    checkout.force();
    repo.reset(head.as_object(), git2::ResetType::Hard, Some(&mut checkout))
        .map_err(|error| format!("Reset merge: {error}"))?;
    repo.cleanup_state()
        .map_err(|error| format!("Clean up merge state: {error}"))?;
    Ok(json!({"state": "aborted"}))
}

/// Apply the remaining rebase operations, stopping at the first conflict.
fn drive_rebase(
    repo: &Repository,
    rebase: &mut git2::Rebase<'_>,
    signature: &git2::Signature<'_>,
) -> Result<Value, String> {
    loop {
        match rebase.next() {
            None => break,
            Some(Err(error)) => return Err(format!("Rebase step failed: {error}")),
            Some(Ok(_)) => {
                let index = repo
                    .index()
                    .map_err(|error| format!("Open index: {error}"))?;
                if index.has_conflicts() {
                    return Ok(json!({
                        "state": "conflicted",
                        "conflicts": conflict_paths(&index)?
                    }));
                }
                match rebase.commit(None, signature, None) {
                    Ok(_) => {}
                    // The change is already in the new base: skip it.
                    Err(error) if error.code() == git2::ErrorCode::Applied => {}
                    Err(error) => return Err(format!("Commit rebase step: {error}")),
                }
            }
        }
    }
    rebase
        .finish(Some(signature))
        .map_err(|error| format!("Finish rebase: {error}"))?;
    Ok(json!({"state": "rebased"}))
}

/// ENG-53: rebase the current branch onto `onto`. Starts only on a clean tree
/// and records `refs/<host slug>/prerebase` first.
fn rebase_start(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let onto = valid_branch(request.onto.as_deref())?;
    let signature = author_signature(request)?;
    require_idle(repo)?;
    if has_changes(repo)? {
        return Err("Working tree must be clean to rebase.".into());
    }
    let head = repo.head().map_err(|error| format!("Read HEAD: {error}"))?;
    if !head.is_branch() {
        return Err("Detached HEAD cannot be rebased.".into());
    }
    let head_commit = head
        .peel_to_commit()
        .map_err(|error| format!("Read HEAD commit: {error}"))?;
    drop(head);
    let upstream_branch = repo
        .find_branch(onto, BranchType::Local)
        .or_else(|_| repo.find_branch(onto, BranchType::Remote))
        .map_err(|error| format!("Find branch: {error}"))?;
    let upstream = repo
        .reference_to_annotated_commit(upstream_branch.get())
        .map_err(|error| format!("Read branch commit: {error}"))?;
    repo.reference(
        &format!("refs/{}/prerebase", rightkit_agent_config::host_slug()),
        head_commit.id(),
        true,
        &format!("{} pre-rebase HEAD", rightkit_agent_config::app_name()),
    )
    .map_err(|error| format!("Record pre-rebase HEAD: {error}"))?;
    let mut rebase = repo
        .rebase(None, Some(&upstream), None, None)
        .map_err(|error| format!("Start rebase: {error}"))?;
    drive_rebase(repo, &mut rebase, &signature)
}

/// ENG-53: commit the current rebase operation (after `resolve`) and go on.
fn rebase_continue(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let signature = author_signature(request)?;
    let mut rebase = repo
        .open_rebase(None)
        .map_err(|_| "No rebase is in progress.".to_string())?;
    let index = repo
        .index()
        .map_err(|error| format!("Open index: {error}"))?;
    if index.has_conflicts() {
        return Err("Resolve all conflicts first.".into());
    }
    if rebase.operation_current().is_some() {
        match rebase.commit(None, &signature, None) {
            Ok(_) => {}
            Err(error) if error.code() == git2::ErrorCode::Applied => {}
            Err(error) => return Err(format!("Commit rebase step: {error}")),
        }
    }
    drive_rebase(repo, &mut rebase, &signature)
}

/// ENG-53: abandon a rebase in progress and restore the original branch.
fn rebase_abort(repo: &Repository) -> Result<Value, String> {
    let mut rebase = repo
        .open_rebase(None)
        .map_err(|_| "No rebase is in progress.".to_string())?;
    rebase
        .abort()
        .map_err(|error| format!("Abort rebase: {error}"))?;
    Ok(json!({"state": "aborted"}))
}

/// Local runtime state (`<state folder>/runtime/...` and the sentinel broker log under the
/// host's state folder) is untracked and must never be swept into a stash or counted as a
/// change to the user's tree.
fn is_runtime_state_path(path: &str) -> bool {
    path.strip_prefix(rightkit_agent_config::state_dir_name())
        .is_some_and(|rest| rest.starts_with("/runtime/") || rest == "/sentinel/broker.jsonl")
}

/// ENG-54: the stash entries, newest first.
fn stash_list(repo: &mut Repository) -> Result<Value, String> {
    let mut entries = Vec::new();
    repo.stash_foreach(|index, message, id| {
        entries.push(json!({"index": index, "message": message, "id": id.to_string()}));
        true
    })
    .map_err(|error| format!("List stashes: {error}"))?;
    Ok(Value::Array(entries))
}

fn stash_create(repo: &mut Repository, request: &GitRequest) -> Result<Value, String> {
    let signature = author_signature(request)?;
    let include_untracked = request.include_untracked.unwrap_or(false);
    if include_untracked {
        let mut options = git2::StatusOptions::new();
        options.include_untracked(true).recurse_untracked_dirs(true);
        let runtime_state = repo
            .statuses(Some(&mut options))
            .map_err(|error| format!("Read status: {error}"))?
            .iter()
            .any(|entry| {
                entry.status() == git2::Status::WT_NEW
                    && entry.path().is_ok_and(is_runtime_state_path)
            });
        if runtime_state {
            return Err(
                "Stashing untracked files would remove local runtime state; stash tracked changes only."
                    .into(),
            );
        }
    }
    let message = request
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let flags = include_untracked.then_some(git2::StashFlags::INCLUDE_UNTRACKED);
    let oid = repo
        .stash_save2(&signature, message, flags)
        .map_err(|error| format!("Stash failed: {error}"))?;
    Ok(json!({"id": oid.to_string(), "message": message}))
}

fn stash_apply(repo: &mut Repository, index: Option<usize>) -> Result<Value, String> {
    let index = index.ok_or_else(|| "stashApply requires stashIndex.".to_string())?;
    repo.stash_apply(index, None)
        .map_err(|error| format!("Apply stash: {error}"))?;
    Ok(json!({"stashIndex": index}))
}

fn stash_drop(repo: &mut Repository, index: Option<usize>) -> Result<Value, String> {
    let index = index.ok_or_else(|| "stashDrop requires stashIndex.".to_string())?;
    repo.stash_drop(index)
        .map_err(|error| format!("Drop stash: {error}"))?;
    Ok(json!({"stashIndex": index, "dropped": true}))
}

/// A hex commit id of 4 to 64 characters, never a revision expression.
fn find_commit_by_sha<'repo>(
    repo: &'repo Repository,
    raw: Option<&str>,
) -> Result<git2::Commit<'repo>, String> {
    let sha = required_text(raw, "sha")?;
    if sha.len() < 4 || sha.len() > 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid commit id.".into());
    }
    repo.revparse_single(sha)
        .and_then(|object| object.peel_to_commit())
        .map_err(|error| format!("Find commit: {error}"))
}

/// ENG-54: revert one commit onto a clean tree as a new commit.
fn revert(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let signature = author_signature(request)?;
    require_idle(repo)?;
    if has_changes(repo)? {
        return Err("Working tree must be clean to revert.".into());
    }
    let target = find_commit_by_sha(repo, request.sha.as_deref())?;
    if target.parent_count() > 1 {
        return Err("Reverting a merge commit is not supported.".into());
    }
    let head_commit = repo
        .head()
        .and_then(|head| head.peel_to_commit())
        .map_err(|error| format!("Read HEAD commit: {error}"))?;
    let mut index = repo
        .revert_commit(&target, &head_commit, 0, None)
        .map_err(|error| format!("Revert failed: {error}"))?;
    if index.has_conflicts() {
        return Err(match repo.cleanup_state() {
            Ok(()) => "Revert conflicts with later changes.".to_string(),
            Err(error) => {
                format!("Revert conflicts with later changes (state cleanup failed: {error}).")
            }
        });
    }
    let tree_id = index
        .write_tree_to(repo)
        .map_err(|error| format!("Write tree: {error}"))?;
    let tree = repo
        .find_tree(tree_id)
        .map_err(|error| format!("Read tree: {error}"))?;
    let summary = target
        .summary()
        .ok()
        .flatten()
        .unwrap_or("commit")
        .to_owned();
    let message = format!(
        "Revert \"{summary}\"\n\nThis reverts commit {}.\n",
        target.id()
    );
    // Update the files first: the checkout baseline is HEAD, so committing
    // first would leave nothing to check out.
    repo.checkout_tree(tree.as_object(), Some(CheckoutBuilder::new().safe()))
        .map_err(|error| format!("Check out revert: {error}"))?;
    let oid = repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            &message,
            &tree,
            &[&head_commit],
        )
        .map_err(|error| format!("Commit revert: {error}"))?;
    Ok(json!({"sha": oid.to_string()}))
}

fn valid_tag(raw: Option<&str>) -> Result<&str, String> {
    let tag = required_text(raw, "tag")?;
    if !git2::Reference::is_valid_name(&format!("refs/tags/{tag}")) {
        return Err("Invalid tag name.".into());
    }
    Ok(tag)
}

/// ENG-54: every tag with its target and, for an annotated one, its message.
fn tag_list(repo: &Repository) -> Result<Value, String> {
    let mut names = Vec::new();
    repo.tag_foreach(|_, name| {
        names.push(String::from_utf8_lossy(name).into_owned());
        true
    })
    .map_err(|error| format!("List tags: {error}"))?;
    let mut tags = Vec::new();
    for full_name in names {
        let Some(name) = full_name.strip_prefix("refs/tags/") else {
            continue;
        };
        let Ok(reference) = repo.find_reference(&full_name) else {
            continue;
        };
        let Some(target) = reference.target() else {
            continue;
        };
        match repo.find_tag(target) {
            Ok(tag) => tags.push(json!({
                "name": name,
                "target": tag.target_id().to_string(),
                "annotated": true,
                "message": tag.message().ok().flatten()
            })),
            Err(_) => tags.push(json!({
                "name": name,
                "target": target.to_string(),
                "annotated": false,
                "message": Value::Null
            })),
        }
    }
    Ok(Value::Array(tags))
}

/// ENG-54: a lightweight tag, or an annotated one when a message is given.
/// Never forced: an existing name is an error.
fn tag_create(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let tag = valid_tag(request.tag.as_deref())?;
    let target = match request.sha.as_deref() {
        Some(_) => find_commit_by_sha(repo, request.sha.as_deref())?,
        None => repo
            .head()
            .and_then(|head| head.peel_to_commit())
            .map_err(|error| format!("Read HEAD commit: {error}"))?,
    };
    let message = request
        .message
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let oid = match message {
        None => repo
            .tag_lightweight(tag, target.as_object(), false)
            .map_err(|error| format!("Create tag: {error}"))?,
        Some(message) => {
            let signature = author_signature(request)?;
            repo.tag(tag, target.as_object(), &signature, message, false)
                .map_err(|error| format!("Create tag: {error}"))?
        }
    };
    Ok(json!({
        "name": tag,
        "target": target.id().to_string(),
        "id": oid.to_string(),
        "annotated": message.is_some()
    }))
}

fn tag_delete(repo: &Repository, tag: Option<&str>) -> Result<Value, String> {
    let tag = valid_tag(tag)?;
    repo.tag_delete(tag)
        .map_err(|error| format!("Delete tag: {error}"))?;
    Ok(json!({"name": tag, "deleted": true}))
}

/// ENG-54: push one tag. The refspec is never forced and never a deletion.
fn tag_push(
    repo: &Repository,
    tag: Option<&str>,
    token: Option<&str>,
    loopback: Option<&str>,
    guard: Option<&OperationGuard>,
) -> Result<Value, String> {
    let tag = valid_tag(tag)?;
    let refspec = format!("refs/tags/{tag}:refs/tags/{tag}");
    let remote_url = push_refspec(repo, &refspec, token, loopback, guard)?;
    Ok(json!({"tag": tag, "remoteUrl": remote_url}))
}

fn commit_summary_json(commit: &git2::Commit<'_>) -> Value {
    let author = commit.author();
    json!({
        "sha": commit.id().to_string(),
        "summary": commit.summary().ok().flatten().unwrap_or(""),
        "author": author.name().unwrap_or(""),
        "email": author.email().unwrap_or(""),
        "time": commit.time().seconds()
    })
}

/// ENG-54: a page of history over HEAD or one local branch.
fn log(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let skip = request.skip.unwrap_or(0);
    let limit = request.limit.unwrap_or(30).clamp(1, 100);
    let mut walk = repo
        .revwalk()
        .map_err(|error| format!("Walk history: {error}"))?;
    walk.set_sorting(git2::Sort::TIME)
        .map_err(|error| format!("Walk history: {error}"))?;
    match request.branch.as_deref() {
        Some(_) => {
            let branch = valid_branch(request.branch.as_deref())?;
            walk.push_ref(&format!("refs/heads/{branch}"))
                .map_err(|error| format!("Find branch: {error}"))?;
        }
        None => {
            if walk.push_head().is_err() {
                // An unborn branch has no history yet.
                return Ok(json!({"commits": [], "hasMore": false}));
            }
        }
    }
    let mut commits = Vec::new();
    for oid in walk.skip(skip).take(limit + 1) {
        let oid = oid.map_err(|error| format!("Walk history: {error}"))?;
        let commit = repo
            .find_commit(oid)
            .map_err(|error| format!("Read commit: {error}"))?;
        commits.push(commit_summary_json(&commit));
    }
    let has_more = commits.len() > limit;
    commits.truncate(limit);
    Ok(json!({"commits": commits, "hasMore": has_more}))
}

/// ENG-54: one commit with its files and a capped diff against its first parent.
fn commit_detail(repo: &Repository, sha: Option<&str>) -> Result<Value, String> {
    const DIFF_LIMIT: usize = 128 * 1024;
    let commit = find_commit_by_sha(repo, sha)?;
    let tree = commit
        .tree()
        .map_err(|error| format!("Read tree: {error}"))?;
    let parent_tree = match commit.parent(0) {
        Ok(parent) => Some(
            parent
                .tree()
                .map_err(|error| format!("Read parent tree: {error}"))?,
        ),
        Err(_) => None,
    };
    let mut diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
        .map_err(|error| format!("Read diff: {error}"))?;
    diff.find_similar(None)
        .map_err(|error| format!("Detect renames: {error}"))?;
    let mut files = Vec::new();
    for (position, delta) in diff.deltas().enumerate() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let status = match delta.status() {
            git2::Delta::Added => "added",
            git2::Delta::Deleted => "deleted",
            git2::Delta::Renamed => "renamed",
            git2::Delta::Copied => "copied",
            git2::Delta::Typechange => "typeChanged",
            _ => "modified",
        };
        let (additions, deletions) = match git2::Patch::from_diff(&diff, position) {
            Ok(Some(patch)) => patch
                .line_stats()
                .map(|(_, additions, deletions)| (additions, deletions))
                .unwrap_or((0, 0)),
            _ => (0, 0),
        };
        files.push(json!({
            "path": path,
            "status": status,
            "additions": additions,
            "deletions": deletions
        }));
    }
    let mut bytes = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_, _, line| {
        if matches!(line.origin(), '+' | '-' | ' ') {
            bytes.push(line.origin() as u8);
        }
        bytes.extend_from_slice(line.content());
        bytes.len() <= DIFF_LIMIT
    })
    // Returning `false` to stop at the cap makes libgit2 report an error;
    // that is the cap being reached, not a failure.
    .or_else(|error| {
        if bytes.len() > DIFF_LIMIT {
            Ok(())
        } else {
            Err(format!("Format diff: {error}"))
        }
    })?;
    if bytes.len() > DIFF_LIMIT {
        bytes.truncate(DIFF_LIMIT);
    }
    let author = commit.author();
    Ok(json!({
        "sha": commit.id().to_string(),
        "message": commit.message().unwrap_or(""),
        "author": author.name().unwrap_or(""),
        "email": author.email().unwrap_or(""),
        "time": commit.time().seconds(),
        "parents": commit.parent_ids().map(|id| id.to_string()).collect::<Vec<_>>(),
        "files": files,
        "diff": String::from_utf8_lossy(&bytes)
    }))
}

/// ENG-55: LFS pointer files are tiny text blobs; anything larger cannot be
/// a pointer and is skipped without reading its full content.
const LFS_MAX_POINTER_TEXT_BYTES: u64 = 4096;

struct LfsPointer {
    path: String,
    oid: String,
    size: u64,
}

fn parse_lfs_pointer_text(text: &str) -> Option<(String, u64)> {
    if !text.starts_with("version https://git-lfs.github.com/spec/v1") {
        return None;
    }
    let mut oid = None;
    let mut size = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("oid sha256:") {
            oid = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("size ") {
            size = rest.trim().parse::<u64>().ok();
        }
    }
    match (oid, size) {
        (Some(oid), Some(size)) => Some((oid, size)),
        _ => None,
    }
}

/// Walk HEAD's tree and collect every blob that looks like an LFS pointer
/// file. Read-only; no network access.
fn scan_lfs_pointers(repo: &Repository) -> Result<Vec<LfsPointer>, String> {
    let mut found = Vec::new();
    if let Some(tree) = repo.head().ok().and_then(|head| head.peel_to_tree().ok()) {
        tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
            if entry.kind() != Some(git2::ObjectType::Blob) {
                return git2::TreeWalkResult::Ok;
            }
            let Ok(name) = entry.name() else {
                return git2::TreeWalkResult::Ok;
            };
            let Ok(object) = entry.to_object(repo) else {
                return git2::TreeWalkResult::Ok;
            };
            let Some(blob) = object.as_blob() else {
                return git2::TreeWalkResult::Ok;
            };
            if blob.size() as u64 > LFS_MAX_POINTER_TEXT_BYTES {
                return git2::TreeWalkResult::Ok;
            }
            if let Ok(text) = std::str::from_utf8(blob.content()) {
                if let Some((oid, size)) = parse_lfs_pointer_text(text) {
                    found.push(LfsPointer {
                        path: format!("{dir}{name}"),
                        oid,
                        size,
                    });
                }
            }
            git2::TreeWalkResult::Ok
        })
        .map_err(|error| format!("Walk tree: {error}"))?;
    }
    if let Some(workdir) = repo.workdir() {
        scan_workdir_lfs_pointers(workdir, workdir, &mut found)?;
    }
    found.sort_by(|left, right| left.path.cmp(&right.path));
    found.dedup_by(|left, right| left.path == right.path);
    Ok(found)
}

fn scan_workdir_lfs_pointers(
    root: &Path,
    dir: &Path,
    found: &mut Vec<LfsPointer>,
) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|error| format!("Read LFS directory: {error}"))? {
        let entry = entry.map_err(|error| format!("Read LFS entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Read LFS file type: {error}"))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some(".git") {
                scan_workdir_lfs_pointers(root, &path, found)?;
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| format!("Read LFS metadata: {error}"))?;
        if metadata.len() > LFS_MAX_POINTER_TEXT_BYTES {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some((oid, size)) = parse_lfs_pointer_text(&text) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "LFS pointer escaped workspace root.".to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        found.retain(|pointer| pointer.path != relative);
        found.push(LfsPointer {
            path: relative,
            oid,
            size,
        });
    }
    Ok(())
}

/// ENG-55: read-only pointer inventory for the iOS host UI. No network
/// access, no capability gate, mirrors `git_lfs_scan` in `rightkit_agent_coding_tools`.
fn lfs_scan(repo: &Repository) -> Result<Value, String> {
    let pointers = scan_lfs_pointers(repo)?;
    let total_declared_bytes: u64 = pointers.iter().map(|pointer| pointer.size).sum();
    Ok(json!({
        "pointers": pointers.iter().map(|pointer| json!({
            "path": pointer.path,
            "oid": pointer.oid,
            "size": pointer.size
        })).collect::<Vec<_>>(),
        "totalDeclaredBytes": total_declared_bytes
    }))
}

fn validate_lfs_paths(paths: Option<&[String]>, operation: &str) -> Result<Vec<PathBuf>, String> {
    let paths = paths.filter(|paths| !paths.is_empty()).ok_or_else(|| {
        format!("{operation} requires at least one exact path; wildcards are refused.")
    })?;
    paths
        .iter()
        .map(|value| {
            let path = Path::new(value);
            if value.trim().is_empty()
                || value.contains(['*', '?', '[', '\n', '\r'])
                || path.is_absolute()
                || path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
            {
                return Err(format!("Invalid exact LFS path '{value}'."));
            }
            Ok(path.to_path_buf())
        })
        .collect()
}

fn contained_existing_path(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("Resolve workspace: {error}"))?;
    let path = root.join(relative);
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("Resolve '{}': {error}", relative.display()))?;
    if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
        return Err(format!(
            "LFS path '{}' is not a regular file inside the workspace.",
            relative.display()
        ));
    }
    Ok(canonical)
}

fn is_lfs_pointer_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if metadata.len() > LFS_MAX_POINTER_TEXT_BYTES {
        return false;
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse_lfs_pointer_text(&text))
        .is_some()
}

fn scan_lfs_candidates(
    root: &Path,
    dir: &Path,
    min_bytes: u64,
    found: &mut Vec<Value>,
) -> Result<(), String> {
    for entry in
        std::fs::read_dir(dir).map_err(|error| format!("Read candidate directory: {error}"))?
    {
        let entry = entry.map_err(|error| format!("Read candidate entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Read candidate type: {error}"))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some(".git") {
                scan_lfs_candidates(root, &path, min_bytes, found)?;
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let size = entry
            .metadata()
            .map_err(|error| format!("Read candidate metadata: {error}"))?
            .len();
        if size < min_bytes || is_lfs_pointer_file(&path) {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "Candidate escaped workspace root.".to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        found.push(json!({"path": relative, "size": size}));
    }
    Ok(())
}

/// Detect large, non-pointer working-tree files that are candidates for LFS.
fn lfs_candidates(repo: &Repository, min_bytes: u64) -> Result<Value, String> {
    if min_bytes == 0 {
        return Err("lfsCandidates minBytes must be greater than zero.".into());
    }
    let root = repo
        .workdir()
        .ok_or_else(|| "Bare repositories are unsupported for LFS candidates.".to_string())?;
    let mut candidates = Vec::new();
    scan_lfs_candidates(root, root, min_bytes, &mut candidates)?;
    candidates.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    Ok(json!({"minBytes": min_bytes, "candidates": candidates}))
}

fn hash_file(path: &Path) -> Result<(String, u64), String> {
    let mut file =
        File::open(path).map_err(|error| format!("Open '{}': {error}", path.display()))?;
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Read '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((hasher.finalize(), size))
}

fn lfs_object_path(objects_root: &Path, oid: &str) -> Result<PathBuf, String> {
    if oid.len() != 64 || !oid.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(format!("Invalid LFS object id '{oid}'."));
    }
    Ok(objects_root.join(&oid[0..2]).join(&oid[2..4]).join(oid))
}

fn copy_to_lfs_cache(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        return Ok(());
    }
    let mut input = File::open(source)
        .map_err(|error| format!("Open '{}' for LFS cache: {error}", source.display()))?;
    rightkit_fs::atomic_write_with(
        destination,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |file| std::io::copy(&mut input, file).map(|_| ()),
    )
    .map_err(|error| format!("Write LFS object: {error}"))
}

fn atomic_write(destination: &Path, bytes: &[u8]) -> Result<(), String> {
    rightkit_fs::atomic_write(destination, bytes)
        .map_err(|error| format!("Replace '{}': {error}", destination.display()))
}

fn attributes_pattern(path: &Path) -> Result<String, String> {
    let raw = path
        .to_str()
        .ok_or_else(|| "LFS paths must be UTF-8.".to_string())?;
    Ok(raw
        .replace('\\', "\\\\")
        .replace(' ', "\\ ")
        .replace('\t', "\\t"))
}

/// Convert exact workspace files into canonical LFS pointers plus cache objects.
/// Every source is preflighted and bounded before the first mutation.
fn lfs_track(repo: &Repository, request: &GitRequest) -> Result<Value, String> {
    let relative_paths = validate_lfs_paths(request.paths.as_deref(), "lfsTrack")?;
    let max_bytes = request
        .max_bytes
        .ok_or_else(|| "lfsTrack requires maxBytes.".to_string())?;
    let root = repo
        .workdir()
        .ok_or_else(|| "Bare repositories are unsupported for LFS tracking.".to_string())?;
    let mut planned = Vec::new();
    let mut total_bytes = 0u64;
    for relative in &relative_paths {
        let source = contained_existing_path(root, relative)?;
        if is_lfs_pointer_file(&source) {
            return Err(format!(
                "'{}' is already an LFS pointer.",
                relative.display()
            ));
        }
        let (oid, size) = hash_file(&source)?;
        total_bytes = total_bytes
            .checked_add(size)
            .ok_or_else(|| "LFS tracking size overflow.".to_string())?;
        planned.push((relative.clone(), source, oid, size));
    }
    if total_bytes > max_bytes {
        return Err(format!(
            "Refusing LFS track: source size {total_bytes} bytes exceeds maxBytes {max_bytes}."
        ));
    }

    let objects_root = repo.path().join("lfs").join("objects");
    let attributes_path = root.join(".gitattributes");
    let mut attributes = std::fs::read_to_string(&attributes_path).unwrap_or_default();
    if !attributes.is_empty() && !attributes.ends_with('\n') {
        attributes.push('\n');
    }
    let mut tracked = Vec::new();
    for (relative, source, oid, size) in planned {
        let object_path = lfs_object_path(&objects_root, &oid)?;
        copy_to_lfs_cache(&source, &object_path)?;
        let pointer =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n");
        atomic_write(&source, pointer.as_bytes())?;
        let pattern = attributes_pattern(&relative)?;
        let rule = format!("{pattern} filter=lfs diff=lfs merge=lfs -text");
        if !attributes.lines().any(|line| line == rule) {
            attributes.push_str(&rule);
            attributes.push('\n');
        }
        tracked.push(json!({
            "path": relative.to_string_lossy().replace('\\', "/"),
            "oid": oid,
            "size": size,
            "objectPath": object_path
        }));
    }
    atomic_write(&attributes_path, attributes.as_bytes())?;
    Ok(json!({
        "tracked": tracked,
        "totalBytes": total_bytes,
        "maxBytes": max_bytes,
        "attributesPath": ".gitattributes"
    }))
}

struct MobileLfsFetchPlan {
    workspace_root: PathBuf,
    remote_name: String,
    remote_url: String,
    objects_root: PathBuf,
    selected: Vec<(String, String, u64)>,
    total_declared_bytes: u64,
    max_bytes: u64,
}

fn lfs_https_remote(raw: &str) -> Result<String, String> {
    if raw.starts_with("https://") {
        return validated_github_url(raw);
    }
    let path = if let Some(path) = raw.strip_prefix("git@github.com:") {
        path
    } else if let Some(path) = raw.strip_prefix("ssh://git@github.com/") {
        path
    } else {
        return Err("LFS supports credential-free HTTPS or GitHub SSH remotes; SSH transfer uses the scoped GitHub token over the equivalent HTTPS LFS endpoint.".into());
    };
    if path.is_empty() || path.contains(['?', '#']) {
        return Err("Invalid GitHub SSH remote for LFS.".into());
    }
    validated_github_url(&format!("https://github.com/{path}"))
}

fn resolve_lfs_fetch_plan(
    repo: &Repository,
    request: &GitRequest,
) -> Result<MobileLfsFetchPlan, String> {
    let relative_paths = validate_lfs_paths(request.paths.as_deref(), "lfsFetch")?;
    let requested = request
        .paths
        .as_ref()
        .ok_or_else(|| "lfsFetch requires paths.".to_string())?;
    let max_bytes = request
        .max_bytes
        .ok_or_else(|| "lfsFetch requires maxBytes.".to_string())?;

    let all_pointers = scan_lfs_pointers(repo)?;
    let mut selected = Vec::new();
    for (path, relative) in requested.iter().zip(&relative_paths) {
        let pointer = all_pointers
            .iter()
            .find(|candidate| candidate.path == *path)
            .ok_or_else(|| format!("'{path}' is not a tracked LFS pointer file."))?;
        selected.push((
            relative.to_string_lossy().replace('\\', "/"),
            pointer.oid.clone(),
            pointer.size,
        ));
    }
    let total_declared_bytes: u64 = selected.iter().map(|(_, _, size)| *size).sum();
    if total_declared_bytes > max_bytes {
        return Err(format!(
            "Refusing LFS fetch: declared size {total_declared_bytes} bytes exceeds maxBytes {max_bytes}."
        ));
    }

    let remote_name = request.remote.as_deref().unwrap_or("origin");
    let remote = repo
        .find_remote(remote_name)
        .map_err(|error| format!("Find remote '{remote_name}': {error}"))?;
    let remote_url = lfs_https_remote(remote.url().map_err(|error| error.to_string())?)?;
    Ok(MobileLfsFetchPlan {
        workspace_root: repo
            .workdir()
            .ok_or_else(|| "Bare repositories are unsupported for LFS hydration.".to_string())?
            .to_path_buf(),
        remote_name: remote_name.to_string(),
        remote_url,
        objects_root: repo.path().join("lfs").join("objects"),
        selected,
        total_declared_bytes,
        max_bytes,
    })
}

fn hydrate_lfs_object(
    root: &Path,
    relative: &str,
    oid: &str,
    size: u64,
    object_path: &Path,
) -> Result<(), String> {
    let target = contained_existing_path(root, Path::new(relative))?;
    let pointer = std::fs::read_to_string(&target)
        .map_err(|error| format!("Read LFS pointer '{relative}': {error}"))?;
    if parse_lfs_pointer_text(&pointer) != Some((oid.to_string(), size)) {
        return Err(format!(
            "LFS pointer '{relative}' changed before hydration."
        ));
    }
    let (actual_oid, actual_size) = hash_file(object_path)?;
    if actual_oid != oid || actual_size != size {
        return Err(format!(
            "Cached LFS object {oid} failed size/hash verification."
        ));
    }
    let mut input = File::open(object_path)
        .map_err(|error| format!("Open cached LFS object {oid}: {error}"))?;
    rightkit_fs::atomic_write_with(&target, rightkit_fs::AtomicOptions::default(), |file| {
        std::io::copy(&mut input, file).map(|_| ())
    })
    .map_err(|error| format!("Hydrate '{relative}': {error}"))?;
    Ok(())
}

/// Fetch each bounded object independently so one server-side failure yields
/// an honest partial-hydration receipt instead of losing successful work.
async fn lfs_fetch(plan: MobileLfsFetchPlan, token: Option<&str>) -> Result<Value, String> {
    let mut objects = Vec::with_capacity(plan.selected.len());
    let mut hydrated_count = 0usize;
    for (path, oid, size) in &plan.selected {
        let cached = lfs_object_path(&plan.objects_root, oid)?;
        let transfer = if cached.exists() {
            Ok(cached.clone())
        } else {
            let request = rightkit_agent_coding_tools::git_lfs_batch_client::BatchObjectRequest {
                oid: oid.clone(),
                size: *size,
            };
            rightkit_agent_coding_tools::git_lfs_batch_client::fetch_objects(
                &plan.remote_url,
                &[request],
                &plan.objects_root,
                token,
                None,
            )
            .await
            .and_then(|mut fetched| {
                fetched
                    .pop()
                    .map(|object| object.path)
                    .ok_or_else(|| format!("LFS server returned no object for {oid}."))
            })
        };
        match transfer.and_then(|object_path| {
            hydrate_lfs_object(&plan.workspace_root, path, oid, *size, &object_path)?;
            Ok(object_path)
        }) {
            Ok(object_path) => {
                hydrated_count += 1;
                objects.push(json!({
                    "path": path,
                    "oid": oid,
                    "size": size,
                    "hydrated": true,
                    "objectPath": object_path
                }));
            }
            Err(error) => objects.push(json!({
                "path": path,
                "oid": oid,
                "size": size,
                "hydrated": false,
                "error": error
            })),
        }
    }
    let failed_count = plan.selected.len().saturating_sub(hydrated_count);
    Ok(json!({
        "remote": plan.remote_name,
        "remoteUrl": plan.remote_url,
        "requestedPaths": plan.selected.iter().map(|(path, _, _)| path).collect::<Vec<_>>(),
        "objects": objects,
        "totalDeclaredBytes": plan.total_declared_bytes,
        "maxBytes": plan.max_bytes,
        "hydratedCount": hydrated_count,
        "failedCount": failed_count,
        "fetched": failed_count == 0,
        "partial": hydrated_count > 0 && failed_count > 0
    }))
}
