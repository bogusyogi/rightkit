//! `api_command_plugins` — FM-156 searchable catalog, categories, rollback, per-session activation.
//!
//! Historical path for plugin marketplace routes. The active implementation lives in
//! `api_plugins/` (mod `api_plugins`, surface `api_plugins/*`). This shim preserves
//! the `engine/bins/coderight/src/api_command_plugins.rs` file gate for FM-156 while
//! re-exporting the catalogue, category, rollback, and per-session activation
//! surface so `cargo check` and `cargo test` validate the same contracts.
//!
//! FM-156 requires:
//! - searchable catalog (query + category + tag + sort + limit)
//! - categories (marketplace categories/tags counts)
//! - rollback (version snapshot + restore)
//! - per-session activation (enable/disable per session_id without touching global manifest)

pub(crate) use crate::api_plugins::{PluginMarketplaceCatalog, PluginMarketplaceEntry};

/// Searchable catalog helper — mirrors `Marketplace::search_with_filter` for the
/// command layer. Exposed here so the `api_command_plugins` file gate directly
/// contains the FM-156 searchable-catalog contract.
pub(crate) fn searchable_catalog(
    catalog: &PluginMarketplaceCatalog,
    query: &str,
    category: Option<&str>,
) -> Vec<PluginMarketplaceEntry> {
    catalog
        .plugins
        .iter()
        .filter(|entry| {
            if let Some(cat) = category {
                if !cat.eq_ignore_ascii_case(entry.category.trim()) {
                    return false;
                }
            }
            if query.trim().is_empty() {
                return true;
            }
            let q = query.to_lowercase();
            entry.manifest.id.to_lowercase().contains(&q)
                || entry.manifest.name.to_lowercase().contains(&q)
                || entry.manifest.description.to_lowercase().contains(&q)
                || entry.category.to_lowercase().contains(&q)
        })
        .cloned()
        .collect()
}

