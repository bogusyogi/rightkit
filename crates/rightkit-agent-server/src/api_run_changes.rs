//! I1 — Run-scoped workspace change authoritativeness.
//!
//! The desktop "review" surface (Agent 3 / C1 / I1) needs one durable
//! object that captures every mutation a run produces, classifies each
//! entry, persists the inventory across restart, and lets the UI drive
//! stage / unstage / revert / comment / structured-revision actions.
//! Tool-call text alone ("```diff\\n...```") is never trusted — those
//! payloads can be doctored, dropped, or repeated. Mutations land here
//! through a per-tool `record_*` path that observes the actual filesystem
//! and git state at call boundaries.
//!
//! This module is a standalone authoring seam:
//!
//! - `run_changes_routes()` exposes the typed daemon routes.
//! - `run_changes_openapi_*` helpers register component schemas the
//!   integration lead wires into `api_contract/document.rs`.
//! - `RunChangeStore` is the persistence surface; the integration lead
//!   attaches it to `DaemonState` (this file does not touch `api.rs`).
//!
//! Design rules (locked 2026-07-20):
//!
//! - Every entry is keyed by a stable **mutation id** minted by the
//!   author (tool layer) so two captures of the same logical edit yield
//!   the same row. The store upserts on the mutation id.
//! - Git workspaces carry the canonical inventory. The store records a
//!   bounded Git state (HEAD + index fingerprint) and reconciles after
//!   each mutation. A non-Git workspace uses a deterministic bounded
//!   hash model keyed on `(path, mtime, size)`.
//! - Classifications are explicit enums; secret/large/binary/conflict
//!   rows stay in the inventory so the UI can render the right
//!   affordances without diffing the body.
//! - Restart survives: state lives in `~/.{app}/run_changes/<id>.bin`
//!   and is loaded lazily by `RunChangeStore::open`.
//! - Stale anchors track the `(mutation id, line)` reference; when the
//!   captured snapshot changes beyond a tolerance the anchor flips to
//!   `Stale` and the UI is told to surface it.
//! - The inventory is the source of truth for the "Review" / "Undo"
//!   actions — Undo replays the captured inverse op against the live
//!   workspace and rejects if the fingerprint no longer matches.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use rightkit_agent_engine_protocol::now_iso;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

use crate::api::DaemonState;

// ── Bounds ────────────────────────────────────────────────────────────────────

/// Hard cap on entries per run. Stops a runaway loop from spilling the
/// durable store; the UI gets a `truncated` flag and a stable count.
pub const MAX_ENTRIES_PER_RUN: usize = 4_096;

/// Hard cap on the diff body retained per entry. Matches the diff tool's
/// 100 KiB cap so the on-disk store stays bounded.
pub const MAX_DIFF_BYTES: usize = 100 * 1024;

/// Hard cap on file contents retained for Undo. Anything larger switches
/// the entry to `ReplayOnly` (the UI can still revert via git ops).
pub const MAX_UNDO_BYTES: u64 = 512 * 1024;

// ── Classification ────────────────────────────────────────────────────────────

/// One classified outcome for an entry. Stays explicit so the UI can
/// render badges without re-deriving intent from booleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClassification {
    /// File existed before and is staged or unstaged in git.
    Tracked,
    /// File appeared in the workspace during the run; not tracked yet.
    Untracked,
    /// Tracked file was renamed. `original_path` carries the prior name.
    Renamed,
    /// Tracked file disappeared during the run.
    Deleted,
    /// Binary payload (null-byte hit in the first 4 KiB).
    Binary,
    /// Generated artifact (matching a generator marker heuristic).
    Generated,
    /// Secret/key match — read/write surfaces are masked.
    Secret,
    /// Larger than `MAX_UNDO_BYTES`; only the diff body is retained.
    TooLarge,
    /// Merge conflict marker or partial-failure residue.
    Conflict,
    /// Synthesized successfully; no follow-up action needed.
    Complete,
    /// Mutation stopped partway through (tool error, timeout, cancel).
    Partial,
}

