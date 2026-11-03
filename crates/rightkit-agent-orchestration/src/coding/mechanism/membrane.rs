//! Membrane bindings — provider-authoritative source references.
//!
//! Canon (PL-010): "Membrane bindings honor provider authority
//! (`context.resolve`/Blueprint/Ledger/Cortex/source.read)."
//!
//! The standing the host app/Membrane handoff contract
//! (`engine/host-app/src/membrane_binding/`) fixes the direction of
//! authority: **the host app supplies provider authority & lifecycle facts;
//! Membrane owns semantic interpretation, proposal validation, and durable
//! admission.** This module carries that direction into mechanism resolution:
//!
//! - A [`MembraneBinding`] is a *reference* to a provider-authoritative
//!   surface, never a carrier of content. The resolver never ranks, reads, or
//!   interprets binding content — that stays with Membrane.
//! - A binding bound into a decision must be honored: a
//!   [`MechanismClass::SourceResolver`](super::class::MechanismClass::SourceResolver)
//!   answer is verified by coverage of the bound sources, and a semantic-model
//!   answer may never *replace* a bound source (the verification contract pins
//!   the bound refs; see `super::decision::VerificationContract`).
//! - A binding without an authority revision cannot be honored (fail-closed,
//!   mirroring `LiveProviderAuthorityV1::validate` in the handoff contracts).

use serde::{Deserialize, Serialize};

/// The provider-authoritative surfaces a Membrane binding may reference.
/// Names follow the canon row verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MembraneSurface {
    /// `context.resolve` — the resolved-context authority.
    ContextResolve,
    /// Blueprint — plan/evidence-ref authority.
    Blueprint,
    /// Ledger — durable indexed-fact authority.
    Ledger,
    /// Cortex — durable record authority.
    Cortex,
    /// `source.read` — direct authoritative source reads.
    SourceRead,
}

impl MembraneSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContextResolve => "context.resolve",
            Self::Blueprint => "blueprint",
            Self::Ledger => "ledger",
            Self::Cortex => "cortex",
            Self::SourceRead => "source.read",
        }
    }
}

/// A typed reference to one provider-authoritative surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MembraneBinding {
    pub surface: MembraneSurface,
    /// Stable source reference within the surface (id/path/query handle).
    /// Opaque here: Membrane owns its meaning.
    pub source_ref: String,
    /// The authority revision this binding was issued against. Empty is
    /// invalid — provider authority is always revision-stamped.
    pub authority_revision: String,
}

impl MembraneBinding {
    pub fn new(
        surface: MembraneSurface,
        source_ref: impl Into<String>,
        authority_revision: impl Into<String>,
    ) -> Self {
        Self {
            surface,
            source_ref: source_ref.into(),
            authority_revision: authority_revision.into(),
        }
    }

    /// Fail-closed validation mirroring the handoff contracts: a binding
    /// without a source ref or without an authority revision cannot honor
    /// provider authority and is rejected before resolution proceeds.
    pub fn validate(&self) -> Result<(), MembraneBindingError> {
        if self.source_ref.trim().is_empty() {
            return Err(MembraneBindingError::MissingSourceRef);
        }
        if self.authority_revision.trim().is_empty() {
            return Err(MembraneBindingError::MissingAuthorityRevision);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MembraneBindingError {
    #[error("membrane binding carries no source_ref; provider authority cannot be honored")]
    MissingSourceRef,
    #[error(
        "membrane binding carries no authority_revision; provider authority cannot be honored"
    )]
    MissingAuthorityRevision,
}
