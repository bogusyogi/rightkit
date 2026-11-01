//! Host traits for RightKit agent runtimes.
//!
//! Skills, skill loaders and slash-command catalogs belong to the host product, not to RightKit.
//! The agent crates (`rightkit-agent-engine-core`, `rightkit-agent-tools`,
//! `rightkit-agent-orchestration`, `rightkit-agent-engine-mobile-runtime`) therefore never parse
//! or discover skills themselves. They ask a [`SkillCatalog`] that the host installs once at start
//! up with [`install_skill_catalog`]. With no catalog installed every query answers "no skills":
//! nothing is triggered, nothing parses and the catalog is empty.
//!
//! The types here are plain data so a host can fill them from whatever skill format it owns.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// Facts about the moment a skill trigger is evaluated (event name, tool, path, command, message).
///
/// Keys are free-form strings chosen by the runtime (`event`, `workspace_root`, `role`, `tool`,
/// `file_path`, `command`, `message`); the host decides which of them its trigger grammar reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggerContext {
    values: BTreeMap<String, String>,
    /// Kind of trigger that fired, for example `message` or `tool_call`.
    pub trigger_type: Option<String>,
    /// Payload that goes with [`TriggerContext::trigger_type`]: the message text or the tool name.
    pub trigger_payload: Option<String>,
}

impl TriggerContext {
    /// An empty context.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a fact, replacing an earlier value for the same key.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.values.insert(key.into(), value.into());
    }

    /// Read a fact.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// Every fact, ordered by key.
    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, String> {
        &self.values
    }
}

/// What a skill asks the runtime to enforce while it is triggered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillEffects {
    /// Named execution mode, empty when the skill sets none.
    pub execution_mode: String,
    /// Tool names (or `*`) the skill allows; empty means no restriction.
    pub allowed_tools: Vec<String>,
    /// Tool names (or `*`) the skill forbids.
    pub disallowed_tools: Vec<String>,
    /// Extra instruction text for the model, empty when the skill sets none.
    pub prompt: String,
}

/// A skill whose trigger or path affinity matched a [`TriggerContext`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggeredSkill {
    /// Slug the model passes to `invoke_skill`.
    pub slug: String,
    /// One-line description shown in the hint.
    pub description: String,
    /// Trigger expression, empty when the skill matches by path affinity only.
    pub trigger: String,
    /// Path-affinity patterns, empty when the skill has none.
    pub path_affinity: Vec<String>,
    /// Effects the runtime enforces while the skill is triggered.
    pub effects: SkillEffects,
}

/// The parts of a parsed skill manifest the agent crates use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillManifestSummary {
    /// Skill name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Instruction body after the front matter.
    pub body: String,
    /// Free-form tags.
    pub tags: Vec<String>,
    /// False when the skill is disabled, retired or otherwise not invocable.
    pub active: bool,
    /// Lifecycle label for diagnostics, for example `active` or `retired`.
    pub lifecycle: String,
}

/// One directory the host should scan when it lists the skill catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCatalogRoot {
    /// Host-defined scope label, for example `user`, `project` or `bundled`.
    pub scope: String,
    /// Directory to scan.
    pub path: PathBuf,
}

/// The skill services a host provides to the agent runtimes.
pub trait SkillCatalog: Send + Sync {
    /// Parse a skill file. `None` means the text is not a valid skill manifest.
    fn parse_manifest(&self, markdown: &str) -> Option<SkillManifestSummary>;

    /// Skills found under `skill_dirs` whose trigger expression or path affinity matches `context`.
    ///
    /// The host owns discovery and matching: it scans each directory for skills, skips skills the
    /// model may not invoke, evaluates trigger expressions and routes path affinity. Results are
    /// ordered by slug. The runtime keeps at most the first few.
    fn triggered_skills(
        &self,
        skill_dirs: &[PathBuf],
        context: &TriggerContext,
    ) -> Vec<TriggeredSkill>;

    /// List the skills found under `roots` as a JSON array, filtered by a case-insensitive
    /// substring `query` over name, description and tags when one is given.
    ///
    /// # Errors
    ///
    /// Returns a message when the catalog cannot be built.
    fn list_catalog(
        &self,
        roots: &[SkillCatalogRoot],
        query: Option<&str>,
    ) -> Result<serde_json::Value, String>;
}

static INSTALLED: RwLock<Option<Arc<dyn SkillCatalog>>> = RwLock::new(None);

/// Install the host's skill catalog, replacing any earlier one.
pub fn install_skill_catalog(catalog: Arc<dyn SkillCatalog>) {
    let mut slot = INSTALLED.write().unwrap_or_else(|e| e.into_inner());
    *slot = Some(catalog);
}

/// Remove the installed skill catalog.
pub fn clear_skill_catalog() {
    let mut slot = INSTALLED.write().unwrap_or_else(|e| e.into_inner());
    *slot = None;
}

/// The installed skill catalog, if the host installed one.
#[must_use]
pub fn skill_catalog() -> Option<Arc<dyn SkillCatalog>> {
    INSTALLED.read().unwrap_or_else(|e| e.into_inner()).clone()
}
