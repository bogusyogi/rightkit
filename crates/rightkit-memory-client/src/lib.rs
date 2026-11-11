//! `rightkit-memory-client` — the client of the RightKit memory engine (`rightkit-memory`, which
//! is `rightkit-memory-core` and `rightkit-memory-store`): a typed backend ([`backend`]) over the
//! external membrane CLI ([`membrane`]), durable-write screening, the consolidation-status
//! projection and a small in-memory `MemoryStore`. Tiers and memory rows are the engine's own
//! types (`rightkit_memory_core::{MemoryTier, MemoryEntry}`). The engine owns ranking, tiering and
//! retrieval; this crate ships no embedder and no retrieval router.

pub mod backend;
pub mod consolidation_status;
pub mod membrane;
pub mod screening;
pub mod store;

pub use consolidation_status::{
    ConsolidationRunOutcome, ConsolidationRunStatus, ConsolidationStatusState,
};
pub use screening::{screen, RejectReason, ScreeningVerdict, MIN_DURABLE_LEN};
pub use store::{MemoryRecord, MemoryStore, NewMemory, WriteRejection};

#[cfg(feature = "agent_host")]
#[allow(missing_docs, clippy::all, rustdoc::all, unused)]
pub mod api_memright;
#[cfg(feature = "agent_host")]
#[allow(missing_docs, clippy::all, rustdoc::all, unused)]
pub mod blueprint_tools;
#[cfg(feature = "agent_host")]
#[allow(missing_docs, clippy::all, rustdoc::all, unused)]
pub mod memory_provenance;
#[cfg(feature = "agent_host")]
#[allow(missing_docs, clippy::all, rustdoc::all, unused)]
pub mod memory_save_tool;
