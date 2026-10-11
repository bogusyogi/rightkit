//! Brand-neutral cloud sync engine. See `API.md` for the adapter traits and
//! the record/conflict contract shared with the iOS CloudKit implementation.

mod backoff;
mod engine;
mod error;
pub mod helper;
mod ledger;
mod memory;
mod record;
mod storage;
mod traits;

pub use backoff::BackoffPolicy;
pub use engine::{
    PassOutcome, PassReport, SyncConfig, SyncEngine, SyncEngineBuilder, SyncMetrics, SyncStatus,
};
pub use error::{SyncError, SyncResult};
pub use ledger::{Ledger, LedgerStore, RemoteVersion};
pub use memory::MemoryBackend;
pub use record::{field, read_tombstone, write_tombstone, FieldValue, RawRecord};
pub use storage::{FileStorage, LedgerStorage, MemoryStorage};
pub use traits::*;