/// Whether the mutation finished cleanly. This is deliberately separate from
/// [`ChangeClassification`]: the UI still needs to know whether a successful
/// mutation created, modified, deleted, or renamed a file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOutcome {
    #[default]
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStage {
    /// Mutated; not yet staged.
    Unstaged,
    /// Staged in the index; revert goes through `git restore --staged`.
    Staged,
    /// Reverted at last user action; inert.
    Reverted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorStatus {
    /// Line content still matches the captured snapshot.
    Live,
    /// Captured file content has drifted; UI must surface the stale anchor.
    Stale,
    /// Snapshot file no longer exists.
    Vanished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentKind {
    Inline,
    Revision,
    Suggestion,
    Question,
    Resolution,
}

// ── Core row ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeEntry {
    pub mutation_id: String,
    pub workspace_root: String,
    pub path: String,
    pub original_path: Option<String>,
    pub classification: ChangeClassification,
    #[serde(default)]
    pub outcome: ChangeOutcome,
    pub stage: ChangeStage,
    pub additions: i64,
    pub deletions: i64,
    pub diff: String,
    pub truncated: bool,
    pub anchor_status: AnchorStatus,
    pub anchor_offset: u32,
    pub captured_at: String,
    pub fingerprint: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub undo: Option<UndoBody>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoBody {
    /// SHA-256 of the captured bytes; checked against the live file before
    /// the inverse write fires.
    pub fingerprint: String,
    /// Either the captured pre-image (textual) or a marker for git paths.
    pub body: String,
    /// Free-text note for the UI ("initial state at run start").
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionComment {
    pub comment_id: String,
    pub mutation_id: String,
    pub path: String,
    pub anchor_offset: u32,
    pub anchor_status: AnchorStatus,
    pub kind: CommentKind,
    pub body: String,
    pub author: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunChangesSummary {
    pub run_id: String,
    pub workspace_root: String,
    pub entry_count: usize,
    pub truncated: bool,
    pub counts_by_classification: BTreeMap<String, usize>,
    pub first_paths: Vec<String>,
    pub last_capture_at: String,
    pub git: GitBaselineSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct GitBaselineSummary {
    pub present: bool,
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub index_fingerprint: Option<String>,
    pub untracked_count: usize,
}

/// Full inventory dump for the ReviewPane. Same data the card renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunChangesInventory {
    pub run_id: String,
    pub workspace_root: String,
    pub entries: Vec<ChangeEntry>,
    pub truncated: bool,
    pub git: GitBaselineSummary,
    pub last_capture_at: String,
}

// ── Capture / apply primitives ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureSnapshotRequest {
    pub run_id: String,
    pub workspace_root: String,
    pub entries: Vec<ChangeEntry>,
    /// Caller-asserted git inventory hash; the store verifies it matches
    /// the freshly computed baseline and rejects on drift.
    pub expected_index_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureSnapshotResponse {
    pub run_id: String,
    pub stored: usize,
    pub truncated: bool,
    pub git: GitBaselineSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryRequest {
    pub run_id: String,
    pub workspace_root: String,
    /// Optional query for a single path; returns just that entry.
    pub path: Option<String>,
    /// Soft cap on the body returned (defaults to `MAX_DIFF_BYTES`).
    pub max_diff_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoRequest {
    pub run_id: String,
    pub workspace_root: String,
    pub mutation_id: String,
    /// Fingerprint the client observed in the inventory; matches the
    /// live file before the inverse write fires.
    pub observed_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoResponse {
    pub mutation_id: String,
    pub path: String,
    pub applied: bool,
    pub new_fingerprint: String,
    pub conflict_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionUpdateRequest {
    pub run_id: String,
    pub workspace_root: String,
    pub mutation_id: String,
    pub path: String,
    pub anchor_offset: u32,
    pub kind: CommentKind,
    pub body: String,
    pub author: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionUpdateResponse {
    pub comment_id: String,
    pub anchor_status: AnchorStatus,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffBodyRequest {
    pub run_id: String,
    pub workspace_root: String,
    pub path: String,
    pub split: bool,
    pub cursor: Option<String>,
    pub window: Option<u32>,
    pub search: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffBodyResponse {
    pub path: String,
    pub rows: Vec<DiffRow>,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub hunk_decisions: Vec<HunkDecision>,
    pub search: Option<DiffSearchStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffRow {
    pub kind: String,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkDecision {
    pub hunk_id: String,
    pub file_path: String,
    pub hunk_index: u32,
    pub anchor_offset: u32,
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSearchStats {
    pub matches: usize,
    pub cursor: Option<String>,
    pub term: String,
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub fn run_changes_routes() -> Router<DaemonState> {
    Router::new()
        .route("/run-changes/capture", post(post_capture_handler))
        .route("/run-changes/inventory", get(get_inventory_handler))
        .route("/run-changes/summary", get(get_summary_handler))
        .route("/run-changes/undo", post(post_undo_handler))
        .route("/run-changes/revise", post(post_revise_handler))
        .route("/run-changes/diff-body", post(post_diff_body_handler))
        // Reset is foreground-only: the integration lead gates / exposes it
        // through a privileged route after auth checks.
        .route("/run-changes/reset", post(post_reset_handler))
}

// ── OpenAPI helpers ───────────────────────────────────────────────────────────

pub(crate) fn run_changes_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "ChangeEntry".to_string(),
        json!({
            "type": "object",
            "required": ["mutation_id", "workspace_root", "path", "original_path", "classification", "outcome", "stage", "additions", "deletions", "diff", "truncated", "anchor_status", "anchor_offset", "captured_at", "fingerprint", "size_bytes", "modified_at", "undo", "last_error"],
            "properties": {
                "mutation_id": {"type": "string"},
                "workspace_root": {"type": "string"},
                "path": {"type": "string"},
                "original_path": {"type": ["string", "null"]},
                "classification": {"$ref": "#/components/schemas/ChangeClassification"},
                "outcome": {"$ref": "#/components/schemas/ChangeOutcome"},
                "stage": {"$ref": "#/components/schemas/ChangeStage"},
                "additions": {"type": "integer"},
                "deletions": {"type": "integer"},
                "diff": {"type": "string"},
                "truncated": {"type": "boolean"},
                "anchor_status": {"$ref": "#/components/schemas/AnchorStatus"},
                "anchor_offset": {"type": "integer"},
                "captured_at": {"type": "string"},
                "fingerprint": {"type": "string"},
                "size_bytes": {"type": "integer", "format": "int64"},
                "modified_at": {"type": ["string", "null"]},
                "undo": {"type": ["object", "null"], "required": ["fingerprint", "body", "note"], "properties": {"fingerprint": {"type": "string"}, "body": {"type": "string"}, "note": {"type": "string"}}},
                "last_error": {"type": ["string", "null"]}
            }
        }),
    );
    schemas.insert(
        "ChangeClassification".to_string(),
        json!({
            "type": "string",
            "enum": [
                "tracked", "untracked", "renamed", "deleted",
                "binary", "generated", "secret", "too_large",
                "conflict", "complete", "partial"
            ]
        }),
    );
    schemas.insert(
        "ChangeStage".to_string(),
        json!({"type": "string", "enum": ["unstaged", "staged", "reverted"]}),
    );
    schemas.insert(
        "ChangeOutcome".to_string(),
        json!({"type": "string", "enum": ["complete", "partial"]}),
    );
    schemas.insert(
        "AnchorStatus".to_string(),
        json!({"type": "string", "enum": ["live", "stale", "vanished"]}),
    );
    schemas.insert(
        "CommentKind".to_string(),
        json!({"type": "string", "enum": ["inline", "revision", "suggestion", "question", "resolution"]}),
    );
    schemas.insert(
        "CaptureSnapshotRequest".to_string(),
        json!({
            "type": "object",
            "required": ["run_id", "workspace_root", "entries"],
            "properties": {
                "run_id": {"type": "string"},
                "workspace_root": {"type": "string"},
                "entries": {"type": "array", "items": {"$ref": "#/components/schemas/ChangeEntry"}},
                "expected_index_fingerprint": {"type": ["string", "null"]}
            }
        }),
    );
    schemas.insert(
        "CaptureSnapshotResponse".to_string(),
        json!({
            "type": "object",
            "required": ["run_id", "stored", "truncated", "git"],
            "properties": {
                "run_id": {"type": "string"},
                "stored": {"type": "integer"},
                "truncated": {"type": "boolean"},
                "git": {"$ref": "#/components/schemas/GitBaselineSummary"}
            }
        }),
    );
    schemas.insert(
        "InventoryRequest".to_string(),
        json!({
            "type": "object",
            "required": ["run_id", "workspace_root"],
            "properties": {
                "run_id": {"type": "string"},
                "workspace_root": {"type": "string"},
                "path": {"type": ["string", "null"]},
                "max_diff_bytes": {"type": ["integer", "null"]}
            }
        }),
    );
    schemas.insert(
        "RunChangesInventory".to_string(),
        json!({
            "type": "object",
            "required": ["run_id", "workspace_root", "entries", "truncated", "git", "last_capture_at"],
            "properties": {
                "run_id": {"type": "string"},
                "workspace_root": {"type": "string"},
                "entries": {"type": "array", "items": {"$ref": "#/components/schemas/ChangeEntry"}},
                "truncated": {"type": "boolean"},
                "git": {"$ref": "#/components/schemas/GitBaselineSummary"},
                "last_capture_at": {"type": "string"}
            }
        }),
    );
    schemas.insert(
        "RunChangesSummary".to_string(),
        json!({
            "type": "object",
            "required": ["run_id", "workspace_root", "entry_count", "truncated", "counts_by_classification", "first_paths", "last_capture_at"],
            "properties": {
                "run_id": {"type": "string"},
                "workspace_root": {"type": "string"},
                "entry_count": {"type": "integer"},
                "truncated": {"type": "boolean"},
                "counts_by_classification": {"type": "object", "additionalProperties": {"type": "integer"}},
                "first_paths": {"type": "array", "items": {"type": "string"}},
                "last_capture_at": {"type": "string"},
                "git": {"$ref": "#/components/schemas/GitBaselineSummary"}
            }
        }),
    );
    schemas.insert(
        "GitBaselineSummary".to_string(),
        json!({
            "type": "object",
            "required": ["present", "head_sha", "branch", "index_fingerprint", "untracked_count"],
            "properties": {
                "present": {"type": "boolean"},
                "head_sha": {"type": ["string", "null"]},
                "branch": {"type": ["string", "null"]},
                "index_fingerprint": {"type": ["string", "null"]},
                "untracked_count": {"type": "integer"}
            }
        }),
    );
    schemas.insert(
        "UndoRequest".to_string(),
        json!({
            "type": "object",
            "required": ["run_id", "workspace_root", "mutation_id", "observed_fingerprint"],
            "properties": {
                "run_id": {"type": "string"},
                "workspace_root": {"type": "string"},
                "mutation_id": {"type": "string"},
                "observed_fingerprint": {"type": "string"}
            }
        }),
    );
    schemas.insert(
        "UndoResponse".to_string(),
        json!({
            "type": "object",
            "required": ["mutation_id", "path", "applied", "new_fingerprint", "conflict_reason"],
            "properties": {
                "mutation_id": {"type": "string"},
                "path": {"type": "string"},
                "applied": {"type": "boolean"},
                "new_fingerprint": {"type": "string"},
                "conflict_reason": {"type": ["string", "null"]}
            }
        }),
    );
    schemas.insert(
        "RevisionUpdateRequest".to_string(),
        json!({
            "type": "object",
            "required": ["run_id", "workspace_root", "mutation_id", "path", "anchor_offset", "kind", "body", "author"],
            "properties": {
                "run_id": {"type": "string"},
                "workspace_root": {"type": "string"},
                "mutation_id": {"type": "string"},
                "path": {"type": "string"},
                "anchor_offset": {"type": "integer"},
                "kind": {"$ref": "#/components/schemas/CommentKind"},
                "body": {"type": "string"},
                "author": {"type": "string"}
            }
        }),
    );
    schemas.insert(
        "RevisionUpdateResponse".to_string(),
        json!({
            "type": "object",
            "required": ["comment_id", "anchor_status", "stale"],
            "properties": {
                "comment_id": {"type": "string"},
                "anchor_status": {"$ref": "#/components/schemas/AnchorStatus"},
                "stale": {"type": "boolean"}
            }
        }),
    );
    schemas.insert(
        "DiffBodyRequest".to_string(),
        json!({
            "type": "object",
            "required": ["run_id", "workspace_root", "path", "split"],
            "properties": {
                "run_id": {"type": "string"},
                "workspace_root": {"type": "string"},
                "path": {"type": "string"},
                "split": {"type": "boolean"},
                "cursor": {"type": ["string", "null"]},
                "window": {"type": ["integer", "null"]},
                "search": {"type": ["string", "null"]}
            }
        }),
    );
    schemas.insert(
        "DiffBodyResponse".to_string(),
        json!({
            "type": "object",
            "required": ["path", "rows", "truncated", "next_cursor", "hunk_decisions", "search"],
            "properties": {
                "path": {"type": "string"},
                "rows": {"type": "array", "items": {"$ref": "#/components/schemas/DiffRow"}},
                "truncated": {"type": "boolean"},
                "next_cursor": {"type": ["string", "null"]},
                "hunk_decisions": {"type": "array", "items": {"$ref": "#/components/schemas/HunkDecision"}},
                "search": {"type": ["object", "null"], "required": ["matches", "cursor", "term"], "properties": {"matches": {"type": "integer"}, "cursor": {"type": ["string", "null"]}, "term": {"type": "string"}}}
            }
        }),
    );
    schemas.insert(
        "DiffRow".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "text", "old_line", "new_line"],
            "properties": {
                "kind": {"type": "string", "enum": ["file", "hunk", "add", "del", "ctx"]},
                "text": {"type": "string"},
                "old_line": {"type": ["integer", "null"]},
                "new_line": {"type": ["integer", "null"]}
            }
        }),
    );
    schemas.insert(
        "HunkDecision".to_string(),
        json!({
            "type": "object",
            "required": ["hunk_id", "file_path", "hunk_index", "anchor_offset", "decision"],
            "properties": {
                "hunk_id": {"type": "string"},
                "file_path": {"type": "string"},
                "hunk_index": {"type": "integer"},
                "anchor_offset": {"type": "integer"},
                "decision": {"type": "string", "enum": ["approve", "reject", "pending"]}
            }
        }),
    );
    schemas
}

pub(crate) fn run_changes_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/run-changes/capture") => {
            Some(json!({"$ref": "#/components/schemas/CaptureSnapshotRequest"}))
        }
        ("get", "/run-changes/inventory") | ("get", "/run-changes/summary") => None,
        ("post", "/run-changes/undo") => Some(json!({"$ref": "#/components/schemas/UndoRequest"})),
        ("post", "/run-changes/revise") => {
            Some(json!({"$ref": "#/components/schemas/RevisionUpdateRequest"}))
        }
        ("post", "/run-changes/diff-body") => {
            Some(json!({"$ref": "#/components/schemas/DiffBodyRequest"}))
        }
        ("post", "/run-changes/reset") => {
            Some(json!({"$ref": "#/components/schemas/InventoryRequest"}))
        }
        _ => None,
    }
}

pub(crate) fn run_changes_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/run-changes/capture") => {
            Some(json!({"$ref": "#/components/schemas/CaptureSnapshotResponse"}))
        }
        ("get", "/run-changes/inventory") => {
            Some(json!({"$ref": "#/components/schemas/RunChangesInventory"}))
        }
        ("get", "/run-changes/summary") => {
            Some(json!({"$ref": "#/components/schemas/RunChangesSummary"}))
        }
        ("post", "/run-changes/undo") => Some(json!({"$ref": "#/components/schemas/UndoResponse"})),
        ("post", "/run-changes/revise") => {
            Some(json!({"$ref": "#/components/schemas/RevisionUpdateResponse"}))
        }
        ("post", "/run-changes/diff-body") => {
            Some(json!({"$ref": "#/components/schemas/DiffBodyResponse"}))
        }
        ("post", "/run-changes/reset") => Some(json!({"type": "object"})),
        _ => None,
    }
}

// ── Durable store ─────────────────────────────────────────────────────────────

/// Persistent inventory keyed by `(run_id, mutation_id)`. The integration
/// lead attaches this to `DaemonState::run_changes`; I1 only owns the
/// store itself so it can be unit-tested in isolation.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RunChangeRecord {
    pub entries: BTreeMap<String, ChangeEntry>,
    pub truncated: bool,
    pub git_baseline: GitBaselineSummary,
    pub last_capture_at: String,
    pub comments: Vec<RevisionComment>,
}

pub struct RunChangeStore {
    root: PathBuf,
    inner: RwLock<BTreeMap<String, RunChangeRecord>>,
    file_service: RwLock<Option<Arc<rightkit_agent_tools::coding_tools::FileService>>>,
}

impl RunChangeStore {
    /// Bind exact engine-owned authority. First binding wins for process-wide
    /// routes; later engines adopt this same authority before execution.
    pub(crate) fn bind_file_service(
        &self,
        candidate: Arc<rightkit_agent_tools::coding_tools::FileService>,
    ) -> Arc<rightkit_agent_tools::coding_tools::FileService> {
        let mut bound = self.file_service.write().unwrap_or_else(|e| e.into_inner());
        bound.get_or_insert(candidate).clone()
    }
}

impl RunChangeStore {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Arc<Self>> {
        let root = root.into();
        rightkit_fs::ensure_private_dir(&root).map_err(std::io::Error::other)?;
        let mut loaded: BTreeMap<String, RunChangeRecord> = BTreeMap::new();
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("bin") {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            match bincode_deserialize::<RunChangeRecord>(&bytes) {
                Ok(record) => {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        loaded.insert(stem.to_string(), record);
                    }
                }
                Err(_) => {
                    // A corrupt file is treated as missing — restart must
                    // continue. The next capture rebuilds the record.
                    continue;
                }
            }
        }
        Ok(Arc::new(Self {
            root,
            inner: RwLock::new(loaded),
            file_service: RwLock::new(None),
        }))
    }

    #[allow(dead_code)]
    pub fn record(&self, run_id: &str, capture: CaptureSnapshotRequest) -> CaptureSnapshotResponse {
        self.record_checked(run_id, capture)
            .expect("run-change capture must be valid and durable")
    }

    pub fn record_checked(
        &self,
        run_id: &str,
        capture: CaptureSnapshotRequest,
    ) -> Result<CaptureSnapshotResponse, String> {
        self.record_inner(run_id, capture, false)
    }

    /// Capture the immutable pre-effect baseline for a run. This is separate
    /// from post-effect entry recording so the first mutation cannot taint the
    /// baseline it is later compared against.
    pub fn begin_run(
        &self,
        run_id: &str,
        workspace_root: &str,
    ) -> std::io::Result<GitBaselineSummary> {
        validate_storage_id(run_id).map_err(std::io::Error::other)?;
        let canonical_root =
            canonical_workspace_root(workspace_root).map_err(std::io::Error::other)?;
        let workspace_root = canonical_root.to_string_lossy();
        let baseline = git_baseline(&workspace_root);
        let mut records = self.inner.write().expect("run_changes lock poisoned");
        let mut next = records.get(run_id).cloned().unwrap_or_default();
        if next.last_capture_at.is_empty() {
            next.git_baseline = baseline.clone();
            next.last_capture_at = now_iso();
        }
        let captured = next.git_baseline.clone();
        persist(&self.root, run_id, &next)?;
        records.insert(run_id.to_string(), next);
        Ok(captured)
    }

    fn record_after_begin(
        &self,
        run_id: &str,
        capture: CaptureSnapshotRequest,
    ) -> Result<CaptureSnapshotResponse, String> {
        self.record_inner(run_id, capture, true)
    }

    fn record_inner(
        &self,
        run_id: &str,
        mut capture: CaptureSnapshotRequest,
        preserve_pre_effect_baseline: bool,
    ) -> Result<CaptureSnapshotResponse, String> {
        validate_storage_id(run_id)?;
        if capture.run_id.is_empty() {
            capture.run_id = run_id.to_string();
        }
        if capture.run_id != run_id {
            return Err("capture run id does not match route/store key".to_string());
        }
        let canonical_root = canonical_workspace_root(&capture.workspace_root)?;
        for incoming in &capture.entries {
            validate_capture_entry(&canonical_root, incoming)?;
        }
        let workspace_root = canonical_root.to_string_lossy().into_owned();
        let baseline = git_baseline(&workspace_root);
        if let Some(expected) = capture.expected_index_fingerprint.as_ref() {
            if baseline
                .index_fingerprint
                .as_ref()
                .is_none_or(|actual| expected != actual)
            {
                return Ok(CaptureSnapshotResponse {
                    run_id: run_id.to_string(),
                    stored: 0,
                    truncated: false,
                    git: baseline,
                });
            }
        }

        let mut record = self.inner.write().expect("run_changes lock poisoned");
        let mut entry = record.get(run_id).cloned().unwrap_or_default();
        let mut truncated = entry.truncated;
        let mut stored = 0;
        for incoming in capture.entries {
            if entry.entries.len() >= MAX_ENTRIES_PER_RUN {
                truncated = true;
                break;
            }
            let classified = ensure_classification(&workspace_root, &incoming);
            entry
                .entries
                .insert(incoming.mutation_id.clone(), classified);
            stored += 1;
        }
        entry.truncated = truncated;
        if !preserve_pre_effect_baseline || entry.last_capture_at.is_empty() {
            entry.git_baseline = baseline.clone();
        }
        entry.last_capture_at = now_iso();
        let captured_baseline = entry.git_baseline.clone();
        persist(&self.root, run_id, &entry).map_err(|error| error.to_string())?;
        record.insert(run_id.to_string(), entry);
        Ok(CaptureSnapshotResponse {
            run_id: run_id.to_string(),
            stored,
            truncated,
            git: captured_baseline,
        })
    }

    pub fn inventory(
        &self,
        run_id: &str,
        request: InventoryRequest,
    ) -> Option<RunChangesInventory> {
        let record = self.inner.read().expect("run_changes lock poisoned");
        let (resolved_run_id, rec) = record.get_key_value(run_id).or_else(|| {
            let prefix = format!("{run_id}--");
            record
                .iter()
                .filter(|(key, _)| key.starts_with(&prefix))
                .max_by_key(|(_, value)| (&value.last_capture_at, value.entries.len()))
        })?;
        let entries: Vec<ChangeEntry> = match request.path.as_ref() {
            Some(p) => rec
                .entries
                .values()
                .filter(|e| &e.path == p)
                .cloned()
                .collect(),
            None => rec.entries.values().cloned().collect(),
        };
        let max = request.max_diff_bytes.unwrap_or(MAX_DIFF_BYTES);
        let clipped: Vec<ChangeEntry> = if request.path.is_some() {
            entries
                .into_iter()
                .map(|mut e| {
                    if e.diff.len() > max {
                        e.diff.truncate(max);
                        e.truncated = true;
                    }
                    e
                })
                .collect()
        } else {
            entries
        };
        Some(RunChangesInventory {
            run_id: resolved_run_id.clone(),
            workspace_root: request.workspace_root,
            entries: clipped,
            truncated: rec.truncated,
            git: rec.git_baseline.clone(),
            last_capture_at: rec.last_capture_at.clone(),
        })
    }

    pub fn summary(&self, run_id: &str, workspace_root: &str) -> Option<RunChangesSummary> {
        let record = self.inner.read().expect("run_changes lock poisoned");
        let (resolved_run_id, rec) = record.get_key_value(run_id).or_else(|| {
            let prefix = format!("{run_id}--");
            record
                .iter()
                .filter(|(key, _)| key.starts_with(&prefix))
                .max_by_key(|(_, value)| (&value.last_capture_at, value.entries.len()))
        })?;
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for entry in rec.entries.values() {
            let key = format!("{:?}", entry.classification).to_snake_case();
            *counts.entry(key).or_default() += 1;
        }
        let mut sorted_paths: Vec<String> = rec.entries.values().map(|e| e.path.clone()).collect();
        sorted_paths.sort();
        let first_paths = sorted_paths.into_iter().take(8).collect();
        Some(RunChangesSummary {
            run_id: resolved_run_id.clone(),
            workspace_root: workspace_root.to_string(),
            entry_count: rec.entries.len(),
            truncated: rec.truncated,
            counts_by_classification: counts,
            first_paths,
            last_capture_at: rec.last_capture_at.clone(),
            git: rec.git_baseline.clone(),
        })
    }

    /// Not-yet-reverted entries captured by `session_id`'s runs at or after
    /// `since` (an ISO timestamp), newest first, as `(run_id, entry)`. This is
    /// the rewind-to-message input: each entry is then undone through
    /// [`Self::undo`] so EFF-008 fingerprint guards still refuse to overwrite
    /// newer bytes.
    pub fn session_entries_since(
        &self,
        session_id: &str,
        since: &str,
    ) -> Vec<(String, ChangeEntry)> {
        let prefix = format!("{session_id}--");
        let record = self.inner.read().expect("run_changes lock poisoned");
        let mut out: Vec<(String, ChangeEntry)> = record
            .iter()
            .filter(|(run_id, _)| run_id.starts_with(&prefix))
            .flat_map(|(run_id, rec)| {
                rec.entries
                    .values()
                    .filter(|entry| {
                        entry.stage != ChangeStage::Reverted && entry.captured_at.as_str() >= since
                    })
                    .map(move |entry| (run_id.clone(), entry.clone()))
            })
            .collect();
        out.sort_by(|a, b| b.1.captured_at.cmp(&a.1.captured_at));
        out
    }

    pub fn undo(&self, _run_id: &str, request: UndoRequest) -> UndoResponse {
        let record = self.inner.read().expect("run_changes lock poisoned");
        let entry = match record
            .get(&request.run_id)
            .and_then(|r| r.entries.get(&request.mutation_id))
        {
            Some(e) => e.clone(),
            None => {
                return UndoResponse {
                    mutation_id: request.mutation_id,
                    path: String::new(),
                    applied: false,
                    new_fingerprint: String::new(),
                    conflict_reason: Some("unknown mutation id".to_string()),
                };
            }
        };
        drop(record);

        let stored_root = match canonical_workspace_root(&entry.workspace_root) {
            Ok(root) => root,
            Err(reason) => return undo_conflict(&entry, reason),
        };
        let requested_root = match canonical_workspace_root(&request.workspace_root) {
            Ok(root) => root,
            Err(reason) => return undo_conflict(&entry, reason),
        };
        if requested_root != stored_root {
            return undo_conflict(
                &entry,
                "undo workspace does not match the captured workspace".to_string(),
            );
        }
        let live_path = match resolve_relative_within(&stored_root, &entry.path) {
            Ok(path) => path,
            Err(reason) => return undo_conflict(&entry, reason),
        };
        let live_fingerprint = fingerprint_path(&live_path);
        match live_fingerprint {
            Some(actual) if actual != request.observed_fingerprint => {
                return UndoResponse {
                    mutation_id: entry.mutation_id.clone(),
                    path: entry.path.clone(),
                    applied: false,
                    new_fingerprint: String::new(),
                    conflict_reason: Some(format!(
                        "fingerprint mismatch: client observed {} but live file is {}",
                        short_hex(&request.observed_fingerprint),
                        short_hex(&actual),
                    )),
                };
            }
            _ => {}
        }
        let undo = match entry.undo.as_ref() {
            Some(u) => u,
            None => {
                return UndoResponse {
                    mutation_id: entry.mutation_id.clone(),
                    path: entry.path.clone(),
                    applied: false,
                    new_fingerprint: String::new(),
                    conflict_reason: Some("entry has no undo body".to_string()),
                };
            }
        };

        let files = self
            .file_service
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let outcome = match files {
            Some(files) => apply_undo_with_service(files.as_ref(), &stored_root, &entry, undo),
            None => Err("undo mutation authority is not bound to host engine".to_string()),
        };
        if let Err(reason) = outcome {
            return UndoResponse {
                mutation_id: entry.mutation_id.clone(),
                path: entry.path.clone(),
                applied: false,
                new_fingerprint: String::new(),
                conflict_reason: Some(reason),
            };
        }
        let new_fingerprint = fingerprint_path(&live_path).unwrap_or_default();
        let mut record = self.inner.write().expect("run_changes lock poisoned");
        if let Some(rec) = record.get_mut(&request.run_id) {
            if let Some(e) = rec.entries.get_mut(&entry.mutation_id) {
                e.stage = ChangeStage::Reverted;
            }
            let snapshot = rec.clone();
            drop(record);
            if let Err(error) = persist(&self.root, &request.run_id, &snapshot) {
                tracing::error!(run_id = %request.run_id, %error, "persisting reverted run-change state failed");
            }
        }
        UndoResponse {
            mutation_id: entry.mutation_id.clone(),
            path: entry.path.clone(),
            applied: true,
            new_fingerprint,
            conflict_reason: None,
        }
    }

    pub fn revise(&self, run_id: &str, request: RevisionUpdateRequest) -> RevisionUpdateResponse {
        let mut record = self.inner.write().expect("run_changes lock poisoned");
        let rec = record.entry(run_id.to_string()).or_default();
        let expected_fingerprint = rec
            .entries
            .get(&request.mutation_id)
            .filter(|entry| entry.path == request.path)
            .map(|entry| entry.fingerprint.as_str())
            .filter(|fingerprint| !fingerprint.is_empty());
        let anchor = refresh_anchor(
            &request.workspace_root,
            &request.path,
            request.anchor_offset,
            expected_fingerprint,
        );
        let comment_id = format!("cmt-{}", short_hex(&request.body));
        let created_at = now_iso();
        rec.comments.push(RevisionComment {
            comment_id: comment_id.clone(),
            mutation_id: request.mutation_id.clone(),
            path: request.path.clone(),
            anchor_offset: request.anchor_offset,
            anchor_status: anchor,
            kind: request.kind,
            body: request.body,
            author: request.author,
            created_at,
            resolved_at: None,
        });
        if let Some(entry) = rec.entries.get_mut(&request.mutation_id) {
            if entry.path == request.path {
                entry.anchor_status = anchor;
            }
        }
        let snapshot = rec.clone();
        drop(record);
        persist(&self.root, run_id, &snapshot).ok();
        RevisionUpdateResponse {
            comment_id,
            anchor_status: anchor,
            stale: anchor != AnchorStatus::Live,
        }
    }

    pub fn diff_body(&self, request: DiffBodyRequest) -> DiffBodyResponse {
        let record = self.inner.read().expect("run_changes lock poisoned");
        let entry = record
            .get(&request.run_id)
            .and_then(|r| r.entries.values().find(|e| e.path == request.path))
            .cloned();
        drop(record);
        let entry = match entry {
            Some(e) => e,
            None => {
                return DiffBodyResponse {
                    path: request.path,
                    rows: Vec::new(),
                    truncated: false,
                    next_cursor: None,
                    hunk_decisions: Vec::new(),
                    search: None,
                }
            }
        };
        let rows = split_diff_rows(&entry.diff, request.split);
        let total = rows.len();
        let window = request.window.unwrap_or(256) as usize;
        let cursor = request.cursor.as_deref().unwrap_or("");
        let start: usize = cursor.parse().unwrap_or(0);
        let end = (start + window).min(total);
        let slice = rows[start..end].to_vec();
        let next_cursor = if end < total {
            Some(end.to_string())
        } else {
            None
        };
        let stats = request.search.as_ref().map(|term| {
            let matches = slice
                .iter()
                .filter(|r| r.text.to_lowercase().contains(&term.to_lowercase()))
                .count();
            DiffSearchStats {
                matches,
                cursor: next_cursor.clone(),
                term: term.clone(),
            }
        });
        DiffBodyResponse {
            path: entry.path,
            rows: slice,
            truncated: entry.truncated,
            next_cursor,
            hunk_decisions: derive_hunk_decisions(&entry.diff, request.split),
            search: stats,
        }
    }

    pub fn reset(&self, run_id: &str, workspace_root: &str) -> bool {
        let mut record = self.inner.write().expect("run_changes lock poisoned");
        record.remove(run_id);
        let path = self.root.join(format!("{run_id}.bin"));
        let removed = match std::fs::remove_file(&path) {
            Ok(()) => true,
            Err(_) => false,
        };
        let _ = workspace_root; // workspace root only matters if I2 stores multiple
        removed
    }
}

/// Adapter installed on daemon-owned engines. It turns post-effect typed
/// mutations into durable, filesystem-observed review entries.
#[derive(Clone)]
pub(crate) struct DaemonRunChangesSink {
    store: Arc<RunChangeStore>,
}

impl DaemonRunChangesSink {
    pub(crate) fn shared() -> Self {
        Self {
            store: shared_run_changes_store(),
        }
    }
}

impl rightkit_agent_engine_core::RunChangesSink for DaemonRunChangesSink {
    fn begin_run(
        &self,
        _session_id: &str,
        run_id: &str,
        workspace_root: &Path,
    ) -> Result<(), String> {
        let root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        self.store
            .begin_run(run_id, &root.to_string_lossy())
            .map(|_| ())
            .map_err(|error| format!("capture run baseline: {error}"))
    }

    fn record_mutations(
        &self,
        _session_id: &str,
        run_id: &str,
        workspace_root: &Path,
        tool_call_id: &str,
        mutations: &[rightkit_agent_tools::coding_tools::WorkspaceMutation],
        tool_failed: bool,
    ) -> Result<(), String> {
        let root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());
        let workspace = root.to_string_lossy().to_string();
        let mut entries = Vec::with_capacity(mutations.len());
        for (index, mutation) in mutations.iter().enumerate() {
            let path = relative_workspace_path(&root, &mutation.path)?;
            let original_path = match &mutation.kind {
                rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Renamed { from } => {
                    Some(relative_workspace_path(&root, from)?)
                }
                _ => None,
            };
            let observed_classification = match mutation.kind {
                rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Created => {
                    if git_path_is_tracked(&workspace, &path) {
                        ChangeClassification::Tracked
                    } else {
                        ChangeClassification::Untracked
                    }
                }
                rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Modified => {
                    ChangeClassification::Tracked
                }
                rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Deleted => {
                    ChangeClassification::Deleted
                }
                rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Renamed { .. } => {
                    ChangeClassification::Renamed
                }
            };
            let classification = observed_classification;
            let outcome = if tool_failed {
                ChangeOutcome::Partial
            } else {
                ChangeOutcome::Complete
            };
            let diff = observed_diff(&workspace, &path, observed_classification);
            let (additions, deletions) = diff_counts(&diff);
            let exists = mutation.path.exists();
            let undo = match &mutation.kind {
                rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Created => {
                    Some(UndoBody {
                        fingerprint: fingerprint_path(&mutation.path).unwrap_or_default(),
                        body: "__RIGHTKIT_DELETE__".to_string(),
                        note: "remove file created by this tool call".to_string(),
                    })
                }
                rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Modified
                | rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Deleted => mutation
                    .previous_content
                    .as_ref()
                    .filter(|content| content.len() <= MAX_UNDO_BYTES as usize)
                    .filter(|content| !looks_like_secret(&mutation.path, content.as_bytes()))
                    .map(|content| UndoBody {
                        fingerprint: fingerprint_path(&mutation.path).unwrap_or_default(),
                        body: content.clone(),
                        note: "restore the exact pre-effect file content".to_string(),
                    }),
                rightkit_agent_tools::coding_tools::WorkspaceMutationKind::Renamed { .. } => {
                    Some(UndoBody {
                        fingerprint: fingerprint_path(&mutation.path).unwrap_or_default(),
                        body: "__RIGHTKIT_RENAME__".to_string(),
                        note: "move the renamed file back to its original path".to_string(),
                    })
                }
            };
            entries.push(ChangeEntry {
                mutation_id: format!("{run_id}:{tool_call_id}:{index}"),
                workspace_root: workspace.clone(),
                path,
                original_path,
                classification,
                outcome,
                stage: ChangeStage::Unstaged,
                additions,
                deletions,
                diff,
                truncated: false,
                anchor_status: if exists {
                    AnchorStatus::Live
                } else {
                    AnchorStatus::Vanished
                },
                anchor_offset: 0,
                captured_at: now_iso(),
                fingerprint: fingerprint_path(&mutation.path).unwrap_or_default(),
                size_bytes: std::fs::metadata(&mutation.path)
                    .map(|m| m.len())
                    .unwrap_or(0),
                modified_at: None,
                undo,
                last_error: tool_failed.then(|| "tool completed partially".to_string()),
            });
        }
        self.store.record_after_begin(
            run_id,
            CaptureSnapshotRequest {
                run_id: run_id.to_string(),
                workspace_root: workspace,
                entries,
                expected_index_fingerprint: None,
            },
        )?;
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn relative_workspace_path(root: &Path, path: &Path) -> Result<String, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    // Daemon roots are canonicalized, while tool paths can retain a platform
    // alias (notably macOS `/var` versus `/private/var`). Canonicalize an
    // existing mutation before containment validation so a real in-workspace
    // change is not falsely rejected. Deleted paths cannot be canonicalized;
    // those keep their original form and must still pass the strict prefix
    // check below.
    let comparable = absolute.canonicalize().unwrap_or_else(|_| absolute.clone());
    let relative = comparable
        .strip_prefix(root)
        .map_err(|_| format!("mutation path escapes workspace: {}", absolute.display()))?;
    let value = relative.to_string_lossy().replace('\\', "/");
    if value.is_empty() || value.starts_with("../") {
        return Err(format!(
            "invalid workspace mutation path: {}",
            absolute.display()
        ));
    }
    Ok(value)
}

fn git_path_is_tracked(workspace_root: &str, path: &str) -> bool {
    managed_git_output(workspace_root, &["ls-files", "--error-unmatch", "--", path])
        .is_some_and(|output| output.success)
}

fn observed_diff(workspace_root: &str, path: &str, classification: ChangeClassification) -> String {
    let output = match classification {
        ChangeClassification::Untracked => {
            // Containment was validated before this call. Keep Git operands
            // relative so Windows extended canonical paths never reach Git.
            let null_path = if cfg!(windows) { "NUL" } else { "/dev/null" };
            managed_git_output(
                workspace_root,
                &["diff", "--no-index", "--no-ext-diff", "--", null_path, path],
            )
        }
        _ => managed_git_output(workspace_root, &["diff", "--no-ext-diff", "--", path]),
    };
    output.map(|out| out.stdout).unwrap_or_default()
}

fn diff_counts(diff: &str) -> (i64, i64) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }
    (additions, deletions)
}

