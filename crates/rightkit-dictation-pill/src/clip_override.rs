//! The "override transcript" a clip card holds: the one transcript that selects the single card
//! (a captured transcript shown on its own) instead of the list of the newest history rows.
//!
//! The slot is this module's one process-wide `static`; these functions are the only code that
//! reads or writes it, so callers pass no slot. Moved from HeardRight `pill_overlays.rs`,
//! `dictation_commands.rs` and `runtime_host.rs`. The host supplies the recent-history reader and
//! the id of the single card; nothing here knows a product, a path or a history format.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

/// The slot and what is known about it, under one lock, so a restore can tell whether the value it
/// would overwrite is still the one it set, and a failed show can only ever restore a value that
/// was committed: the provisional text of another show that has not finished is never the
/// rollback baseline.
struct State {
    /// The override.
    value: Option<String>,
    /// Counts every write of `value`.
    generation: u64,
    /// The last value that is not provisional: set by `set` or `clear`, or kept by a
    /// `set_while_showing` whose `show` succeeded. This is the only value a failed show restores.
    committed: Option<String>,
    /// The generation at which `committed` was written. A show that started later than this and
    /// succeeded replaces `committed`; one that started earlier does not (a newer value wins).
    committed_at: u64,
    /// Refresh tokens issued by snapshots of this committed card. Settlement advances the
    /// ordering counter without changing card identity, so these tokens name the same baseline.
    committed_views: BTreeSet<u64>,
    /// The generation of the latest explicit `set` or `clear`.
    explicit_at: u64,
    /// The generation of the latest write that starts a card or ends one (`commit`, `clear_commit`,
    /// `clear_if_current`, the start of a `set_while_showing`). The settle of overlapping shows
    /// moves `generation` but not this: it is the identity of "the newest card request".
    latest: u64,
    /// The generation of the latest provisional write (`set_while_showing`).
    provisional_at: u64,
    /// The `set_while_showing` calls whose `show` has not finished, by the generation each took;
    /// the value is whether the card was dismissed (retired) before its producer finished. A
    /// producer removes only its own entry, and a retired entry never commits.
    pending: BTreeMap<u64, bool>,
}

static STATE: Mutex<State> = Mutex::new(State {
    value: None,
    generation: 0,
    committed: None,
    committed_at: 0,
    committed_views: BTreeSet::new(),
    explicit_at: 0,
    latest: 0,
    provisional_at: 0,
    pending: BTreeMap::new(),
});

fn lock() -> Result<MutexGuard<'static, State>, String> {
    STATE
        .lock()
        .map_err(|err| format!("clip override lock poisoned: {err}"))
}

/// The lock for an explicit clear, which must work whatever happened to the slot: a poisoned lock
/// is recovered (`into_inner`), its state is reset (no value, no baseline, no provisional show,
/// no pending show; the generation counter is kept so it stays monotonic) and the poison is
/// cleared. The state a panicking writer left half-written is never trusted.
fn lock_for_clear() -> MutexGuard<'static, State> {
    match STATE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            let generation = guard.generation;
            *guard = State {
                value: None,
                generation,
                committed: None,
                committed_at: generation,
                committed_views: BTreeSet::new(),
                explicit_at: generation,
                latest: generation,
                provisional_at: 0,
                // A producer that started before the recovery finds no entry and does nothing: it
                // can neither commit nor retire anything of a show that started after it.
                pending: BTreeMap::new(),
            };
            STATE.clear_poison();
            guard
        }
    }
}

/// The current override, if one is set.
pub fn get() -> Result<Option<String>, String> {
    Ok(lock()?.value.clone())
}

/// Whether an override is set. A poisoned lock reads as not set (the same answer a reader that
/// only wants the mode gets).
pub fn is_set() -> bool {
    STATE
        .lock()
        .map(|state| state.value.is_some())
        .unwrap_or(false)
}

