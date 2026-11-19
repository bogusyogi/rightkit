//! A process-wide handle over one [`Store`]: a lazy, bounded open that survives a refused file,
//! one cross-process transaction entry point that returns the committed revision, and the
//! committed value with its revision from one owner read.
//!
//! Settings type, file location, store options & defaults come from the host. This module
//! names no product, key or path. It starts no thread, timer or watcher. Open retries add only
//! bounded sleep on the calling thread.
//!
//! ## Refused open
//!
//! When the file cannot be opened at all (unreadable, a newer schema, oversized, corrupt under a
//! refusing policy; not merely missing) the handle holds no store: reads return the host's
//! defaults and every write is refused with an error that names no path, until a later call opens
//! the file. Opening retains the host's migration & recovery policy.
//!
//! ## Text of the errors
//!
//! Store error text contains no path or settings value. Host edit errors are returned unchanged;
//! hosts must redact their own messages. A stale revision begins with
//! [`REVISION_CONFLICT_PREFIX`], the code a settings UI maps to a conflict.

use crate::{Change, Error, Store, StoreOptions, Subscription};
use serde::{de::DeserializeOwned, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Upper bound of [`HandleConfig::open_attempts`]; larger values are lowered to it.
pub const MAX_OPEN_ATTEMPTS: u32 = 8;
/// Upper bound of [`HandleConfig::open_retry_delay`]; larger values are lowered to it. With the
/// attempt bound the longest wait one open can add is `(8 - 1) * 250 ms`.
pub const MAX_OPEN_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Prefix of the error a stale expected revision produces: `revision_conflict: <detail>`.
pub const REVISION_CONFLICT_PREFIX: &str = "revision_conflict";
/// Event name passed to the sink when the file could not be opened. The detail is a stable tag
/// from [`error_tag`] plus a fixed sentence, never a path.
pub const EVENT_UNREADABLE_AT_LOAD: &str = "settings_unreadable_at_load";
/// Event name passed to the sink when the handle's lock is poisoned.
pub const EVENT_LOCK_ERROR: &str = "settings_lock_error";

/// Host event sink: `(event name, stable detail)`. Receives no path, no value and no user text.
pub type EventSink = fn(&str, &str);

/// Everything the handle needs from the host. All fields are required; there is no default.
pub struct HandleConfig<T> {
    /// The settings file. Called for every open attempt.
    pub path: fn() -> PathBuf,
    /// The store options. Called for every open attempt (options are consumed by an open).
    /// Path/options providers & migration/default hooks run during serialized open; they must
    /// not reenter this handle.
    pub options: fn() -> StoreOptions,
    /// Fallback while no store is open, including after a poisoned handle lock.
    /// An open store's missing-file defaults come from `options`.
    pub defaults: fn() -> T,
    /// Opens tried before giving up on an I/O error. Clamped to `1..=MAX_OPEN_ATTEMPTS`.
    pub open_attempts: u32,
    /// Pause between two attempts. Clamped to `MAX_OPEN_RETRY_DELAY`.
    pub open_retry_delay: Duration,
    /// Receives [`EVENT_UNREADABLE_AT_LOAD`] and [`EVENT_LOCK_ERROR`].
    pub event_sink: EventSink,
    /// Called after a commit through this handle that changed at least one top-level key
    /// ([`Store::subscribe`]), with the changed keys. It runs outside handle & store locks,
    /// before the transaction returns. `None` subscribes nothing.
    pub on_change: Option<fn(&Change)>,
}

/// The handle. Put one in a `static OnceLock` and build it with [`StoreHandle::new`]; the file is
/// opened on first use, not at construction.
pub struct StoreHandle<T> {
    path: fn() -> PathBuf,
    options: fn() -> StoreOptions,
    defaults: fn() -> T,
    open_attempts: u32,
    open_retry_delay: Duration,
    event_sink: EventSink,
    on_change: Option<fn(&Change)>,
    /// Serializes initial open & retries; transactions clone the open store then release it.
    slot: Mutex<HandleState<T>>,
}

struct HandleState<T> {
    attempted: bool,
    opened: Option<Arc<Opened<T>>>,
}

/// An open store and its change subscription. The subscription ends when this is dropped, and a
/// store opened later subscribes again, so a reopened store never leaves a listener behind.
struct Opened<T> {
    store: Store<T>,
    _subscription: Option<Subscription>,
}

/// A stable, path-free tag for an error, for events.
pub fn error_tag(error: &Error) -> &'static str {
    match error {
        Error::Io(_) => "io",
        Error::Replaced { .. } => "replaced",
        Error::Json(_) => "json",
        Error::NewerSchema { .. } => "newer_schema",
        Error::Invalid(_) => "invalid",
        Error::BadSchema(_) => "bad_schema",
        Error::Conflict { .. } => "conflict",
        Error::LockTimeout(_) => "lock_timeout",
        Error::Corrupt(_) => "corrupt",
        Error::PreserveFailed { .. } => "preserve_failed",
        Error::Protection { .. } => "protection",
    }
}

/// Error text with no path. Variants whose own text carries a path or a payload are replaced by
/// a fixed sentence.
fn describe(error: &Error) -> String {
    match error {
        Error::Io(_) => "settings io error".to_string(),
        Error::Json(_) => "settings serialization error".to_string(),
        Error::NewerSchema { found, supported } => {
            format!("settings file is schema v{found}, this build supports up to v{supported}")
        }
        Error::Invalid(_) => "invalid settings value".to_string(),
        Error::BadSchema(_) => "invalid settings schema".to_string(),
        Error::Conflict { expected, actual } => {
            format!("settings changed since revision {expected}; now at revision {actual}")
        }
        Error::LockTimeout(_) => "timed out waiting for the settings lock".to_string(),
        Error::Corrupt(_) => "settings file is corrupt".to_string(),
        Error::Replaced { .. } => "settings file was replaced, but a later step failed".to_string(),
        Error::PreserveFailed { step, .. } => {
            format!("settings file could not be {step}; nothing was written")
        }
        Error::Protection { .. } => {
            "settings file cannot be written with the requested protection".to_string()
        }
    }
}

