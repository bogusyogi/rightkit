//! ENG-55: Git LFS pointer awareness and bounded object fetch.
//!
//! the host app never runs `git lfs pull` (or any equivalent whole-repo LFS
//! fetch). A phone on a metered connection must never silently download
//! unbounded binary data. Instead this module gives the caller:
//!
//! 1. `git_lfs_scan` — read-only. Walks the working tree (HEAD or index) and
//!    reports which tracked files are LFS pointer files, parsing each
//!    pointer's `oid` and declared `size` from its text body. No network
//!    access, no capability gate.
//! 2. `git_lfs_fetch` — gated like the other Git-write tools. The caller
//!    must name exact pointer paths to fetch; the tool resolves each to its
//!    pointer's declared oid/size, sums the declared sizes, and refuses the
//!    whole operation up front — before any network access — if the total
//!    exceeds an explicit byte ceiling (`max_bytes`, required, no default
//!    that could be raised silently) or if any named path is not a tracked
//!    pointer file. There is no "fetch everything under this ceiling" mode
//!    and no wildcard form — every object must be named.
//!
//!    `git2` (even with `vendored-libgit2`) exposes no LFS batch/transfer
//!    API — LFS itself is a Git extension implemented by the separate
//!    `git-lfs` client speaking its own HTTPS batch protocol, not by
//!    libgit2. So the actual object transfer is implemented out-of-band in
//!    `git_lfs_batch_client.rs`, which `lfs_fetch` below calls after the
//!    bounding contract passes. Only HTTPS remotes are supported; an SSH
//!    remote fails fast with an explicit error rather than silently
//!    reporting `"fetched": false` (see `git_lfs_batch_client::batch_endpoint`).
//!
//! Follows the exact guarded pattern established by `git_write`/`git_clone`/
//! `git_fetch`: typed `String` errors, workspace containment via
//! `crate::coding_tools::read_file::resolve_in_workspace` (through
//! `git_read::open_workspace_repository`), `PermissionEngine` gating on
//! `CapabilityClass::GitWrite` for the fetch tool (scan stays read-only and
//! ungated, matching `git_read`), and `ctx.github_token` / `ctx.cancel_rx`
//! plumbed straight through to `git_lfs_batch_client::fetch_objects`.
//!
//! git2 in this workspace: `remote.url()` returns `Result<_, git2::Error>`,
//! `reference.symbolic_target()` returns `Result<Option<&str>, _>`, and
//! `repo.remotes().iter()` needs `.flatten().flatten()`. This module does
//! not call `head.shorthand()`, `remote.pushurl()`, or `index.conflict_get()`,
//! so those divergences do not apply here, but every git2 call below was
//! checked against the working call sites in `git_read.rs`, `git_clone.rs`,
//! and `git_fetch.rs` before use.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use git2::Repository;
use rightkit_agent_policy::{
    CapabilityClass, PermissionDecision, PermissionEngine, PermissionMode,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// LFS pointer files are tiny text blobs (the real spec caps them well under
/// 1 KiB); anything larger cannot be a pointer and is skipped without
/// reading its full content into memory.
const MAX_POINTER_TEXT_BYTES: u64 = 4096;

/// A parsed LFS pointer file.
struct LfsPointer {
    path: String,
    oid: String,
    size: u64,
}

fn parse_pointer_text(text: &str) -> Option<(String, u64)> {
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

/// Walk HEAD's tree (falling back to the working tree if there is no HEAD
/// commit yet) and collect every blob that looks like an LFS pointer file.
fn scan_pointers(repo: &Repository) -> Result<Vec<LfsPointer>, String> {
    let mut found = Vec::new();
    match repo.head().ok().and_then(|head| head.peel_to_tree().ok()) {
        Some(tree) => {
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
                if blob.size() as u64 > MAX_POINTER_TEXT_BYTES {
                    return git2::TreeWalkResult::Ok;
                }
                if let Ok(text) = std::str::from_utf8(blob.content()) {
                    if let Some((oid, size)) = parse_pointer_text(text) {
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
        None => {
            // No commits yet: fall back to scanning the working tree so a
            // freshly cloned/initialised repo still reports pointers.
            let workdir = repo
                .workdir()
                .ok_or_else(|| "Bare repositories are unsupported for LFS scan.".to_string())?;
            scan_workdir(workdir, workdir, &mut found)?;
        }
    }
    Ok(found)
}

fn scan_workdir(root: &Path, dir: &Path, found: &mut Vec<LfsPointer>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|error| format!("Read dir: {error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("Read dir entry: {error}"))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Read file type: {error}"))?;
        if file_type.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) == Some(".git") {
                continue;
            }
            scan_workdir(root, &path, found)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| format!("Stat file: {error}"))?;
        if metadata.len() > MAX_POINTER_TEXT_BYTES {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Some((oid, size)) = parse_pointer_text(&text) {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push(LfsPointer {
                    path: relative,
                    oid,
                    size,
                });
            }
        }
    }
    Ok(())
}

pub struct GitLfsScanTool;

#[async_trait::async_trait]
impl Tool for GitLfsScanTool {
    fn name(&self) -> &str {
        "git_lfs_scan"
    }

    fn description(&self) -> &str {
        "Read-only: list Git LFS pointer files tracked at HEAD (oid, declared size, path). Performs no network access and downloads nothing."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": { "type": "string" }
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        None
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let repo = match crate::coding_tools::git_read::open_workspace_repository(
            args["repo_path"].as_str(),
            ctx,
        ) {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };
        match scan_pointers(&repo) {
            Ok(pointers) => {
                let total_declared_bytes: u64 = pointers.iter().map(|pointer| pointer.size).sum();
                ToolResult::ok(
                    json!({
                        "operation": "lfs_scan",
                        "pointers": pointers.iter().map(|pointer| json!({
                            "path": pointer.path,
                            "oid": pointer.oid,
                            "size": pointer.size
                        })).collect::<Vec<_>>(),
                        "totalDeclaredBytes": total_declared_bytes
                    })
                    .to_string(),
                )
            }
            Err(error) => ToolResult::error(error),
        }
    }
}

// The `RemoteCallbacks`/credential/cancellation pattern from `git_fetch.rs`
// does not apply directly here — libgit2 never touches the LFS transfer, so
// there is no `git2::RemoteCallbacks` to hang the GitHub-token gate or the
// cancel flag off of. `git_lfs_batch_client` reimplements the equivalent
// `Arc<AtomicBool>` cancel pattern and the same GitHub-host token gate over
// `rightkit-http` calls instead (see `git_lfs_batch_client::spawn_cancel_flag`
// / `github_bearer`).

pub struct GitLfsFetchTool;

#[async_trait::async_trait]
impl Tool for GitLfsFetchTool {
    fn name(&self) -> &str {
        "git_lfs_fetch"
    }

    fn description(&self) -> &str {
        "Fetch exact, named Git LFS objects (by pointer path) up to a caller-declared byte ceiling. Refuses to run if the named objects' declared sizes exceed max_bytes, and never performs an unbounded whole-repo LFS pull."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["paths", "max_bytes"],
            "properties": {
                "repo_path": { "type": "string" },
                "remote": { "type": "string", "description": "Remote name to fetch LFS objects from (defaults to origin)" },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exact working-tree paths of LFS pointer files to materialize. Wildcards are refused."
                },
                "max_bytes": {
                    "type": "integer",
                    "description": "Hard ceiling on the sum of declared pointer sizes for this call. The operation is refused outright, before any network access, if the named objects exceed it."
                }
            }
        })
    }

    fn capability_class(&self) -> Option<CapabilityClass> {
        Some(CapabilityClass::GitWrite)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        if ctx.permission_mode != PermissionMode::Bypass && !ctx.durable_write_approved {
            return ToolResult::error(
                "git_lfs_fetch requires approval bound to this exact tool call.".to_string(),
            );
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
                "Git LFS fetch requires approval: {} (policy: {})",
                verdict.reason, verdict.policy_source
            ));
        }

        let repo = match crate::coding_tools::git_read::open_workspace_repository(
            args["repo_path"].as_str(),
            ctx,
        ) {
            Ok(repo) => repo,
            Err(error) => return ToolResult::error(error),
        };
        // Resolve everything that needs `git2` synchronously and drop the
        // repository before awaiting: `git2::Repository` is not `Sync`, so it
        // cannot be held across the network calls that follow.
        let plan = match resolve_lfs_fetch_plan(&repo, &args) {
            Ok(plan) => plan,
            Err(error) => return ToolResult::error(error),
        };
        drop(repo);
        match lfs_fetch(plan, ctx.github_token.as_deref(), ctx.cancel_rx.clone()).await {
            Ok(effect) => ToolResult::ok(
                json!({
                    "operation": "lfs_fetch",
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

fn requested_paths(args: &Value) -> Result<Vec<String>, String> {
    let raw = args["paths"]
        .as_array()
        .ok_or_else(|| "git_lfs_fetch requires a non-empty 'paths' array.".to_string())?;
    if raw.is_empty() {
        return Err(
            "git_lfs_fetch requires at least one path; whole-repo LFS fetch is refused."
                .to_string(),
        );
    }
    raw.iter()
        .map(|value| {
            let text = value
                .as_str()
                .ok_or_else(|| "Every entry in 'paths' must be a string.".to_string())?
                .trim();
            if text.is_empty()
                || text.contains(['*', '?', '['])
                || Path::new(text)
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(format!("Invalid exact LFS pointer path '{text}'."));
            }
            Ok(text.to_string())
        })
        .collect()
}

/// Everything the bounding contract needs to decide, computed synchronously
/// through `git2` while `repo` is in scope. Plain, owned, non-`git2` data
/// only — this is what crosses into the async transfer below, so that no
/// `git2` type (none of which are `Send`) is ever live across an `.await`.
struct LfsFetchPlan {
    remote_name: String,
    remote_url: String,
    /// `<repo .git dir>/lfs/objects`, matching the standard `git-lfs`
    /// client's on-disk object cache location.
    objects_root: PathBuf,
    /// (pointer path, oid, declared size), in the caller's requested order.
    selected: Vec<(String, String, u64)>,
    total_declared_bytes: u64,
    max_bytes: u64,
}

fn resolve_lfs_fetch_plan(repo: &Repository, args: &Value) -> Result<LfsFetchPlan, String> {
    let requested = requested_paths(args)?;
    let max_bytes = args["max_bytes"]
        .as_u64()
        .ok_or_else(|| "git_lfs_fetch requires integer 'max_bytes'.".to_string())?;

    let all_pointers = scan_pointers(repo)?;
    let mut selected = Vec::new();
    for path in &requested {
        let pointer = all_pointers
            .iter()
            .find(|candidate| &candidate.path == path)
            .ok_or_else(|| format!("'{path}' is not a tracked LFS pointer file."))?;
        selected.push(pointer);
    }

    let total_declared_bytes: u64 = selected.iter().map(|pointer| pointer.size).sum();
    if total_declared_bytes > max_bytes {
        return Err(format!(
            "Refusing LFS fetch: declared size {total_declared_bytes} bytes exceeds max_bytes {max_bytes}. Name fewer paths or raise the ceiling explicitly."
        ));
    }

    let remote_name = args["remote"].as_str().unwrap_or("origin").to_string();
    let remote = repo
        .find_remote(&remote_name)
        .map_err(|error| format!("Unknown remote '{remote_name}': {error}"))?;
    let remote_url = remote
        .url()
        .map_err(|error| format!("Read remote '{remote_name}' URL: {error}"))?
        .to_string();

    // Standard `git-lfs` on-disk cache location, alongside the ordinary
    // object store under the repository's `.git` directory (or the
    // top-level directory itself for a bare repo — `repo.path()` already
    // resolves correctly for both).
    let objects_root = repo.path().join("lfs").join("objects");

    Ok(LfsFetchPlan {
        remote_name,
        remote_url,
        objects_root,
        selected: selected
            .into_iter()
            .map(|pointer| (pointer.path.clone(), pointer.oid.clone(), pointer.size))
            .collect(),
        total_declared_bytes,
        max_bytes,
    })
}

async fn lfs_fetch(
    plan: LfsFetchPlan,
    github_token: Option<&str>,
    cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> Result<Value, String> {
    // The caller resolved `plan` synchronously and dropped the `git2`
    // repository before calling in, so nothing non-`Send` crosses an await.

    let batch_objects: Vec<crate::coding_tools::git_lfs_batch_client::BatchObjectRequest> = plan
        .selected
        .iter()
        .map(
            |(_, oid, size)| crate::coding_tools::git_lfs_batch_client::BatchObjectRequest {
                oid: oid.clone(),
                size: *size,
            },
        )
        .collect();

    let fetched = crate::coding_tools::git_lfs_batch_client::fetch_objects(
        &plan.remote_url,
        &batch_objects,
        &plan.objects_root,
        github_token,
        cancel_rx,
    )
    .await?;

    let fetched_by_oid: std::collections::HashMap<
        &str,
        &crate::coding_tools::git_lfs_batch_client::FetchedObject,
    > = fetched
        .iter()
        .map(|object| (object.oid.as_str(), object))
        .collect();

    let all_fetched = !plan.selected.is_empty()
        && plan
            .selected
            .iter()
            .all(|(_, oid, _)| fetched_by_oid.contains_key(oid.as_str()));

    let objects_json = plan
        .selected
        .iter()
        .map(|(path, oid, size)| {
            let local_path = fetched_by_oid
                .get(oid.as_str())
                .map(|object| object.path.display().to_string());
            json!({
                "path": path,
                "oid": oid,
                "size": size,
                "localPath": local_path
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "remote": plan.remote_name,
        "remoteUrl": crate::coding_tools::output_compaction::mask_secrets(&plan.remote_url),
        "requestedPaths": plan.selected.iter().map(|(path, _, _)| path.clone()).collect::<Vec<_>>(),
        "objects": objects_json,
        "totalDeclaredBytes": plan.total_declared_bytes,
        "maxBytes": plan.max_bytes,
        "fetched": all_fetched
    }))
}
