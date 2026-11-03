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
