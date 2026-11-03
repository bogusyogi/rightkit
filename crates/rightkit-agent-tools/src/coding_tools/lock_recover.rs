//! Poisoned-mutex recovery for `host_tools`.
//!
//! Mirrors `host app` bin's `api_state_types::lock_recover` — the pattern
//! already used across the daemon so a panic on ANY holder of a `Mutex`
//! doesn't permanently wedge every future caller behind a poisoned lock.
//! Tools crate has no dependency on the `host app` binary, so this is a
//! local copy of the same one-line recovery rather than a shared import.

use std::sync::{Mutex, MutexGuard};

/// Lock `mutex`, recovering from poisoning instead of panicking.
///
/// A poisoned mutex means some earlier holder panicked while holding the
/// lock. The data itself is very likely still structurally valid (most
/// panics happen on an unrelated assertion, not mid-mutation) — refusing to
/// ever touch the mutex again is strictly worse than recovering and letting
/// the caller keep working. `label` is logged so poisoning is traceable to a
/// specific store.
pub fn lock_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!(mutex = label, "recovering from poisoned tools mutex");
            poisoned.into_inner()
        }
    }
}