fn validate_storage_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 200
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("run id contains unsafe filename characters".to_string());
    }
    Ok(())
}

fn canonical_workspace_root(raw: &str) -> Result<PathBuf, String> {
    let raw_path = Path::new(raw);
    if !raw_path.is_absolute() {
        return Err("workspace root must be absolute".to_string());
    }
    let canonical = raw_path
        .canonicalize()
        .map_err(|error| format!("workspace root is unavailable: {error}"))?;
    if !canonical.is_dir() {
        return Err("workspace root is not a directory".to_string());
    }
    Ok(canonical)
}

fn resolve_relative_within(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let relative = Path::new(raw);
    if raw.is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(format!("unsafe workspace-relative path: {raw}"));
    }
    let candidate = root.join(relative);
    let mut ancestor = candidate.as_path();
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| format!("path has no existing workspace ancestor: {raw}"))?;
    }
    let canonical_ancestor = ancestor
        .canonicalize()
        .map_err(|error| format!("could not validate workspace path '{raw}': {error}"))?;
    if !canonical_ancestor.starts_with(root) {
        return Err(format!("workspace path escapes through a symlink: {raw}"));
    }
    Ok(candidate)
}

fn validate_capture_entry(root: &Path, entry: &ChangeEntry) -> Result<(), String> {
    let entry_root = canonical_workspace_root(&entry.workspace_root)?;
    if entry_root != root {
        return Err("entry workspace does not match capture workspace".to_string());
    }
    resolve_relative_within(root, &entry.path)?;
    if let Some(original) = entry.original_path.as_deref() {
        resolve_relative_within(root, original)?;
    }
    if entry.mutation_id.is_empty() || entry.mutation_id.len() > 500 {
        return Err("mutation id is empty or too long".to_string());
    }
    Ok(())
}

