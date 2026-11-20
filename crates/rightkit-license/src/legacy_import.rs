//! Retry bookkeeping for the one-time import of an older stored licence record.
//!
//! An app that upgrades from a bare (pre-envelope) record imports it once at launch
//! (`LicenseClient::import_legacy`). A transient failure (the credential store briefly unavailable)
//! must not leave the record unimported for the whole session: the licence status would still read
//! Pro while every native paid-command gate refused it. [`LegacyImportRetry`] keeps a "pending"
//! flag and a cooldown so the next licence call retries the import; [`LegacyImport`] wraps it with
//! the import closure and the two log lines.
//!
//! Always compiled (std only). The import closure, the cooldown choice and the event names are the
//! host's; nothing here reads a credential or logs a value (the closure's error text is logged, so
//! it must not carry one: `LicenseError` text never does).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Gap between two import attempts: long enough that a gate called per command does not hammer the
/// credential store, short enough that a store that comes back is picked up within one dictation.
pub const DEFAULT_RETRY_COOLDOWN: Duration = Duration::from_secs(15);
/// A failure that never clears (a damaged record) stops being retried after this many attempts.
pub const DEFAULT_MAX_RETRIES: u32 = 8;

/// Pending flag, retry counter and cooldown for one import.
pub struct LegacyImportRetry {
    pending: AtomicBool,
    retries: AtomicU32,
    next_attempt: Mutex<Option<Instant>>,
    cooldown: Duration,
}

impl LegacyImportRetry {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            pending: AtomicBool::new(false),
            retries: AtomicU32::new(0),
            next_attempt: Mutex::new(None),
            cooldown,
        }
    }

    /// The import failed (at launch): mark it pending, first retry one cooldown from `now`.
    pub fn mark_failed(&self, now: Instant) {
        self.pending.store(true, Ordering::SeqCst);
        *self.slot() = Some(now + self.cooldown);
    }

    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::SeqCst)
    }

    /// Run `import` when one is pending, due and under the retry cap; `None` when nothing ran.
    /// The attempt is reserved before `import` runs, so concurrent callers skip rather than queue
    /// behind a slow credential store. `Ok` clears the pending flag; `Err` keeps it.
    pub fn retry_if_due(
        &self,
        now: Instant,
        import: impl FnOnce() -> Result<(), String>,
    ) -> Option<Result<(), String>> {
        if !self.is_pending() || self.retries.load(Ordering::SeqCst) >= DEFAULT_MAX_RETRIES {
            return None;
        }
        {
            let mut next = self.slot();
            if next.is_some_and(|due| now < due) {
                return None;
            }
            *next = Some(now + self.cooldown);
        }
        self.retries.fetch_add(1, Ordering::SeqCst);
        let outcome = import();
        if outcome.is_ok() {
            self.pending.store(false, Ordering::SeqCst);
        }
        Some(outcome)
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        self.next_attempt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Log event names of one [`LegacyImport`] (the host's log vocabulary).
#[derive(Debug, Clone, Copy)]
pub struct LegacyImportEvents {
    /// The launch attempt failed; the detail is the error text.
    pub failed: &'static str,
    /// A retry ran; the detail is `outcome=imported` or `outcome=failed error=<text>`.
    pub retried: &'static str,
}

/// The in-place import of an older bare licence record: one launch attempt, then a retry on the
/// next licence call while the cooldown has passed.
pub struct LegacyImport {
    retry: LegacyImportRetry,
    run: Box<dyn Fn() -> Result<(), String> + Send + Sync>,
    log: Box<dyn Fn(&str, &str) + Send + Sync>,
    events: LegacyImportEvents,
}

impl LegacyImport {
    /// `run` performs one import attempt (typically `client.import_legacy(..)` mapped to
    /// `Result<(), String>`); `log` is the host's `(event, detail)` sink.
    pub fn new(
        cooldown: Duration,
        events: LegacyImportEvents,
        log: impl Fn(&str, &str) + Send + Sync + 'static,
        run: impl Fn() -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            retry: LegacyImportRetry::new(cooldown),
            run: Box::new(run),
            log: Box::new(log),
            events,
        }
    }

    /// The launch attempt. A failure is logged and left pending for [`Self::retry_if_due`].
    pub fn first_attempt(&self) {
        if let Err(error) = (self.run)() {
            (self.log)(self.events.failed, &error);
            self.retry.mark_failed(Instant::now());
        }
    }

    /// Retry the import when the launch attempt failed and the cooldown has passed. Cheap and
    /// lock-free when nothing is pending, so every licence call may run it first.
    pub fn retry_if_due(&self) {
        let outcome = self.retry.retry_if_due(Instant::now(), || (self.run)());
        match outcome {
            Some(Ok(())) => (self.log)(self.events.retried, "outcome=imported"),
            Some(Err(error)) => (self.log)(
                self.events.retried,
                &format!("outcome=failed error={error}"),
            ),
            None => {}
        }
    }
}
