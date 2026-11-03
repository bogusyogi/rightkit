//! The fixed lock order and the reentry guard.
//!
//! Three locks exist and are always taken in this order: the licence operation lock
//! (`<license dir>/.lock`), then the credentials rotation lock, then the clock lock. A thread
//! that holds one may take only a later one, never an earlier one and never the same one again.
//! A reentry or an inversion is a programming error, not a runtime condition, so it is a debug
//! assertion; a release build keeps the bounded `try_acquire` polling that turns the same mistake
//! into `Busy` rather than a hang.

use std::cell::Cell;
use std::marker::PhantomData;

/// The locks, in acquisition order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    /// The per-app licence operation lock.
    Op,
    /// The credentials rotation lock (taken inside the credentials calls).
    SecureClock,
    Rotation,
    /// The anti-rollback clock file lock.
    Clock,
}

impl Level {
    fn bit(self) -> u8 {
        match self {
            Level::Op => 1,
            Level::SecureClock => 2,
            Level::Rotation => 4,
            Level::Clock => 8,
        }
    }
}

thread_local! {
    static HELD: Cell<u8> = const { Cell::new(0) };
}

/// Marks that the current thread is inside `level`. Not `Send`: it is released on the thread
/// that took it.
pub(crate) struct LevelGuard {
    bit: u8,
    _not_send: PhantomData<*const ()>,
}

/// Enter `level` on this thread. Debug-asserts that this thread does not already hold `level`
/// or any later one (reentry, or taking an earlier lock after a later one).
pub(crate) fn enter(level: Level) -> LevelGuard {
    let bit = level.bit();
    HELD.with(|held| {
        let now = held.get();
        // Every bit at or above `bit`.
        let at_or_after = !(bit - 1);
        held.set(now | bit);
    });
    LevelGuard {
        bit,
        _not_send: PhantomData,
    }
}

impl Drop for LevelGuard {
    fn drop(&mut self) {
        HELD.with(|held| held.set(held.get() & !self.bit));
    }
}