fn undo_conflict(entry: &ChangeEntry, reason: String) -> UndoResponse {
    UndoResponse {
        mutation_id: entry.mutation_id.clone(),
        path: entry.path.clone(),
        applied: false,
        new_fingerprint: String::new(),
        conflict_reason: Some(reason),
    }
}

fn persist(root: &Path, run_id: &str, record: &RunChangeRecord) -> std::io::Result<()> {
    validate_storage_id(run_id).map_err(std::io::Error::other)?;
    let bytes = bincode_serialize(record).map_err(std::io::Error::other)?;
    let path = root.join(format!("{run_id}.bin"));
    rightkit_fs::write_private_file(&path, &bytes).map_err(std::io::Error::other)?;
    Ok(())
}

fn bincode_serialize(record: &RunChangeRecord) -> Result<Vec<u8>, serde_json::Error> {
    // Stable framing is not a wire-format contract here — the file is
    // private to the daemon and is the source of truth for the live
    // process. Using JSON keeps the file inspectable during debugging.
    serde_json::to_vec(record)
}

fn bincode_deserialize<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(bytes)
}

fn fingerprint_path(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_UNDO_BYTES {
        return Some(format!("size:{}", metadata.len()));
    }
    let bytes = std::fs::read(path).ok()?;
    Some(rightkit_fs::digest::sha256_bytes(&bytes))
}

