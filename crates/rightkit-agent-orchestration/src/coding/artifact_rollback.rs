//! Per-artifact rollback — track versions of generated artifacts (prompts, configs, etc.)
//! and allow rolling back to prior versions independently from model rollout.

use std::collections::HashMap;

/// A versioned artifact with metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactVersion {
    /// Unique artifact identifier.
    pub artifact_id: String,
    /// Version number (0, 1, 2, ...).
    pub version: u32,
    /// Content hash (e.g., SHA256) for integrity verification.
    pub content_hash: String,
    /// ISO 8601 timestamp when this version was created.
    pub created_at: String,
}

/// Manages per-artifact version history and rollback.
pub struct ArtifactRollback {
    /// All versions per artifact: artifact_id -> `Vec<ArtifactVersion>` ordered by version ascending.
    history: HashMap<String, Vec<ArtifactVersion>>,
}

impl Default for ArtifactRollback {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtifactRollback {
    pub fn new() -> Self {
        Self {
            history: HashMap::new(),
        }
    }

    /// Record a new version of an artifact.
    /// If this is the first recording, version 0 is assigned; otherwise the version
    /// is one higher than the current max.
    pub fn record(
        &mut self,
        artifact_id: impl Into<String>,
        content_hash: impl Into<String>,
        created_at: impl Into<String>,
    ) -> ArtifactVersion {
        let artifact_id = artifact_id.into();
        let content_hash = content_hash.into();
        let created_at = created_at.into();

        let version = self
            .history
            .get(&artifact_id)
            .map(|versions| versions.len() as u32)
            .unwrap_or(0);

        let av = ArtifactVersion {
            artifact_id: artifact_id.clone(),
            version,
            content_hash,
            created_at,
        };

        self.history
            .entry(artifact_id)
            .or_default()
            .push(av.clone());

        av
    }

    /// Retrieve a specific version of an artifact, or None if not found.
    pub fn get(&self, artifact_id: &str, version: u32) -> Option<&ArtifactVersion> {
        self.history
            .get(artifact_id)
            .and_then(|versions| versions.iter().find(|av| av.version == version))
    }

    /// Get the latest version of an artifact, or None if the artifact has no versions.
    pub fn latest(&self, artifact_id: &str) -> Option<&ArtifactVersion> {
        self.history
            .get(artifact_id)
            .and_then(|versions| versions.last())
    }

    /// Roll back an artifact to a prior version.
    /// Returns the artifact at that version, or None if the artifact/version does not exist.
    pub fn rollback(&mut self, artifact_id: &str, to_version: u32) -> Option<ArtifactVersion> {
        let versions = self.history.get(artifact_id)?;
        versions.iter().find(|av| av.version == to_version).cloned()
    }

    /// Retrieve the complete version history for an artifact (all versions in order).
    pub fn history(&self, artifact_id: &str) -> Option<&[ArtifactVersion]> {
        self.history.get(artifact_id).map(|v| v.as_slice())
    }

