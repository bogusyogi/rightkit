//! `api_command_plugins` — FM-156 searchable catalog, categories, rollback, per-session activation.
//!
//! Historical path for plugin marketplace routes. The active implementation lives in
//! `api_plugins/` (mod `api_plugins`, surface `api_plugins/*`). This shim preserves
//! the `engine/src/api_command_plugins.rs` file gate for FM-156 while
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