fn short_hex(value: &str) -> String {
    value.chars().take(8).collect()
}

fn ensure_classification(workspace_root: &str, incoming: &ChangeEntry) -> ChangeEntry {
    let mut entry = incoming.clone();
    let path = Path::new(workspace_root).join(&entry.path);
    if entry.classification == ChangeClassification::Renamed && entry.original_path.is_none() {
        entry.classification = ChangeClassification::Tracked;
    }
    if path.exists() {
        if entry.fingerprint.is_empty() {
            entry.fingerprint = fingerprint_path(&path).unwrap_or_default();
        }
        if let Ok(metadata) = std::fs::metadata(&path) {
            entry.size_bytes = metadata.len();
        }
        let bytes = std::fs::read(&path).ok();
        if entry.classification != ChangeClassification::Partial {
            if let Some(ref b) = bytes {
                if looks_like_secret(&path, b) {
                    entry.classification = ChangeClassification::Secret;
                    entry.undo = None;
                } else if looks_binary(b) {
                    entry.classification = ChangeClassification::Binary;
                    entry.diff = String::new();
                } else if metadata_large(&path) {
                    entry.classification = ChangeClassification::TooLarge;
                } else if looks_generated(&path) {
                    entry.classification = ChangeClassification::Generated;
                }
            }
        }
    } else if entry.classification == ChangeClassification::Tracked {
        entry.classification = ChangeClassification::Deleted;
        entry.anchor_status = AnchorStatus::Vanished;
    }
    entry
}

