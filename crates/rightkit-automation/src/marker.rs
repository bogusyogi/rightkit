//! A caller-supplied marker stamped on every input event this crate posts, so the caller's own
//! event tap or hook can tell its posted events from the user's.
//!
//! * macOS: written to `kCGEventSourceUserData` (event field 42) of every event posted by the
//!   engine's tier 3 and tier 4 paths and by `macos::background`. In the caller's `CGEventTap`
//!   callback read it with `CGEventGetIntegerValueField(event, EVENT_SOURCE_USER_DATA_FIELD)` and
//!   compare it with [`is_own_event`].
//! * Windows: written to `dwExtraInfo` of the engine's tier 4 `SendInput` events (a low-level
//!   hook sees it as `KBDLLHOOKSTRUCT::dwExtraInfo`). `win::input::SendOptions` keeps its own
//!   per-batch tag and is not affected.
//!
//! The marker is process-wide and starts at `0`, which means "no marker": events are posted as
//! before. Pick a value that is not `0` and unlikely to collide with another tool in the process.

use std::sync::atomic::{AtomicI64, Ordering};

/// `kCGEventSourceUserData`, the `CGEventField` that carries the marker on macOS.
pub const EVENT_SOURCE_USER_DATA_FIELD: u32 = 42;

static MARKER: AtomicI64 = AtomicI64::new(0);

/// Set the marker stamped on every event posted from now on. `0` clears it.
pub fn set_event_marker(value: i64) {
    MARKER.store(value, Ordering::SeqCst);
}

/// The marker currently stamped on posted events (`0` = none).
pub fn event_marker() -> i64 {
    MARKER.load(Ordering::SeqCst)
}

/// `true` when `user_data` read from an event is this process's marker. Always `false` while no
/// marker is set, so an unmarked event is never claimed.
pub fn is_own_event(user_data: i64) -> bool {
    let marker = event_marker();
    marker != 0 && user_data == marker
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The marker is process-wide; keep these tests from racing each other.
    static GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn unset_marker_claims_nothing() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_event_marker(0);
        assert_eq!(event_marker(), 0);
        assert!(!is_own_event(0));
        assert!(!is_own_event(42));
    }

    #[test]
    fn set_marker_is_recognised_and_clearable() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_event_marker(0x5055_4C53_45);
        assert_eq!(event_marker(), 0x5055_4C53_45);
        assert!(is_own_event(0x5055_4C53_45));
        assert!(!is_own_event(0x5055_4C53_46));
        assert!(!is_own_event(0));
        set_event_marker(0);
        assert!(!is_own_event(0x5055_4C53_45));
    }

    #[test]
    fn field_matches_the_cg_user_data_field() {
        assert_eq!(EVENT_SOURCE_USER_DATA_FIELD, 42);
    }
}
