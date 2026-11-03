// Workspace module — route registration plus the tree/file/artifact/recent
// handlers.
//
// Extracted from `api_workspace.rs` (ra-019 decomposition). Pure structural
// move: route paths, methods, and every handler's status/JSON contract are
// unchanged. Search lives in `search.rs`, the watch SSE route in
// `watcher.rs`; both are merged into the router built here so the exported
// `workspace_routes()` facade is byte-identical to the pre-split router.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
    Json, Router,
};
use rightkit_agent_engine_protocol::now_iso;
use std::path::Path;

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;

use super::policy::{
    derive_trust_root, format_mode, looks_binary, permission_mode,
    resolve_directory_within_trust_root, resolve_within_trust_root,
    resolve_writable_within_trust_root,
};
use super::search::post_workspace_search_handler;
use super::types::{
    modified_iso, pretty_path, PolicyError, WorkspaceArtifact, WorkspaceArtifactsResponse,
    WorkspaceEntry, WorkspaceEntryKind, WorkspaceFileKind, WorkspaceFileQuery,
    WorkspaceFileResponse, WorkspaceFileSaveRequest, WorkspaceFileSaveResponse,
    WorkspaceRecentResponse, WorkspaceTreeQuery, WorkspaceTreeResponse, MAX_CHILDREN_PER_LIST,
    MAX_TEXT_BYTES,
};
use super::watcher::get_workspace_watch_handler;

// ── Routes ────────────────────────────────────────────────────────────────────

pub fn workspace_routes() -> Router<DaemonState> {
    Router::new()
        .route("/workspace/tree", get(get_workspace_tree_handler))
        .route(
            "/workspace/tree/expand",
            post(post_workspace_tree_expand_handler),
        )
        .route("/workspace/file", get(get_workspace_file_handler))
        .route("/workspace/file", put(put_workspace_file_handler))
        .route("/workspace/search", post(post_workspace_search_handler))
        .route("/workspace/artifacts", get(get_workspace_artifacts_handler))
        .route("/workspace/recent", get(get_workspace_recent_handler))
        .route("/workspace/watch", get(get_workspace_watch_handler))
}

// ── Tree listing ──────────────────────────────────────────────────────────────

async fn get_workspace_tree_handler(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<Json<WorkspaceTreeResponse>, StatusCode> {
    let _ = state;
    enforce_trusted_source_authority(&headers)?;
    let trust_root = derive_trust_root(&query.workspace_root).map_err(|e| e.status())?;
    let rel = query.path.unwrap_or_default();
    let (canonical, pretty, _leaf) =
        resolve_directory_within_trust_root(&trust_root, &rel).map_err(|e| e.status())?;
    let metadata = std::fs::metadata(&canonical).map_err(|_| StatusCode::NOT_FOUND)?;
    if !metadata.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let entries = list_children(&canonical, &trust_root).map_err(|e| e.status())?;
    Ok(Json(WorkspaceTreeResponse {
        workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
        path: pretty,
        truncated: entries.len() >= MAX_CHILDREN_PER_LIST,
        entries,
        last_known: false,
    }))
}

async fn post_workspace_tree_expand_handler(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Json(body): Json<WorkspaceTreeQuery>,
) -> Result<Json<WorkspaceTreeResponse>, StatusCode> {
    let _ = state;
    enforce_trusted_source_authority(&headers)?;
    let trust_root = derive_trust_root(&body.workspace_root).map_err(|e| e.status())?;
    let (canonical, pretty, _leaf) =
        resolve_directory_within_trust_root(&trust_root, &body.path.unwrap_or_default())
            .map_err(|e| e.status())?;
    let metadata = std::fs::metadata(&canonical).map_err(|_| StatusCode::NOT_FOUND)?;
    if !metadata.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let entries = list_children(&canonical, &trust_root).map_err(|e| e.status())?;
    Ok(Json(WorkspaceTreeResponse {
        workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
        path: pretty,
        truncated: entries.len() >= MAX_CHILDREN_PER_LIST,
        entries,
        last_known: false,
    }))
}

pub(super) fn is_workspace_tree_noise(name: &str) -> bool {
    matches!(
        name,
        ".DS_Store"
            | ".git"
            | ".pnpm-store"
            | ".fastembed_cache"
            | ".audit"
            | ".tether"
            | "node_modules"
            | "target"
            | "coverage"
    )
}

fn list_children(dir: &Path, trust_root: &Path) -> Result<Vec<WorkspaceEntry>, PolicyError> {
    let read = std::fs::read_dir(dir)
        .map_err(|err| PolicyError::Forbidden(format!("read_dir failed: {err}")))?;
    let mut entries = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for child in read.flatten() {
        if entries.len() >= MAX_CHILDREN_PER_LIST {
            break;
        }
        let name = child.file_name().to_string_lossy().to_string();
        if name.is_empty() || is_workspace_tree_noise(&name) {
            continue;
        }
        let path = child.path();
        let rel = match path.strip_prefix(trust_root) {
            Ok(r) => r.to_string_lossy().replace('\\', "/"),
            Err(_) => continue,
        };
        if !seen.insert(rel.clone()) {
            continue;
        }
        let metadata = match child.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let kind = if metadata.is_dir() {
            WorkspaceEntryKind::Directory
        } else {
            WorkspaceEntryKind::File
        };
        let suffix = if kind == WorkspaceEntryKind::File {
            Path::new(&name)
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| format!(".{e}"))
        } else {
            None
        };
        let modified_at = metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| super::types::epoch_seconds_to_iso(d.as_secs()));
        entries.push(WorkspaceEntry {
            name,
            path: rel,
            kind,
            suffix,
            size_bytes: if kind == WorkspaceEntryKind::File {
                Some(metadata.len())
            } else {
                None
            },
            modified_at,
            git_status: None,
            child_count: if kind == WorkspaceEntryKind::Directory {
                Some(0)
            } else {
                None
            },
            truncated: None,
            last_known: None,
        });
    }
    // Direct-child only — order, dedupe, no recursion. Recursion is the
    // tree panel's job (it expands per-directory on user gesture).
    Ok(entries)
}