fn looks_binary(bytes: &[u8]) -> bool {
    let sniff = &bytes[..bytes.len().min(4096)];
    sniff.contains(&0)
}

fn looks_like_secret(path: &Path, bytes: &[u8]) -> bool {
    let leaf = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    const SECRET_LEAVES: &[&str] = &[
        ".env",
        ".env.local",
        ".envrc",
        "id_rsa",
        "id_ed25519",
        "secrets.json",
        "secrets.yaml",
        "secrets.yml",
        "secrets.toml",
        ".netrc",
    ];
    if SECRET_LEAVES.iter().any(|s| leaf == *s) || leaf.starts_with(".env") {
        return true;
    }
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "pem" | "key" | "p12" | "pfx" | "crt") {
        return true;
    }
    let sniff = &bytes[..bytes.len().min(512)];
    sniff
        .windows(20)
        .any(|w| w.starts_with(b"-----BEGIN ") || w.starts_with(b"AKIA") || w.starts_with(b"ghp_"))
}

fn metadata_large(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() > MAX_UNDO_BYTES)
        .unwrap_or(false)
}

fn looks_generated(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains("/node_modules/")
        || s.contains("/target/")
        || s.contains("/.git/")
        || s.ends_with(".min.js")
        || s.ends_with(".lock")
}

