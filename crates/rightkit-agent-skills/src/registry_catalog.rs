//! Skills catalog (ENG-105): a read-only listing of every discovered skill
//! with the metadata a browsing UI needs, independent of whether the skill
//! is currently active. Distinct from [`crate::manifest::discover_skill_roots`], which
//! is the runtime discovery/precedence resolver used to decide *which*
//! manifest wins by name — this module wraps that same discovery output into
//! a stable, serializable shape a client (the iOS `SkillsCatalogView`, a
//! future CLI `skills list`, etc.) can render without re-parsing manifests.

use serde::{Deserialize, Serialize};

use crate::manifest::{DiscoveredSkill, SkillCategory, SkillLifecycle, SkillSourceScope};

/// One catalog row: everything a browsing UI needs to list and filter a
/// skill without holding onto the full manifest (in particular, never the
/// markdown `body`, which can be large and is irrelevant to a catalog view).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogEntry {
    pub name: String,
    pub description: String,
    pub category: SkillCategory,
    pub lifecycle: SkillLifecycle,
    pub is_active: bool,
    pub scope: SkillSourceScope,
    pub tags: Vec<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    /// Path as a display string (not a filesystem capability) — a UI can show
    /// provenance without being handed anything it could use to escape its
    /// sandbox.
    pub source_path: String,
}

impl From<&DiscoveredSkill> for SkillCatalogEntry {
    fn from(skill: &DiscoveredSkill) -> Self {
        Self {
            name: skill.manifest.name.clone(),
            description: skill.manifest.description.clone(),
            category: skill.manifest.category,
            lifecycle: skill.manifest.lifecycle,
            is_active: skill.manifest.is_active(),
            scope: skill.scope,
            tags: skill.manifest.tags.clone(),
            version: skill.manifest.version.clone(),
            author: skill.manifest.author.clone(),
            source_path: skill.path.display().to_string(),
        }
    }
}

/// Build a stable catalog from already-discovered skills, sorted by name so
/// repeated calls over the same discovery set produce identical output
/// (important for UI diffing and for deterministic tests).
pub fn build_catalog(skills: &[DiscoveredSkill]) -> Vec<SkillCatalogEntry> {
    let mut entries: Vec<SkillCatalogEntry> = skills.iter().map(SkillCatalogEntry::from).collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Filter a catalog down to entries matching a case-insensitive substring
/// search over name, description, and tags. An empty query returns the full
/// catalog unchanged.
pub fn search_catalog<'a>(
    catalog: &'a [SkillCatalogEntry],
    query: &str,
) -> Vec<&'a SkillCatalogEntry> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return catalog.iter().collect();
    }
    catalog
        .iter()
        .filter(|entry| {
            entry.name.to_lowercase().contains(&needle)
                || entry.description.to_lowercase().contains(&needle)
                || entry
                    .tags
                    .iter()
                    .any(|tag| tag.to_lowercase().contains(&needle))
        })
        .collect()
}
