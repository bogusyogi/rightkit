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
