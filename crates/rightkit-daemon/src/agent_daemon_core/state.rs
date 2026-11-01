use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

/// In-memory authority for whether a session currently owns an engine turn.
///
/// Persistent `sessions.status` is only a durable projection and can be left
/// at `running` by a crash. The daemon's self-exit gate therefore consults
/// this handle first and uses the persisted value only for reconciliation.
#[derive(Clone, Default)]
pub struct LiveTurnHandle {
    active: Arc<AtomicBool>,
    /// Turn generation fence: every `begin` takes the next generation, and a
    /// guard may only clear `active` while it still holds the latest one. A
    /// stale guard dropping late (an old turn unwinding after a newer turn
    /// began) must not mark the session idle under the live turn.
    generation: Arc<AtomicU64>,
}

impl LiveTurnHandle {
    pub fn begin(&self) -> LiveTurnGuard {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.active.store(true, Ordering::SeqCst);
        LiveTurnGuard {
            handle: self.clone(),
            generation,
        }
    }

    /// Generation of the most recently begun turn (0 = none yet).
    pub fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

pub struct LiveTurnGuard {
    handle: LiveTurnHandle,
    generation: u64,
}

impl LiveTurnGuard {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// True while no newer turn has superseded this one.
    pub fn is_current(&self) -> bool {
        self.handle.current_generation() == self.generation
    }
}

impl Drop for LiveTurnGuard {
    fn drop(&mut self) {
        // Only the latest generation may clear the flag. Checked and cleared
        // with a compare on the generation after the store so a racing
        // `begin` that bumps the generation keeps the session active.
        if self.is_current() {
            self.handle.active.store(false, Ordering::SeqCst);
            // A `begin` may have landed between the check and the store; it
            // set `active` before we cleared it, so restore it.
            if !self.is_current() {
                self.handle.active.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_guard_drop_does_not_clear_newer_turn() {
        let handle = LiveTurnHandle::default();
        let old = handle.begin();
        let new = handle.begin();
        assert!(!old.is_current());
        assert!(new.is_current());
        drop(old);
        assert!(handle.is_active(), "stale drop must not idle a live turn");
        drop(new);
        assert!(!handle.is_active());
    }

    #[test]
    fn generations_are_monotonic_and_latest_drop_clears() {
        let handle = LiveTurnHandle::default();
        assert_eq!(handle.current_generation(), 0);
        let first = handle.begin();
        assert_eq!(first.generation(), 1);
        drop(first);
        assert!(!handle.is_active());
        let second = handle.begin();
        assert_eq!(second.generation(), 2);
        assert!(handle.is_active());
    }
}
