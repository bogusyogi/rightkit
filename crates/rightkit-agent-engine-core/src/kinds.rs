//! Artifact kinds — the typed catalog of reviewable artifacts an agent can produce.

/// A recognized artifact kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    Plan,
    Diff,
    TestReport,
    Screenshot,
    CostReport,
    Other,
}

impl ArtifactKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArtifactKind::Plan => "plan",
            ArtifactKind::Diff => "diff",
            ArtifactKind::TestReport => "test_report",
            ArtifactKind::Screenshot => "screenshot",
            ArtifactKind::CostReport => "cost_report",
            ArtifactKind::Other => "other",
        }
    }

    /// Whether this kind participates in trust attestation (jury/approval gates).
    pub fn is_trust_bearing(&self) -> bool {
        matches!(
            self,
            ArtifactKind::Diff | ArtifactKind::TestReport | ArtifactKind::Plan
        )
    }
}

/// Parse a wire string into an `ArtifactKind`. Unknown strings map to `Other`.
pub fn parse_artifact_kind(s: &str) -> ArtifactKind {
    match s {
        "plan" => ArtifactKind::Plan,
        "diff" => ArtifactKind::Diff,
        "test_report" => ArtifactKind::TestReport,
        "screenshot" => ArtifactKind::Screenshot,
        "cost_report" => ArtifactKind::CostReport,
        _ => ArtifactKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_known_kinds() {
        for k in [
            ArtifactKind::Plan,
            ArtifactKind::Diff,
            ArtifactKind::TestReport,
            ArtifactKind::Screenshot,
            ArtifactKind::CostReport,
        ] {
            assert_eq!(parse_artifact_kind(k.as_str()), k);
        }
    }

    #[test]
    fn unknown_maps_to_other() {
        assert_eq!(parse_artifact_kind("zzz"), ArtifactKind::Other);
    }

    #[test]
    fn trust_bearing_classification() {
        assert!(ArtifactKind::Diff.is_trust_bearing());
        assert!(ArtifactKind::TestReport.is_trust_bearing());
        assert!(!ArtifactKind::Screenshot.is_trust_bearing());
        assert!(!ArtifactKind::Other.is_trust_bearing());
    }
}
