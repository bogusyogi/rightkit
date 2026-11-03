//! Thin RightKit/Membrane handoff contracts.
//!
//! Contracts stay separate from live adapters. Membrane remains owner of
//! context planning, ranking, admission, reduction, and semantic meaning.

// Crate-internal, not public API: `provider_client` reaches these delivery
// helpers by path. Widening to `pub` would export the adapter surface and
// break the contracts/adapters separation this module documents above.
pub(crate) mod delivery;
pub(crate) mod host_observation;
mod review;
mod unavailable;

pub use review::{
    BackgroundEventProvenanceV1, BackgroundMeasuredUsageV1, BackgroundReviewJobKindV1,
    BackgroundSemanticReviewBlockReasonV1, BackgroundSemanticReviewBudgetV1,
    BackgroundSemanticReviewFailureReasonV1, BackgroundSemanticReviewProvenanceV1,
    BackgroundSemanticReviewRequestV1, BackgroundSemanticReviewResultV1,
    BackgroundSemanticReviewStatusV1, BackgroundSessionEventV1, ForegroundMemoryCursorV1,
    ForegroundMemoryStateV1, LiveProviderAuthorityV1, MemoryCandidateV1, RestrictedCapabilitiesV1,
    SemanticCurationProposalV1,
};
pub use unavailable::{BindingSurfaceV1, UnavailableReasonV1, UnavailableStateV1};
