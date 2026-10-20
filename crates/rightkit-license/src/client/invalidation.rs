//! Native entitlement invalidation (feature `client`).
//!
//! A licence change (activation, refresh, deactivation, lease renewal, import, a Pro-bridge
//! exchange, a link redemption, a leftover write rolled forward) must reach every live engine
//! verifier, not only the next one built. [`LicenseInvalidation`] is the native channel: the
//! client signals it after each operation that changed, or tried to change, the stored record,
//! and the engine side listens in one of two ways.
//!
//! - **In process.** [`EngineEntitlements::with_invalidation`](crate::EngineEntitlements) attaches
//!   the handle. Every cached verdict records the generation it was verified under, and a verdict
//!   cached under an older generation is never served: the next `require` re-reads and
//!   re-verifies the stored record through the client's fence. No call into the engine is needed,
//!   and an engine verifier that is rebuilt (a new "generation" of the engine) simply attaches the
//!   same handle again.
//! - **Another process.** [`LicenseInvalidation::subscribe`] registers a listener that receives
//!   the new generation number. The shell uses it to send the engine a content-free "licence
//!   changed" hint (the existing window to engine pattern), whose only effect is
//!   `EngineEntitlements::invalidate`.
//!
//! The signal carries no entitlement data and can only make the next check stricter or equal: it
//! never grants anything, so there is nothing for a caller to forge. A listener runs after the
//! client has released its operation lock, so it may call back into the client.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

type Listener = Arc<dyn Fn(u64) + Send + Sync>;

struct Inner {
    generation: AtomicU64,
    listeners: Mutex<Vec<Listener>>,
}

/// A cloneable handle on one licence change counter. Hand one to the client
/// (`LicenseClientConfig::with_invalidation`) or take the client's own (`LicenseClient::invalidation`),
/// and give clones to the engine verifiers and the shell.
#[derive(Clone)]
pub struct LicenseInvalidation {
    inner: Arc<Inner>,
}

impl Default for LicenseInvalidation {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for LicenseInvalidation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LicenseInvalidation")
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl LicenseInvalidation {
    /// A new counter at generation 0 with no listener.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                generation: AtomicU64::new(0),
                listeners: Mutex::new(Vec::new()),
            }),
        }
    }

    /// The current generation. It only ever grows.
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    /// Register `listener`, called with the new generation after every signal. It runs on the
    /// thread that finished the licence operation, with no client lock held; keep it short and do
    /// not block in it. A panic in a listener is contained and does not reach the client or the
    /// other listeners.
    pub fn subscribe(&self, listener: impl Fn(u64) + Send + Sync + 'static) {
        if let Ok(mut listeners) = self.inner.listeners.lock() {
            listeners.push(Arc::new(listener));
        }
    }

    /// Advance the generation and notify the listeners. The client calls this itself; a host may
    /// call it to force every attached verifier to re-read (for example after it restored the
    /// credential store from a backup). It carries no data and never grants anything.
    pub fn signal(&self) {
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        // The listeners are copied out so none runs while this handle's own lock is held.
        let listeners: Vec<Listener> = match self.inner.listeners.lock() {
            Ok(listeners) => listeners.clone(),
            Err(_) => return,
        };
        for listener in listeners {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(generation)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn a_signal_advances_the_generation_and_calls_every_listener_with_it() {
        let invalidation = LicenseInvalidation::new();
        assert_eq!(invalidation.generation(), 0);
        let seen = Arc::new(Mutex::new(Vec::new()));
        for _ in 0..2 {
            let seen = seen.clone();
            invalidation.subscribe(move |generation| seen.lock().unwrap().push(generation));
        }
        invalidation.signal();
        invalidation.signal();
        assert_eq!(invalidation.generation(), 2);
        assert_eq!(*seen.lock().unwrap(), vec![1, 1, 2, 2]);
    }

    #[test]
    fn clones_share_one_counter() {
        let one = LicenseInvalidation::new();
        let two = one.clone();
        two.signal();
        assert_eq!(one.generation(), 1);
    }

    #[test]
    fn a_panicking_listener_does_not_stop_the_others_or_the_signal() {
        let invalidation = LicenseInvalidation::new();
        let calls = Arc::new(AtomicUsize::new(0));
        invalidation.subscribe(|_| panic!("listener failure"));
        let counted = calls.clone();
        invalidation.subscribe(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        invalidation.signal();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(invalidation.generation(), 1);
    }

    #[test]
    fn a_listener_may_signal_again_without_deadlock() {
        let invalidation = LicenseInvalidation::new();
        let inner = invalidation.clone();
        let fired = Arc::new(AtomicUsize::new(0));
        let counted = fired.clone();
        invalidation.subscribe(move |generation| {
            // Re-entrancy: bounded by the guard so the test itself cannot loop forever.
            if counted.fetch_add(1, Ordering::SeqCst) == 0 && generation == 1 {
                inner.signal();
            }
        });
        invalidation.signal();
        assert_eq!(invalidation.generation(), 2);
        assert_eq!(fired.load(Ordering::SeqCst), 2);
    }
}
