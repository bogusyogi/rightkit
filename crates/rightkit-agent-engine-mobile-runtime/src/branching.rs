//! Conversation branching, labels, checkpoints, & receipt-based rollback
//! (ENG-69; product atom 18: fork/handoff/rollback).
//!
//! # Model
//!
//! A [`Branch`] is a labelled snapshot of a conversation at one durable
//! journal sequence: it freezes the caller-supplied `checkpoint_json` blob
//! (the same opaque value [`crate::MobileRuntime::checkpoint_json`] already
//! produces and [`crate::MobileConfig::checkpoint_json`] already consumes —
//! see `lib.rs`) alongside the [`crate::journal::Journal`] sequence it was
//! taken at. Creating a branch never mutates the parent session's own
//! journal, checkpoint, or transcript: it only appends a new record to a
//! side file keyed by the parent session id, so branching is always
//! non-destructive to the line of work the user was already on (ENG-69:
//! "switch branches without losing current work").
//!
//! Two related but distinct operations sit on this model:
//!
//! - [`create_branch`] labels a point in the *same* conversation lineage so
//!   the user can return to it later (ENG-69's "label meaningful states").
//!   [`abandon_branch`] records a summary for a branch the user is stepping
//!   away from, per ENG-69's "summarize abandoned branches before
//!   returning" — nothing here deletes it; a branch is either `Active` or
//!   `Abandoned { summary }`, never removed, so history is never silently
//!   lost.
//! - [`fork_session`] mints a branch whose `id` is intended for use as a
//!   *new, independent* session id (product atom 18's "fork"): the caller
//!   starts a second [`crate::MobileRuntime`] with that id and this branch's
//!   `checkpoint_json`, giving the phone two live conversations that share a
//!   documented ancestor but never share storage.
//!
//! # Why not git worktrees
//!
//! Product atom 18 is explicit that iOS rollback is receipt-based, not
//! git-worktree-based (unlike the desktop's `src-tauri` worktree commands).
//! A phone has one working tree and no facility for the throwaway
//! filesystem clones a worktree needs, so "rollback" here means replaying
//! the durable effect ledger backwards, not checking out a different tree.
//!
//! # Rollback safety
//!
//! [`plan_rollback`] and [`execute_rollback`] never guess. They walk the
//! journal's `EffectApplied` records strictly newer than the checkpoint
//! being restored to, in reverse chronological order (last effect first —
//! reversing forward order could ask a later effect to undo before an
//! effect it depended on has been undone). Before reversing anything,
//! [`plan_rollback`] asks the caller-supplied `is_reversible` predicate
//! about *every* step; if even one cannot be safely reversed, it returns
//! [`RollbackError::Irreversible`] and [`execute_rollback`] performs no
//! mutation at all. This is deliberately all-or-nothing: a rollback that
//! silently skipped the one effect it couldn't undo (e.g. a pushed git
//! commit, a filed GitHub PR, a sent network request) would leave the
//! workspace in a state that matches no checkpoint the user ever saw, which
//! is worse than refusing outright.
//!
//! # Integration gaps (outside this lane's file allowlist)
//!
//! - `engine/crates/mobile_runtime/src/lib.rs` needs `pub mod branching;`
//!   before any other crate or `ffi.rs` entry point can reach these types.
//! - `ffi.rs` needs C ABI entry points wrapping [`create_branch`],
//!   [`list_branches`], [`abandon_branch`], [`fork_session`],
//!   [`plan_rollback`], and [`execute_rollback`] — none of that FFI
//!   plumbing exists yet, and `ffi.rs` is not in this lane's allowlist.
//! - The `is_reversible` predicate this module requires the caller to
//!   supply has no production implementation yet. The natural source is
//!   `rightkit_agent_tools::coding_tools::effect_receipt_store` (an in-process, non-durable
//!   per-session ledger) merged with tool-specific knowledge of what each
//!   `effect_id` prefix means (e.g. a `write_file`/`edit_file` effect is
//!   reversible via `undo_edit`'s snapshot stack; a `git push` or GitHub PR
//!   creation is not). Wiring that classifier is `engine_session_controls.rs`
//!   or `ffi.rs` work, both outside this lane.
//! - `checkpoint_json` production/validation stays owned by
//!   `MobileCheckpoint` in `lib.rs` (private to that module); this module
//!   only stores and returns the blob verbatim, exactly as
//!   `MobileConfig::checkpoint_json` already does at the FFI boundary.
//!
//! Wanted verification (not run in this lane): `rustright cargo test
//! --manifest-path engine/Cargo.toml -p host_mobile-runtime branching`.

