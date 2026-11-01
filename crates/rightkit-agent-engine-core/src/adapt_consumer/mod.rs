//! Adapt-proposal host consumer (canon atom `PL-011`, Refactor 0.5 dispatch D1, action `ORIGINAL`).
//!
//! ## Contract (canon PL-011 row + Membrane/Adapt H7 handoff contract)
//!
//! An approved Adapt proposal is **typed evidence & intent — never direct
//! mutation authority**. the host app applies it under its own authority/policy
//! with:
//!
//! 1. **Replace-first discipline** — modify/refine the narrowest existing
//!    owning guidance; append only when no owner can carry the contract
//!    without semantic loss. Enforced mechanically by [`guidance::resolve_owners`]
//!    and the apply pipeline's `ReplaceFirstViolation` rejection.
//! 2. **Duplicate/contradiction checks** — a proposal that restates existing
//!    guidance verbatim is rejected as `Duplicate`; guidance competing with an
//!    unresolved same-subject owner is rejected as `Contradiction`.
//! 3. **Derived-projection regeneration** — after a committed mutation, every
//!    registered [`projection::GuidanceProjection`] reading the mutated
//!    guidance surface is regenerated, and the receipt records each outcome
//!    (never silently dropped).
//! 4. **Stale-base CAS** — the proposal binds `expected_base_digest`; the host
//!    resolves `observed_base_digest` from the owning artifact. Mismatch
//!    rejects as `StaleBase` — never silently applied. The commit itself is a
//!    second compare-and-swap ([`guidance::GuidanceStore::replace_if_unchanged`])
//!    so a mutation racing between observation and commit is also rejected as
//!    stale.
//! 5. **Typed mutation kinds** — [`proposal::MutationKind::Refine`] (itemized
//!    section replacement of the owning artifact, never a wholesale rewrite)
//!    and [`proposal::MutationKind::Append`] (only when unowned).
//! 6. **Application receipts** — every attempt, applied or rejected, produces
//!    an [`apply::ApplicationReceipt`] with a derived (replay-dedupable) id.
//!
//! ## Shape lineage
//!
//! No compatible donor exists (ORIGINAL atom). The type shapes follow the
//! Membrane/Adapt H7 handoff contract documented in
//! `docs/pending/CODERIGHT-MEMBRANE-INTEGRATION.md` §10:
//! `SealedRemediationProposalV1` + `intervention_target` (+ mutation-eligible
//! `InterventionAttributionV1`), `expected_base_digest` binding per candidate,
//! itemized deltas against the base artifact, and the gate-10 rule that a
//! variant preferring replacement over appending wins and a wholesale rewrite
//! loses. No third-party code is used; no NOTICE entry is required.
//!
//! ## Seam (house style, cf. `run_origin.rs`)
//!
//! This crate does not depend on the daemon's storage/skills/prompt stores.
//! The host wires concrete implementations of three traits at integration:
//!
//! - [`guidance::GuidanceStore`] — the mutable guidance artifacts (skills,
//!   procedures, system prompts, tool descriptions, documentation policy).
//!   The in-memory implementation is the reference model used by tests.
//! - [`authority::ApplicationAuthorityGate`] — the host app's own policy decision
//!   that authorizes application. The fail-closed default
//!   ([`authority::DenyAllAuthority`]) refuses everything; a proposal's
//!   approval alone never authorizes anything.
//! - [`projection::GuidanceProjection`] — deterministic derived projections
//!   (compiled prompt sections, indexes) rebuilt after each mutation.
//!
//! The single entry point is [`apply::apply_proposal`]. It is synchronous by
//! design (small text artifacts; the daemon can wrap it in its own async
//! plumbing, as it does for the run-origin seam).
//!
//! Not wired into any daemon route yet: nothing outside this module calls
//! [`apply::apply_proposal`] — see the lane wiring note for the orchestrator's
//! integration steps.

pub mod apply;
pub mod authority;
pub mod guidance;
pub mod projection;
pub mod proposal;

pub use apply::{
    apply_proposal, derived_receipt_id, ApplicationOutcome, ApplicationReceipt,
    ApplicationReceiptLog, DigestBinding, InMemoryApplicationReceiptLog, RejectionKind,
    APPLICATION_RECEIPT_DOMAIN,
};
pub use authority::{
    ApplicationAuthorityGate, AuthorityDecision, DenyAllAuthority, PolicyRule,
    StaticPolicyAuthority,
};
pub use guidance::{
    covers, resolve_owners, surface_digest, GuidanceRecord, GuidanceStore, GuidanceStoreError,
    InMemoryGuidanceStore, NewGuidanceRecord, OwnerResolution,
};
pub use projection::{GuidanceProjection, ProjectionOutcome, ProjectionRegistry, ProjectionResult};
pub use proposal::{
    guidance_content_digest, normalize_guidance_text, parse_sections, render_section_marker,
    render_sections, replace_section_body, validate_subject, AdaptReviewAction, EvidenceKind,
    IntakeError, InterventionAttributionV1, InterventionTargetV1, MutationKind, ProposalApproval,
    ProposalDelta, ProposalEvidenceRef, SealedRemediationProposalV1, Section, SectionDeltaError,
    SectionedContent, ADAPT_CONTRACT_VERSION, SECTION_MARKER_PREFIX, SECTION_MARKER_SUFFIX,
};