    /// Get all artifacts being tracked.
    pub fn all_artifacts(&self) -> Vec<&str> {
        self.history.keys().map(|s| s.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_first_version() {
        let mut ar = ArtifactRollback::new();
        let v = ar.record("prompt.txt", "hash1", "2026-06-05T10:00:00Z");
        assert_eq!(v.version, 0);
        assert_eq!(v.artifact_id, "prompt.txt");
        assert_eq!(v.content_hash, "hash1");
    }

    #[test]
    fn record_incremental_versions() {
        let mut ar = ArtifactRollback::new();
        let v0 = ar.record("prompt.txt", "hash0", "2026-06-05T10:00:00Z");
        let v1 = ar.record("prompt.txt", "hash1", "2026-06-05T11:00:00Z");
        let v2 = ar.record("prompt.txt", "hash2", "2026-06-05T12:00:00Z");

        assert_eq!(v0.version, 0);
        assert_eq!(v1.version, 1);
        assert_eq!(v2.version, 2);
    }

    #[test]
    fn get_specific_version() {
        let mut ar = ArtifactRollback::new();
        ar.record("prompt.txt", "hash0", "2026-06-05T10:00:00Z");
        ar.record("prompt.txt", "hash1", "2026-06-05T11:00:00Z");
        ar.record("prompt.txt", "hash2", "2026-06-05T12:00:00Z");

        let v1 = ar.get("prompt.txt", 1).unwrap();
        assert_eq!(v1.content_hash, "hash1");
        assert_eq!(v1.version, 1);

        assert!(ar.get("prompt.txt", 999).is_none());
        assert!(ar.get("nonexistent.txt", 0).is_none());
    }

    #[test]
    fn latest_version() {
        let mut ar = ArtifactRollback::new();
        ar.record("prompt.txt", "hash0", "2026-06-05T10:00:00Z");
        ar.record("prompt.txt", "hash1", "2026-06-05T11:00:00Z");
        ar.record("prompt.txt", "hash2", "2026-06-05T12:00:00Z");

        let latest = ar.latest("prompt.txt").unwrap();
        assert_eq!(latest.version, 2);
        assert_eq!(latest.content_hash, "hash2");

        assert!(ar.latest("nonexistent.txt").is_none());
    }

    #[test]
    fn rollback_to_prior_version() {
        let mut ar = ArtifactRollback::new();
        ar.record("prompt.txt", "hash0", "2026-06-05T10:00:00Z");
        ar.record("prompt.txt", "hash1", "2026-06-05T11:00:00Z");
        ar.record("prompt.txt", "hash2", "2026-06-05T12:00:00Z");

        let rolled_back = ar.rollback("prompt.txt", 0).unwrap();
        assert_eq!(rolled_back.content_hash, "hash0");
        assert_eq!(rolled_back.version, 0);
    }

    #[test]
    fn rollback_nonexistent() {
        let mut ar = ArtifactRollback::new();
        assert!(ar.rollback("nonexistent.txt", 0).is_none());
    }

    #[test]
    fn history_retrieval() {
        let mut ar = ArtifactRollback::new();
        ar.record("prompt.txt", "hash0", "2026-06-05T10:00:00Z");
        ar.record("prompt.txt", "hash1", "2026-06-05T11:00:00Z");
        ar.record("prompt.txt", "hash2", "2026-06-05T12:00:00Z");

        let hist = ar.history("prompt.txt").unwrap();
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0].version, 0);
        assert_eq!(hist[1].version, 1);
        assert_eq!(hist[2].version, 2);

        assert!(ar.history("nonexistent.txt").is_none());
    }

    #[test]
    fn multiple_artifacts() {
        let mut ar = ArtifactRollback::new();
        ar.record("prompt.txt", "hash0", "2026-06-05T10:00:00Z");
        ar.record("config.yaml", "confighash0", "2026-06-05T10:00:00Z");

        assert_eq!(ar.latest("prompt.txt").unwrap().content_hash, "hash0");
        assert_eq!(
            ar.latest("config.yaml").unwrap().content_hash,
            "confighash0"
        );

        let artifacts = ar.all_artifacts();
        assert_eq!(artifacts.len(), 2);
        assert!(artifacts.contains(&"prompt.txt"));
        assert!(artifacts.contains(&"config.yaml"));
    }

    #[test]
    fn rollback_preserves_history() {
        let mut ar = ArtifactRollback::new();
        ar.record("prompt.txt", "hash0", "2026-06-05T10:00:00Z");
        ar.record("prompt.txt", "hash1", "2026-06-05T11:00:00Z");

        let _ = ar.rollback("prompt.txt", 0);

        let hist = ar.history("prompt.txt").unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].content_hash, "hash0");
        assert_eq!(hist[1].content_hash, "hash1");
    }
}