use crate::journal::{Journal, JournalError, JournalEventKind};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// On-disk format version for the branch store file. Bumped only for a
/// backward-incompatible record change.
pub const BRANCH_STORE_FORMAT_VERSION: u32 = 1;

/// Whether a branch is still being worked on or was set aside.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum BranchStatus {
    Active,
    /// ENG-69: "summarize abandoned branches before returning". `summary` is
    /// caller-supplied prose, never inferred here.
    Abandoned {
        summary: String,
    },
}

/// One labelled fork point in a conversation's history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Branch {
    /// Unique within the parent session's lineage. For a branch created via
    /// [`fork_session`], this value is what the caller should use as the
    /// new, independent session id.
    pub id: String,
    /// Session id of the conversation this branch was taken from. Never the
    /// branch's own id, even for a fork.
    pub parent_session_id: String,
    /// User-facing label. Never empty after [`create_branch`]/
    /// [`fork_session`] validation.
    pub label: String,
    pub created_at_ns: u128,
    /// The parent journal's [`Journal::last_sequence`] at the moment this
    /// branch was taken. Rollback and future replay both anchor on this.
    pub fork_sequence: u64,
    /// Frozen `checkpoint_json` blob (opaque to this module; see the module
    /// doc comment) a host can hand back to [`crate::MobileConfig`] to
    /// resume exactly this point.
    pub checkpoint_json: String,
    pub status: BranchStatus,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchStoreFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    next_sequence: u64,
    #[serde(default)]
    branches: Vec<Branch>,
}

fn default_version() -> u32 {
    BRANCH_STORE_FORMAT_VERSION
}

#[derive(Debug)]
pub enum BranchError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
    /// The store file names a format version newer than this crate
    /// understands.
    UnsupportedVersion(u32),
    /// `label` was empty or all-whitespace.
    EmptyLabel,
    InvalidCheckpoint(String),
    /// No branch with the given id exists for the given parent session.
    NotFound(String),
}

impl std::fmt::Display for BranchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "branch store io error: {error}"),
            Self::Serialize(error) => write!(f, "branch store serialize error: {error}"),
            Self::UnsupportedVersion(version) => {
                write!(f, "branch store version {version} is unsupported")
            }
            Self::EmptyLabel => write!(f, "branch label must not be empty"),
            Self::InvalidCheckpoint(reason) => write!(f, "{reason}"),
            Self::NotFound(id) => write!(f, "no branch with id '{id}'"),
        }
    }
}

impl std::error::Error for BranchError {}

impl From<std::io::Error> for BranchError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for BranchError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialize(error)
    }
}

fn branch_store_path(storage_dir: &Path, parent_session_id: &str) -> PathBuf {
    storage_dir.join(format!("{parent_session_id}.branches.json"))
}

fn read_store(storage_dir: &Path, parent_session_id: &str) -> Result<BranchStoreFile, BranchError> {
    let path = branch_store_path(storage_dir, parent_session_id);
    if !path.exists() {
        return Ok(BranchStoreFile {
            version: BRANCH_STORE_FORMAT_VERSION,
            next_sequence: 0,
            branches: Vec::new(),
        });
    }
    let raw = fs::read_to_string(&path)?;
    if raw.trim().is_empty() {
        return Ok(BranchStoreFile {
            version: BRANCH_STORE_FORMAT_VERSION,
            next_sequence: 0,
            branches: Vec::new(),
        });
    }
    let store: BranchStoreFile = serde_json::from_str(&raw)?;
    if store.version > BRANCH_STORE_FORMAT_VERSION {
        return Err(BranchError::UnsupportedVersion(store.version));
    }
    Ok(store)
}

fn write_store(
    storage_dir: &Path,
    parent_session_id: &str,
    store: &BranchStoreFile,
) -> Result<(), BranchError> {
    fs::create_dir_all(storage_dir)?;
    let path = branch_store_path(storage_dir, parent_session_id);
    let body = serde_json::to_string_pretty(store)?;
    rightkit_fs::atomic_write(&path, body.as_bytes())
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(())
}

