//! Per-session compaction coordinator (CR-17).
//!
//! Fixes a defect where production compaction constructed a FRESH
//! `Compactor` on every single attempt (see the old body of
//! `engine_session_controls::compact_context`): `Compactor::prefired` and
//! `Compactor::breaker` are per-instance state, so a compactor thrown away
//! after one call can never accumulate a background prefire result or a
//! multi-attempt consecutive-failure count. `store_prefired`'s only call
//! sites were `#[tokio::test]` functions in `compactor_authoritative.rs`
//! (constructing their own long-lived `Compactor` directly) -- no real
//! session ever reached that path.
//!
//! The fix is a process-wide registry of one [`Compactor`] per live
//! `session_id`, handed out as an `Arc` so every call into
//! `compact_context` / `compact_context_now` for a given session shares the
//! same prefire cache, breaker history, and surface-generation bookkeeping.
//! The registry is intentionally NOT a field on `SessionEngine` --
//! `engine.rs` (where `SessionEngine` is defined) is outside this fix's file
//! allowlist -- so it lives here, keyed by the session id string that
//! `SessionEngine` already carries and passes to every compaction call.
//!
//! Entries are removed via [`retire_session`] when a session ends so a long
//! -running daemon does not accumulate one `Compactor` per session forever.

use crate::compactor::Compactor;
use crate::pinned_context::PinnedContextRegistry;
use crate::transcript::Transcript;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

fn registry() -> &'static Mutex<HashMap<String, Arc<Compactor>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Compactor>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// ENG-104: same "not a field on `SessionEngine`" rationale as the
/// `Compactor` registry above -- `engine.rs` is outside this fix's file
/// allowlist, so the per-session [`PinnedContextRegistry`] lives here too,
/// keyed by the same `session_id` string.
fn pinned_registry() -> &'static Mutex<HashMap<String, PinnedContextRegistry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, PinnedContextRegistry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Apply this session's pins to `transcript` (setting the `pinned` bit
/// `compactor_preservation` already honours) immediately before compaction
/// runs. Callers should invoke this as the last step before constructing the
/// `Compactor` input in `compact_context` / `compact_context_now`
/// (`engine_session_controls.rs`).
///
/// GAP (ENG-104, reported per Lane D2 instructions rather than faked): this
/// function is unreachable in practice today. There is no existing call
/// site that can invoke it from here, and no FFI entry point yet writes into
/// `pinned_registry()` -- Lane D1's planned pin/unpin FFI wrappers
/// (`pinned_context` in the D1 list) have no session-scoped store to target
/// yet either. Wiring this fully requires one of:
///   1. `engine_session_controls.rs` (outside this lane's allowlist) calling
///      `apply_pinned_context_for` here before it builds the `Compactor`
///      input in `compact_context`/`compact_context_now`, AND
///   2. the pin/unpin FFI surface writing into `pinned_registry()` (via a
///      new accessor here) instead of a throwaway `PinnedContextRegistry`.
/// Until both land, calling `pin`/`unpin` has no effect on real compaction.
pub fn apply_pinned_context_for(session_id: &str, transcript: &mut Transcript) {
    let guard = pinned_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(pins) = guard.get(session_id) {
        let mut messages = transcript.messages().to_vec();
        pins.apply_to(session_id, &mut messages);
        transcript.replace(messages);
    }
}

/// Drop the persistent pin registry for a session that has ended. Mirrors
/// `retire_session` for the `Compactor` registry; safe to call even if no
/// pins were ever registered for `session_id`.
pub fn retire_pinned_session(session_id: &str) {
    let mut guard = pinned_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.remove(session_id);
}

/// Return the persistent [`Compactor`] for `session_id`, constructing one via
/// `build` on first access. `build` is only invoked the first time a given
/// session is seen; every subsequent call for the same `session_id` returns
/// the SAME `Arc<Compactor>` instance, so its `prefired` cache and `breaker`
/// consecutive-failure counter persist across turns -- the whole point of
/// this module.
pub fn coordinator_for(session_id: &str, build: impl FnOnce() -> Compactor) -> Arc<Compactor> {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = guard.get(session_id) {
        return existing.clone();
    }
    let compactor = Arc::new(build());
    guard.insert(session_id.to_string(), compactor.clone());
    compactor
}

/// Drop the persistent compactor for a session that has ended (deleted,
/// dormant-and-archived, or otherwise no longer live). Safe to call even if
/// no coordinator was ever created for `session_id`.
pub fn retire_session(session_id: &str) {
    let mut guard = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.remove(session_id);
}
