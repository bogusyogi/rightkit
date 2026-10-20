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
        debug_assert!(
            now & at_or_after == 0,
            "licence lock order violated or reentered: entering {level:?} while holding mask {now:#05b}"
        );
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

#[cfg(test)]
pub(crate) fn held_mask() -> u8 {
    HELD.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_nest_in_order_and_release_on_drop() {
        assert_eq!(held_mask(), 0);
        {
            let _op = enter(Level::Op);
            let _rotation = enter(Level::Rotation);
            assert_eq!(held_mask(), 0b101);
            {
                let _clock = enter(Level::Clock);
                assert_eq!(held_mask(), 0b1101);
            }
            assert_eq!(held_mask(), 0b101);
            // A sibling at the same depth after the first one ended is fine.
            let _clock = enter(Level::Clock);
        }
        assert_eq!(held_mask(), 0);
    }

    #[test]
    fn sequential_clock_then_rotation_is_not_nesting() {
        // The clock is written BEFORE the record: Op, clock (released), then rotation.
        let _op = enter(Level::Op);
        drop(enter(Level::Clock));
        let _rotation = enter(Level::Rotation);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "lock order violated or reentered")]
    fn reentering_the_operation_lock_panics_in_debug() {
        let _first = enter(Level::Op);
        let _second = enter(Level::Op);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "lock order violated or reentered")]
    fn taking_an_earlier_lock_after_a_later_one_panics_in_debug() {
        let _clock = enter(Level::Clock);
        let _op = enter(Level::Op);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "lock order violated or reentered")]
    fn taking_rotation_while_holding_the_clock_panics_in_debug() {
        let _clock = enter(Level::Clock);
        let _rotation = enter(Level::Rotation);
    }

    #[test]
    fn another_thread_has_its_own_levels() {
        let _op = enter(Level::Op);
        std::thread::spawn(|| {
            assert_eq!(held_mask(), 0);
            let _op = enter(Level::Op);
        })
        .join()
        .unwrap();
    }
}