fn now_ns() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn mint_branch(
    storage_dir: &Path,
    parent_session_id: &str,
    label: &str,
    fork_sequence: u64,
    checkpoint_json: String,
) -> Result<Branch, BranchError> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return Err(BranchError::EmptyLabel);
    }
    let mut store = read_store(storage_dir, parent_session_id)?;
    let sequence = store.next_sequence;
    store.next_sequence += 1;
    let branch = Branch {
        id: format!("{parent_session_id}::branch::{sequence}"),
        parent_session_id: parent_session_id.to_string(),
        label: trimmed.to_string(),
        created_at_ns: now_ns(),
        fork_sequence,
        checkpoint_json,
        status: BranchStatus::Active,
    };
    store.branches.push(branch.clone());
    write_store(storage_dir, parent_session_id, &store)?;
    Ok(branch)
}

/// Label the conversation's current state as a branch point without
/// mutating the parent session's own journal or checkpoint (ENG-69). Safe
/// to call any number of times from the same fork point; every call mints a
/// distinct branch id.
pub fn create_branch(
    storage_dir: &Path,
    parent_session_id: &str,
    label: &str,
    fork_sequence: u64,
    checkpoint_json: String,
) -> Result<Branch, BranchError> {
    mint_branch(
        storage_dir,
        parent_session_id,
        label,
        fork_sequence,
        checkpoint_json,
    )
}

/// Mint a branch intended for use as a brand-new, independently running
/// session id (product atom 18's "fork"). Identical bookkeeping to
/// [`create_branch`]; kept as a distinct entry point because callers use it
/// at a different moment (spinning up a second live conversation) and the
/// distinction is worth keeping legible at call sites and in tests.
pub fn fork_session(
    storage_dir: &Path,
    parent_session_id: &str,
    fork_sequence: u64,
    checkpoint_json: String,
    label: &str,
) -> Result<Branch, BranchError> {
    mint_branch(
        storage_dir,
        parent_session_id,
        label,
        fork_sequence,
        checkpoint_json,
    )
}

/// Fork a production mobile checkpoint, resetting its event cursor & binding
/// it to the new session before persisting anything. Parent remains unchanged.
pub fn fork_mobile_session(
    storage_dir: &Path,
    parent_session_id: &str,
    label: &str,
    checkpoint_json: &str,
    keep_user_turns: Option<usize>,
) -> Result<Branch, BranchError> {
    if label.trim().is_empty() {
        return Err(BranchError::EmptyLabel);
    }
    if parent_session_id.contains('/')
        || parent_session_id.contains('\\')
        || parent_session_id == ".."
    {
        return Err(BranchError::InvalidCheckpoint(
            "Invalid parent conversation identifier.".into(),
        ));
    }
    let mut store = read_store(storage_dir, parent_session_id)?;
    let id = format!("{parent_session_id}::branch::{}", store.next_sequence);
    let snapshot =
        crate::fork_checkpoint_json(checkpoint_json, parent_session_id, &id, keep_user_turns)
            .map_err(BranchError::InvalidCheckpoint)?;
    let branch = Branch {
        id,
        parent_session_id: parent_session_id.to_owned(),
        label: label.trim().to_owned(),
        created_at_ns: now_ns(),
        fork_sequence: 0,
        checkpoint_json: snapshot,
        status: BranchStatus::Active,
    };
    store.next_sequence += 1;
    store.branches.push(branch.clone());
    write_store(storage_dir, parent_session_id, &store)?;
    Ok(branch)
}

/// All branches ever taken from `parent_session_id`'s lineage, oldest
/// first. Never mutates the store.
pub fn list_branches(
    storage_dir: &Path,
    parent_session_id: &str,
) -> Result<Vec<Branch>, BranchError> {
    Ok(read_store(storage_dir, parent_session_id)?.branches)
}

/// Mark a branch abandoned with a caller-supplied summary (ENG-69:
/// "summarize abandoned branches before returning"). The branch record is
/// kept, not deleted — only its `status` changes — so listing history never
/// loses it.
pub fn abandon_branch(
    storage_dir: &Path,
    parent_session_id: &str,
    branch_id: &str,
    summary: impl Into<String>,
) -> Result<Branch, BranchError> {
    let mut store = read_store(storage_dir, parent_session_id)?;
    let branch = store
        .branches
        .iter_mut()
        .find(|branch| branch.id == branch_id)
        .ok_or_else(|| BranchError::NotFound(branch_id.to_string()))?;
    branch.status = BranchStatus::Abandoned {
        summary: summary.into(),
    };
    let updated = branch.clone();
    write_store(storage_dir, parent_session_id, &store)?;
    Ok(updated)
}

// ---------------------------------------------------------------------
// Rollback
// ---------------------------------------------------------------------