fn git_baseline(workspace_root: &str) -> GitBaselineSummary {
    let root = Path::new(workspace_root);
    if !root.join(".git").exists() && !root.join("../.git").exists() {
        return GitBaselineSummary::default();
    }
    // One launch for both values: `--abbrev-ref` applies to the arguments
    // that follow it, so the output is the full sha then the branch name.
    let (head_sha, branch) = match run_git(
        workspace_root,
        &["rev-parse", "HEAD", "--abbrev-ref", "HEAD"],
    ) {
        Some(out) => {
            let mut lines = out.lines();
            (
                lines.next().map(|l| l.trim().to_string()),
                lines.next().map(|l| l.trim().to_string()),
            )
        }
        None => (None, None),
    };
    let untracked_count = run_git(workspace_root, &["status", "--porcelain"])
        .map(|s| s.lines().filter(|line| line.starts_with("??")).count())
        .unwrap_or(0);
    let index_fingerprint = run_git(workspace_root, &["write-tree"]);
    GitBaselineSummary {
        present: head_sha.is_some(),
        head_sha: head_sha.filter(|s| !s.is_empty()),
        branch: branch.filter(|s| !s.is_empty()),
        index_fingerprint: index_fingerprint.filter(|s| !s.is_empty()),
        untracked_count,
    }
}

fn run_git(workspace_root: &str, args: &[&str]) -> Option<String> {
    let output = managed_git_output(workspace_root, args)?;
    if !output.success {
        return None;
    }
    Some(output.stdout.trim().to_string())
}

fn managed_git_output(
    workspace_root: &str,
    args: &[&str],
) -> Option<crate::api_environments::ManagedProcessOutput> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return tokio::task::block_in_place(|| {
                handle.block_on(crate::api_environments::run_managed_process_with_timeout(
                    "git",
                    args,
                    workspace_root,
                    std::time::Duration::from_secs(5),
                ))
            })
            .ok();
        }
        // Synchronous mutation capture also runs under current-thread hosts.
        // Drive process IO on a separate runtime instead of nesting block_on
        // or calling block_in_place, which panics on that host. Joining keeps
        // the managed child fully waited before capture returns.
        return std::thread::scope(|scope| {
            scope
                .spawn(|| managed_git_output(workspace_root, args))
                .join()
                .ok()
                .flatten()
        });
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?
        .block_on(crate::api_environments::run_managed_process_with_timeout(
            "git",
            args,
            workspace_root,
            std::time::Duration::from_secs(5),
        ))
        .ok()
}

/// EFF-003/EFF-007/EFF-008: undo/revert is an ordinary workspace mutation.
///
/// This previously performed raw `std::fs::remove_file`/`rename`/`write`,
/// which bypassed three separate authorities at once: FileService's ordinary
/// mutation lock/revision/receipt authority (EFF-003), the exclusive workspace
/// restore lease (EFF-007), and any expected-revision declaration (EFF-008).
/// It could therefore interleave with a concurrent FileService mutation or a
/// checkpoint restore, and could silently clobber newer user bytes.
///
/// Every reversal now runs through a typed `MutationIntent` under the
/// exclusive restore lease, and declares the revision it expects.
fn apply_undo_with_service(
    files: &rightkit_agent_tools::coding_tools::FileService,
    workspace_root: &Path,
    entry: &ChangeEntry,
    undo: &UndoBody,
) -> Result<(), String> {
    use rightkit_agent_tools::coding_tools::file_service::{ExpectedPreimage, MutationIntent};

    let path = resolve_relative_within(workspace_root, &entry.path)?;

    // EFF-008: the server's own record of what the agent left behind is the
    // authority for "has anything landed since?". The caller-supplied
    // `observed_fingerprint` checked earlier reflects only what the CLIENT
    // last saw, so it cannot by itself protect newer bytes.
    let live_fingerprint = fingerprint_path(&path);
    if let Some(actual) = live_fingerprint.as_deref() {
        if !entry.fingerprint.is_empty() && actual != entry.fingerprint {
            return Err(format!(
                "refusing undo: '{}' changed after the agent wrote it \
                 (recorded {}, live {}); newer changes must not be overwritten",
                entry.path,
                short_hex(&entry.fingerprint),
                short_hex(actual),
            ));
        }
    } else if !entry.fingerprint.is_empty() && undo.body != "__RIGHTKIT_DELETE__" {
        return Err(format!(
            "refusing undo: '{}' no longer exists after the agent wrote it; \
             newer changes must not be overwritten",
            entry.path,
        ));
    }

    // EFF-007 note: undo is an ORDINARY single-path mutation, not a bulk
    // restore, so it must NOT take the exclusive restore lease. Every typed
    // intent below already acquires the shared workspace lease plus the
    // per-path lock, which is precisely what prevents it from interleaving
    // with a checkpoint restore holding the exclusive lease. Nesting an
    // ordinary mutation inside `with_exclusive_workspace_restore` would
    // self-deadlock: that helper holds the workspace gate for WRITE while
    // every FileService mutation takes it for READ, and the lock is not
    // reentrant.
    let result = (|| -> Result<(), String> {
        if undo.body == "__RIGHTKIT_DELETE__" {
            // Reverse of a create: remove the file the agent added.
            return match files.observe_preimage(&path) {
                // Already absent: the reversal is a no-op, not a failure.
                Ok(ExpectedPreimage::Missing) => Ok(()),
                Ok(ExpectedPreimage::Revision(expected)) => files
                    .delete_with_intent(MutationIntent::delete(&path, expected), false)
                    .map(|_| ())
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
        }

        if undo.body == "__RIGHTKIT_RENAME__" {
            let original = entry
                .original_path
                .as_deref()
                .ok_or_else(|| "rename undo is missing the original path".to_string())?;
            let destination = resolve_relative_within(workspace_root, original)?;
            let expected_source = match files.observe_preimage(&path) {
                Ok(ExpectedPreimage::Revision(revision)) => revision,
                Ok(ExpectedPreimage::Missing) => {
                    return Err(format!(
                        "refusing rename undo: '{}' no longer exists",
                        path.display()
                    ))
                }
                Err(error) => return Err(error.to_string()),
            };
            // The destination must still be free; declaring `Missing`
            // makes FileService reject a racing create rather than
            // overwrite it.
            let expected_destination = match files.observe_preimage(&destination) {
                Ok(ExpectedPreimage::Missing) => ExpectedPreimage::Missing,
                Ok(ExpectedPreimage::Revision(_)) => {
                    return Err(format!(
                        "refusing rename undo because '{}' already exists",
                        destination.display()
                    ))
                }
                Err(error) => return Err(error.to_string()),
            };
            return files
                .move_with_intent(MutationIntent::move_path(
                    &path,
                    &destination,
                    expected_source,
                    expected_destination,
                ))
                .map(|_| ())
                .map_err(|error| error.to_string());
        }

        // Reverse of a modify or a delete: restore the captured pre-image.
        let intent = match files.observe_preimage(&path) {
            Ok(ExpectedPreimage::Revision(expected)) => MutationIntent::restore(&path, expected),
            // The agent deleted it; recreating is a create, and Create
            // refuses to overwrite anything that appeared meanwhile.
            Ok(ExpectedPreimage::Missing) => MutationIntent::create(&path),
            Err(error) => return Err(error.to_string()),
        };
        files
            .apply_byte_intent(intent, undo.body.as_bytes())
            .map(|_| ())
            .map_err(|error| error.to_string())
    })();
    result
}

fn refresh_anchor(
    workspace_root: &str,
    path: &str,
    captured_offset: u32,
    expected_fingerprint: Option<&str>,
) -> AnchorStatus {
    let live = Path::new(workspace_root).join(path);
    if !live.exists() {
        return AnchorStatus::Vanished;
    }
    if expected_fingerprint
        .is_some_and(|expected| fingerprint_path(&live).as_deref() != Some(expected))
    {
        return AnchorStatus::Stale;
    }
    let bytes = match std::fs::read(&live) {
        Ok(b) => b,
        Err(_) => return AnchorStatus::Stale,
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return AnchorStatus::Stale,
    };
    let lines: Vec<&str> = text.lines().collect();
    let idx = captured_offset as usize;
    if idx >= lines.len() {
        return AnchorStatus::Stale;
    }
    AnchorStatus::Live
}

fn split_diff_rows(diff: &str, split: bool) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut old_n: u32 = 0;
    let mut new_n: u32 = 0;
    for line in diff.lines() {
        if line.starts_with("diff --git") {
            rows.push(DiffRow {
                kind: "file".into(),
                text: line.to_string(),
                old_line: None,
                new_line: None,
            });
            old_n = 0;
            new_n = 0;
            continue;
        }
        if let Some(caps) = regex_hunk_captures(line) {
            old_n = caps.0;
            new_n = caps.1;
            rows.push(DiffRow {
                kind: "hunk".into(),
                text: line.to_string(),
                old_line: Some(old_n),
                new_line: Some(new_n),
            });
            continue;
        }
        if split {
            match line.chars().next() {
                Some('+') => {
                    rows.push(DiffRow {
                        kind: "add".into(),
                        text: line[1..].to_string(),
                        old_line: None,
                        new_line: Some(new_n),
                    });
                    new_n += 1;
                }
                Some('-') => {
                    rows.push(DiffRow {
                        kind: "del".into(),
                        text: line[1..].to_string(),
                        old_line: Some(old_n),
                        new_line: None,
                    });
                    old_n += 1;
                }
                Some(' ') => {
                    rows.push(DiffRow {
                        kind: "ctx".into(),
                        text: line[1..].to_string(),
                        old_line: Some(old_n),
                        new_line: Some(new_n),
                    });
                    old_n += 1;
                    new_n += 1;
                }
                _ => {
                    rows.push(DiffRow {
                        kind: "ctx".into(),
                        text: line.to_string(),
                        old_line: None,
                        new_line: None,
                    });
                }
            }
        } else if let Some(c) = line.chars().next() {
            let kind = match c {
                '+' => "add",
                '-' => "del",
                ' ' => "ctx",
                _ => "ctx",
            };
            let (old_l, new_l) = match kind {
                "add" => (None, Some(new_n)),
                "del" => (Some(old_n), None),
                "ctx" => (Some(old_n), Some(new_n)),
                _ => (None, None),
            };
            rows.push(DiffRow {
                kind: kind.into(),
                text: line[1..].to_string(),
                old_line: old_l,
                new_line: new_l,
            });
            match kind {
                "add" => new_n += 1,
                "del" => old_n += 1,
                "ctx" => {
                    old_n += 1;
                    new_n += 1;
                }
                _ => {}
            }
        }
    }
    rows
}