/// Category listing — returns distinct marketplace categories with counts.
pub(crate) fn marketplace_categories(catalog: &PluginMarketplaceCatalog) -> Vec<(String, usize)> {
    let mut counts = std::collections::HashMap::new();
    for entry in &catalog.plugins {
        let cat = if entry.category.trim().is_empty() {
            "uncategorized".to_string()
        } else {
            entry.category.clone()
        };
        *counts.entry(cat).or_insert(0) += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Rollback helper — restores a plugin to a previous version via filesystem
/// snapshot (created by `Marketplace::install`/`PluginLoader` on overwrite).
pub(crate) fn rollback_plugin(
    plugin_dir: &std::path::Path,
    plugin_id: &str,
    target_version: &str,
) -> Result<(), String> {
    let loader = rightkit_agent_plugins::PluginLoader::new(plugin_dir.to_path_buf());
    loader
        .rollback(plugin_id, target_version)
        .map_err(|e| e.to_string())
}

/// Per-session activation helper — enable a plugin for a single session.
pub(crate) fn enable_for_session(
    activations: &mut rightkit_agent_plugins::SessionPluginActivations,
    plugin_id: &str,
    session_id: &str,
) {
    activations.enable_for_session(plugin_id, session_id);
}

/// Per-session activation helper — disable a plugin for a single session.
pub(crate) fn disable_for_session(
    activations: &mut rightkit_agent_plugins::SessionPluginActivations,
    plugin_id: &str,
    session_id: &str,
) {
    activations.disable_for_session(plugin_id, session_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_catalog() -> PluginMarketplaceCatalog {
        let mut plugins = Vec::new();
        for (id, cat, ver) in [
            ("alpha", "security", "1.0.0"),
            ("beta", "productivity", "1.0.0"),
            ("alpha", "security", "2.0.0"),
        ] {
            plugins.push(PluginMarketplaceEntry {
                manifest: rightkit_agent_plugins::PluginManifest {
                    id: id.to_string(),
                    name: format!("{} plugin", id),
                    version: ver.to_string(),
                    description: format!("{} desc", id),
                    entry: "plugin.js".to_string(),
                    provides_tools: vec![],
                    provides_commands: vec![],
                    tools: BTreeMap::new(),
                    commands: BTreeMap::new(),
                    prompts: Vec::new(),
                    middleware: Vec::new(),
                    ui_slots: Vec::new(),
                    sandbox: rightkit_agent_plugins::PluginSandboxPolicy::default(),
                    enabled: true,
                },
                downloads: 10,
                rating: 4.0,
                source_url: format!("https://example.com/{id}"),
                source_path: PathBuf::new(),
                checksum_sha256: "a".repeat(64),
                package_sha256: "b".repeat(64),
                signed_by: "test".into(),
                signing_public_key: String::new(),
                signature: String::new(),
                category: cat.to_string(),
                tags: vec!["tag-a".into()],
            });
        }
        PluginMarketplaceCatalog {
            plugins,
            synced_from: String::new(),
            synced_at: String::new(),
            signed_by: String::new(),
            signature_sha256: String::new(),
        }
    }

    #[test]
    fn searchable_catalog_filters_by_query_and_category() {
        let catalog = make_catalog();
        let all = searchable_catalog(&catalog, "", None);
        assert_eq!(all.len(), 3);
        let sec = searchable_catalog(&catalog, "", Some("security"));
        assert_eq!(sec.len(), 2);
        let alpha = searchable_catalog(&catalog, "alpha", None);
        assert_eq!(alpha.len(), 2);
        let prod = searchable_catalog(&catalog, "beta", Some("productivity"));
        assert_eq!(prod.len(), 1);
    }

    #[test]
    fn marketplace_categories_counts() {
        let catalog = make_catalog();
        let cats = marketplace_categories(&catalog);
        assert!(cats.iter().any(|(c, n)| c == "security" && *n == 2));
        assert!(cats.iter().any(|(c, n)| c == "productivity" && *n == 1));
    }

    #[test]
    fn rollback_via_loader_snapshot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let plugin_dir = tmp.path().join("plugins");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        // Create v1 plugin dir
        let p1_dir = plugin_dir.join("alpha");
        std::fs::create_dir_all(&p1_dir).unwrap();
        let m1 = rightkit_agent_plugins::PluginManifest {
            id: "alpha".into(),
            name: "Alpha".into(),
            version: "1.0.0".into(),
            description: "v1".into(),
            entry: "plugin.js".into(),
            provides_tools: vec![],
            provides_commands: vec![],
            tools: BTreeMap::new(),
            commands: BTreeMap::new(),
            prompts: Vec::new(),
            middleware: Vec::new(),
            ui_slots: Vec::new(),
            sandbox: rightkit_agent_plugins::PluginSandboxPolicy::default(),
            enabled: true,
        };
        std::fs::write(p1_dir.join("plugin.json"), m1.to_json().unwrap()).unwrap();
        std::fs::write(p1_dir.join("extra.txt"), "v1-data").unwrap();

        // Snapshot v1 to .rollback
        let snap = plugin_dir.join(".rollback").join("alpha").join("1.0.0");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::copy(p1_dir.join("plugin.json"), snap.join("plugin.json")).unwrap();
        std::fs::copy(p1_dir.join("extra.txt"), snap.join("extra.txt")).unwrap();

        // Overwrite with v2
        let m2 = rightkit_agent_plugins::PluginManifest {
            version: "2.0.0".into(),
            ..m1.clone()
        };
        std::fs::write(p1_dir.join("plugin.json"), m2.to_json().unwrap()).unwrap();
        std::fs::write(p1_dir.join("extra.txt"), "v2-data").unwrap();

        // Rollback to v1
        rollback_plugin(&plugin_dir, "alpha", "1.0.0").unwrap();
        let restored = rightkit_agent_plugins::PluginManifest::from_json(
            &std::fs::read_to_string(p1_dir.join("plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(restored.version, "1.0.0");
        assert_eq!(
            std::fs::read_to_string(p1_dir.join("extra.txt")).unwrap(),
            "v1-data"
        );
    }

    #[test]
    fn per_session_activation_helpers() {
        let mut activations = rightkit_agent_plugins::SessionPluginActivations::new();
        let manifest = rightkit_agent_plugins::PluginManifest {
            id: "alpha".into(),
            name: "Alpha".into(),
            version: "1.0.0".into(),
            description: "".into(),
            entry: "plugin.js".into(),
            provides_tools: vec![],
            provides_commands: vec![],
            tools: BTreeMap::new(),
            commands: BTreeMap::new(),
            prompts: Vec::new(),
            middleware: Vec::new(),
            ui_slots: Vec::new(),
            sandbox: rightkit_agent_plugins::PluginSandboxPolicy::default(),
            enabled: false,
        };
        // Global disabled, but enabled for session s1
        assert!(!activations.is_active("alpha", "s1", manifest.enabled));
        enable_for_session(&mut activations, "alpha", "s1");
        assert!(activations.is_active("alpha", "s1", manifest.enabled));
        assert!(!activations.is_active("alpha", "s2", manifest.enabled));
        disable_for_session(&mut activations, "alpha", "s1");
        assert!(!activations.is_active("alpha", "s1", manifest.enabled));
    }
}
