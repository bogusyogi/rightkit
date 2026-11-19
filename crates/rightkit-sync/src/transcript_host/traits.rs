//! The interfaces a host implements to run transcript sync, and the transport seam.
//!
//! Three small surfaces, none of which returns key material or user content to the crate's logs:
//!
//! * [`TranscriptHost`] (8 methods): the host's paths, history store, activity gate, clock and
//!   UI notification.
//! * [`HistorySession`] (4 methods): one open view of the host's history store, read in windows.
//! * [`SyncTransport`] (9 methods) with [`NativeObserver`] (3 methods): the backend behind its
//!   own seam. [`super::cloudkit::CloudKitTransport`] is the macOS implementation; another
//!   backend implements the same trait.
//!
//! Threading words used below: "worker" is a thread from the host runtime's blocking pool (or any
//! thread that is allowed to block); "any thread" means the method may be called from the
//! scheduler task, a worker, a native callback thread or a command handler, and must not block on
//! another lock the host holds while calling into sync.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::transcripts::CloudTranscript;
use crate::{AuthProvider, Backend, SyncObserver};

/// Starts a future on the host's async runtime. The scheduler calls it at most twice per
/// process: once for its coordinator task and once for its single retry timer. The future needs a
/// runtime with the time driver enabled. It must not block the caller.
pub type Spawner = fn(Pin<Box<dyn Future<Output = ()> + Send + 'static>>);

/// One history row as sync sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    /// The host's own row id. The crate maps it to a logical id with the profile.
    pub delivery_id: String,
    /// Final text.
    pub text: String,
    /// Text before polishing, when the host kept it.
    pub raw_text: Option<String>,
    /// Immutable origin time of the row, milliseconds since the Unix epoch.
    pub created_at_ms: u64,
    /// `Some(device)` when the row was received through sync from `device`, `None` when it was
    /// recorded on this device. A locally recorded row wins every logical-id collision.
    pub synced_from: Option<String>,
}

/// One history row's key: enough to find the row again and to tell where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryKey {
    /// The host's own row id. The crate maps it to a logical id with the profile.
    pub delivery_id: String,
    /// `Some(device)` when the row was received through sync from `device`, `None` when it was
    /// recorded on this device.
    pub synced_from: Option<String>,
}

/// One bounded window of history keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyWindow {
    /// At most the `limit` that was asked for.
    pub keys: Vec<HistoryKey>,
    /// Where the next window starts, `None` after the last window. The text is the host's own
    /// (an offset, a row id, a sequence number); the crate only hands it back.
    pub next: Option<String>,
}

/// One open view of the host's history store.
///
/// Created by [`TranscriptHost::open_history`] for one operation and dropped at its end. All
/// methods run on a worker and may block. An `Err` aborts the sync operation that called it
/// without advancing the change cursor; the crate never treats an `Err` as "no rows" and never
/// deletes anything because a read failed.
///
/// History is never read whole. The crate walks it in windows of keys (no row text), keeps an
/// index of the keys of one pass in memory (at most `TranscriptSyncConfig::max_history_rows`
/// entries, about 150 bytes each; estimated, not measured), and then reads row text for at most a
/// few hundred rows at a time by id.
pub trait HistorySession {
    /// The next window of at most `limit` row keys, in an order the store keeps stable (for
    /// example ascending row id or insertion sequence), starting at `cursor` (`None` = the
    /// start). `limit` is at most 2,000. A walk is a series of calls that each pass the previous
    /// call's `next`; the crate ends it at `next == None`, and stops it with an error if a window
    /// makes no progress or the walk exceeds `max_history_rows` keys.
    ///
    /// Rows written or deleted by someone else between two calls may be missed or repeated; the
    /// crate tolerates repeats (it merges keys by logical id), and a row it missed is read at the
    /// next pass. A store that can tell that the history changed during the walk (for example a
    /// row count that moved) should return `Err` instead, so the crate does not act on a view that
    /// is known to be unstable. The host does not need to enforce the row limit.
    fn key_window(&mut self, cursor: Option<&str>, limit: usize) -> Result<KeyWindow, String>;
    /// The rows with these host ids (at most a few hundred per call), in any order. An id with no
    /// row is left out; it is not an error.
    fn rows_by_id(&mut self, delivery_ids: &[String]) -> Result<Vec<HistoryRow>, String>;
    /// Delete rows by the host's own id. Deleting an unknown id is not an error. `Err` means at
    /// least one id may still exist.
    fn delete(&mut self, delivery_ids: &[String]) -> Result<(), String>;
    /// Upsert rows received through sync (each marked as synced from `item.source`, created at
    /// `item.created_at_ms` or, when that is 0, `item.updated_at_ms`). The host chooses the row
    /// id and must derive it from `item.id` so that a repeated pull refreshes the same row. An
    /// empty slice is a no-op. `Err` means the batch may not have been stored.
    fn insert_synced(&mut self, items: &[CloudTranscript]) -> Result<(), String>;
}

