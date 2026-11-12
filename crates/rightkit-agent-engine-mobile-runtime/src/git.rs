//! Typed Git transport for iOS host UI. No shell, URL credentials, force
//! operations, wildcard pathspecs, merges, or rebases.

use git2::{
    build::RepoBuilder, BranchType, FetchOptions, PushOptions, RemoteCallbacks, Repository,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
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
    match request.operation.as_str() {
        "clone" => clone_repo(&request),
        "status" => status(&open_repo(&request.workspace_root)?),
        "diff" => diff(&open_repo(&request.workspace_root)?),
        "pull" => pull(
            &open_repo(&request.workspace_root)?,
            request.token.as_deref(),
        ),
        "stage" => stage(
            &open_repo(&request.workspace_root)?,
            request.paths.as_deref(),
        ),
        "commit" => commit(&open_repo(&request.workspace_root)?, &request),
        "push" => push(
            &open_repo(&request.workspace_root)?,
            request.token.as_deref(),
        ),
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
        "fetch" => fetch(
            &open_repo(&request.workspace_root)?,
            request.remote.as_deref(),
            request.token.as_deref(),
        ),
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

fn callbacks(token: Option<&str>) -> RemoteCallbacks<'static> {
    let token = token.map(str::to_owned);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, _username, allowed| {
        if !url.starts_with("https://github.com/") {
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

fn clone_repo(request: &GitRequest) -> Result<Value, String> {
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
    let mut fetch = FetchOptions::new();
    fetch.remote_callbacks(callbacks(request.token.as_deref()));
    let mut builder = RepoBuilder::new();
    builder.fetch_options(fetch);
    let repo = builder
        .clone(&remote_url, &request.workspace_root)
        .map_err(|error| format!("Clone failed: {error}"))?;
    status(&repo)
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
        "branches": branches
    }))
}

fn diff(repo: &Repository) -> Result<Value, String> {
    let mut options = git2::DiffOptions::new();
    options.include_untracked(true).recurse_untracked_dirs(true);
    let diff = repo
        .diff_tree_to_workdir_with_index(None, Some(&mut options))
        .map_err(|error| format!("Read diff: {error}"))?;
    let mut bytes = Vec::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
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
                && entry.path().is_ok_and(|path| {
                    path.starts_with(".rightkit/runtime/")
                        || path == ".rightkit/sentinel/broker.jsonl"
                }))
        }))
}

fn pull(repo: &Repository, token: Option<&str>) -> Result<Value, String> {
    if has_changes(repo)? {
        return Err("Pull requires a clean working tree.".into());
    }
    let mut remote = repo
        .find_remote("origin")
        .map_err(|error| format!("Find origin: {error}"))?;
    validated_github_url(remote.url().map_err(|error| error.to_string())?)?;
    let mut fetch = FetchOptions::new();
    fetch.remote_callbacks(callbacks(token));
    remote
        .fetch(&[] as &[&str], Some(&mut fetch), None)
        .map_err(|error| format!("Fetch failed: {error}"))?;
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
        .set_target(annotated.id(), "Agent fast-forward pull")
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

fn push(repo: &Repository, token: Option<&str>) -> Result<Value, String> {
    let head = repo.head().map_err(|error| format!("Read HEAD: {error}"))?;
    let branch = head
        .shorthand()
        .map_err(|_| "Detached HEAD cannot push.".to_string())?;
    let remote_url = repo
        .find_remote("origin")
        .map_err(|error| error.to_string())?
        .url()
        .map_err(|error| error.to_string())?
        .to_owned();
    validated_github_url(&remote_url)?;
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    let mut remote = repo
        .find_remote("origin")
        .map_err(|error| format!("Find origin: {error}"))?;
    let mut options = PushOptions::new();
    options.remote_callbacks(callbacks(token));
    remote
        .push(&[&refspec], Some(&mut options))
        .map_err(|error| format!("Push failed: {error}"))?;
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
fn fetch(repo: &Repository, remote: Option<&str>, token: Option<&str>) -> Result<Value, String> {
    let remote_name = remote.unwrap_or("origin");
    let mut remote = repo
        .find_remote(remote_name)
        .map_err(|error| format!("Find remote '{remote_name}': {error}"))?;
    let remote_url = remote.url().map_err(|error| error.to_string())?.to_owned();
    validated_github_url(&remote_url)?;
    let mut fetch = FetchOptions::new();
    fetch.remote_callbacks(callbacks(token));
    remote
        .fetch(&[] as &[&str], Some(&mut fetch), None)
        .map_err(|error| format!("Fetch failed: {error}"))?;
    let stats = remote.stats();
    Ok(json!({
        "remote": remote_name,
        "remoteUrl": remote_url,
        "receivedObjects": stats.received_objects(),
        "totalObjects": stats.total_objects()
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
