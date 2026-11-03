//! Bounded, per-session store of mutation "effect receipts" — the durable
//! record ENG-18 requires each write/edit tool to leave behind: exact
//! path(s) touched, a diff/summary of the change, and the result. This is
//! the conflict ledger `undo_edit` consults so undo can be rejected once a
//! later mutation has touched the same path (ENG-18: "supports bounded undo
//! before later conflicting effects").
//!
//! This is deliberately independent of `rightkit_agent_engine_history::HistoryStore`
//! (which already gives `undo_edit` its byte-snapshot restore path). The
//! history store answers "what were the previous bytes?"; this store
//! answers "has anything happened to this path since the edit I'm about to
//! undo?" — a question the snapshot stack alone cannot answer once more
//! than one edit has landed on a path.
//!
//! Session-scoping follows the existing pattern in
//! `context.rs::cloudflare_credential_registry` (a process-wide,
//! session-keyed `Mutex<HashMap<..>>>` cell) rather than adding a field to
//! `ToolContext`, because `context.rs` is outside this lane's exclusive
//! file allowlist. `ToolContext::session_id` (already a public field every
//! tool receives) is enough to key into this registry.
//!
//! NOTE (integration): this module must be declared with `pub mod
//! effect_receipt_store;` in `engine/crates/tools/src/lib.rs` before
//! `write_file.rs`, `edit_file.rs`, `multi_edit.rs`, and `undo_edit.rs` can
//! reference `crate::coding_tools::effect_receipt_store::*`. That line is outside this
//! lane's exclusive file allowlist; see the dispatch report for this lane.
//!
//! Wanted verification (not run in this lane): `rustright cargo test
//! --manifest-path engine/Cargo.toml -p host_tools effect_receipt_store`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Outcome of the mutation the receipt describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectResult {
    Success,
    Error(String),
}

/// One durable record of a mutation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectReceipt {
    /// Monotonically increasing id, unique within a session. Ordering by id
    /// is equivalent to ordering by time, and is what `undo_edit` uses to
    /// decide whether a "later" conflicting effect exists.
    pub id: u64,
    pub paths: Vec<PathBuf>,
    /// A diff or a before/after summary of the change (tool-supplied; this
    /// store does not compute diffs itself).
    pub summary: String,
    pub result: EffectResult,
    pub recorded_at_ns: u128,
}

impl EffectReceipt {
    fn touches(&self, path: &Path) -> bool {
        self.paths.iter().any(|touched| touched == path)
    }
}

/// Maximum receipts retained per session. Bounded per ENG-18 ("supports
/// bounded undo") — old receipts age out on a FIFO basis once the cap is
/// exceeded, mirroring the count-based bound already used elsewhere in this
/// crate's session-scoped state (see `output_compaction`'s retained-turn
/// cap) rather than an unbounded log.
pub const MAX_RECEIPTS_PER_SESSION: usize = 500;

#[derive(Default)]
struct SessionLedger {
    receipts: VecDeque<EffectReceipt>,
    next_id: u64,
}

fn registry() -> &'static Mutex<HashMap<String, SessionLedger>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, SessionLedger>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record one mutation attempt for `session_id` and return its receipt id.
/// The id is what a later `undo_edit` call must pass to
/// [`has_conflicting_effect_after`] to check whether anything else touched
/// the same path since.
pub fn record_effect(
    session_id: &str,
    paths: Vec<PathBuf>,
    summary: impl Into<String>,
    result: EffectResult,
) -> u64 {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let ledger = guard.entry(session_id.to_string()).or_default();
    let id = ledger.next_id;
    ledger.next_id += 1;
    let recorded_at_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    ledger.receipts.push_back(EffectReceipt {
        id,
        paths,
        summary: summary.into(),
        result,
        recorded_at_ns,
    });
    while ledger.receipts.len() > MAX_RECEIPTS_PER_SESSION {
        ledger.receipts.pop_front();
    }
    id
}

/// The most recent receipt touching `path` for `session_id`, if any.
pub fn latest_effect_for_path(session_id: &str, path: &Path) -> Option<EffectReceipt> {
    let guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(session_id).and_then(|ledger| {
        ledger
            .receipts
            .iter()
            .rev()
            .find(|r| r.touches(path))
            .cloned()
    })
}

/// `true` when some receipt touching `path` was recorded strictly after
/// `after_id` — i.e. a later, potentially conflicting effect landed on this
/// path since the receipt being undone. `undo_edit` uses this to refuse an
/// undo that would silently discard that later effect.
pub fn has_conflicting_effect_after(session_id: &str, path: &Path, after_id: u64) -> bool {
    let guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .get(session_id)
        .map(|ledger| {
            ledger
                .receipts
                .iter()
                .any(|r| r.id > after_id && r.touches(path))
        })
        .unwrap_or(false)
}

/// Drop all receipts for `session_id`. Callers should invoke this alongside
/// other per-session cleanup (see `retire_cloudflare_credential` in
/// `context.rs` for the established pattern) so a long-running daemon does
/// not accumulate one ledger per session forever.
pub fn retire_session_effects(session_id: &str) {
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
}
