//! Skills catalog (ENG-105): a read-only listing of every discovered skill
//! with the metadata a browsing UI needs, independent of whether the skill
//! is currently active. Distinct from [`crate::discover_skill_roots`], which
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::SkillManifest;
    use std::path::PathBuf;

    fn skill(name: &str, description: &str, tags: &[&str]) -> DiscoveredSkill {
        let manifest = SkillManifest {
            name: name.to_string(),
            description: description.to_string(),
            trigger: String::new(),
            enabled: true,
            lifecycle: SkillLifecycle::Active,
            version: Some("1.0.0".to_string()),
            author: Some("acme".to_string()),
            compatibility: Vec::new(),
            platforms: Vec::new(),
            required_environment_variables: Vec::new(),
            metadata: Default::default(),
            category: SkillCategory::Tool,
            priority: 0,
            disable_model_invocation: false,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            path_affinity: Vec::new(),
            effects: Default::default(),
            body: String::new(),
        };
        DiscoveredSkill {
            manifest,
            path: PathBuf::from(format!("/skills/{name}/SKILL.md")),
            scope: SkillSourceScope::User,
        }
    }

    #[test]
    fn build_catalog_sorts_by_name() {
        let skills = vec![
            skill("zeta", "z skill", &[]),
            skill("alpha", "a skill", &[]),
        ];
        let catalog = build_catalog(&skills);
        assert_eq!(catalog[0].name, "alpha");
        assert_eq!(catalog[1].name, "zeta");
    }

    #[test]
    fn build_catalog_excludes_body() {
        // Structural guarantee: SkillCatalogEntry has no `body` field at all,
        // so there is nothing to accidentally serialize a large skill's
        // instructions into a catalog listing.
        let skills = vec![skill("alpha", "a skill", &[])];
        let catalog = build_catalog(&skills);
        let json = serde_json::to_string(&catalog[0]).unwrap();
        assert!(!json.contains("\"body\""));
    }

    #[test]
    fn search_catalog_matches_name_description_and_tags() {
        let skills = vec![
            skill("rust-expert", "helps with rust code", &["lang"]),
            skill("react-expert", "helps with react code", &["frontend"]),
        ];
        let catalog = build_catalog(&skills);

        assert_eq!(search_catalog(&catalog, "rust").len(), 1);
        assert_eq!(search_catalog(&catalog, "helps with").len(), 2);
        assert_eq!(search_catalog(&catalog, "frontend").len(), 1);
        assert_eq!(search_catalog(&catalog, "").len(), 2);
        assert_eq!(search_catalog(&catalog, "nonexistent").len(), 0);
    }

    #[test]
    fn search_catalog_is_case_insensitive() {
        let skills = vec![skill("Rust-Expert", "Helps With RUST", &[])];
        let catalog = build_catalog(&skills);
        assert_eq!(search_catalog(&catalog, "RUST").len(), 1);
        assert_eq!(search_catalog(&catalog, "rust").len(), 1);
    }
}
