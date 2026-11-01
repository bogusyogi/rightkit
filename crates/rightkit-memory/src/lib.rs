//! `rightkit-memory` — agent memory: store, screening, adaptive retrieval routing (FM-071), and a
//! typed backend ([`backend`]) over the external membrane CLI ([`membrane`]).
//!
//! Three-tier bandit router: `Low` (lexical only), `Mid` (hybrid),
//! `High` (full hybrid with wider candidate set). Bandit learns per-tier
//! rewards from run outcomes; falls back to lexical when the embedder is
//! unavailable.

pub mod backend;
pub mod consolidation_status;
pub mod membrane;
pub mod retriever;
pub mod screening;
pub mod store;

pub use consolidation_status::{
    ConsolidationRunOutcome, ConsolidationRunStatus, ConsolidationStatusState,
};
pub use retriever::{
    candidate_limit_for, curate_recall_candidates, rank_by_relevance_and_recency, use_embeddings,
    BanditStats, CuratedRecall, RecallCandidate, RecallDrop, RecallDropReason, RetrievalTier,
    RoutingPolicy, Tier,
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
pub mod memory;
#[cfg(feature = "agent_host")]
#[allow(missing_docs, clippy::all, rustdoc::all, unused)]
pub mod memory_provenance;
#[cfg(feature = "agent_host")]
#[allow(missing_docs, clippy::all, rustdoc::all, unused)]
pub mod memory_save_tool;