// ── File read ─────────────────────────────────────────────────────────────────

async fn get_workspace_file_handler(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Query(query): Query<WorkspaceFileQuery>,
) -> Result<Json<WorkspaceFileResponse>, StatusCode> {
    let _ = state;
    enforce_trusted_source_authority(&headers)?;
    let trust_root = derive_trust_root(&query.workspace_root).map_err(|e| e.status())?;
    let (canonical, pretty, leaf) = match resolve_within_trust_root(&trust_root, &query.path, true)
    {
        Ok(v) => v,
        Err(PolicyError::NotFound) => {
            return Ok(Json(WorkspaceFileResponse {
                workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
                path: pretty_path(&query.path),
                kind: WorkspaceFileKind::Missing,
                size_bytes: 0,
                modified_at: None,
                fingerprint: None,
                content: None,
                permission: "0000".into(),
                truncated: false,
                reason: Some("path does not exist".into()),
            }));
        }
        Err(e) => return Err(e.status()),
    };
    let _ = leaf;
    let metadata = std::fs::metadata(&canonical).map_err(|_| StatusCode::NOT_FOUND)?;
    if metadata.is_dir() {
        return Ok(Json(WorkspaceFileResponse {
            workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
            path: pretty,
            kind: WorkspaceFileKind::Directory,
            size_bytes: 0,
            modified_at: None,
            fingerprint: None,
            content: None,
            permission: format_mode(permission_mode(&metadata)),
            truncated: false,
            reason: Some("path is a directory".into()),
        }));
    }
    let mode = permission_mode(&metadata);
    if mode & 0o400 == 0 {
        return Ok(Json(WorkspaceFileResponse {
            workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
            path: pretty,
            kind: WorkspaceFileKind::Missing,
            size_bytes: metadata.len(),
            modified_at: modified_iso(metadata.modified().ok()),
            fingerprint: None,
            content: None,
            permission: format_mode(mode),
            truncated: false,
            reason: Some("owner has no read permission".into()),
        }));
    }
    let size = metadata.len();
    if size > MAX_TEXT_BYTES {
        return Ok(Json(WorkspaceFileResponse {
            workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
            path: pretty,
            kind: WorkspaceFileKind::TooLarge,
            size_bytes: size,
            modified_at: modified_iso(metadata.modified().ok()),
            fingerprint: None,
            content: None,
            permission: format_mode(mode),
            truncated: false,
            reason: Some(format!("file is {size} bytes; limit is {MAX_TEXT_BYTES}")),
        }));
    }
    let bytes = match std::fs::read(&canonical) {
        Ok(b) => b,
        Err(err) => {
            return Ok(Json(WorkspaceFileResponse {
                workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
                path: pretty,
                kind: WorkspaceFileKind::Missing,
                size_bytes: size,
                modified_at: modified_iso(metadata.modified().ok()),
                fingerprint: None,
                content: None,
                permission: format_mode(mode),
                truncated: false,
                reason: Some(format!("read failed: {err}")),
            }));
        }
    };
    if looks_binary(&bytes) {
        let fp = rightkit_fs::digest::sha256_bytes(&bytes);
        return Ok(Json(WorkspaceFileResponse {
            workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
            path: pretty,
            kind: WorkspaceFileKind::Binary,
            size_bytes: size,
            modified_at: modified_iso(metadata.modified().ok()),
            fingerprint: Some(fp),
            content: None,
            permission: format_mode(mode),
            truncated: false,
            reason: Some("binary file; preview disabled".into()),
        }));
    }
    let fp = rightkit_fs::digest::sha256_bytes(&bytes);
    let content = String::from_utf8_lossy(&bytes).to_string();
    Ok(Json(WorkspaceFileResponse {
        workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
        path: pretty,
        kind: WorkspaceFileKind::File,
        size_bytes: size,
        modified_at: modified_iso(metadata.modified().ok()),
        fingerprint: Some(fp),
        content: Some(content),
        permission: format_mode(mode),
        truncated: false,
        reason: None,
    }))
}