/// The override and the generation of the latest write of the slot, read under one lock. A reader
/// that needs the override and anything derived from it (the card's mode, its rows) takes both from
/// this one value, so a `set` or `clear` between two separate reads cannot give it a payload from
/// one override and a mode from another. The generation is the one to pass to
/// [`is_superseded`] when the result is applied. A settled snapshot also registers that token
/// as a view of the current committed card, so its eventual dismissal resolves the same identity
/// even when settlement advanced the ordering counter. These aliases end when that baseline is
/// replaced or explicitly cleared.
pub fn snapshot() -> Result<(Option<String>, u64), String> {
    let mut state = lock()?;
    if state.pending.is_empty() {
        let generation = state.generation;
        state.committed_views.insert(generation);
    }
    Ok((state.value.clone(), state.generation))
}

/// Set (or, with `None`, clear) the override. The value is committed: it becomes the value a later
/// failed show restores.
pub fn set(text: Option<String>) -> Result<(), String> {
    commit(text).map(|_| ())
}

/// [`set`], returning the generation of the write: the generation of the show this override
/// selects. A show applied to the view carries it, and the view's apply step drops the show once
/// [`is_superseded`] says a later write exists (see `native_view`).
pub fn commit(text: Option<String>) -> Result<u64, String> {
    let mut state = lock()?;
    state.value = text.clone();
    state.generation = state.generation.wrapping_add(1);
    state.committed = text;
    state.committed_at = state.generation;
    state.committed_views.clear();
    state.explicit_at = state.generation;
    state.latest = state.generation;
    Ok(state.generation)
}

/// Clear the override (also when the lock was poisoned: see [`clear_commit`]).
pub fn clear() {
    let _ = clear_commit();
}

/// Clear the override and return the generation of the write (the generation a close that follows
/// the clear carries). It cannot fail: a poisoned lock is recovered and the slot reset (see
/// `lock_for_clear`), and the generation is allocated under the same lock, fresh (greater than
/// every generation handed out before), so a queued show that carries an earlier generation, equal
/// ones included, is older than this clear.
pub fn clear_commit() -> u64 {
    let mut state = lock_for_clear();
    state.value = None;
    state.generation = state.generation.wrapping_add(1);
    state.committed = None;
    state.committed_at = state.generation;
    state.committed_views.clear();
    state.explicit_at = state.generation;
    state.latest = state.generation;
    state.generation
}

/// The view dismissed the card that was shown for `generation` (the generation of its show, which
/// the view carried back): clear the override only when that card is still the newest request
/// (`latest == generation`, or a settled snapshot token of that same committed card), compared
/// and cleared under one lock. `Ok(Some(g))` is the generation
/// of the clear (a close for it carries `g`); `Ok(None)` means a newer show or clear exists, so the
/// dismissal belongs to a card that has since been replaced and changes nothing. No generation is
/// allocated unless the identity matched.
///
/// Whatever the identity says, a dismissed card never commits: when `generation` is a pending
/// `set_while_showing` (its producer has not finished) the entry is marked retired, even if a newer
/// show started meanwhile, so that producer's `Finish` cannot make the dismissed card the baseline
/// a later failure restores.
///
/// A second exception touches the slot without clearing it: the dismissed card is the committed
/// baseline (`committed_at == generation`) while a `set_while_showing` for another card is still
/// pending. That provisional entry is left as it is (value, identity, generation), but the
/// baseline is retired (no value), so if the provisional show fails the slot rolls back to cleared
/// and the dismissed card cannot come back. `Ok(None)` is returned in both: nothing the caller
/// holds for the newer card is released.
pub fn clear_if_current(generation: u64) -> Result<Option<u64>, String> {
    let mut state = lock()?;
    if let Some(retired) = state.pending.get_mut(&generation) {
        *retired = true;
    }
    let committed_view =
        state.committed_at == generation || state.committed_views.contains(&generation);
    if state.latest != generation && !(state.pending.is_empty() && committed_view) {
        if !state.pending.is_empty() && committed_view && state.committed.is_some() {
            state.committed = None;
            state.committed_views.clear();
        }
        return Ok(None);
    }
    state.value = None;
    state.generation = state.generation.wrapping_add(1);
    state.committed = None;
    state.committed_at = state.generation;
    state.committed_views.clear();
    state.explicit_at = state.generation;
    state.latest = state.generation;
    Ok(Some(state.generation))
}

