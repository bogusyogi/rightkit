//! Derived-projection regeneration seam (PL-011).
//!
//! Guidance artifacts feed derived projections (compiled prompt sections,
//! indexes, rendered packets). After the consumer commits a guidance mutation,
//! every registered projection reading the mutated surface is regenerated and
//! each outcome is recorded on the application receipt — regeneration is never
//! silently skipped and never silently fails.
//!
//! ## Determinism invariant (FM-118/AO-02 shape)
//!
//! [`GuidanceProjection::regenerate`] MUST be a pure, deterministic derivation
//! of the records it is given: same input records → same output digest. That
//! is what makes restart/replay rebuild identical derived state instead of
//! persisting divergent projections.

use super::guidance::GuidanceRecord;
use super::proposal::GuidanceKind;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One projection's regeneration outcome on the receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ProjectionResult {
    Regenerated { digest: String },
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionOutcome {
    pub projection_id: String,
    pub result: ProjectionResult,
}

impl ProjectionOutcome {
    pub fn is_failed(&self) -> bool {
        matches!(self.result, ProjectionResult::Failed { .. })
    }
}

/// A derived projection over one guidance surface (or all surfaces when
/// [`GuidanceProjection::applies_to`] returns `None`).
pub trait GuidanceProjection: Send + Sync {
    fn projection_id(&self) -> &str;

    /// `Some(kind)` scopes the projection to one guidance surface; `None`
    /// applies to every surface.
    fn applies_to(&self) -> Option<GuidanceKind>;

    /// Rebuild the projection from `records`. Implementations MUST be pure and
    /// deterministic (same records → same digest) and MUST NOT consult
    /// external mutable state; on failure they return a typed error string
    /// which the consumer surfaces on the application receipt.
    fn regenerate(&self, kind: GuidanceKind, records: &[GuidanceRecord]) -> Result<String, String>;
}

/// Registry of projections the consumer regenerates after each mutation.
#[derive(Default)]
pub struct ProjectionRegistry {
    projections: Vec<Arc<dyn GuidanceProjection>>,
}

impl ProjectionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, projection: Arc<dyn GuidanceProjection>) {
        self.projections.push(projection);
    }

    pub fn is_empty(&self) -> bool {
        self.projections.is_empty()
    }

    /// Regenerate every projection applicable to `kind`. Each projection's
    /// failure is captured per-projection — one failing projection never
    /// prevents the others from regenerating, and never disappears from the
    /// receipt.
    pub fn regenerate_for(
        &self,
        kind: GuidanceKind,
        records: &[GuidanceRecord],
    ) -> Vec<ProjectionOutcome> {
        self.projections
            .iter()
            .filter(|p| p.applies_to().map_or(true, |k| k == kind))
            .map(|p| {
                let outcome = match p.regenerate(kind, records) {
                    Ok(digest) => ProjectionResult::Regenerated { digest },
                    Err(error) => ProjectionResult::Failed { error },
                };
                ProjectionOutcome {
                    projection_id: p.projection_id().to_string(),
                    result: outcome,
                }
            })
            .collect()
    }
}
