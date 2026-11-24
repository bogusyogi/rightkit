//! Restart decision for an engine whose backend depends on the paid entitlement.
//!
//! An engine that loads its backend only at launch cannot swap it in place when a licence lapses
//! or is renewed. [`BackendRestartTracker`] remembers the entitlement bit last observed and says
//! whether a new observation changes the backend the engine must run; [`spawn_check`] runs that
//! decision on its own thread so a licence change signal never waits on the host's runtime lock.
//! What "effective backend" means, how to read the stored setting, and how to restart are the
//! host's.

use std::sync::Mutex;

/// The entitlement bit last observed. `None` until the first change signal.
pub struct BackendRestartTracker {
    last: Mutex<Option<bool>>,
}

impl Default for BackendRestartTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl BackendRestartTracker {
    /// A tracker with no observation yet.
    pub const fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    /// Record `now_entitled` and say whether the engine must restart for this observation.
    ///
    /// `effective(raw_backend, entitled)` is the host's mapping from the stored backend and the
    /// entitlement to the backend the engine runs. With no earlier observation the running
    /// engine is assumed to hold the stored setting as is (it may have launched entitled), so an
    /// unentitled install whose stored backend needs the entitlement restarts once, which fails
    /// closed. The stored setting is never edited.
    pub fn observe(
        &self,
        raw_backend: &str,
        now_entitled: bool,
        effective: impl Fn(&str, bool) -> String,
    ) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|error| error.into_inner());
        let restart = needs_restart(*last, raw_backend, now_entitled, &effective);
        *last = Some(now_entitled);
        restart
    }
}

/// True when moving from `previous_entitled` to `now_entitled` changes the effective backend.
/// With `previous_entitled` `None` the running backend is `raw_backend` itself.
pub fn needs_restart(
    previous_entitled: Option<bool>,
    raw_backend: &str,
    now_entitled: bool,
    effective: &impl Fn(&str, bool) -> String,
) -> bool {
    let running = match previous_entitled {
        Some(entitled) => effective(raw_backend, entitled),
        None => raw_backend.to_string(),
    };
    running != effective(raw_backend, now_entitled)
}

/// What the host supplies to [`spawn_check`].
pub struct EntitlementChange {
    /// Thread name for the check.
    pub thread_name: String,
    /// Event name logged when a restart is needed (detail: `context=<..> stored_backend=<..>`).
    pub restart_event: &'static str,
    /// Event name logged when the thread could not be started.
    pub spawn_failed_event: &'static str,
}

/// Run the restart decision on a named thread after an entitlement change.
///
/// `read_backend` returns the stored backend, `is_entitled` the current verdict, `effective` the
/// host mapping, `restart` the host's (idle-gated) restart, and `log` the `(event, detail)` sink.
/// `context` is a short label recorded in the log line. A thread-start failure is logged, never
/// raised.
#[allow(clippy::too_many_arguments)]
pub fn spawn_check(
    tracker: &'static BackendRestartTracker,
    change: EntitlementChange,
    context: &str,
    read_backend: impl FnOnce() -> String + Send + 'static,
    is_entitled: impl FnOnce() -> bool + Send + 'static,
    effective: impl Fn(&str, bool) -> String + Send + 'static,
    restart: impl FnOnce() + Send + 'static,
    log: fn(&str, &str),
) {
    let context = context.to_string();
    let event = change.restart_event;
    let spawned = std::thread::Builder::new()
        .name(change.thread_name)
        .spawn(move || {
            let raw = read_backend();
            if !tracker.observe(&raw, is_entitled(), effective) {
                return;
            }
            log(event, &format!("context={context} stored_backend={raw}"));
            restart();
        });
    if let Err(error) = spawned {
        log(change.spawn_failed_event, &error.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effective(raw: &str, entitled: bool) -> String {
        if raw == "paid" && !entitled {
            "free".to_string()
        } else {
            raw.to_string()
        }
    }

    #[test]
    fn first_unentitled_observation_restarts_a_paid_backend() {
        let tracker = BackendRestartTracker::new();
        assert!(tracker.observe("paid", false, effective));
        assert!(!tracker.observe("paid", false, effective));
        assert!(tracker.observe("paid", true, effective));
    }

    #[test]
    fn first_entitled_observation_restarts_nothing() {
        let tracker = BackendRestartTracker::new();
        assert!(!tracker.observe("paid", true, effective));
        assert!(!tracker.observe("free", false, effective));
    }
}
