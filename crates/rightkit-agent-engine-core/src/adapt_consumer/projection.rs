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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapt_consumer::proposal::guidance_content_digest;

    /// Deterministic test projection: digest over sorted record ids+digests.
    struct RecordingProjection {
        id: &'static str,
        scope: Option<GuidanceKind>,
    }

    impl GuidanceProjection for RecordingProjection {
        fn projection_id(&self) -> &str {
            self.id
        }

        fn applies_to(&self) -> Option<GuidanceKind> {
            self.scope
        }

        fn regenerate(
            &self,
            _kind: GuidanceKind,
            records: &[GuidanceRecord],
        ) -> Result<String, String> {
            let mut lines: Vec<String> = records
                .iter()
                .map(|r| format!("{}\0{}\n", r.guidance_id, r.digest))
                .collect();
            lines.sort();
            Ok(rightkit_fs::digest::sha256_bytes(lines.concat().as_bytes()))
        }
    }

    struct FailingProjection;

    impl GuidanceProjection for FailingProjection {
        fn projection_id(&self) -> &str {
            "failing"
        }

        fn applies_to(&self) -> Option<GuidanceKind> {
            None
        }

        fn regenerate(
            &self,
            _kind: GuidanceKind,
            _records: &[GuidanceRecord],
        ) -> Result<String, String> {
            Err("projection backend unavailable".into())
        }
    }

    fn record(id: &str, kind: GuidanceKind, content: &str) -> GuidanceRecord {
        GuidanceRecord {
            guidance_id: id.into(),
            kind,
            subject: "s".into(),
            content: content.into(),
            digest: guidance_content_digest(content),
        }
    }

    #[test]
    fn registry_regenerates_only_applicable_projections() {
        let mut registry = ProjectionRegistry::new();
        registry.register(Arc::new(RecordingProjection {
            id: "skill-index",
            scope: Some(GuidanceKind::Skill),
        }));
        registry.register(Arc::new(RecordingProjection {
            id: "all-surface",
            scope: None,
        }));

        let records = vec![record("a", GuidanceKind::Skill, "A")];
        let for_skill = registry.regenerate_for(GuidanceKind::Skill, &records);
        assert_eq!(for_skill.len(), 2, "scoped + unscoped both run");
        assert!(for_skill.iter().all(|o| !o.is_failed()));

        let for_prompt = registry.regenerate_for(GuidanceKind::SystemPrompt, &records);
        assert_eq!(
            for_prompt.len(),
            1,
            "the skill-scoped projection must not run for another surface"
        );
        assert_eq!(for_prompt[0].projection_id, "all-surface");
    }

    #[test]
    fn projection_failure_is_typed_not_silent() {
        let mut registry = ProjectionRegistry::new();
        registry.register(Arc::new(FailingProjection));
        let outcomes = registry.regenerate_for(GuidanceKind::Skill, &[]);
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].is_failed());
        match &outcomes[0].result {
            ProjectionResult::Failed { error } => {
                assert!(error.contains("backend unavailable"));
            }
            ProjectionResult::Regenerated { .. } => panic!("expected typed failure"),
        }
    }

    #[test]
    fn regeneration_is_deterministic_for_identical_records() {
        let mut registry = ProjectionRegistry::new();
        registry.register(Arc::new(RecordingProjection {
            id: "det",
            scope: None,
        }));
        let records = vec![
            record("a", GuidanceKind::Skill, "A"),
            record("b", GuidanceKind::Skill, "B"),
        ];
        let first = registry.regenerate_for(GuidanceKind::Skill, &records);
        let second = registry.regenerate_for(GuidanceKind::Skill, &records);
        assert_eq!(
            first, second,
            "same records must rebuild identical derived state"
        );
    }
}