/// One durable effect that ran after the checkpoint being rolled back to,
/// in the order the journal recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackStep {
    pub sequence: u64,
    pub effect_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackError {
    Journal(String),
    /// `step` cannot be safely reversed. Returned before any reversal runs
    /// (see the module doc comment): rollback is all-or-nothing.
    Irreversible {
        step: RollbackStep,
    },
    /// The caller's reversal mechanism itself failed while undoing `step`.
    /// Any steps reversed before this one in the same call have already
    /// happened; this is a runtime failure, not a policy refusal.
    ReversalFailed {
        step: RollbackStep,
        reason: String,
    },
}

impl std::fmt::Display for RollbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Journal(message) => write!(f, "rollback journal error: {message}"),
            Self::Irreversible { step } => write!(
                f,
                "refusing rollback: effect '{}' at sequence {} is not safely reversible",
                step.effect_id, step.sequence
            ),
            Self::ReversalFailed { step, reason } => write!(
                f,
                "rollback failed reversing effect '{}' at sequence {}: {reason}",
                step.effect_id, step.sequence
            ),
        }
    }
}

impl std::error::Error for RollbackError {}

impl From<JournalError> for RollbackError {
    fn from(error: JournalError) -> Self {
        Self::Journal(error.to_string())
    }
}

/// The ordered steps a rollback to `since_sequence` would have to reverse,
/// most-recent-first (the order [`execute_rollback`] actually applies
/// them in).
///
/// Every `EffectApplied` journal record strictly newer than
/// `since_sequence` is included. Nothing is filtered out here — filtering
/// is exactly the "silently skipping" behaviour the rollback safety rule
/// forbids; a step this function omitted would never be checked for
/// reversibility at all.
pub fn effects_since(
    journal: &Journal,
    since_sequence: u64,
) -> Result<Vec<RollbackStep>, RollbackError> {
    let events = journal.replay(since_sequence)?;
    let mut steps: Vec<RollbackStep> = events
        .into_iter()
        .filter_map(|event| match event.kind {
            JournalEventKind::EffectApplied { effect_id } => Some(RollbackStep {
                sequence: event.sequence,
                effect_id,
            }),
            _ => None,
        })
        .collect();
    // Most-recent-first: reversing forward order could ask a later effect
    // to undo before an effect it depended on has been undone.
    steps.sort_by_key(|b| std::cmp::Reverse(b.sequence));
    Ok(steps)
}

/// Validate that every effect since `since_sequence` can be safely
/// reversed, without reversing anything. Returns the steps in the order
/// [`execute_rollback`] would apply them (most-recent-first) on success,
/// or the first irreversible step encountered on failure.
///
/// `is_reversible` is the caller's policy — this module intentionally has
/// no built-in notion of which `effect_id`s are safe, since `effect_id` is
/// an opaque idempotency key minted by whichever tool ran (see
/// [`JournalEventKind::EffectApplied`]'s doc comment); see the module doc
/// comment's "Integration gaps" section for where a real classifier needs
/// to be wired in.
pub fn plan_rollback(
    journal: &Journal,
    since_sequence: u64,
    mut is_reversible: impl FnMut(&str) -> bool,
) -> Result<Vec<RollbackStep>, RollbackError> {
    let steps = effects_since(journal, since_sequence)?;
    for step in &steps {
        if !is_reversible(&step.effect_id) {
            return Err(RollbackError::Irreversible { step: step.clone() });
        }
    }
    Ok(steps)
}

/// Roll back to `since_sequence` by reversing every effect applied after
/// it, most-recent-first. Calls [`plan_rollback`] first, so this performs
/// no mutation at all — does not even invoke `reverse` once — unless every
/// step is reversible.
///
/// `reverse` is the caller's mechanism for actually undoing one effect
/// (e.g. dispatching to `undo_edit` for a file effect). If `reverse` fails
/// partway through, the steps already reversed stay reversed; this
/// function does not attempt to re-apply them, since a rollback mechanism
/// that also had to roll back its own partial rollback would need the same
/// safety guarantee applied to itself, recursively.
pub fn execute_rollback(
    journal: &Journal,
    since_sequence: u64,
    is_reversible: impl FnMut(&str) -> bool,
    mut reverse: impl FnMut(&str) -> Result<(), String>,
) -> Result<Vec<RollbackStep>, RollbackError> {
    let steps = plan_rollback(journal, since_sequence, is_reversible)?;
    let mut applied = Vec::with_capacity(steps.len());
    for step in steps {
        if let Err(reason) = reverse(&step.effect_id) {
            return Err(RollbackError::ReversalFailed { step, reason });
        }
        applied.push(step);
    }
    Ok(applied)
}