// ── File save (fingerprint-guarded, atomic temp+rename) ──────────────────────

async fn put_workspace_file_handler(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Json(body): Json<WorkspaceFileSaveRequest>,
) -> Result<Json<WorkspaceFileSaveResponse>, StatusCode> {
    let _ = state;
    enforce_trusted_source_authority(&headers)?;
    let trust_root = derive_trust_root(&body.workspace_root).map_err(|e| e.status())?;
    let target =
        resolve_writable_within_trust_root(&trust_root, &body.path).map_err(|e| e.status())?;

    if let Some(base_revision) = body.base_revision.as_deref() {
        if base_revision.len() > 128 || base_revision.chars().any(char::is_control) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    // Enforce write-side size + binary policy BEFORE touching disk.
    if body.content.len() as u64 > MAX_TEXT_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    if looks_binary(body.content.as_bytes()) {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    let metadata = std::fs::metadata(&target).map_err(|_| StatusCode::NOT_FOUND)?;
    if metadata.is_dir() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mode = permission_mode(&metadata);
    if mode & 0o200 == 0 {
        return Err(StatusCode::FORBIDDEN);
    }
    let current = std::fs::read(&target).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let current_fp = rightkit_fs::digest::sha256_bytes(&current);
    if !current_fp.eq_ignore_ascii_case(body.expected_fingerprint.trim()) {
        return Err(StatusCode::CONFLICT);
    }

    // Atomic replace via rightkit-fs (same-directory temp, durable publish;
    // an existing file's mode is carried over on Unix).
    rightkit_fs::atomic_write(&target, body.content.as_bytes()).map_err(|err| {
        match err.kind() {
            Some(std::io::ErrorKind::PermissionDenied) => StatusCode::FORBIDDEN,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    })?;

    let new_metadata = std::fs::metadata(&target).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let new_bytes = std::fs::read(&target).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let new_fp = rightkit_fs::digest::sha256_bytes(&new_bytes);
    let modified_at = modified_iso(new_metadata.modified().ok()).unwrap_or_else(now_iso);
    Ok(Json(WorkspaceFileSaveResponse {
        ok: true,
        path: body.path,
        fingerprint: new_fp,
        size_bytes: new_metadata.len(),
        modified_at,
    }))
}

// ── Artifacts ─────────────────────────────────────────────────────────────────

async fn get_workspace_artifacts_handler(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<Json<WorkspaceArtifactsResponse>, StatusCode> {
    let _ = state;
    enforce_trusted_source_authority(&headers)?;
    let trust_root = derive_trust_root(&query.workspace_root).map_err(|e| e.status())?;
    let mut artifacts = Vec::new();
    let candidates = [
        ("generated-document", "AGENTS.md"),
        ("report", "REPORT.md"),
        ("changelog", "CHANGELOG.md"),
        ("readme", "README.md"),
    ];
    for (kind, leaf) in candidates {
        let candidate = trust_root.join(leaf);
        if !candidate.exists() {
            continue;
        }
        let metadata = match candidate.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let bytes = match std::fs::read(&candidate) {
            Ok(b) => b,
            Err(_) => continue,
        };
        artifacts.push(WorkspaceArtifact {
            id: format!("{kind}:{leaf}"),
            kind: kind.into(),
            path: leaf.into(),
            title: Some(leaf.into()),
            provenance: Some("workspace-root".into()),
            verdict: None,
            fingerprint: rightkit_fs::digest::sha256_bytes(&bytes),
            size_bytes: Some(metadata.len()),
            created_at: modified_iso(metadata.modified().ok()),
        });
    }
    Ok(Json(WorkspaceArtifactsResponse {
        workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
        artifacts,
    }))
}

// ── Recent ────────────────────────────────────────────────────────────────────

async fn get_workspace_recent_handler(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<Json<WorkspaceRecentResponse>, StatusCode> {
    let _ = state;
    enforce_trusted_source_authority(&headers)?;
    let trust_root = derive_trust_root(&query.workspace_root).map_err(|e| e.status())?;
    let recents = Vec::new();
    Ok(Json(WorkspaceRecentResponse {
        workspace_root: trust_root.to_string_lossy().replace('\\', "/"),
        recents,
    }))
}
