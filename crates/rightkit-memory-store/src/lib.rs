//! Cortex durable store implementation.
//!
//! These modules own the SQLite schema (created once, in final form), durable identifiers, and installation lineage.
//!
//! This crate's explicit re-exports are its stable storage API. Crate semver
//! and typed record schema versions provide compatibility; SQLite tables and
//! connection details remain implementation-owned.

/// Diagnostic line on stderr that can never panic. `eprintln!` panics when stderr is a closed pipe
/// (a resident outliving its supervisor, or a service with no console); in a watcher thread that
/// killed the thread and left Blueprint `watcher_unavailable` (#555). A failed write is dropped.
macro_rules! stderr_line {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

pub mod absorbed_records;
pub mod absorbed_schema;
pub mod context_telemetry;
pub mod db;
pub mod fts5;
pub mod installation_identity;
pub mod memdb;
pub mod qa_env;
pub mod scope;
pub mod temporal;
pub mod time;

pub use absorbed_records::{
    AbsorbedStore, AbsorbedStoreError, AppendOutcome, ArtifactRecord, EventCursor, ProvenanceRef,
    SessionEvent, SessionRecord, TaskRecord, ABSORBED_SCHEMA_VERSION,
};
pub use fts5::{
    sanitize_match_query, Fts5Document, Fts5Error, Fts5Hit, Fts5Projection, ProjectionState,
    FTS5_TABLE,
};

pub use memdb::{store_replacement_status, MemDb, StoreReplaceError};
pub use temporal::{
    TemporalFact, TemporalFactQuery, TemporalFactReceipt, TemporalFactStore, TemporalTransition,
};

pub mod reference_events;
