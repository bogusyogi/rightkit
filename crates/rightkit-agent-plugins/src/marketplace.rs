use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use crate::error::PluginError;
use crate::manifest::PluginManifest;

#[derive(Debug, Clone)]
pub struct MarketplaceEntry {
    pub manifest: PluginManifest,
    pub downloads: u64,
    pub rating: f64,
    pub source_url: String,
    pub category: String,
    pub tags: Vec<String>,
}

pub struct Marketplace {
    entries: Vec<MarketplaceEntry>,
    version_history: HashMap<String, Vec<MarketplaceEntry>>,
}

impl Marketplace {
    pub fn new() -> Self {
        Marketplace {
            entries: Vec::new(),
            version_history: HashMap::new(),
        }
    }

    pub fn register(&mut self, entry: MarketplaceEntry) {
        let id = entry.manifest.id.clone();
        let version = entry.manifest.version.clone();
        self.version_history
            .entry(id.clone())
            .or_default()
            .push(entry.clone());
        // Keep version_history sorted by version string for deterministic ordering
        if let Some(history) = self.version_history.get_mut(&id) {
            history.sort_by(|a, b| a.manifest.version.cmp(&b.manifest.version));
            history.dedup_by(|a, b| a.manifest.version == b.manifest.version);
        }
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.manifest.id == id && e.manifest.version == version)
        {
            self.entries[pos] = entry;
        } else {
            self.entries.push(entry);
        }
    }

    pub fn search(&self, query: &str) -> Vec<&MarketplaceEntry> {
        self.search_with_filter(query, None, None)
    }

    pub fn search_with_filter(
        &self,
        query: &str,
        category: Option<&str>,
        tag: Option<&str>,
    ) -> Vec<&MarketplaceEntry> {
        let query_lower = query.to_lowercase();
        let query_empty = query_lower.trim().is_empty();
        let category_lower = category.map(|c| c.to_lowercase());
        let tag_lower = tag.map(|t| t.to_lowercase());
        self.entries
            .iter()
            .filter(|e| {
                if let Some(ref cat) = category_lower {
                    if !cat.is_empty() && e.category.to_lowercase() != *cat {
                        return false;
                    }
                }
                if let Some(ref t) = tag_lower {
                    if !t.is_empty() && !e.tags.iter().any(|tag| tag.to_lowercase() == *t) {
                        return false;
                    }
                }
                if query_empty {
                    return true;
                }
                e.manifest.name.to_lowercase().contains(&query_lower)
                    || e.manifest.description.to_lowercase().contains(&query_lower)
                    || e.manifest.id.to_lowercase().contains(&query_lower)
                    || e.category.to_lowercase().contains(&query_lower)
                    || e.tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&query_lower))
            })
            .collect()
    }

    pub fn filter_by_category(&self, category: &str) -> Vec<&MarketplaceEntry> {
        let cat_lower = category.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.category.to_lowercase() == cat_lower)
            .collect()
    }

    pub fn categories(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for entry in &self.entries {
            if !entry.category.trim().is_empty() {
                set.insert(entry.category.clone());
            }
        }
        set.into_iter().collect()
    }

    pub fn all_tags(&self) -> Vec<String> {
        let mut set = BTreeSet::new();
        for entry in &self.entries {
            for tag in &entry.tags {
                if !tag.trim().is_empty() {
                    set.insert(tag.clone());
                }
            }
        }
        set.into_iter().collect()
    }

    pub fn available_versions(&self, id: &str) -> Vec<&MarketplaceEntry> {
        if let Some(history) = self.version_history.get(id) {
            history.iter().collect()
        } else {
            self.entries
                .iter()
                .filter(|e| e.manifest.id == id)
                .collect()
        }
    }

    pub fn latest_version(&self, id: &str) -> Option<&MarketplaceEntry> {
        self.available_versions(id)
            .into_iter()
            .max_by(|a, b| a.manifest.version.cmp(&b.manifest.version))
    }

    pub fn top_rated(&self, n: usize) -> Vec<&MarketplaceEntry> {
        let mut sorted = self.entries.iter().collect::<Vec<_>>();
        sorted.sort_by(|a, b| {
            b.rating
                .partial_cmp(&a.rating)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.downloads.cmp(&a.downloads))
        });
        sorted.into_iter().take(n).collect()
    }

    pub fn top_rated_in_category(&self, category: &str, n: usize) -> Vec<&MarketplaceEntry> {
        let mut filtered = self.filter_by_category(category);
        filtered.sort_by(|a, b| {
            b.rating
                .partial_cmp(&a.rating)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.downloads.cmp(&a.downloads))
        });
        filtered.into_iter().take(n).collect()
    }

    pub fn install(&self, id: &str, loader_dir: &Path) -> Result<(), PluginError> {
        let entry = self.latest_version(id).ok_or_else(|| {
            PluginError::NotFound(format!("Plugin {} not found in marketplace", id))
        })?;
        self.install_entry(entry, loader_dir)
    }

    pub fn install_version(
        &self,
        id: &str,
        version: &str,
        loader_dir: &Path,
    ) -> Result<(), PluginError> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.manifest.id == id && e.manifest.version == version)
            .or_else(|| {
                self.version_history
                    .get(id)
                    .and_then(|history| history.iter().find(|e| e.manifest.version == version))
            })
            .ok_or_else(|| {
                PluginError::NotFound(format!(
                    "Plugin {} version {} not found in marketplace",
                    id, version
                ))
            })?;
        self.install_entry(entry, loader_dir)
    }

    pub fn rollback(
        &self,
        id: &str,
        target_version: &str,
        loader_dir: &Path,
    ) -> Result<(), PluginError> {
        self.install_version(id, target_version, loader_dir)
    }

    fn install_entry(
        &self,
        entry: &MarketplaceEntry,
        loader_dir: &Path,
    ) -> Result<(), PluginError> {
        self.install_entry_verified(entry, loader_dir, &|staged: &Path| {
            let json = fs::read_to_string(staged.join("plugin.json"))
                .map_err(|e| PluginError::Io(format!("Failed to read staged manifest: {}", e)))?;
            PluginManifest::from_json(&json)
                .map(|_| ())
                .map_err(|e| PluginError::Parse(e.to_string()))
        })
    }

    /// Atomic install/upgrade: the new version is staged beside the live
    /// directory, verified, then swapped in. Any failure restores the previous
    /// version and removes staging debris; a failed fresh install leaves nothing.
    pub(crate) fn install_entry_verified(
        &self,
        entry: &MarketplaceEntry,
        loader_dir: &Path,
        verify: &dyn Fn(&Path) -> Result<(), PluginError>,
    ) -> Result<(), PluginError> {
        if !crate::scope::valid_plugin_id(&entry.manifest.id) {
            return Err(PluginError::Io(format!(
                "Invalid plugin id '{}'",
                entry.manifest.id
            )));
        }
        let plugin_dir = loader_dir.join(&entry.manifest.id);
        // Preserve existing version for rollback: snapshot current manifest if it exists
        if plugin_dir.exists() {
            let manifest_path = plugin_dir.join("plugin.json");
            if manifest_path.exists() {
                if let Ok(existing_json) = fs::read_to_string(&manifest_path) {
                    if let Ok(existing_manifest) = PluginManifest::from_json(&existing_json) {
                        let existing_version = existing_manifest.version.clone();
                        if existing_version != entry.manifest.version {
                            let rollback_dir = loader_dir
                                .join(".rollback")
                                .join(&entry.manifest.id)
                                .join(&existing_version);
                            let _ = fs::create_dir_all(rollback_dir.parent().unwrap());
                            let _ = fs::remove_dir_all(&rollback_dir);
                            // Copy existing plugin dir to rollback snapshot
                            if let Err(e) = Self::copy_dir_all(&plugin_dir, &rollback_dir) {
                                return Err(PluginError::Io(format!(
                                    "Failed to snapshot previous version for rollback: {}",
                                    e
                                )));
                            }
                            // Record version history file
                            let history_path = loader_dir
                                .join(".rollback")
                                .join(&entry.manifest.id)
                                .join("history.json");
                            let mut history: Vec<String> = if history_path.exists() {
                                fs::read_to_string(&history_path)
                                    .ok()
                                    .and_then(|s| serde_json::from_str(&s).ok())
                                    .unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            if !history.contains(&existing_version) {
                                history.push(existing_version);
                                history.sort();
                                let _ = rightkit_fs::atomic_write_with(
                                    &history_path,
                                    rightkit_fs::AtomicOptions {
                                        create_parents: true,
                                        ..rightkit_fs::AtomicOptions::default()
                                    },
                                    |f| {
                                        std::io::Write::write_all(
                                            f,
                                            serde_json::to_string_pretty(&history)
                                                .unwrap_or_default()
                                                .as_bytes(),
                                        )
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }

        fs::create_dir_all(loader_dir)
            .map_err(|e| PluginError::Io(format!("Failed to create plugin root: {}", e)))?;
        let staging_root = loader_dir.join(".staging");
        let stage = staging_root.join(&entry.manifest.id);
        let cleanup = || {
            let _ = fs::remove_dir_all(&stage);
            let _ = fs::remove_dir(&staging_root);
        };
        cleanup();
        let staged = (|| -> Result<(), PluginError> {
            fs::create_dir_all(&stage)
                .map_err(|e| PluginError::Io(format!("Failed to create staging dir: {}", e)))?;
            if plugin_dir.exists() {
                Self::copy_dir_all(&plugin_dir, &stage)
                    .map_err(|e| PluginError::Io(format!("Failed to stage plugin: {}", e)))?;
            }
            let json = entry
                .manifest
                .to_json()
                .map_err(|e| PluginError::Io(e.to_string()))?;
            fs::write(stage.join("plugin.json"), json)
                .map_err(|e| PluginError::Io(format!("Failed to write manifest: {}", e)))?;
            verify(&stage)
        })();
        if let Err(e) = staged {
            cleanup();
            return Err(e);
        }
        if let Err(e) = rightkit_fs::publish_dir(&stage, &plugin_dir) {
            cleanup();
            return Err(PluginError::Io(format!("Failed to activate plugin: {}", e)));
        }
        cleanup();
        Ok(())
    }

    fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if ty.is_dir() {
                Self::copy_dir_all(&src_path, &dst_path)?;
            } else {
                fs::copy(&src_path, &dst_path)?;
            }
        }
        Ok(())
    }

    pub fn get_all(&self) -> &[MarketplaceEntry] {
        &self.entries
    }

    pub fn get_all_versions(&self) -> Vec<&MarketplaceEntry> {
        self.entries.iter().collect()
    }

    pub fn search_catalog(
        &self,
        query: &str,
        category: Option<&str>,
        sort_by: CatalogSort,
        limit: Option<usize>,
    ) -> Vec<&MarketplaceEntry> {
        let mut results = self.search_with_filter(query, category, None);
        match sort_by {
            CatalogSort::Relevance => {
                // Preserve insertion order for relevance (already filtered)
            }
            CatalogSort::Rating => {
                results.sort_by(|a, b| {
                    b.rating
                        .partial_cmp(&a.rating)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.downloads.cmp(&a.downloads))
                });
            }
            CatalogSort::Downloads => {
                results.sort_by_key(|b| std::cmp::Reverse(b.downloads));
            }
            CatalogSort::Name => {
                results.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
            }
        }
        if let Some(n) = limit {
            results.truncate(n);
        }
        results
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogSort {
    Relevance,
    Rating,
    Downloads,
    Name,
}

impl Default for Marketplace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_entry(id: &str, name: &str, rating: f64, downloads: u64) -> MarketplaceEntry {
        MarketplaceEntry {
            manifest: PluginManifest {
                id: id.to_string(),
                name: name.to_string(),
                version: "1.0.0".to_string(),
                description: format!("{} plugin", name),
                entry: "/path/to/plugin.wasm".to_string(),
                provides_tools: vec![],
                provides_commands: vec![],
                tools: std::collections::BTreeMap::new(),
                commands: std::collections::BTreeMap::new(),
                prompts: Vec::new(),
                middleware: Vec::new(),
                ui_slots: Vec::new(),
                sandbox: crate::manifest::PluginSandboxPolicy::default(),
                enabled: false,
            },
            downloads,
            rating,
            source_url: format!("https://example.com/{}", id),
            category: String::new(),
            tags: Vec::new(),
        }
    }

    fn create_categorized_entry(
        id: &str,
        name: &str,
        category: &str,
        version: &str,
    ) -> MarketplaceEntry {
        MarketplaceEntry {
            manifest: PluginManifest {
                id: id.to_string(),
                name: name.to_string(),
                version: version.to_string(),
                description: format!("{} plugin", name),
                entry: "/path/to/plugin.wasm".to_string(),
                provides_tools: vec![],
                provides_commands: vec![],
                tools: std::collections::BTreeMap::new(),
                commands: std::collections::BTreeMap::new(),
                prompts: Vec::new(),
                middleware: Vec::new(),
                ui_slots: Vec::new(),
                sandbox: crate::manifest::PluginSandboxPolicy::default(),
                enabled: false,
            },
            downloads: 10,
            rating: 4.0,
            source_url: format!("https://example.com/{}", id),
            category: category.to_string(),
            tags: vec![],
        }
    }

    #[test]
    fn test_marketplace_search_by_name() {
        let mut marketplace = Marketplace::new();
        marketplace.register(create_test_entry("plugin1", "Auth Plugin", 4.5, 100));
        marketplace.register(create_test_entry("plugin2", "Logger Plugin", 4.0, 80));
        marketplace.register(create_test_entry("plugin3", "API Helper", 3.5, 50));

        let results = marketplace.search("auth");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].manifest.id, "plugin1");
    }

    #[test]
    fn test_marketplace_search_by_description() {
        let mut marketplace = Marketplace::new();
        marketplace.register(create_test_entry("plugin1", "Auth", 4.5, 100));
        marketplace.register(create_test_entry("plugin2", "Logger", 4.0, 80));
        marketplace.register(create_test_entry("plugin3", "Helper", 3.5, 50));

        let results = marketplace.search("plugin");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_marketplace_search_case_insensitive() {
        let mut marketplace = Marketplace::new();
        marketplace.register(create_test_entry("plugin1", "Auth Plugin", 4.5, 100));

        let results_lower = marketplace.search("auth");
        let results_upper = marketplace.search("AUTH");
        let results_mixed = marketplace.search("AuTh");

        assert_eq!(results_lower.len(), 1);
        assert_eq!(results_upper.len(), 1);
        assert_eq!(results_mixed.len(), 1);
    }

    #[test]
    fn test_marketplace_top_rated() {
        let mut marketplace = Marketplace::new();
        marketplace.register(create_test_entry("plugin1", "Auth", 3.0, 100));
        marketplace.register(create_test_entry("plugin2", "Logger", 4.5, 80));
        marketplace.register(create_test_entry("plugin3", "Helper", 4.5, 50));
        marketplace.register(create_test_entry("plugin4", "Utils", 2.0, 200));

        let top_2 = marketplace.top_rated(2);
        assert_eq!(top_2.len(), 2);
        assert_eq!(top_2[0].manifest.id, "plugin2");
        assert_eq!(top_2[1].manifest.id, "plugin3");

        let top_10 = marketplace.top_rated(10);
        assert_eq!(top_10.len(), 4);
    }

    #[test]
    fn test_marketplace_top_rated_sorting_by_downloads() {
        let mut marketplace = Marketplace::new();
        marketplace.register(create_test_entry("plugin1", "Auth", 4.5, 100));
        marketplace.register(create_test_entry("plugin2", "Logger", 4.5, 200));

        let top = marketplace.top_rated(2);
        assert_eq!(top[0].manifest.id, "plugin2");
        assert_eq!(top[1].manifest.id, "plugin1");
    }

    #[test]
    fn test_marketplace_install() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let loader_dir = temp_dir.path().join("plugins");

        let mut marketplace = Marketplace::new();
        marketplace.register(create_test_entry("test-plugin", "Test", 4.0, 50));

        marketplace
            .install("test-plugin", &loader_dir)
            .expect("install failed");

        let manifest_path = loader_dir.join("test-plugin").join("plugin.json");
        assert!(manifest_path.exists());

        let json_str = fs::read_to_string(&manifest_path).expect("read failed");
        let manifest = PluginManifest::from_json(&json_str).expect("parse failed");
        assert_eq!(manifest.id, "test-plugin");
    }

    #[test]
    fn test_marketplace_install_not_found() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let loader_dir = temp_dir.path().join("plugins");

        let marketplace = Marketplace::new();
        let result = marketplace.install("nonexistent", &loader_dir);

        assert!(result.is_err());
        match result {
            Err(PluginError::NotFound(_)) => (),
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_marketplace_categories() {
        let mut marketplace = Marketplace::new();
        marketplace.register(create_categorized_entry("p1", "Auth", "security", "1.0.0"));
        marketplace.register(create_categorized_entry(
            "p2",
            "Logger",
            "observability",
            "1.0.0",
        ));
        marketplace.register(create_categorized_entry(
            "p3", "Helper", "security", "1.0.0",
        ));

        let cats = marketplace.categories();
        assert_eq!(cats, vec!["observability", "security"]);

        let sec = marketplace.filter_by_category("security");
        assert_eq!(sec.len(), 2);

        let obs = marketplace.filter_by_category("SECURITY");
        assert_eq!(obs.len(), 2, "category filter is case-insensitive");
    }

    #[test]
    fn test_marketplace_search_with_category() {
        let mut marketplace = Marketplace::new();
        marketplace.register(create_categorized_entry("p1", "Auth", "security", "1.0.0"));
        marketplace.register(create_categorized_entry(
            "p2",
            "Auth Helper",
            "productivity",
            "1.0.0",
        ));

        let results = marketplace.search_with_filter("auth", Some("security"), None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].manifest.id, "p1");

        let all_auth = marketplace.search_with_filter("auth", None, None);
        assert_eq!(all_auth.len(), 2);
    }

    #[test]
    fn test_marketplace_search_includes_category_and_tags() {
        let mut marketplace = Marketplace::new();
        let mut entry = create_categorized_entry("p1", "My Plugin", "ai", "1.0.0");
        entry.tags = vec!["llm".into(), "completion".into()];
        marketplace.register(entry);

        assert_eq!(marketplace.search("ai").len(), 1);
        assert_eq!(marketplace.search("llm").len(), 1);
        assert_eq!(marketplace.search("completion").len(), 1);
    }

    #[test]
    fn test_marketplace_search_catalog_sorted() {
        let mut marketplace = Marketplace::new();
        marketplace.register(create_test_entry("plugin1", "Zeta", 2.0, 10));
        marketplace.register(create_test_entry("plugin2", "Alpha", 5.0, 100));
        marketplace.register(create_test_entry("plugin3", "Beta", 4.0, 50));

        let by_rating = marketplace.search_catalog("", None, CatalogSort::Rating, None);
        assert_eq!(by_rating[0].manifest.id, "plugin2");
        assert_eq!(by_rating[1].manifest.id, "plugin3");

        let by_name = marketplace.search_catalog("", None, CatalogSort::Name, None);
        assert_eq!(by_name[0].manifest.id, "plugin2"); // Alpha
        assert_eq!(by_name[1].manifest.id, "plugin3"); // Beta
        assert_eq!(by_name[2].manifest.id, "plugin1"); // Zeta

        let limited = marketplace.search_catalog("", None, CatalogSort::Rating, Some(1));
        assert_eq!(limited.len(), 1);
    }

    #[test]
    fn test_marketplace_top_rated_in_category() {
        let mut marketplace = Marketplace::new();
        let mut e1 = create_categorized_entry("p1", "A", "security", "1.0.0");
        e1.rating = 4.0;
        e1.downloads = 10;
        let mut e2 = create_categorized_entry("p2", "B", "security", "1.0.0");
        e2.rating = 5.0;
        e2.downloads = 5;
        let mut e3 = create_categorized_entry("p3", "C", "other", "1.0.0");
        e3.rating = 5.0;
        marketplace.register(e1);
        marketplace.register(e2);
        marketplace.register(e3);

        let top = marketplace.top_rated_in_category("security", 1);
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].manifest.id, "p2");
    }

    #[test]
    fn test_marketplace_version_history_and_rollback() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let loader_dir = temp_dir.path().join("plugins");

        let mut marketplace = Marketplace::new();
        marketplace.register(create_categorized_entry("p1", "Plugin", "tools", "1.0.0"));
        marketplace.register(create_categorized_entry("p1", "Plugin", "tools", "1.1.0"));
        marketplace.register(create_categorized_entry("p1", "Plugin", "tools", "2.0.0"));

        assert_eq!(marketplace.available_versions("p1").len(), 3);
        assert_eq!(
            marketplace.latest_version("p1").unwrap().manifest.version,
            "2.0.0"
        );

        marketplace
            .install_version("p1", "1.0.0", &loader_dir)
            .unwrap();
        let v1 = PluginManifest::from_json(
            &fs::read_to_string(loader_dir.join("p1").join("plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v1.version, "1.0.0");

        marketplace
            .install_version("p1", "2.0.0", &loader_dir)
            .unwrap();
        let v2 = PluginManifest::from_json(
            &fs::read_to_string(loader_dir.join("p1").join("plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v2.version, "2.0.0");

        // Rollback to 1.0.0 should snapshot 2.0.0 and restore 1.0.0
        marketplace.rollback("p1", "1.0.0", &loader_dir).unwrap();
        let rolled = PluginManifest::from_json(
            &fs::read_to_string(loader_dir.join("p1").join("plugin.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(rolled.version, "1.0.0");
        // Rollback snapshot exists
        assert!(loader_dir
            .join(".rollback")
            .join("p1")
            .join("2.0.0")
            .exists());
    }

    #[test]
    fn test_marketplace_install_version_not_found() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let mut marketplace = Marketplace::new();
        marketplace.register(create_categorized_entry("p1", "Plugin", "tools", "1.0.0"));
        let result = marketplace.install_version("p1", "9.9.9", temp_dir.path());
        assert!(matches!(result, Err(PluginError::NotFound(_))));
    }

    #[test]
    fn test_marketplace_install_snapshots_previous_version() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let loader_dir = temp_dir.path().join("plugins");
        let mut marketplace = Marketplace::new();
        marketplace.register(create_categorized_entry("snap", "Snap", "tools", "1.0.0"));
        marketplace.register(create_categorized_entry("snap", "Snap", "tools", "1.1.0"));

        marketplace
            .install_version("snap", "1.0.0", &loader_dir)
            .unwrap();
        // Add a custom file to the installed plugin dir to verify snapshot copies it
        fs::write(loader_dir.join("snap").join("extra.txt"), "v1-data").unwrap();

        marketplace
            .install_version("snap", "1.1.0", &loader_dir)
            .unwrap();
        let snapshot_extra = loader_dir
            .join(".rollback")
            .join("snap")
            .join("1.0.0")
            .join("extra.txt");
        assert!(snapshot_extra.exists());
        assert_eq!(fs::read_to_string(snapshot_extra).unwrap(), "v1-data");
    }
}