/// What the host provides to the sync host. Implemented once per app.
pub trait TranscriptHost: Send + Sync + 'static {
    /// Absolute path of the file `name` under the host's data root. `name` is a single plain
    /// file name that the crate has already validated (no separator, not `.` or `..`).
    /// Any thread; computes a path only, never touches the disk, never fails.
    fn data_file(&self, name: &str) -> PathBuf;

    /// Open the history store for one sync operation, independently of any store a recording
    /// session holds. Worker only; may block while the store opens. `Err` (store unavailable,
    /// corrupt, locked) fails the operation; nothing is deleted or marked as synced.
    fn open_history(&self) -> Result<Box<dyn HistorySession>, String>;

    /// A session wrote to history (delete or insert). Called after the write, so the host can
    /// refresh what it shows. Any thread; must not block; infallible.
    fn history_changed(&self);

    /// `Some(tag)` while the host wants background sync to stay out of the way (for example a
    /// recording is in progress); `None` otherwise. `tag` is a short closed tag that appears in
    /// events and in `ApplyOutcome::Deferred`. Any thread; lock-free and non-blocking (the
    /// scheduler polls it). A deferral loses nothing: pulled records are stashed durably by the
    /// engine and the pass is requeued. The host calls [`super::TranscriptSync::wake`] when the
    /// condition ends.
    fn busy_reason(&self) -> Option<&'static str>;

    /// Whether a history rebuild is waiting on a full replay of the remote zone for the account
    /// `scope` (the account identity the consent was authorized for). `scope` is an opaque
    /// digest; do not log it. Worker only; may read a small file. Return `false` when unknown:
    /// no replay is requested then.
    fn recovery_pending(&self, scope: &str) -> bool;

    /// The replay requested because `recovery_pending` was true has completed in a fully synced
    /// pass under `scope` and the consent epoch `consent_generation` that was current when the
    /// pass started. Worker only; may block. `Err` makes the pass return an error (the scheduler
    /// retries) and the host keeps its recovery marker.
    fn recovery_complete(&self, scope: &str, consent_generation: u64) -> Result<(), String>;

    /// Wall clock, milliseconds since the Unix epoch. Any thread; non-blocking; infallible.
    fn now_ms(&self) -> u64;

    /// A pass downloaded `count > 0` records. Called on the scheduler task. Must not block
    /// (post to the UI thread and return); infallible.
    fn downloaded(&self, count: u32);
}

/// Receives native, out-of-process wakeups (silent pushes, account and reachability changes) that
/// a platform observer turns into the three small event kinds below. The platform observer runs
/// in the main process and is signed for one environment, so a transport hands it out only when
/// it is allowed to run.
pub trait NativeObserver: Send + Sync {
    /// Register `callback` with the platform observer. `callback(kind)` may be called from any
    /// native thread at any time after this returns: `1` = remote change, `2` = reconcile
    /// (account, reachability or lifecycle recovery); any other value is ignored. The callback
    /// never blocks and never panics. Called once per process by `TranscriptSync::start`.
    fn install(&self, callback: extern "C" fn(i32));
    /// Mirror the user's consent switch into the observer. Any thread; non-blocking.
    fn set_enabled(&self, enabled: bool);
    /// The remote zone was recreated: re-establish the observer's cached subscription. Any
    /// thread; non-blocking.
    fn invalidate_subscription(&self);
}

/// The backend seam: where records go. The crate owns ids, the mapper, consent, the scheduler,
/// the deletion outbox and the engine; a transport owns only the wire and the account.
///
/// Every method may be called from a worker; `state_file_name`, `legacy_import` and
/// `native_observer` are also called from any thread and must not block. Methods that return
/// `Err(String)` return a short diagnostic that is classified by `transcripts::error_is_terminal`
/// and shown to the user as the "unavailable" detail: it must not contain user content, paths or
/// credentials.
pub trait SyncTransport: Send + Sync {
    /// File name under the data root for the state file `base`, so that a development
    /// environment can never feed the production ledger, cursor or deletion queue. Pure.
    fn state_file_name(&self, base: &str) -> String;

    /// Whether the pre-engine state files (tombstones, aliases, acknowledgements) belong to this
    /// transport's environment and may be imported once. Pure.
    fn legacy_import(&self) -> bool;

    /// Cheap readiness check run before every pass (the transport can be constructed: helper
    /// found and trusted). May block briefly. `Err` fails the pass; no network traffic happened.
    fn prepare(&self) -> Result<(), String>;

    /// Record I/O. Constructed lazily; the same object backs every pass.
    fn backend(&self) -> Result<Arc<dyn Backend>, String>;

    /// The transport's own account state. Its `generation` must change when the account
    /// changes and when the transport is disabled (that cancels an in-flight pass). Its
    /// `account_scope` is the opaque identity of the signed-in account, container and zone that
    /// every cached state is bound to. The crate wraps it with the consent gate.
    fn auth(&self) -> Result<Arc<dyn AuthProvider>, String>;

    /// Engine diagnostics. `detail` is sanitized by the crate's event sink wrapper, not here.
    fn observer(&self) -> Arc<dyn SyncObserver>;

    /// The transport-side enabled flag. `false` cancels an in-flight pass without waiting for
    /// it; it must not block on the pass. `Err` = the transport could not be constructed.
    ///
    /// A transport starts DISABLED (rev 68): no code path may leave a freshly constructed
    /// transport enabled, so observing the account (`auth()`, `availability()`,
    /// `account_scope()`) never lets data move. The host calls `set_enabled(true)` only after the
    /// consent is durable and the transition is still current (enable) or the trusted consent
    /// says on and the pass is admitted (start-up), under its stop gate. `set_enabled(false)`
    /// must not construct anything.
    fn set_enabled(&self, enabled: bool) -> Result<(), String>;

    /// Stop verification (rev 68): does the EXISTING transport-side state report itself disabled?
    /// Never constructs anything. `Some(true)` = it exists and reports disabled, `Some(false)` =
    /// it reports enabled, `None` = unknown (nothing exists yet, or its state cannot be read).
    /// An unknown answer is not evidence of a stop.
    fn confirmed_stopped(&self) -> Option<bool>;

    /// The native observer, when this transport has one and it is allowed to run in this
    /// session; `None` otherwise.
    fn native_observer(&self) -> Option<Arc<dyn NativeObserver>>;
}
