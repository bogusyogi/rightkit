//! Typed memory backend contract, implemented over an external `membrane` CLI (see [`crate::membrane`]).
//!
//! Membrane is reached only through the [`crate::membrane`] module. This module
//! never links Membrane, Cortex or Memright code.

pub mod conformance;
pub mod contract;
pub mod error;
pub mod native;
pub mod repository_scope;
pub mod types;
pub mod unavailable;

pub use conformance::{
    run_conformance, ConformanceFactory, ConformanceFailure, ConformanceReport,
    OperationObservation, TypedErrorObservation, REQUIRED_ERROR_KINDS, REQUIRED_OPERATIONS,
};
pub use contract::{CallContext, MemoryBackend};
pub use error::{MemoryBackendError, MemoryBackendErrorKind};
pub use native::{AdaptClient, MembraneHandle, MembraneMemoryBackend};
pub use types::{
    normalize_scope, path_to_scope, scope_chain, BackendBinding, BackendIdentity, BackendKind,
    FullMemoryRecord, JsonValue, MemoryEntry, MemoryId, MemoryListRow, MemoryTier, Score,
    ScoredMemoryEntry,
};