/// Whether a write of the slot (`set`, `clear`, or the settle of a `set_while_showing`) happened
/// after `generation`, so a show or close that carries `generation` is older than the committed
/// override and must not reach the view: a card queued for override A must never replace the card
/// of override B that was committed first. A poisoned lock still answers (the generation is a
/// plain counter).
pub fn is_superseded(generation: u64) -> bool {
    let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    generation < state.generation
}

/// Ends one `set_while_showing` (also when `show` unwinds, which counts as a failure).
struct Finish {
    ours: u64,
    text: Option<String>,
    shown: bool,
}

impl Drop for Finish {
    fn drop(&mut self) {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        // Only this call's own entry: none means a recovery from poison emptied `pending` after
        // this call started, so nothing here belongs to the slot any more and nothing changes.
        let Some(retired) = state.pending.remove(&self.ours) else {
            return;
        };
        // A card dismissed before its producer finished (`retired`) never commits.
        if self.shown && !retired && self.ours > state.committed_at {
            state.committed = self.text.take();
            state.committed_at = self.ours;
            state.committed_views.clear();
        }
        // The last show to finish settles the slot: when no `set` or `clear` came after the latest
        // provisional write, the slot still holds provisional text, and the committed value
        // replaces it (so a failed show restores what was there before, and several overlapping
        // failed shows end on the last committed value, never on a failed one's text).
        // The baseline comes back with its identity: `latest` is the generation the baseline was
        // committed at, so the card that shows it (or the cleared state) is dismissed by that
        // generation and not by the failed show's.
        if state.pending.is_empty() && state.provisional_at > state.explicit_at {
            state.value = state.committed.clone();
            state.generation = state.generation.wrapping_add(1);
            state.latest = state.committed_at;
            state.provisional_at = 0;
        }
    }
}

/// Set the override, run `show`, and keep the override only when `show` succeeded: otherwise it
/// would describe a card that never showed, so the last committed value is put back. The override
/// is set before `show` runs because it selects the card `show` creates, and that text is
/// provisional until `show` returns `Ok`: it is never the baseline another show's failure restores.
/// Each call owns a generation taken under the lock; a call that started later wins the committed
/// value, and a `set` or `clear` that ran meanwhile wins over every provisional text. The slot is
/// settled when the last overlapping show finishes. `show` receives the generation of this call:
/// the show it hands to a view carries it (see `driver::show_clipboard`). Returns `show`'s result, or `Err` when the
/// lock is poisoned (nothing ran).
pub fn set_while_showing<E>(
    text: Option<String>,
    show: impl FnOnce(u64) -> Result<(), E>,
) -> Result<Result<(), E>, String> {
    let ours = {
        let mut state = lock()?;
        state.value = text.clone();
        state.generation = state.generation.wrapping_add(1);
        state.provisional_at = state.generation;
        state.latest = state.generation;
        let ours = state.generation;
        state.pending.insert(ours, false);
        ours
    };
    let mut finish = Finish {
        ours,
        text,
        shown: false,
    };
    let result = show(ours);
    finish.shown = result.is_ok();
    drop(finish);
    Ok(result)
}

/// A history row offered to a clip card: the polished text is the primary payload, the raw one the
/// alternate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipSource {
    pub id: String,
    pub primary: String,
    pub alternate: Option<String>,
}

/// The rows a clip card shows. With an override: one row, id `current_id`, primary the override and
/// alternate the raw text of the history row with the same primary text (none when there is none).
/// Without one: the `limit` newest rows. `recent` reads the host's history, newest first; it runs
/// once and its error is returned.
pub fn select_items(
    override_text: Option<String>,
    recent: impl FnOnce() -> Result<Vec<ClipSource>, String>,
    current_id: &str,
    limit: usize,
) -> Result<Vec<ClipSource>, String> {
    let all = recent()?;
    match override_text {
        Some(text) => {
            let alternate = all
                .iter()
                .find(|row| row.primary == text)
                .and_then(|row| row.alternate.clone());
            Ok(vec![ClipSource {
                id: current_id.to_string(),
                primary: text,
                alternate,
            }])
        }
        None => Ok(all.into_iter().take(limit).collect()),
    }
}
