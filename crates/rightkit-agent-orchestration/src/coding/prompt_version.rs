//! Prompt versioning and workflow variant infrastructure.
//! Tracks multiple versions of prompts and allows organizing them into workflow variants.

use std::collections::HashMap;

/// A versioned prompt template.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptVersion {
    /// Unique prompt identifier (e.g., "coder-system-prompt").
    pub id: String,
    /// The prompt template text.
    pub template: String,
    /// Version number (0, 1, 2, ...).
    pub version: u32,
    /// Tags for categorization (e.g., ["stable", "production"] or ["experimental"]).
    pub tags: Vec<String>,
}

/// A workflow variant — a named collection of prompt step ids.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowVariant {
    /// Unique workflow variant identifier (e.g., "aggressive-parallelization").
    pub id: String,
    /// Ordered list of prompt/step ids that form this workflow.
    pub steps: Vec<String>,
}

/// Manages a registry of prompt versions indexed by id.
pub struct PromptRegistry {
    /// All versions per prompt: prompt_id -> `Vec<PromptVersion>` ordered by version.
    versions: HashMap<String, Vec<PromptVersion>>,
    /// Maps turn_id -> prompt_version_id for tagging turns.
    turn_tags: HashMap<String, String>,
}

impl Default for PromptRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptRegistry {
    pub fn new() -> Self {
        Self {
            versions: HashMap::new(),
            turn_tags: HashMap::new(),
        }
    }

    /// Register a new version of a prompt.
    /// If this is the first registration, version 0 is assigned.
    pub fn register(
        &mut self,
        id: impl Into<String>,
        template: impl Into<String>,
        tags: Vec<String>,
    ) -> PromptVersion {
        let id = id.into();
        let template = template.into();

        let version = self
            .versions
            .get(&id)
            .map(|versions| versions.len() as u32)
            .unwrap_or(0);

        let pv = PromptVersion {
            id: id.clone(),
            template,
            version,
            tags,
        };

        self.versions.entry(id).or_default().push(pv.clone());

        pv
    }

    /// Retrieve a specific version of a prompt.
    pub fn get(&self, id: &str, version: u32) -> Option<&PromptVersion> {
        self.versions
            .get(id)
            .and_then(|versions| versions.iter().find(|pv| pv.version == version))
    }

    /// Get the latest version of a prompt.
    pub fn latest(&self, id: &str) -> Option<&PromptVersion> {
        self.versions.get(id).and_then(|versions| versions.last())
    }

    /// Tag a turn (task execution) with a specific prompt version.
    /// Useful for tracking which prompt version was used in a given run.
    pub fn tag_turn(&mut self, turn_id: impl Into<String>, prompt_version_id: impl Into<String>) {
        self.turn_tags
            .insert(turn_id.into(), prompt_version_id.into());
    }

    /// Retrieve the prompt version tagged for a given turn.
    pub fn get_turn_tag(&self, turn_id: &str) -> Option<&str> {
        self.turn_tags.get(turn_id).map(|s| s.as_str())
    }

    /// Get all versions of a prompt.
    pub fn all_versions(&self, id: &str) -> Option<&[PromptVersion]> {
        self.versions.get(id).map(|v| v.as_slice())
    }

    /// Get all prompts being tracked.
    pub fn all_prompt_ids(&self) -> Vec<&str> {
        self.versions.keys().map(|s| s.as_str()).collect()
    }
}

/// Manages a registry of workflow variants.
pub struct WorkflowVariantRegistry {
    /// All variants keyed by id.
    variants: HashMap<String, WorkflowVariant>,
}

impl Default for WorkflowVariantRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowVariantRegistry {
    pub fn new() -> Self {
        Self {
            variants: HashMap::new(),
        }
    }

    /// Register a workflow variant.
    pub fn register(&mut self, id: impl Into<String>, steps: Vec<String>) -> WorkflowVariant {
        let id = id.into();
        let wv = WorkflowVariant {
            id: id.clone(),
            steps,
        };
        self.variants.insert(id, wv.clone());
        wv
    }

    /// Retrieve a workflow variant by id.
    pub fn get(&self, id: &str) -> Option<&WorkflowVariant> {
        self.variants.get(id)
    }

    /// Get all variants.
    pub fn all(&self) -> Vec<&WorkflowVariant> {
        self.variants.values().collect()
    }
}
