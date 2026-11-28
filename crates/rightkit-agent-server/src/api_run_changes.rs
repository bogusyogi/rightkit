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
//! - Restart survives: state lives in `<state root>/run_changes/<id>.bin`
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
use std::collections::{BTreeMap, HashMap};
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
    /// Set on a change made by a sub-agent: it is the parent chat's to keep or discard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Proposal>,
}

/// Where a sub-agent's change stands with the person reviewing the parent chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    /// Waiting for a decision.
    Proposed,
    Kept,
    Discarded,
}

/// A sub-agent's change, offered to the chat that started it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// The top-level chat this change is offered to.
    pub for_session: String,
    /// The sub-agent session that made it, and its role.
    pub by_session: String,
    pub by_role: Option<String>,
    pub state: ProposalState,
    /// `true`: the change is already in the project's files (the sub-agent worked in the
    /// project folder). `false`: it is held in the sub-agent's own worktree and reaches the
    /// project only when kept.
    pub applied: bool,
    /// The sub-agent's worktree, when the change is held there.
    pub source_root: Option<String>,
    pub decided_at: Option<String>,
    /// How many hunks of a held change were kept one by one so far.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub kept_hunks: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
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

/// `POST /run-changes/proposal`: keep or discard a sub-agent's change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalDecisionRequest {
    /// The parent chat's session id (or one of its run ids).
    pub run_id: String,
    pub workspace_root: String,
    pub mutation_id: String,
    /// `keep` or `discard`.
    pub action: String,
    /// The fingerprint the client last saw for the file, as for undo. Needed to discard a
    /// change that is already in the project's files.
    #[serde(default)]
    pub observed_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalDecisionResponse {
    pub mutation_id: String,
    pub path: String,
    /// `proposed`, `kept` or `discarded` after this call.
    pub state: ProposalState,
    /// Whether the change is in the project's files after this call.
    pub applied: bool,
    /// Why nothing was done, in plain words; `null` on success.
    pub conflict_reason: Option<String>,
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
    /// Which entry, when two entries share a path (a sub-agent's proposal beside the chat's
    /// own change). Optional; without it the chat's own entry for the path wins.
    #[serde(default)]
    pub mutation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffBodyResponse {
    pub path: String,
    pub rows: Vec<DiffRow>,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub hunk_decisions: Vec<HunkDecision>,
    /// The file change's hunks, in diff order (the k-th `hunk` row is `hunks[k]`).
    #[serde(default)]
    pub hunks: Vec<DiffHunkInfo>,
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

/// One hunk of a file change, listed with the diff body so a client can keep or undo it alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunkInfo {
    /// `<index>-<hash of the hunk's lines>`; the hash does not move when other hunks go.
    pub hunk_id: String,
    pub index: u32,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub additions: u32,
    pub deletions: u32,
}

/// `POST /run-changes/hunk`: keep or undo one hunk of a change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkDecisionRequest {
    /// The chat's session id (or one of its run ids), as for undo.
    pub run_id: String,
    pub workspace_root: String,
    pub path: String,
    #[serde(default)]
    pub mutation_id: Option<String>,
    pub hunk_id: String,
    /// `undo` or `keep`.
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HunkDecisionResponse {
    pub mutation_id: String,
    pub path: String,
    pub hunk_id: String,
    pub action: String,
    /// Whether this call changed the project's file.
    pub applied: bool,
    /// Hunks of this change still listed after the call.
    pub remaining_hunks: usize,
    /// For a sub-agent's change: where it stands now.
    pub state: Option<ProposalState>,
    pub new_fingerprint: String,
    /// Why nothing was done, in plain words; `null` on success.
    pub conflict_reason: Option<String>,
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub fn run_changes_routes() -> Router<DaemonState> {
    Router::new()
        .route("/run-changes/capture", post(post_capture_handler))
        .route("/run-changes/inventory", get(get_inventory_handler))
        .route("/run-changes/summary", get(get_summary_handler))
        .route("/run-changes/undo", post(post_undo_handler))
        .route("/run-changes/proposal", post(post_proposal_handler))
        .route("/run-changes/hunk", post(post_hunk_handler))
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
                "last_error": {"type": ["string", "null"]},
                "proposal": {"type": ["object", "null"], "required": ["for_session", "by_session", "by_role", "state", "applied", "source_root", "decided_at"], "properties": {"for_session": {"type": "string"}, "by_session": {"type": "string"}, "by_role": {"type": ["string", "null"]}, "state": {"type": "string", "enum": ["proposed", "kept", "discarded"]}, "applied": {"type": "boolean"}, "source_root": {"type": ["string", "null"]}, "decided_at": {"type": ["string", "null"]}}}
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
            "required": ["path", "rows", "truncated", "next_cursor", "hunk_decisions", "hunks", "search"],
            "properties": {
                "path": {"type": "string"},
                "rows": {"type": "array", "items": {"$ref": "#/components/schemas/DiffRow"}},
                "truncated": {"type": "boolean"},
                "next_cursor": {"type": ["string", "null"]},
                "hunk_decisions": {"type": "array", "items": {"$ref": "#/components/schemas/HunkDecision"}},
                "hunks": {"type": "array", "items": {"$ref": "#/components/schemas/DiffHunkInfo"}},
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
        "DiffHunkInfo".to_string(),
        json!({
            "type": "object",
            "required": ["hunk_id", "index", "old_start", "old_lines", "new_start", "new_lines", "additions", "deletions"],
            "properties": {
                "hunk_id": {"type": "string"},
                "index": {"type": "integer"},
                "old_start": {"type": "integer"},
                "old_lines": {"type": "integer"},
                "new_start": {"type": "integer"},
                "new_lines": {"type": "integer"},
                "additions": {"type": "integer"},
                "deletions": {"type": "integer"}
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
    file_service: RwLock<Option<Arc<rightkit_agent_coding_tools::FileService>>>,
}

impl RunChangeStore {
    /// Bind exact engine-owned authority. First binding wins for process-wide
    /// routes; later engines adopt this same authority before execution.
    pub(crate) fn bind_file_service(
        &self,
        candidate: Arc<rightkit_agent_coding_tools::FileService>,
    ) -> Arc<rightkit_agent_coding_tools::FileService> {
        let mut bound = self.file_service.write().unwrap_or_else(|e| e.into_inner());
        bound.get_or_insert(candidate).clone()
    }
}

/// Who started each sub-agent, so a sub-agent's changes can be offered to the chat that
/// started it. Filled when a sub-agent session is created; the facts a review needs later are
/// copied onto each entry, so nothing here has to survive a restart.
struct SubagentParent {
    for_session: String,
    role: String,
    parent_workspace: String,
}

fn subagent_parents() -> &'static std::sync::Mutex<HashMap<String, SubagentParent>> {
    static PARENTS: OnceLock<std::sync::Mutex<HashMap<String, SubagentParent>>> = OnceLock::new();
    PARENTS.get_or_init(Default::default)
}

/// Record that `child` is a sub-agent started (directly or through other sub-agents) by
/// `parent`, which works in `parent_workspace`. A nested sub-agent's changes are offered to
/// the top-level chat, the one a person reviews.
pub(crate) fn register_subagent_parent(
    child: &str,
    parent: &str,
    role: &str,
    parent_workspace: &str,
) {
    let mut parents = subagent_parents().lock().unwrap_or_else(|e| e.into_inner());
    let (for_session, parent_workspace) = match parents.get(parent) {
        Some(above) => (above.for_session.clone(), above.parent_workspace.clone()),
        None => (parent.to_string(), parent_workspace.to_string()),
    };
    parents.insert(
        child.to_string(),
        SubagentParent {
            for_session,
            role: role.to_string(),
            parent_workspace,
        },
    );
}

/// The proposal tag for a change made by `session_id` in `workspace_root`, or `None` when the
/// session is not a sub-agent.
fn proposal_for(session_id: &str, workspace_root: &str) -> Option<Proposal> {
    let parents = subagent_parents().lock().unwrap_or_else(|e| e.into_inner());
    let parent = parents.get(session_id)?;
    Some(new_proposal(
        &parent.for_session,
        session_id,
        &parent.role,
        &parent.parent_workspace,
        workspace_root,
    ))
}

fn new_proposal(
    for_session: &str,
    by_session: &str,
    role: &str,
    parent_workspace: &str,
    workspace_root: &str,
) -> Proposal {
    let same_folder = match (
        canonical_workspace_root(parent_workspace),
        canonical_workspace_root(workspace_root),
    ) {
        (Ok(parent), Ok(own)) => parent == own,
        _ => parent_workspace == workspace_root,
    };
    Proposal {
        for_session: for_session.to_string(),
        by_session: by_session.to_string(),
        by_role: (!role.is_empty()).then(|| role.to_string()),
        state: ProposalState::Proposed,
        applied: same_folder,
        source_root: (!same_folder).then(|| workspace_root.to_string()),
        decided_at: None,
        kept_hunks: 0,
    }
}

/// The session a run key or session id names.
fn session_of(run_or_session: &str, records: &BTreeMap<String, RunChangeRecord>) -> String {
    if records.contains_key(run_or_session) {
        if let Some((session, _)) = run_or_session.rsplit_once("--") {
            return session.to_string();
        }
    }
    run_or_session.to_string()
}

/// Sub-agent changes offered to `session`, with the record each lives in. Reverted ones are
/// left out, as elsewhere.
fn proposals_for_session(
    records: &BTreeMap<String, RunChangeRecord>,
    session: &str,
) -> Vec<(String, ChangeEntry)> {
    let mut out: Vec<(String, ChangeEntry)> = records
        .iter()
        .flat_map(|(key, record)| {
            record
                .entries
                .values()
                .filter(|entry| {
                    entry
                        .proposal
                        .as_ref()
                        .is_some_and(|proposal| proposal.for_session == session)
                })
                .map(move |entry| (key.clone(), entry.clone()))
        })
        .collect();
    out.sort_by(|a, b| a.1.captured_at.cmp(&b.1.captured_at));
    out
}

/// One entry by mutation id: in the session's own latest run, or among the sub-agent changes
/// offered to it. Returns the record key it lives under.
fn find_entry(
    records: &BTreeMap<String, RunChangeRecord>,
    run_or_session: &str,
    mutation_id: &str,
) -> Option<(String, ChangeEntry)> {
    if let Some(key) = resolve_run_key(records, run_or_session) {
        if let Some(entry) = records.get(&key).and_then(|r| r.entries.get(mutation_id)) {
            return Some((key, entry.clone()));
        }
    }
    let session = session_of(run_or_session, records);
    proposals_for_session(records, &session)
        .into_iter()
        .find(|(_, entry)| entry.mutation_id == mutation_id)
}

/// Whether a record key (`<session>--<n>`) belongs to `session_id`. A plain prefix test also
/// matched the runs of any session whose id merely starts with `<session_id>--`, so one
/// session's review, undo and deletion could reach another's changes.
fn run_key_is_of_session(key: &str, session_id: &str) -> bool {
    key.rsplit_once("--")
        .is_some_and(|(session, _)| session == session_id)
}

/// Whether `incoming` is a later edit of the file `existing` already records. Renames are
/// left alone: their entry is about two paths.
fn is_repeat_edit(existing: &ChangeEntry, incoming: &ChangeEntry) -> bool {
    existing.mutation_id != incoming.mutation_id
        && existing.path == incoming.path
        && existing.workspace_root == incoming.workspace_root
        && existing.classification != ChangeClassification::Renamed
        && incoming.classification != ChangeClassification::Renamed
}

/// One entry for a file edited more than once in a run: the newest observed state (diff, counts,
/// fingerprint, size) under the first edit's identity. The first edit's undo body is the file
/// as it was before the run touched it, so undo restores that, not the state between two edits;
/// its expected fingerprint moves to the file's current state so the undo is not refused as a
/// conflict. A file the run created stays "created" however often it was edited afterwards.
fn merge_repeat_edit(existing: &ChangeEntry, incoming: ChangeEntry) -> ChangeEntry {
    let mut merged = incoming;
    merged.mutation_id = existing.mutation_id.clone();
    if existing.stage != ChangeStage::Reverted {
        merged.stage = existing.stage;
    }
    merged.anchor_offset = existing.anchor_offset;
    if existing.classification == ChangeClassification::Untracked
        && merged.classification == ChangeClassification::Tracked
    {
        merged.classification = ChangeClassification::Untracked;
    }
    merged.undo = existing.undo.clone().map(|mut undo| {
        if let Some(latest) = merged.undo.as_ref() {
            undo.fingerprint = latest.fingerprint.clone();
        }
        undo
    });
    merged
}

/// The record key a client's `run_id` names: the exact key when it exists, otherwise the latest
/// run of the session it names (runs are keyed `<session>--<n>`, and the desktop asks with the
/// session id). Inventory and summary always resolved this way; diff-body, undo and revise looked
/// the id up exactly, so the review pane listed a changed file and then showed no diff for it.
fn resolve_run_key(records: &BTreeMap<String, RunChangeRecord>, run_id: &str) -> Option<String> {
    if records.contains_key(run_id) {
        return Some(run_id.to_string());
    }
    records
        .iter()
        .filter(|(key, _)| run_key_is_of_session(key, run_id))
        .max_by_key(|(_, value)| (&value.last_capture_at, value.entries.len()))
        .map(|(key, _)| key.clone())
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
            // A second edit of a file in the same run updates that file's entry; it does not
            // add another. The captured diff is the file's whole diff at that moment, so two
            // entries for one path listed the file twice, counted its lines twice, and the
            // review pane (which asks by path) showed the older diff.
            let earlier = entry
                .entries
                .iter()
                .find(|(_, existing)| is_repeat_edit(existing, &classified))
                .map(|(key, _)| key.clone());
            match earlier {
                Some(key) => {
                    if let Some(existing) = entry.entries.get_mut(&key) {
                        *existing = merge_repeat_edit(existing, classified);
                    }
                }
                None => {
                    entry
                        .entries
                        .insert(incoming.mutation_id.clone(), classified);
                }
            }
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
        let own = record.get_key_value(run_id).or_else(|| {
            record
                .iter()
                .filter(|(key, _)| run_key_is_of_session(key, run_id))
                .max_by_key(|(_, value)| (&value.last_capture_at, value.entries.len()))
        });
        // Changes its sub-agents made are part of what this chat has to review. They live in
        // the sub-agents' own run records and carry a `proposal` tag. A chat that only
        // delegated has no record of its own and still has a review list.
        let session = match own {
            Some((key, _)) => session_of(key, &record),
            None => run_id.to_string(),
        };
        let proposals = proposals_for_session(&record, &session);
        if own.is_none() && proposals.is_empty() {
            return None;
        }
        let empty = RunChangeRecord::default();
        let (resolved_run_id, rec) = match own {
            Some((key, rec)) => (key.clone(), rec),
            None => (run_id.to_string(), &empty),
        };
        let mut entries: Vec<ChangeEntry> = match request.path.as_ref() {
            Some(p) => rec
                .entries
                .values()
                .filter(|e| &e.path == p)
                .cloned()
                .collect(),
            None => rec.entries.values().cloned().collect(),
        };
        for (_, proposed) in proposals {
            let wanted = request.path.as_ref().is_none_or(|p| &proposed.path == p);
            if wanted
                && !entries
                    .iter()
                    .any(|e| e.mutation_id == proposed.mutation_id)
            {
                entries.push(proposed);
            }
        }
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
            run_id: resolved_run_id,
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
            record
                .iter()
                .filter(|(key, _)| run_key_is_of_session(key, run_id))
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

    /// Delete every run record captured by `session_id` (runs are named `<session>--<n>`), in
    /// memory and on disk. Returns how many run records were removed. Deleting a session removes
    /// its files (storage rule 35).
    pub(crate) fn forget_session(&self, session_id: &str) -> usize {
        let mut records = self.inner.write().expect("run_changes lock poisoned");
        let doomed: Vec<String> = records
            .keys()
            .filter(|run_id| run_key_is_of_session(run_id, session_id))
            .cloned()
            .collect();
        for run_id in &doomed {
            records.remove(run_id);
            let path = self.root.join(format!("{run_id}.bin"));
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(file = %path.display(), %error, "run-change record of a deleted session could not be removed");
                }
            }
        }
        doomed.len()
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
        let record = self.inner.read().expect("run_changes lock poisoned");
        let mut out: Vec<(String, ChangeEntry)> = record
            .iter()
            .filter(|(run_id, _)| run_key_is_of_session(run_id, session_id))
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
        let (run_key, entry) = match find_entry(&record, &request.run_id, &request.mutation_id) {
            Some(found) => found,
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
        if let Some(rec) = record.get_mut(&run_key) {
            if let Some(e) = rec.entries.get_mut(&entry.mutation_id) {
                e.stage = ChangeStage::Reverted;
            }
            let snapshot = rec.clone();
            drop(record);
            if let Err(error) = persist(&self.root, &run_key, &snapshot) {
                tracing::error!(run_id = %run_key, %error, "persisting reverted run-change state failed");
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

    /// Keep or discard a sub-agent's change.
    ///
    /// - Keep, change already in the project's files: it is marked kept.
    /// - Keep, change held in the sub-agent's worktree: the file is brought into the project
    ///   through the file service, unless the project's copy changed since the sub-agent
    ///   branched; an undo body is recorded so it can still be taken back.
    /// - Discard, change already in the project's files: it is undone (same checks as undo).
    /// - Discard, change held in the worktree: it is marked discarded; the project was never
    ///   touched.
    pub fn decide_proposal(&self, request: ProposalDecisionRequest) -> ProposalDecisionResponse {
        let found = {
            let record = self.inner.read().expect("run_changes lock poisoned");
            find_entry(&record, &request.run_id, &request.mutation_id)
        };
        let refuse = |entry: Option<&ChangeEntry>, reason: &str| ProposalDecisionResponse {
            mutation_id: request.mutation_id.clone(),
            path: entry.map(|e| e.path.clone()).unwrap_or_default(),
            state: entry
                .and_then(|e| e.proposal.as_ref())
                .map(|p| p.state)
                .unwrap_or(ProposalState::Proposed),
            applied: entry
                .and_then(|e| e.proposal.as_ref())
                .is_some_and(|p| p.applied),
            conflict_reason: Some(reason.to_string()),
        };
        let Some((record_key, entry)) = found else {
            return refuse(None, "That change is not in this chat's review list.");
        };
        let Some(proposal) = entry.proposal.clone() else {
            return refuse(
                Some(&entry),
                "That change was made by this chat itself, not proposed by a sub-agent.",
            );
        };
        if proposal.state != ProposalState::Proposed {
            return refuse(Some(&entry), "That change has already been decided.");
        }
        let keep = match request.action.as_str() {
            "keep" => true,
            "discard" => false,
            _ => return refuse(Some(&entry), "The action must be keep or discard."),
        };

        let mut updated = entry.clone();
        let mut next = proposal.clone();
        next.decided_at = Some(now_iso());
        match (keep, proposal.applied) {
            (true, true) => next.state = ProposalState::Kept,
            (false, false) => next.state = ProposalState::Discarded,
            (false, true) => {
                let Some(observed) = request.observed_fingerprint.clone() else {
                    return refuse(Some(&entry), "Send the file's fingerprint to discard a change that is already in the project.");
                };
                let undone = self.undo(
                    &request.run_id,
                    UndoRequest {
                        run_id: request.run_id.clone(),
                        workspace_root: request.workspace_root.clone(),
                        mutation_id: request.mutation_id.clone(),
                        observed_fingerprint: observed,
                    },
                );
                if !undone.applied {
                    let reason = undone
                        .conflict_reason
                        .unwrap_or_else(|| "The change could not be undone.".to_string());
                    return refuse(Some(&entry), &reason);
                }
                updated.stage = ChangeStage::Reverted;
                next.state = ProposalState::Discarded;
                next.applied = false;
            }
            (true, false) => {
                let project_root = match canonical_workspace_root(&request.workspace_root) {
                    Ok(root) => root,
                    Err(reason) => return refuse(Some(&entry), &reason),
                };
                let Some(source_root) = proposal.source_root.as_deref() else {
                    return refuse(
                        Some(&entry),
                        "The sub-agent's copy of this change is not recorded.",
                    );
                };
                let source_root = match canonical_workspace_root(source_root) {
                    Ok(root) => root,
                    Err(_) => {
                        return refuse(Some(&entry), "The sub-agent's worktree no longer exists, so its change cannot be brought in.")
                    }
                };
                let files = self
                    .file_service
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let Some(files) = files else {
                    return refuse(
                        Some(&entry),
                        "The engine that owns this project's files is not running.",
                    );
                };
                match apply_held_change(files.as_ref(), &project_root, &source_root, &entry) {
                    Ok(applied) => {
                        updated.workspace_root = project_root.to_string_lossy().into_owned();
                        updated.undo = applied.undo;
                        updated.fingerprint = applied.fingerprint;
                        updated.stage = ChangeStage::Unstaged;
                        next.state = ProposalState::Kept;
                        next.applied = true;
                    }
                    Err(reason) => return refuse(Some(&entry), &reason),
                }
            }
        }
        updated.proposal = Some(next.clone());

        let mut record = self.inner.write().expect("run_changes lock poisoned");
        if let Some(rec) = record.get_mut(&record_key) {
            rec.entries
                .insert(updated.mutation_id.clone(), updated.clone());
            let snapshot = rec.clone();
            drop(record);
            if let Err(error) = persist(&self.root, &record_key, &snapshot) {
                tracing::error!(run_id = %record_key, %error, "persisting a proposal decision failed");
            }
        }
        ProposalDecisionResponse {
            mutation_id: updated.mutation_id,
            path: updated.path,
            state: next.state,
            applied: next.applied,
            conflict_reason: None,
        }
    }

    /// Keep or undo one hunk of a file change.
    ///
    /// - Undo, change in the project's files: just that hunk is taken out of the working file,
    ///   through the file service, when the file still has the hunk's lines as the change left
    ///   them (anywhere else in the file is free to differ). Otherwise 409 and nothing is written.
    /// - Keep, change held in a sub-agent's worktree: just that hunk is applied to the project's
    ///   file, when the file has the lines the hunk starts from. The other hunks stay proposed;
    ///   the change is decided when none are left (kept if any hunk was kept).
    /// - Undo, change held in a worktree: the hunk is dropped; the project is never touched.
    /// - Keep, change already in the project's files: nothing to do.
    pub fn decide_hunk(&self, request: HunkDecisionRequest) -> (StatusCode, HunkDecisionResponse) {
        use rightkit_agent_coding_tools::file_service::{ExpectedPreimage, MutationIntent};

        let found = {
            let record = self.inner.read().expect("run_changes lock poisoned");
            request
                .mutation_id
                .as_deref()
                .and_then(|id| find_entry(&record, &request.run_id, id))
                .or_else(|| {
                    if request.mutation_id.is_some() {
                        return None;
                    }
                    resolve_run_key(&record, &request.run_id)
                        .and_then(|key| {
                            record
                                .get(&key)
                                .and_then(|r| r.entries.values().find(|e| e.path == request.path))
                                .map(|entry| (key, entry.clone()))
                        })
                        .or_else(|| {
                            let session = session_of(&request.run_id, &record);
                            proposals_for_session(&record, &session)
                                .into_iter()
                                .find(|(_, e)| e.path == request.path)
                        })
                })
        };
        let reply = |status: StatusCode,
                     entry: Option<&ChangeEntry>,
                     applied: bool,
                     remaining: usize,
                     fingerprint: String,
                     reason: Option<&str>| {
            (
                status,
                HunkDecisionResponse {
                    mutation_id: entry
                        .map(|e| e.mutation_id.clone())
                        .or_else(|| request.mutation_id.clone())
                        .unwrap_or_default(),
                    path: entry
                        .map(|e| e.path.clone())
                        .unwrap_or_else(|| request.path.clone()),
                    hunk_id: request.hunk_id.clone(),
                    action: request.action.clone(),
                    applied,
                    remaining_hunks: remaining,
                    state: entry.and_then(|e| e.proposal.as_ref()).map(|p| p.state),
                    new_fingerprint: fingerprint,
                    conflict_reason: reason.map(str::to_string),
                },
            )
        };
        let refuse = |status: StatusCode, entry: Option<&ChangeEntry>, reason: &str| {
            let remaining = entry.map(|e| split_hunks(&e.diff).1.len()).unwrap_or(0);
            reply(status, entry, false, remaining, String::new(), Some(reason))
        };

        let keep = match request.action.as_str() {
            "keep" => true,
            "undo" => false,
            _ => {
                return refuse(
                    StatusCode::BAD_REQUEST,
                    None,
                    "The action must be keep or undo.",
                )
            }
        };
        let Some((record_key, entry)) = found else {
            return refuse(
                StatusCode::NOT_FOUND,
                None,
                "That change is not in this chat's review list.",
            );
        };
        if entry.path != request.path {
            return refuse(
                StatusCode::NOT_FOUND,
                Some(&entry),
                "That change is for a different file.",
            );
        }
        if entry.classification != ChangeClassification::Tracked {
            return refuse(StatusCode::CONFLICT, Some(&entry), "Single parts can be kept or undone in an edited file; use the whole-file action for this change.");
        }
        if entry.stage == ChangeStage::Reverted {
            return refuse(
                StatusCode::CONFLICT,
                Some(&entry),
                "That change was already undone.",
            );
        }
        let proposal = entry.proposal.clone();
        if proposal
            .as_ref()
            .is_some_and(|p| p.state != ProposalState::Proposed)
        {
            return refuse(
                StatusCode::CONFLICT,
                Some(&entry),
                "That change has already been decided.",
            );
        }
        let (header, hunks) = split_hunks(&entry.diff);
        let Some(index) = find_hunk(&hunks, &request.hunk_id) else {
            return refuse(
                StatusCode::NOT_FOUND,
                Some(&entry),
                "That part of the change is no longer in the diff.",
            );
        };
        let held = proposal.as_ref().is_some_and(|p| !p.applied);
        if keep && !held {
            let fingerprint = canonical_workspace_root(&entry.workspace_root)
                .ok()
                .and_then(|root| resolve_relative_within(&root, &entry.path).ok())
                .and_then(|path| fingerprint_path(&path))
                .unwrap_or_default();
            return reply(
                StatusCode::OK,
                Some(&entry),
                false,
                hunks.len(),
                fingerprint,
                None,
            );
        }

        let mut updated = entry.clone();
        let mut next = proposal.clone();
        let mut applied = false;
        let mut new_fingerprint = String::new();
        let mut project_root: Option<PathBuf> = None;
        let write_through = |root: &Path| -> Result<(PathBuf, Option<String>, String), String> {
            let files = self
                .file_service
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| {
                    "The engine that owns this project's files is not running.".to_string()
                })?;
            let path = resolve_relative_within(root, &entry.path)?;
            // Observe the file first, then read it: a write that lands in between is refused by
            // the file service rather than overwritten.
            let expected = match files
                .observe_preimage(&path)
                .map_err(|error| error.to_string())?
            {
                ExpectedPreimage::Revision(expected) => expected,
                ExpectedPreimage::Missing => {
                    return Err(format!("'{}' is no longer in the project.", entry.path))
                }
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|_| format!("'{}' could not be read as text.", entry.path))?;
            let changed = swap_hunk(&text, &hunks[index], keep)?;
            files
                .apply_byte_intent(MutationIntent::restore(&path, expected), changed.as_bytes())
                .map_err(|error| error.to_string())?;
            Ok((path, Some(text), changed))
        };

        let new_diff = if held {
            if keep {
                let root = match canonical_workspace_root(&request.workspace_root) {
                    Ok(root) => root,
                    Err(reason) => return refuse(StatusCode::CONFLICT, Some(&entry), &reason),
                };
                let (path, before, _) = match write_through(&root) {
                    Ok(done) => done,
                    Err(reason) => return refuse(StatusCode::CONFLICT, Some(&entry), &reason),
                };
                applied = true;
                new_fingerprint = fingerprint_path(&path).unwrap_or_default();
                updated.fingerprint = new_fingerprint.clone();
                let first = next.as_ref().is_some_and(|p| p.kept_hunks == 0);
                if first {
                    // What the project had before the first kept hunk, for the whole-file undo.
                    updated.undo = before
                        .filter(|text| text.len() <= MAX_UNDO_BYTES as usize)
                        .filter(|text| !looks_like_secret(&path, text.as_bytes()))
                        .map(|body| UndoBody {
                            fingerprint: new_fingerprint.clone(),
                            body,
                            note: "restore the project's copy from before the proposal was kept"
                                .to_string(),
                        });
                } else if let Some(undo) = updated.undo.as_mut() {
                    undo.fingerprint = new_fingerprint.clone();
                }
                if let Some(p) = next.as_mut() {
                    p.kept_hunks += 1;
                }
                project_root = Some(root);
            }
            // Undo of a held hunk: dropped from the list; the project was never touched.
            diff_without_hunk(&header, &hunks, index)
        } else {
            let stored_root = match canonical_workspace_root(&entry.workspace_root) {
                Ok(root) => root,
                Err(reason) => return refuse(StatusCode::CONFLICT, Some(&entry), &reason),
            };
            match canonical_workspace_root(&request.workspace_root) {
                Ok(requested) if requested == stored_root => {}
                Ok(_) => {
                    return refuse(
                        StatusCode::CONFLICT,
                        Some(&entry),
                        "undo workspace does not match the captured workspace",
                    )
                }
                Err(reason) => return refuse(StatusCode::CONFLICT, Some(&entry), &reason),
            }
            let live = resolve_relative_within(&stored_root, &entry.path).ok();
            let before_fingerprint = live.as_deref().and_then(fingerprint_path);
            let (path, _, _) = match write_through(&stored_root) {
                Ok(done) => done,
                Err(reason) => return refuse(StatusCode::CONFLICT, Some(&entry), &reason),
            };
            applied = true;
            new_fingerprint = fingerprint_path(&path).unwrap_or_default();
            // Only when the file was exactly as the agent left it before this call: otherwise a
            // hand edit elsewhere in it must keep blocking the whole-file undo.
            if !entry.fingerprint.is_empty()
                && before_fingerprint.as_deref() == Some(entry.fingerprint.as_str())
            {
                updated.fingerprint = new_fingerprint.clone();
                if let Some(undo) = updated.undo.as_mut() {
                    undo.fingerprint = new_fingerprint.clone();
                }
            }
            updated.size_bytes = std::fs::metadata(&path)
                .map(|m| m.len())
                .unwrap_or(updated.size_bytes);
            observed_diff(
                &stored_root.to_string_lossy(),
                &entry.path,
                ChangeClassification::Tracked,
            )
        };
        let remaining = split_hunks(&new_diff).1.len();
        let (additions, deletions) = diff_counts(&new_diff);
        updated.diff = new_diff;
        updated.additions = additions;
        updated.deletions = deletions;
        if remaining == 0 {
            match next.as_mut() {
                Some(p) if held && p.kept_hunks > 0 => {
                    p.state = ProposalState::Kept;
                    p.applied = true;
                    p.decided_at = Some(now_iso());
                    if let Some(root) = project_root.as_ref() {
                        updated.workspace_root = root.to_string_lossy().into_owned();
                    }
                    updated.stage = ChangeStage::Unstaged;
                }
                Some(p) => {
                    p.state = ProposalState::Discarded;
                    p.applied = false;
                    p.decided_at = Some(now_iso());
                    updated.stage = ChangeStage::Reverted;
                }
                None => updated.stage = ChangeStage::Reverted,
            }
        }
        updated.proposal = next;

        let mut record = self.inner.write().expect("run_changes lock poisoned");
        if let Some(rec) = record.get_mut(&record_key) {
            rec.entries
                .insert(updated.mutation_id.clone(), updated.clone());
            let snapshot = rec.clone();
            drop(record);
            if let Err(error) = persist(&self.root, &record_key, &snapshot) {
                tracing::error!(run_id = %record_key, %error, "persisting a hunk decision failed");
            }
        }
        reply(
            StatusCode::OK,
            Some(&updated),
            applied,
            remaining,
            new_fingerprint,
            None,
        )
    }

    pub fn revise(&self, run_id: &str, request: RevisionUpdateRequest) -> RevisionUpdateResponse {
        let mut record = self.inner.write().expect("run_changes lock poisoned");
        let run_key = resolve_run_key(&record, run_id).unwrap_or_else(|| run_id.to_string());
        let rec = record.entry(run_key.clone()).or_default();
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
        persist(&self.root, &run_key, &snapshot).ok();
        RevisionUpdateResponse {
            comment_id,
            anchor_status: anchor,
            stale: anchor != AnchorStatus::Live,
        }
    }

    pub fn diff_body(&self, request: DiffBodyRequest) -> DiffBodyResponse {
        let record = self.inner.read().expect("run_changes lock poisoned");
        let by_id = request
            .mutation_id
            .as_deref()
            .and_then(|id| find_entry(&record, &request.run_id, id))
            .map(|(_, entry)| entry);
        let entry = by_id
            .or_else(|| {
                resolve_run_key(&record, &request.run_id)
                    .and_then(|key| record.get(&key))
                    .and_then(|r| r.entries.values().find(|e| e.path == request.path))
                    .cloned()
            })
            .or_else(|| {
                let session = session_of(&request.run_id, &record);
                proposals_for_session(&record, &session)
                    .into_iter()
                    .map(|(_, entry)| entry)
                    .find(|e| e.path == request.path)
            });
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
                    hunks: Vec::new(),
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
            hunks: hunk_infos(&entry.diff),
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
        mutations: &[rightkit_agent_coding_tools::WorkspaceMutation],
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
                rightkit_agent_coding_tools::WorkspaceMutationKind::Renamed { from } => {
                    Some(relative_workspace_path(&root, from)?)
                }
                _ => None,
            };
            let observed_classification = match mutation.kind {
                rightkit_agent_coding_tools::WorkspaceMutationKind::Created => {
                    if git_path_is_tracked(&workspace, &path) {
                        ChangeClassification::Tracked
                    } else {
                        ChangeClassification::Untracked
                    }
                }
                rightkit_agent_coding_tools::WorkspaceMutationKind::Modified => {
                    ChangeClassification::Tracked
                }
                rightkit_agent_coding_tools::WorkspaceMutationKind::Deleted => {
                    ChangeClassification::Deleted
                }
                rightkit_agent_coding_tools::WorkspaceMutationKind::Renamed { .. } => {
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
                rightkit_agent_coding_tools::WorkspaceMutationKind::Created => Some(UndoBody {
                    fingerprint: fingerprint_path(&mutation.path).unwrap_or_default(),
                    body: undo_marker("DELETE"),
                    note: "remove file created by this tool call".to_string(),
                }),
                rightkit_agent_coding_tools::WorkspaceMutationKind::Modified
                | rightkit_agent_coding_tools::WorkspaceMutationKind::Deleted => mutation
                    .previous_content
                    .as_ref()
                    .filter(|content| content.len() <= MAX_UNDO_BYTES as usize)
                    .filter(|content| !looks_like_secret(&mutation.path, content.as_bytes()))
                    .map(|content| UndoBody {
                        fingerprint: fingerprint_path(&mutation.path).unwrap_or_default(),
                        body: content.clone(),
                        note: "restore the exact pre-effect file content".to_string(),
                    }),
                rightkit_agent_coding_tools::WorkspaceMutationKind::Renamed { .. } => {
                    Some(UndoBody {
                        fingerprint: fingerprint_path(&mutation.path).unwrap_or_default(),
                        body: undo_marker("RENAME"),
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
                proposal: proposal_for(_session_id, &workspace),
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
/// What bringing a held change into the project left behind.
struct AppliedHeldChange {
    undo: Option<UndoBody>,
    fingerprint: String,
}

/// Whether the project's copy of `path` is as the last commit has it. A sub-agent's worktree
/// starts from that commit, so a clean path is one the sub-agent's version can replace
/// without losing anything.
fn project_path_is_clean(project_root: &Path, path: &str) -> bool {
    managed_git_output(
        &project_root.to_string_lossy(),
        &["status", "--porcelain", "--", path],
    )
    .is_some_and(|out| out.stdout.trim().is_empty())
}

/// Bring one file from a sub-agent's worktree into the project, through the file service.
fn apply_held_change(
    files: &rightkit_agent_coding_tools::FileService,
    project_root: &Path,
    source_root: &Path,
    entry: &ChangeEntry,
) -> Result<AppliedHeldChange, String> {
    use rightkit_agent_coding_tools::file_service::{ExpectedPreimage, MutationIntent};

    let destination = resolve_relative_within(project_root, &entry.path)?;
    let source = resolve_relative_within(source_root, &entry.path)?;
    if !project_path_is_clean(project_root, &entry.path) {
        return Err(format!(
            "'{}' has changed in your project since the sub-agent started. Nothing was applied; open the diff and bring it in by hand.",
            entry.path
        ));
    }
    let before = std::fs::read(&destination).ok();
    let preimage = files
        .observe_preimage(&destination)
        .map_err(|error| error.to_string())?;
    match std::fs::read(&source) {
        Ok(bytes) => {
            let intent = match preimage {
                ExpectedPreimage::Revision(expected) => {
                    MutationIntent::restore(&destination, expected)
                }
                ExpectedPreimage::Missing => MutationIntent::create(&destination),
            };
            files
                .apply_byte_intent(intent, &bytes)
                .map_err(|error| error.to_string())?;
        }
        // The sub-agent deleted the file.
        Err(_) => {
            if let ExpectedPreimage::Revision(expected) = preimage {
                files
                    .delete_with_intent(MutationIntent::delete(&destination, expected), false)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    let fingerprint = fingerprint_path(&destination).unwrap_or_default();
    let undo = match before {
        None => Some(UndoBody {
            fingerprint: fingerprint.clone(),
            body: undo_marker("DELETE"),
            note: "remove the file this proposal added".to_string(),
        }),
        Some(bytes) if bytes.len() <= MAX_UNDO_BYTES as usize => String::from_utf8(bytes)
            .ok()
            .filter(|text| !looks_like_secret(&destination, text.as_bytes()))
            .map(|body| UndoBody {
                fingerprint: fingerprint.clone(),
                body,
                note: "restore the project's copy from before the proposal was kept".to_string(),
            }),
        Some(_) => None,
    };
    Ok(AppliedHeldChange { undo, fingerprint })
}

fn apply_undo_with_service(
    files: &rightkit_agent_coding_tools::FileService,
    workspace_root: &Path,
    entry: &ChangeEntry,
    undo: &UndoBody,
) -> Result<(), String> {
    use rightkit_agent_coding_tools::file_service::{ExpectedPreimage, MutationIntent};

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
    } else if !entry.fingerprint.is_empty() && undo.body != undo_marker("DELETE") {
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
        if undo.body == undo_marker("DELETE") {
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

        if undo.body == undo_marker("RENAME") {
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

/// Rows for the review pane. The same rows serve the split and the one-column
/// view (the pane lays them out); `_split` is kept for the request shape.
///
/// Only the `diff --git` line of a file's header is shown: `index`, `---`,
/// `+++` and mode lines carry no content. Lines inside a hunk lose their
/// one-character marker; any other line (such as "\ No newline at end of
/// file") keeps its full text and has no line numbers.
fn split_diff_rows(diff: &str, _split: bool) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    let mut old_n: u32 = 0;
    let mut new_n: u32 = 0;
    let mut in_hunk = false;
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
            in_hunk = false;
            continue;
        }
        if let Some(caps) = regex_hunk_captures(line) {
            old_n = caps.0;
            new_n = caps.1;
            in_hunk = true;
            rows.push(DiffRow {
                kind: "hunk".into(),
                text: line.to_string(),
                old_line: Some(old_n),
                new_line: Some(new_n),
            });
            continue;
        }
        if !in_hunk {
            if line.starts_with("Binary files ") {
                rows.push(DiffRow {
                    kind: "file".into(),
                    text: line.to_string(),
                    old_line: None,
                    new_line: None,
                });
            }
            continue;
        }
        let mut chars = line.chars();
        let marker = chars.next();
        let rest = chars.as_str().to_string();
        match marker {
            Some('+') => {
                rows.push(DiffRow {
                    kind: "add".into(),
                    text: rest,
                    old_line: None,
                    new_line: Some(new_n),
                });
                new_n += 1;
            }
            Some('-') => {
                rows.push(DiffRow {
                    kind: "del".into(),
                    text: rest,
                    old_line: Some(old_n),
                    new_line: None,
                });
                old_n += 1;
            }
            Some(' ') => {
                rows.push(DiffRow {
                    kind: "ctx".into(),
                    text: rest,
                    old_line: Some(old_n),
                    new_line: Some(new_n),
                });
                old_n += 1;
                new_n += 1;
            }
            None => {
                // An empty line inside a hunk is an empty context line.
                rows.push(DiffRow {
                    kind: "ctx".into(),
                    text: String::new(),
                    old_line: Some(old_n),
                    new_line: Some(new_n),
                });
                old_n += 1;
                new_n += 1;
            }
            Some(_) => {
                rows.push(DiffRow {
                    kind: "ctx".into(),
                    text: line.to_string(),
                    old_line: None,
                    new_line: None,
                });
            }
        }
    }
    rows
}

#[cfg(test)]
mod repeat_edit_tests {
    use super::*;

    fn entry(id: &str, path: &str, diff: &str, before: &str, after_print: &str) -> ChangeEntry {
        ChangeEntry {
            mutation_id: id.to_string(),
            workspace_root: "/ws".to_string(),
            path: path.to_string(),
            original_path: None,
            classification: ChangeClassification::Tracked,
            outcome: ChangeOutcome::Complete,
            stage: ChangeStage::Unstaged,
            additions: diff.matches("\n+").count() as i64,
            deletions: 0,
            diff: diff.to_string(),
            truncated: false,
            anchor_status: AnchorStatus::Live,
            anchor_offset: 0,
            captured_at: id.to_string(),
            fingerprint: after_print.to_string(),
            size_bytes: 1,
            modified_at: None,
            undo: Some(UndoBody {
                fingerprint: after_print.to_string(),
                body: before.to_string(),
                note: String::new(),
            }),
            last_error: None,
            proposal: None,
        }
    }

    #[test]
    fn a_second_edit_of_a_file_updates_its_entry_and_keeps_the_first_undo() {
        let first = entry("r:1:0", "a.py", "@@\n+one", "original", "print-1");
        let second = entry(
            "r:2:0",
            "a.py",
            "@@\n+one\n+two",
            "after first edit",
            "print-2",
        );
        assert!(is_repeat_edit(&first, &second));
        let merged = merge_repeat_edit(&first, second);
        assert_eq!(merged.mutation_id, "r:1:0");
        assert_eq!(merged.diff, "@@\n+one\n+two");
        assert_eq!(merged.additions, 2);
        assert_eq!(merged.fingerprint, "print-2");
        let undo = merged.undo.expect("undo kept");
        assert_eq!(undo.body, "original");
        assert_eq!(undo.fingerprint, "print-2");
    }

    #[test]
    fn another_file_or_a_rename_is_not_a_repeat_edit() {
        let first = entry("r:1:0", "a.py", "", "", "");
        assert!(!is_repeat_edit(&first, &entry("r:2:0", "b.py", "", "", "")));
        assert!(!is_repeat_edit(&first, &entry("r:1:0", "a.py", "", "", "")));
        let mut renamed = entry("r:3:0", "a.py", "", "", "");
        renamed.classification = ChangeClassification::Renamed;
        assert!(!is_repeat_edit(&first, &renamed));
    }

    #[test]
    fn a_file_created_in_the_run_stays_created_and_a_reverted_one_is_live_again() {
        let mut created = entry("r:1:0", "new.py", "", "", "p1");
        created.classification = ChangeClassification::Untracked;
        created.stage = ChangeStage::Reverted;
        let merged = merge_repeat_edit(&created, entry("r:2:0", "new.py", "", "", "p2"));
        assert_eq!(merged.classification, ChangeClassification::Untracked);
        assert_eq!(merged.stage, ChangeStage::Unstaged);
    }

    #[test]
    fn a_sub_agents_change_is_offered_to_the_chat_that_started_it() {
        register_subagent_parent("child-a", "root-chat", "tests", "/ws");
        register_subagent_parent("grandchild", "child-a", "docs", "/elsewhere");
        let same_folder =
            proposal_for("child-a", "/ws").expect("a sub-agent's change is a proposal");
        assert_eq!(same_folder.for_session, "root-chat");
        assert_eq!(same_folder.by_role.as_deref(), Some("tests"));
        assert_eq!(same_folder.state, ProposalState::Proposed);
        assert!(
            same_folder.applied,
            "same folder: the change is already in the project"
        );
        assert!(same_folder.source_root.is_none());
        let held = proposal_for("child-a", "/ws-worktree").expect("proposal");
        assert!(!held.applied, "own worktree: held apart");
        assert_eq!(held.source_root.as_deref(), Some("/ws-worktree"));
        // A sub-agent of a sub-agent still reports to the top-level chat.
        let nested = proposal_for("grandchild", "/ws").expect("proposal");
        assert_eq!(nested.for_session, "root-chat");
        assert!(nested.applied);
        assert!(
            proposal_for("root-chat", "/ws").is_none(),
            "the chat's own change is not a proposal"
        );
    }

    #[test]
    fn a_chats_review_list_includes_its_sub_agents_proposals() {
        let mut own = entry("root-chat--1:call_1:0", "a.py", "", "", "p1");
        own.captured_at = "2026-11-12T10:00:00Z".into();
        let mut proposed = entry("child-a--9:call_1:0", "b.py", "", "", "p2");
        proposed.captured_at = "2026-11-12T10:01:00Z".into();
        proposed.proposal = Some(new_proposal("root-chat", "child-a", "tests", "/ws", "/ws"));
        let mut other = entry("child-z--3:call_1:0", "c.py", "", "", "p3");
        other.proposal = Some(new_proposal(
            "another-chat",
            "child-z",
            "tests",
            "/ws",
            "/ws",
        ));
        let mut records: BTreeMap<String, RunChangeRecord> = BTreeMap::new();
        for (key, e) in [
            ("root-chat--1", own),
            ("child-a--9", proposed),
            ("child-z--3", other),
        ] {
            records
                .entry(key.to_string())
                .or_default()
                .entries
                .insert(e.mutation_id.clone(), e);
        }
        let offered = proposals_for_session(&records, "root-chat");
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].0, "child-a--9");
        assert_eq!(offered[0].1.path, "b.py");
        // Found by the parent's session id or by its run id, and reported with its own record.
        for name in ["root-chat", "root-chat--1"] {
            let (key, found) = find_entry(&records, name, "child-a--9:call_1:0").expect("found");
            assert_eq!(key, "child-a--9");
            assert_eq!(found.path, "b.py");
        }
        assert!(find_entry(&records, "root-chat", "child-z--3:call_1:0").is_none());
        assert_eq!(session_of("root-chat--1", &records), "root-chat");
        assert_eq!(session_of("root-chat", &records), "root-chat");
    }

    #[test]
    fn a_run_key_matches_only_its_own_session() {
        assert!(run_key_is_of_session("abc--0199-7f", "abc"));
        assert!(!run_key_is_of_session("abc--x--0199-7f", "abc"));
        assert!(run_key_is_of_session("abc--x--0199-7f", "abc--x"));
        assert!(!run_key_is_of_session("abc", "abc"));
        assert!(!run_key_is_of_session("abcd--1", "abc"));
    }
}

#[cfg(test)]
mod split_diff_rows_tests {
    use super::split_diff_rows;

    const DIFF: &str = "diff --git a/R.md b/R.md\nindex b528dd5..9347fa5 100644\n--- a/R.md\n+++ b/R.md\n@@ -1,2 +1,3 @@\n keep\n-old\n+new\n+\u{e9}t\u{e9}\n\\ No newline at end of file\n";

    #[test]
    fn header_lines_are_not_rows_and_markers_are_stripped_once() {
        for split in [false, true] {
            let rows = split_diff_rows(DIFF, split);
            let texts: Vec<(&str, &str)> = rows
                .iter()
                .map(|r| (r.kind.as_str(), r.text.as_str()))
                .collect();
            assert_eq!(
                texts,
                vec![
                    ("file", "diff --git a/R.md b/R.md"),
                    ("hunk", "@@ -1,2 +1,3 @@"),
                    ("ctx", "keep"),
                    ("del", "old"),
                    ("add", "new"),
                    ("add", "\u{e9}t\u{e9}"),
                    ("ctx", "\\ No newline at end of file"),
                ],
                "split = {split}"
            );
            assert_eq!((rows[2].old_line, rows[2].new_line), (Some(1), Some(1)));
            assert_eq!((rows[4].old_line, rows[4].new_line), (None, Some(2)));
            assert_eq!((rows[6].old_line, rows[6].new_line), (None, None));
        }
    }

    #[test]
    fn a_line_starting_with_a_multibyte_character_does_not_panic() {
        let rows = split_diff_rows("@@ -1 +1 @@\n\u{e9}\n", false);
        assert_eq!(rows[1].text, "\u{e9}");
    }
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

// ── Per-hunk keep and undo ────────────────────────────────────────────────────

/// One hunk of a file's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffHunk {
    old_start: u32,
    old_lines: u32,
    new_start: u32,
    new_lines: u32,
    /// The hunk as git printed it: the `@@` line and the lines after it, each ending in a newline.
    raw: String,
    /// Lines after the `@@` line: the marker (` `, `+`, `-` or `\`) and the text after it.
    body: Vec<(char, String)>,
}

impl DiffHunk {
    /// The hunk's lines as they read before (`'-'`) or after (`'+'`) the change.
    fn side(&self, changed: char) -> Vec<String> {
        self.body
            .iter()
            .filter(|(marker, _)| *marker == ' ' || *marker == changed)
            .map(|(_, text)| text.clone())
            .collect()
    }

    fn count(&self, marker: char) -> u32 {
        self.body.iter().filter(|(m, _)| *m == marker).count() as u32
    }

    /// Hash of the hunk's lines only, so line numbers shifting does not change it.
    fn hash(&self) -> String {
        let mut text = String::new();
        for (marker, line) in &self.body {
            text.push(*marker);
            text.push_str(line);
            text.push('\n');
        }
        short_hex(&rightkit_fs::digest::sha256_bytes(text.as_bytes()))
    }
}

/// `@@ -a[,b] +c[,d] @@` as (a, b, c, d).
fn hunk_ranges(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let new = rest.split(" @@").next()?;
    let pair = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, len)) => Some((start.parse().ok()?, len.trim().parse().ok()?)),
            None => Some((text.trim().parse().ok()?, 1)),
        }
    };
    let (old_start, old_lines) = pair(old)?;
    let (new_start, new_lines) = pair(new)?;
    Some((old_start, old_lines, new_start, new_lines))
}

/// A one-file diff split into its header lines and its hunks. Anything after a second
/// `diff --git` is ignored: an entry is one file.
fn split_hunks(diff: &str) -> (String, Vec<DiffHunk>) {
    let mut header = String::new();
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut seen_file = false;
    for segment in diff.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        if line.starts_with("diff --git") {
            if seen_file {
                break;
            }
            seen_file = true;
        }
        if let Some((old_start, old_lines, new_start, new_lines)) = hunk_ranges(line.trim_end()) {
            hunks.push(DiffHunk {
                old_start,
                old_lines,
                new_start,
                new_lines,
                raw: segment.to_string(),
                body: Vec::new(),
            });
            if !segment.ends_with('\n') {
                if let Some(hunk) = hunks.last_mut() {
                    hunk.raw.push('\n');
                }
            }
            continue;
        }
        match hunks.last_mut() {
            None => header.push_str(segment),
            Some(hunk) => {
                let mut chars = line.chars();
                let (marker, text) = match chars.next() {
                    Some(m @ ('+' | '-' | ' ' | '\\')) => (m, chars.as_str().to_string()),
                    Some(_) => (' ', line.to_string()),
                    None => (' ', String::new()),
                };
                hunk.body.push((marker, text));
                hunk.raw.push_str(segment);
                if !segment.ends_with('\n') {
                    hunk.raw.push('\n');
                }
            }
        }
    }
    (header, hunks)
}

fn hunk_id(index: usize, hunk: &DiffHunk) -> String {
    format!("{index}-{}", hunk.hash())
}

fn hunk_infos(diff: &str) -> Vec<DiffHunkInfo> {
    split_hunks(diff)
        .1
        .iter()
        .enumerate()
        .map(|(index, hunk)| DiffHunkInfo {
            hunk_id: hunk_id(index, hunk),
            index: index as u32,
            old_start: hunk.old_start,
            old_lines: hunk.old_lines,
            new_start: hunk.new_start,
            new_lines: hunk.new_lines,
            additions: hunk.count('+'),
            deletions: hunk.count('-'),
        })
        .collect()
}

/// The hunk a client named: the exact id first, then the one hunk with the same hash (the
/// index moves when earlier hunks are gone).
fn find_hunk(hunks: &[DiffHunk], id: &str) -> Option<usize> {
    if let Some(index) = hunks
        .iter()
        .enumerate()
        .position(|(index, hunk)| hunk_id(index, hunk) == id)
    {
        return Some(index);
    }
    let wanted = id.split_once('-').map(|(_, hash)| hash)?;
    let mut matching = hunks
        .iter()
        .enumerate()
        .filter(|(_, hunk)| hunk.hash() == wanted)
        .map(|(index, _)| index);
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

/// Apply one hunk to `text`: forward (`'-'` lines become `'+'` lines, to bring a change in) or
/// backward (to take it out). The lines it replaces must be there as the hunk says, at its place
/// or, when other changes moved them, at exactly one other place; otherwise nothing is guessed.
fn swap_hunk(text: &str, hunk: &DiffHunk, forward: bool) -> Result<String, String> {
    if hunk.body.iter().any(|(marker, _)| *marker == '\\') {
        return Err("This change touches the end of the file without a final line break; use the whole-file action.".to_string());
    }
    let (find, put, expected) = if forward {
        (hunk.side('-'), hunk.side('+'), hunk.old_start)
    } else {
        (hunk.side('+'), hunk.side('-'), hunk.new_start)
    };
    let mismatch = if forward {
        "The project's file no longer has the lines this part of the change starts from, so nothing was applied."
    } else {
        "The file no longer matches this part of the change (it was edited since), so nothing was undone."
    };
    if find.is_empty() {
        return Err(mismatch.to_string());
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let matches_at = |at: usize| {
        at + find.len() <= lines.len()
            && find
                .iter()
                .zip(&lines[at..])
                .all(|(want, have)| have.strip_suffix('\n') == Some(want.as_str()))
    };
    let expected = (expected as usize).saturating_sub(1);
    let at = if matches_at(expected) {
        expected
    } else {
        let found: Vec<usize> = (0..lines.len()).filter(|at| matches_at(*at)).collect();
        match found.as_slice() {
            [only] => *only,
            _ => return Err(mismatch.to_string()),
        }
    };
    let mut out = String::with_capacity(text.len());
    for line in &lines[..at] {
        out.push_str(line);
    }
    for line in &put {
        out.push_str(line);
        out.push('\n');
    }
    for line in &lines[at + find.len()..] {
        out.push_str(line);
    }
    Ok(out)
}

/// The diff with one hunk left out.
fn diff_without_hunk(header: &str, hunks: &[DiffHunk], skip: usize) -> String {
    let mut out = header.to_string();
    for (index, hunk) in hunks.iter().enumerate() {
        if index != skip {
            out.push_str(&hunk.raw);
        }
    }
    out
}

#[cfg(test)]
mod hunk_tests {
    use super::*;

    const DIFF: &str = "diff --git a/f.txt b/f.txt\nindex 1..2 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n@@ -20,4 +20,5 @@\n t\n u\n-v\n+V\n+W\n x\n";

    fn base() -> String {
        let mut lines: Vec<String> = (1..=24).map(|n| format!("l{n}")).collect();
        lines[0] = "a".into();
        lines[1] = "B".into();
        lines[2] = "c".into();
        lines[3] = "d".into();
        lines[19] = "t".into();
        lines[20] = "u".into();
        lines[21] = "V".into();
        lines.insert(22, "W".into());
        lines.insert(23, "x".into());
        lines.join("\n") + "\n"
    }

    #[test]
    fn a_diff_splits_into_hunks_with_ranges_and_counts() {
        let (header, hunks) = split_hunks(DIFF);
        assert!(header.starts_with("diff --git") && header.ends_with("+++ b/f.txt\n"));
        assert_eq!(hunks.len(), 2);
        assert_eq!(
            (
                hunks[1].old_start,
                hunks[1].old_lines,
                hunks[1].new_start,
                hunks[1].new_lines
            ),
            (20, 4, 20, 5)
        );
        let infos = hunk_infos(DIFF);
        assert_eq!((infos[0].additions, infos[0].deletions), (1, 1));
        assert_eq!((infos[1].additions, infos[1].deletions), (2, 1));
        assert!(infos[0].hunk_id.starts_with("0-") && infos[1].hunk_id.starts_with("1-"));
        assert_ne!(infos[0].hunk_id[2..], infos[1].hunk_id[2..]);
    }

    #[test]
    fn a_hunk_is_found_by_id_and_by_hash_after_the_index_moved() {
        let (header, hunks) = split_hunks(DIFF);
        let second = hunk_id(1, &hunks[1]);
        assert_eq!(find_hunk(&hunks, &second), Some(1));
        let rest = diff_without_hunk(&header, &hunks, 0);
        let (_, left) = split_hunks(&rest);
        assert_eq!(left.len(), 1);
        assert_eq!(find_hunk(&left, &second), Some(0));
        assert_eq!(find_hunk(&left, "0-deadbeef"), None);
    }

    #[test]
    fn undoing_a_hunk_puts_back_its_old_lines_and_leaves_the_other_edit() {
        let (_, hunks) = split_hunks(DIFF);
        let undone = swap_hunk(&base(), &hunks[0], false).unwrap();
        assert!(undone.starts_with("a\nb\nc\nd\n"));
        assert!(undone.contains("V\nW\nx\n"));
    }

    #[test]
    fn a_hunk_is_found_when_earlier_lines_moved_it() {
        let (_, hunks) = split_hunks(DIFF);
        let moved = format!("new top\n{}", base());
        let undone = swap_hunk(&moved, &hunks[1], false).unwrap();
        assert!(undone.contains("t\nu\nv\nx\n"));
        assert!(!undone.contains("W\n"));
        assert!(undone.starts_with("new top\na\nB\n"));
    }

    #[test]
    fn an_edited_hunk_is_refused_not_guessed() {
        let (_, hunks) = split_hunks(DIFF);
        let by_hand = base().replace("B\n", "B by hand\n");
        assert!(swap_hunk(&by_hand, &hunks[0], false).is_err());
        assert!(swap_hunk("", &hunks[0], false).is_err());
    }

    #[test]
    fn keeping_a_hunk_applies_it_to_the_project_copy_only() {
        let (_, hunks) = split_hunks(DIFF);
        let project = base().replace("B\n", "b\n").replace("V\nW\n", "v\n");
        let kept = swap_hunk(&project, &hunks[0], true).unwrap();
        assert!(kept.starts_with("a\nB\nc\nd\n"));
        assert!(kept.contains("u\nv\nx\n"));
        // The other way round: a hunk that does not start from these lines is refused.
        assert!(swap_hunk(&kept, &hunks[0], true).is_err());
    }

    #[test]
    fn a_hunk_at_the_end_without_a_final_line_break_is_refused() {
        let diff = "@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file\n";
        let (_, hunks) = split_hunks(diff);
        assert!(swap_hunk("b", &hunks[0], false).is_err());
    }
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

async fn post_proposal_handler(
    State(state): State<DaemonState>,
    Json(body): Json<ProposalDecisionRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    let response = store.decide_proposal(body);
    (StatusCode::OK, Json(response))
}

async fn post_hunk_handler(
    State(state): State<DaemonState>,
    Json(body): Json<HunkDecisionRequest>,
) -> impl IntoResponse {
    let store = run_changes_store(&state);
    let (status, response) = store.decide_hunk(body);
    (status, Json(response))
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
/// `<state root>/run_changes`.
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

/// Resolve the run_changes root with the same state-root rule as the rest of the app: the AppRoot
/// state root. With `host_options::state_store_paths_from_environment` on, the host's
/// `RUN_CHANGES_DIR` variable remains an explicit override.
fn resolve_run_changes_root() -> PathBuf {
    if let Some(dir) =
        crate::api_stores::state_store_override(crate::api_stores::StateStoreKey::RunChangesDir)
    {
        return dir;
    }
    dirs_default_home().join("run_changes")
}

/// The body that marks an undo as "delete the file" or "rename it back": `__<PREFIX>_<KIND>__`
/// under the host's environment prefix, the spelling the host has always stored. It is computed
/// at use, after the host has named itself.
fn undo_marker(kind: &str) -> String {
    format!("__{}_{kind}__", rightkit_agent_config::env_prefix())
}

fn dirs_default_home() -> PathBuf {
    if rightkit_agent_config::state_root_is_isolated() {
        if let Some(root) = rightkit_agent_config::state_root() {
            return root;
        }
    }
    {
        rightkit_agent_config::state_root_required()
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