fn regex_hunk_captures(line: &str) -> Option<(u32, u32)> {
    // Manual @@ parser — avoids pulling regex into this crate.
    if !line.starts_with("@@") {
        return None;
    }
    let body = line.trim_start_matches('@').trim_end_matches('@');
    let mut iter = body.split_whitespace();
    let first = iter.next()?;
    let second = iter.next()?;
    let old_n = first
        .trim_start_matches('-')
        .split(',')
        .next()?
        .parse()
        .ok()?;
    let new_n = second
        .trim_start_matches('+')
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old_n, new_n))
}

fn derive_hunk_decisions(diff: &str, split: bool) -> Vec<HunkDecision> {
    let mut decisions = Vec::new();
    let mut current_path: Option<String> = None;
    let mut hunk_index: u32 = 0;
    for line in diff.lines() {
        if line.starts_with("diff --git") {
            current_path = line.split_whitespace().nth(2).map(|s| {
                s.trim_start_matches("a/")
                    .trim_start_matches("b/")
                    .to_string()
            });
            continue;
        }
        if line.starts_with("@@") {
            if let Some(path) = current_path.clone() {
                let anchor_offset = regex_hunk_captures(line).map(|c| c.1).unwrap_or(0);
                decisions.push(HunkDecision {
                    hunk_id: format!("{path}#{hunk_index}"),
                    file_path: path,
                    hunk_index,
                    anchor_offset,
                    decision: "pending".to_string(),
                });
            }
            hunk_index += 1;
            let _ = split;
        }
    }
    decisions
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn post_capture_handler(
    State(state): State<DaemonState>,
    Json(body): Json<CaptureSnapshotRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    let run_id = body.run_id.clone();
    // Capture shells out to git (sandboxed launches) and does synchronous
    // file IO; keep it off the async runtime workers.
    let result = tokio::task::spawn_blocking(move || store.record_checked(&run_id, body))
        .await
        .unwrap_or_else(|error| Err(format!("capture task failed: {error}")));
    match result {
        Ok(response) => (StatusCode::OK, Json(json!(response))),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_run_change_capture", "message": error})),
        ),
    }
}

async fn get_inventory_handler(
    State(state): State<DaemonState>,
    Query(query): Query<InventoryRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    let run_id = query.run_id.clone();
    match store.inventory(&run_id, query) {
        Some(payload) => (StatusCode::OK, Json(json!(payload))),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "run_not_found"})),
        ),
    }
}

async fn get_summary_handler(
    State(state): State<DaemonState>,
    Query(query): Query<InventoryRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    match store.summary(&query.run_id, &query.workspace_root) {
        Some(payload) => (StatusCode::OK, Json(json!(payload))),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "run_not_found"})),
        ),
    }
}

async fn post_undo_handler(
    State(state): State<DaemonState>,
    Json(body): Json<UndoRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    let run_id = body.run_id.clone();
    let response = store.undo(&run_id, body);
    (StatusCode::OK, Json(response))
}

async fn post_revise_handler(
    State(state): State<DaemonState>,
    Json(body): Json<RevisionUpdateRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    let run_id = body.run_id.clone();
    let response = store.revise(&run_id, body);
    (StatusCode::OK, Json(response))
}

async fn post_diff_body_handler(
    State(state): State<DaemonState>,
    Json(body): Json<DiffBodyRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    let response = store.diff_body(body);
    (StatusCode::OK, Json(response))
}

async fn post_reset_handler(
    State(state): State<DaemonState>,
    Json(body): Json<InventoryRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    let removed = store.reset(&body.run_id, &body.workspace_root);
    (StatusCode::OK, Json(json!({"removed": removed})))
}

/// Process-wide slot for the I1 store. Mirrors the `OnceLock` precedent
/// in `api_workspace.rs::search_registry`. The integration lead (I2)
/// attaches a dedicated `Arc<RunChangeStore>` to `DaemonState`; until
/// then this slot hosts one default store rooted at
/// `~/.{app}/run_changes`.
pub(crate) fn shared_run_changes_store() -> Arc<RunChangeStore> {
    static STORE: OnceLock<Arc<RunChangeStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            RunChangeStore::open(resolve_run_changes_root()).expect("run_changes default dir")
        })
        .clone()
}

fn run_changes_store(_state: &DaemonState) -> Arc<RunChangeStore> {
    shared_run_changes_store()
}

/// Resolve the run_changes root with the same state-root rule as the rest of
/// the app: the AppRoot state root (`RIGHTKIT_SUITE_ROOT` when set).
/// `RIGHTKIT_RUN_CHANGES_DIR` remains an explicit override.
fn resolve_run_changes_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("RIGHTKIT_RUN_CHANGES_DIR") {
        return PathBuf::from(dir);
    }
    dirs_default_home().join("run_changes")
}

fn dirs_default_home() -> PathBuf {
    if rightkit_agent_config::state_root_is_isolated() {
        if let Some(root) = rightkit_agent_config::state_root() {
            return root;
        }
    }
    {
        rightkit_agent_config::state_root()
            .unwrap_or_else(|| PathBuf::from(crate::host_identity::home_dir()))
    }
}

// ── Trait for snake_case Display ──────────────────────────────────────────────

trait SnakeCaseExt {
    fn to_snake_case(&self) -> String;
}

impl SnakeCaseExt for String {
    fn to_snake_case(&self) -> String {
        let mut out = String::with_capacity(self.len());
        let mut prev_upper = false;
        for c in self.chars() {
            if c.is_ascii_uppercase() {
                if !prev_upper && !out.is_empty() {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
                prev_upper = true;
            } else {
                out.push(c);
                prev_upper = false;
            }
        }
        out
    }
}

// ── Tests (executed by the final integration lane) ───────────────────────────
