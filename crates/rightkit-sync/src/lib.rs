//! Brand-neutral cloud sync engine. See `API.md` for the adapter traits and
//! the record/conflict contract shared with the iOS CloudKit implementation.

mod backoff;
#[cfg(feature = "cipher")]
pub mod cipher;
mod engine;
mod error;
#[cfg(feature = "helper")]
pub mod helper;
#[cfg(feature = "http")]
pub mod http;
mod ledger;
mod memory;
mod record;
mod storage;
mod traits;
pub mod wire;

pub use backoff::BackoffPolicy;
pub use engine::{
    ConsentChoice, PassOutcome, PassReport, SyncConfig, SyncEngine, SyncEngineBuilder, SyncMetrics, SyncStatus,
};
pub use error::{SyncError, SyncResult};
pub use ledger::{ConsentRequest, Ledger, LedgerStore, RemoteVersion};
pub use memory::MemoryBackend;
pub use record::{field, read_tombstone, write_tombstone, FieldValue, RawRecord};
pub use storage::{FileStorage, LedgerStorage, MemoryStorage};
pub use traits::*;
