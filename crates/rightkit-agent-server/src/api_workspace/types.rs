// Workspace module — shared bounds, wire types (DTOs), and policy error.
//
// Extracted from `api_workspace.rs` (ra-019 decomposition). Pure structural
// move: no field, name, wire-format, or bound changed. `policy`, `routes`,
// `search`, and `watcher` all depend on this module; it depends on none of
// them (handler-independent, per the Architect decomposition map).

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

// ── Bounds ────────────────────────────────────────────────────────────────────

/// Hard upper bound on a text read or write. Larger files return a
/// `tooLarge` flag instead of streaming content — the bounded preview is
/// enforced here so the UI can never accidentally ask the daemon to
/// slurp a 500 MB log.
pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;

/// Sniff window for the binary detector.
pub const BINARY_SNIFF_BYTES: usize = 4096;

/// Max children returned in a single list/expand call.
pub const MAX_CHILDREN_PER_LIST: usize = 1024;

/// Max bytes a search is allowed to scan per workspace before cancelling.
pub const SEARCH_BUDGET_BYTES: u64 = 32 * 1024 * 1024;

/// Max results a search returns regardless of matches.
pub const SEARCH_MAX_HITS: usize = 256;

/// Max subscribers a single workspace watcher will fan out to.
pub const WATCH_MAX_SUBSCRIBERS: usize = 16;

/// Poll cadence for the watcher snapshot loop.
pub const WATCH_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(750);

/// How long an idle workspace watcher survives without subscribers.
pub const WATCH_IDLE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

pub(crate) fn epoch_seconds_to_iso(secs: u64) -> String {
    i64::try_from(secs)
        .ok()
        .and_then(|secs| rightkit_time::DateTime::from_unix(secs, 0))
        .map(|dt| dt.format_rfc3339())
        .unwrap_or_default()
}

pub(crate) fn modified_iso(modified: Option<std::time::SystemTime>) -> Option<String> {
    modified
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| epoch_seconds_to_iso(d.as_secs()))
}

pub(crate) fn pretty_path(p: &str) -> String {
    p.trim().replace('\\', "/")
}

// ── Trust-root policy error ────────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) enum PolicyError {
    BadRequest(String),
    Forbidden(String),
    NotFound,
}

impl PolicyError {
    pub fn status(&self) -> StatusCode {
        match self {
            PolicyError::BadRequest(reason) => StatusCode::BAD_REQUEST,
            PolicyError::Forbidden(reason) => StatusCode::FORBIDDEN,
            PolicyError::NotFound => StatusCode::NOT_FOUND,
        }
    }
}

// ── Shared request/response types ─────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    pub name: String,
    pub path: String,
    pub kind: WorkspaceEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known: Option<bool>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceEntryKind {
    Directory,
    File,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTreeResponse {
    pub workspace_root: String,
    pub path: String,
    pub entries: Vec<WorkspaceEntry>,
    pub truncated: bool,
    #[serde(default)]
    pub last_known: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileResponse {
    pub workspace_root: String,
    pub path: String,
    pub kind: WorkspaceFileKind,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub fingerprint: Option<String>,
    pub content: Option<String>,
    pub permission: String,
    pub truncated: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceFileKind {
    File,
    Missing,
    Binary,
    Directory,
    TooLarge,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTreeQuery {
    pub workspace_root: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileQuery {
    pub workspace_root: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileSaveRequest {
    pub workspace_root: String,
    pub path: String,
    pub content: String,
    pub expected_fingerprint: String,
    #[serde(default)]
    pub base_revision: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceFileSaveResponse {
    pub ok: bool,
    pub path: String,
    pub fingerprint: String,
    pub size_bytes: u64,
    pub modified_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSearchRequest {
    pub workspace_root: String,
    pub query: String,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_binary: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSearchHit {
    pub path: String,
    pub name: String,
    pub line: usize,
    pub column: usize,
    pub snippet: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSearchResponse {
    pub workspace_root: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub hits: Vec<WorkspaceSearchHit>,
    pub cancelled: bool,
    pub scanned_bytes: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArtifact {
    pub id: String,
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArtifactsResponse {
    pub workspace_root: String,
    pub artifacts: Vec<WorkspaceArtifact>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRecentResponse {
    pub workspace_root: String,
    pub recents: Vec<WorkspaceRecentEntry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRecentEntry {
    pub path: String,
    pub name: String,
    pub opened_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceWatchEvent {
    pub kind: String,
    pub path: String,
    pub at: String,
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}
