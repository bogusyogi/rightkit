//! `rightkit-watch`: one filesystem-watching foundation for every Right Suite app.
//!
//! Three layers, all on `notify`:
//! * [`subscribe`] — async multi-subscriber watcher (CodeRight donor): reference-counted OS
//!   watches, canonical-path matching, missing-path fallback, throttled/debounced receivers.
//! * [`sync`] — thread-based debounced watchers (ViewRight donor): a grant-gated file-set watcher
//!   that watches parent directories (so rename-over-original saves surface) and a capped,
//!   evicting directory watch set for explorer trees. No async runtime or UI framework needed.
//! * [`coalesce`] — rename-storm coalescing that reports net filesystem truth per batch.

pub mod coalesce;
pub mod subscribe;
pub mod sync;
pub mod typed;

pub use coalesce::{ChangeBatch, ChangeCoalescer, Rename};
pub use subscribe::{
    DebouncedWatchReceiver, FileWatcher, FileWatcherEvent, FileWatcherSubscriber, Receiver,
    ThrottledWatchReceiver, WatchPath, WatchRegistration,
};
pub use sync::{
    DirWatchSet, FileChange, FileChangeKind, FileSetWatcher, GrantCheck, DEFAULT_DEBOUNCE,
    DEFAULT_MAX_WAIT, EXPLORER_DEBOUNCE, EXPLORER_WATCH_CAP,
};
pub use typed::{TypedWatcher, WatchError, WatchEvent};