impl<T: Serialize + DeserializeOwned + Default + Clone> StoreHandle<T> {
    /// Build the handle. Out-of-range numbers are clamped (see [`HandleConfig`]); nothing is
    /// opened and nothing is read.
    pub fn new(config: HandleConfig<T>) -> Self {
        Self {
            path: config.path,
            options: config.options,
            defaults: config.defaults,
            open_attempts: config.open_attempts.clamp(1, MAX_OPEN_ATTEMPTS),
            open_retry_delay: config.open_retry_delay.min(MAX_OPEN_RETRY_DELAY),
            event_sink: config.event_sink,
            on_change: config.on_change,
            slot: Mutex::new(HandleState {
                attempted: false,
                opened: None,
            }),
        }
    }

    /// Open the file, retrying only an I/O error. Any other error ends the attempt at once.
    fn open(&self) -> Result<Opened<T>, Error> {
        let mut attempt = 1;
        loop {
            match Store::<T>::open_path((self.path)(), (self.options)()) {
                Ok(store) => {
                    let _subscription = self
                        .on_change
                        .map(|notify| store.subscribe(move |change| notify(change)));
                    return Ok(Opened {
                        store,
                        _subscription,
                    });
                }
                Err(Error::Io(_)) if attempt < self.open_attempts => {
                    attempt += 1;
                    std::thread::sleep(self.open_retry_delay);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Opens are serialized. Only path/options providers & StoreOptions migration/default hooks
    /// run under the handle lock; they must not reenter this handle. Event & change callbacks,
    /// fallback defaults, reads & edits run after this lock is released.
    fn opened(&self, retry: bool) -> Result<Option<Arc<Opened<T>>>, String> {
        let (opened, error) = {
            let mut state = self
                .slot
                .lock()
                .map_err(|_| "settings lock poisoned".to_string())?;
            let error = if !state.attempted || (retry && state.opened.is_none()) {
                state.attempted = true;
                match self.open() {
                    Ok(opened) => {
                        state.opened = Some(Arc::new(opened));
                        None
                    }
                    Err(error) => Some(error),
                }
            } else {
                None
            };
            (state.opened.clone(), error)
        };
        if let Some(error) = error {
            (self.event_sink)(
                EVENT_UNREADABLE_AT_LOAD,
                &format!(
                    "{}; running on defaults, saving disabled",
                    error_tag(&error)
                ),
            );
        }
        Ok(opened)
    }

    /// The committed settings, or the host's defaults while no store is open. A refused open
    /// is retried by a write or by [`StoreHandle::snapshot_with_revision`].
    pub fn snapshot(&self) -> T {
        match self.opened(false) {
            Ok(Some(opened)) => opened.store.get(),
            Ok(None) => (self.defaults)(),
            Err(_) => {
                (self.event_sink)(EVENT_LOCK_ERROR, "poisoned");
                (self.defaults)()
            }
        }
    }

    /// The committed settings and the revision they were read at, from one owner read
    /// ([`Store::snapshot`]): a commit landing in between can never pair old values with a newer
    /// revision. A refused open is retried once here; if it is still refused the result is an
    /// error, never the fallback defaults. Missing-file persistence follows the host's options.
    pub fn snapshot_with_revision(&self) -> Result<(T, u64), String> {
        let Some(opened) = self.opened(true)? else {
            return Err(
                "settings not read: the settings file could not be read at startup".to_string(),
            );
        };
        opened
            .store
            .snapshot()
            .map_err(|error| format!("read settings: {}", describe(&error)))
    }

    /// Run `edit` as one cross-process transaction. A refused open is retried here, so a file
    /// that becomes readable later starts saving again. `Err` from `edit` is returned as is and
    /// writes nothing.
    pub fn transact<R>(&self, edit: impl FnOnce(&mut T) -> Result<R, String>) -> Result<R, String> {
        self.transact_at(None, edit).map(|(value, _)| value)
    }

    /// As [`StoreHandle::transact`], returning the committed revision with the result. With
    /// `expected` the store's compare-and-swap ([`Store::update_at`]) refuses a stale revision and
    /// writes nothing; that error starts with [`REVISION_CONFLICT_PREFIX`].
    pub fn transact_at<R>(
        &self,
        expected: Option<u64>,
        edit: impl FnOnce(&mut T) -> Result<R, String>,
    ) -> Result<(R, u64), String> {
        let Some(opened) = self.opened(true)? else {
            return Err(
                "settings not saved: the settings file could not be read at startup and was left untouched"
                    .to_string(),
            );
        };
        let mut refused = None;
        let edit = |value: &mut T| match edit(value) {
            Ok(result) => Ok(result),
            Err(message) => {
                refused = Some(message);
                Err(Error::Invalid(String::new()))
            }
        };
        let store = &opened.store;
        let result = match expected {
            Some(revision) => store.update_at_with_revision(revision, edit),
            None => store.update_with_revision(edit),
        };
        match result {
            Ok(value) => Ok(value),
            Err(_) if refused.is_some() => Err(refused.unwrap_or_default()),
            Err(error @ Error::Conflict { .. }) => {
                Err(format!("{REVISION_CONFLICT_PREFIX}: {}", describe(&error)))
            }
            Err(error) => Err(format!("settings not saved: {}", describe(&error))),
        }
    }
}
