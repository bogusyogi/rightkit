//! cortex-core — pure in-memory logic for the Cortex memory tier system.
//!
//! Models memory tiers, retrieval ranking, a quality eval gate, and rough token
//! accounting. Persistence lives in a separate storage crate; this crate has no I/O.
//!
//! The re-exports below form the stable crate API. Compatibility is governed
//! by crate semver and each typed record's schema version; internal modules
//! remain private unless explicitly marked as part of this surface.

pub mod absorbed;
mod accounting;
mod dream;
mod effectiveness;
pub mod embed;
mod eval_gate;
pub mod fingerprint;
mod graph;
pub mod planner;
mod quant;
mod registry;
pub mod retriever;
pub mod review;
mod routing;
pub mod transcript;
pub mod types;
mod vector_index;

pub use absorbed::{
    content_hash, event_range, validate_event, validate_event_import, AbsorbedValidationError,
    ArtifactRecord, EventCursor, ProvenanceRef, RecordGovernance, SessionEvent, SessionRecord,
    TaskRecord, ABSORBED_SCHEMA_VERSION,
};
pub use accounting::{estimate_tokens, ContextTokenAccounting};
pub use dream::{
    consolidate_dream_memories, DreamAgentPolicy, DreamConsolidatedMemory, DreamPlan, DreamStatus,
};
pub use effectiveness::{EffectivenessGate, MemoryUsageRecord, Outcome};
pub use embed::{EmbedProviderReport, Embedder, HashEmbedder, EMBEDDING_MAX_SEQUENCE_TOKENS};
pub use eval_gate::{EvalGateConfig, MemoryRetrievalEvalGate};
pub use fingerprint::PipelineFingerprint;
pub use graph::{
    is_canonical_relation, is_observation_relation, relation_category, EdgeRejection,
    EvidenceRelation, MemoryEdge, MemoryGraph, MemoryNode, RelationCategory, RelationDiagnostic,
    RelationProvenance, CANONICAL_RELATIONS,
};
pub use planner::{
    plan, BlockV1, BudgetV1, CandidateV1, ContextCandidateSetV1, ContextPacketV1, ContextReceiptV2,
    DegradationReason, FallbackMode, OmissionV1, PlannerError, PlannerInput, PlannerOutput,
    ProviderCeilingV1, ProviderStatus, StructuredFallbackEvent,
};
pub use quant::{quantized_cosine, QuantizedVector};
pub use registry::{MemoryRegistry, RegistryError};
pub use retriever::{CortexProjection, LexicalHit, MemoryRetriever, ProjectedEntry, ProjectedKind};
pub use review::{
    bound_memory_candidate_extraction_window, ExtractionWindowV1, ForegroundMemoryEmissionV1,
    MemoryCandidateExtractionBlockerV1, MemoryCandidateExtractionDecisionV1,
    MemoryCandidateExtractionLimitsV1, MemoryCandidateExtractionSkipV1, MemoryCandidateV1,
    ReviewContractError, ReviewEvidenceRefV1, SemanticCurationBlockerV1, SemanticCurationKindV1,
    SemanticCurationProposalV1, SemanticCurationStatusV1,
    MEMORY_CANDIDATE_EXTRACTION_SCHEMA_VERSION, SEMANTIC_CURATION_PROPOSAL_SCHEMA_VERSION,
};
pub use routing::{QueryFeatures, RetrievalTier, RoutingPolicy};
pub use transcript::{
    build_transcript_chunks, retrieve_transcript_chunks, TranscriptChunk, TranscriptChunkBuilder,
    TranscriptChunkConfig, TranscriptError, TranscriptRetrievalHit,
    TRANSCRIPT_CHUNK_SCHEMA_VERSION,
};
pub use types::{default_scope, MemoryEntry, MemoryTier};
pub use vector_index::{
    HostVectorPolicy, VectorCandidate, VectorIndex, VectorKernel, VectorSearchError,
};
