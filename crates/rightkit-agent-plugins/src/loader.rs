use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::PluginError;
use crate::manifest::PluginManifest;

pub struct PluginLoader {
    dir: PathBuf,
    loaded: Vec<PluginManifest>,
}

impl PluginLoader {
    pub fn new(dir: PathBuf) -> Self {
        PluginLoader {
            dir,
            loaded: Vec::new(),
        }
    }

    pub fn discover(&mut self) -> Result<usize, PluginError> {
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir)
                .map_err(|e| PluginError::Io(format!("Failed to create plugin dir: {}", e)))?;
        }

        self.loaded.clear();

        for entry in fs::read_dir(&self.dir)
            .map_err(|e| PluginError::Io(format!("Failed to read plugin dir: {}", e)))?
        {
            let entry = entry.map_err(|e| PluginError::Io(e.to_string()))?;
            let path = entry.path();

            let hidden = entry.file_name().to_string_lossy().starts_with('.');
            if path.is_dir() && !hidden {
                let manifest_path = path.join("plugin.json");
                if manifest_path.exists() {
                    let json_str = fs::read_to_string(&manifest_path)
                        .map_err(|e| PluginError::Io(e.to_string()))?;
                    match PluginManifest::from_json(&json_str) {
                        Ok(manifest) => self.loaded.push(manifest),
                        Err(_e) => {
                            // Skip manifests that fail to parse
                        }
                    }
                }
            }
        }

        Ok(self.loaded.len())
    }

    pub fn enable(&mut self, id: &str) -> Result<(), PluginError> {
        let found = self.loaded.iter_mut().find(|m| m.id == id).is_some();
        if !found {
            return Err(PluginError::NotFound(format!("Plugin {} not found", id)));
        }

        if let Some(manifest) = self.loaded.iter_mut().find(|m| m.id == id) {
            manifest.enabled = true;
        }

        if let Some(manifest) = self.loaded.iter().find(|m| m.id == id) {
            self.save_manifest(manifest.clone())?;
        }

        Ok(())
    }

    pub fn disable(&mut self, id: &str) -> Result<(), PluginError> {
        let found = self.loaded.iter_mut().find(|m| m.id == id).is_some();
        if !found {
            return Err(PluginError::NotFound(format!("Plugin {} not found", id)));
        }

        if let Some(manifest) = self.loaded.iter_mut().find(|m| m.id == id) {
            manifest.enabled = false;
        }

        if let Some(manifest) = self.loaded.iter().find(|m| m.id == id) {
            self.save_manifest(manifest.clone())?;
        }

        Ok(())
    }

    pub fn enabled_plugins(&self) -> Vec<&PluginManifest> {
        self.loaded.iter().filter(|m| m.enabled).collect()
    }

    pub fn get(&self, id: &str) -> Option<&PluginManifest> {
        self.loaded.iter().find(|m| m.id == id)
    }

    /// Install the plugin in `source` under the plugin directory.
    ///
    /// The new version is copied into a hidden staging folder first and swapped in with
    /// renames, so a failed copy leaves the installed version untouched. The previous
    /// version is deleted once the swap succeeds: no snapshot, no `.rollback` store. An
    /// older version is reinstalled from its folder like any other install.
    pub fn install(&self, source: &Path, overwrite: bool) -> Result<InstalledPlugin, PluginError> {
        if !source.is_dir() {
            return Err(PluginError::Refused(format!(
                "{} is not a folder; point the install at a plugin folder that holds plugin.json",
                source.display()
            )));
        }
        let manifest_path = source.join("plugin.json");
        let raw = fs::read_to_string(&manifest_path).map_err(|e| {
            PluginError::Refused(format!(
                "plugin.json is missing or unreadable in {}: {e}",
                source.display()
            ))
        })?;
        let manifest = PluginManifest::from_json(&raw).map_err(|e| {
            PluginError::Refused(format!(
                "plugin.json in {} is not a valid plugin manifest: {e}",
                source.display()
            ))
        })?;
        if !validate_plugin_id(&manifest.id) {
            return Err(PluginError::Refused(format!(
                "the plugin id '{}' is not allowed: use letters, digits, '-', '_' and '.', and do not start with a dot",
                manifest.id
            )));
        }
        let source_canonical = source.canonicalize().map_err(|e| {
            PluginError::Refused(format!("cannot resolve {}: {e}", source.display()))
        })?;
        fs::create_dir_all(&self.dir)
            .map_err(|e| PluginError::Io(format!("Failed to create plugin dir: {e}")))?;
        let destination = self.dir.join(&manifest.id);
        let overwritten = destination.exists();
        // What the replaced version offered, read before it is swapped out (best effort: an
        // unreadable old manifest only means there is no summary to print).
        let previous = if overwritten {
            fs::read_to_string(destination.join("plugin.json"))
                .ok()
                .and_then(|raw| PluginManifest::from_json(&raw).ok())
        } else {
            None
        };
        if overwritten {
            let destination_canonical = destination
                .canonicalize()
                .map_err(|e| PluginError::Io(format!("cannot resolve the installed copy: {e}")))?;
            if destination_canonical == source_canonical {
                return Err(PluginError::Refused(format!(
                    "plugin '{}' is installed from this folder already; install from a different folder to update it",
                    manifest.id
                )));
            }
            if !overwrite {
                return Err(PluginError::Conflict(format!(
                    "plugin '{}' is already installed; reinstall with overwrite to replace it",
                    manifest.id
                )));
            }
        }

        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let staging = self.dir.join(format!(".staging-{}-{nonce}", manifest.id));
        if let Err(e) = copy_tree(&source_canonical, &staging) {
            let _ = fs::remove_dir_all(&staging);
            return Err(PluginError::Io(format!(
                "copying the plugin failed ({e}); the installed version is unchanged"
            )));
        }
        if overwritten {
            let retired = self.dir.join(format!(".retired-{}-{nonce}", manifest.id));
            if let Err(e) = fs::rename(&destination, &retired) {
                let _ = fs::remove_dir_all(&staging);
                return Err(PluginError::Io(format!(
                    "could not replace the installed version ({e}); it is unchanged"
                )));
            }
            if let Err(e) = fs::rename(&staging, &destination) {
                // Put the old version back; nothing was lost.
                let restored = fs::rename(&retired, &destination);
                let _ = fs::remove_dir_all(&staging);
                return Err(PluginError::Io(format!(
                    "could not swap in the new version ({e}); {}",
                    if restored.is_ok() {
                        "the previous version is still installed"
                    } else {
                        "the previous version could not be restored"
                    }
                )));
            }
            let _ = fs::remove_dir_all(&retired);
        } else if let Err(e) = fs::rename(&staging, &destination) {
            let _ = fs::remove_dir_all(&staging);
            return Err(PluginError::Io(format!(
                "could not install the plugin: {e}"
            )));
        }
        let replaced = previous
            .as_ref()
            .map(|previous| ReplacedPlugin::between(previous, &manifest));
        Ok(InstalledPlugin {
            manifest,
            path: destination,
            overwritten,
            replaced,
        })
    }

    /// Remove an installed plugin: its folder and with it its `plugin.json` (so its enabled
    /// record). The plugin's commands and tools leave the next catalog refresh. An id that is
    /// not installed is `NotFound`; an id that cannot name one folder is refused.
    pub fn remove(&self, id: &str) -> Result<(), PluginError> {
        if !validate_plugin_id(id) {
            return Err(PluginError::Refused(format!(
                "the plugin id '{id}' is not allowed: use letters, digits, '-', '_' and '.', and do not start with a dot"
            )));
        }
        let folder = self.dir.join(id);
        if !folder.join("plugin.json").is_file() {
            return Err(PluginError::NotFound(format!(
                "plugin '{id}' is not installed"
            )));
        }
        fs::remove_dir_all(&folder)
            .map_err(|e| PluginError::Io(format!("could not remove plugin '{id}': {e}")))
    }

    pub fn versions(&self, id: &str) -> Vec<&str> {
        let mut versions: Vec<&str> = self
            .loaded
            .iter()
            .filter(|m| m.id == id)
            .map(|m| m.version.as_str())
            .collect();
        // Also include rollback snapshots on disk that are not currently loaded
        let rollback_dir = self.dir.join(".rollback").join(id);
        if rollback_dir.exists() {
            if let Ok(entries) = fs::read_dir(&rollback_dir) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        if let Some(name) = entry.file_name().to_str() {
                            if name != "history.json" && !versions.contains(&name) {
                                // Leak to get &str — versions Vec holds references into loaded,
                                // but snapshot versions are owned. Store owned strings via separate method
                                // below; for this legacy accessor we return only loaded versions.
                            }
                        }
                    }
                }
            }
        }
        versions.sort();
        versions.dedup();
        versions
    }

    pub fn available_rollback_versions(&self, id: &str) -> Vec<String> {
        let mut versions = Vec::new();
        let rollback_base = self.dir.join(".rollback").join(id);
        if let Ok(entries) = fs::read_dir(&rollback_base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name == "history.json" {
                            continue;
                        }
                        if path.join("plugin.json").exists() {
                            versions.push(name.to_string());
                        }
                    }
                }
            }
        }
        versions.sort();
        versions
    }

    pub fn rollback(&self, id: &str, target_version: &str) -> Result<(), PluginError> {
        if !validate_plugin_id(id) {
            return Err(PluginError::Io(format!("Invalid plugin id '{}'", id)));
        }
        let plugin_dir = self.dir.join(id);
        if !plugin_dir.exists() {
            return Err(PluginError::NotFound(format!("Plugin {} not found", id)));
        }
        let snapshot_dir = self.dir.join(".rollback").join(id).join(target_version);
        if !snapshot_dir.exists() {
            return Err(PluginError::NotFound(format!(
                "Rollback version {} for plugin {} not found",
                target_version, id
            )));
        }
        let snapshot_manifest = snapshot_dir.join("plugin.json");
        if !snapshot_manifest.exists() {
            return Err(PluginError::Io(format!(
                "Rollback snapshot for {} version {} is corrupt",
                id, target_version
            )));
        }
        // Snapshot current version before overwriting, so rollback is reversible
        let current_manifest_path = plugin_dir.join("plugin.json");
        let current_version = fs::read_to_string(&current_manifest_path)
            .ok()
            .and_then(|s| PluginManifest::from_json(&s).ok())
            .map(|m| m.version)
            .unwrap_or_else(|| "unknown".to_string());
        if current_version != target_version {
            let current_snapshot = self.dir.join(".rollback").join(id).join(&current_version);
            if !current_snapshot.exists() {
                let _ = fs::create_dir_all(current_snapshot.parent().unwrap());
                let _ = copy_dir_all(&plugin_dir, &current_snapshot);
                let history_path = self.dir.join(".rollback").join(id).join("history.json");
                let mut history: Vec<String> = if history_path.exists() {
                    fs::read_to_string(&history_path)
                        .ok()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                if !history.contains(&current_version) {
                    history.push(current_version.clone());
                    history.sort();
                    let _ = fs::write(
                        &history_path,
                        serde_json::to_string_pretty(&history).unwrap_or_default(),
                    );
                }
            }
        }

        // Replace plugin dir with snapshot
        fs::remove_dir_all(&plugin_dir)
            .map_err(|e| PluginError::Io(format!("Failed to remove current plugin dir: {}", e)))?;
        copy_dir_all(&snapshot_dir, &plugin_dir)
            .map_err(|e| PluginError::Io(format!("Failed to restore rollback snapshot: {}", e)))?;
        Ok(())
    }

    fn save_manifest(&self, manifest: PluginManifest) -> Result<(), PluginError> {
        if !validate_plugin_id(&manifest.id) {
            return Err(PluginError::Refused(format!(
                "the plugin id '{}' is not allowed",
                manifest.id
            )));
        }
        let plugin_dir = self.dir.join(&manifest.id);
        if !plugin_dir.is_dir() {
            return Err(PluginError::NotFound(format!(
                "no installed folder for plugin {}",
                manifest.id
            )));
        }
        let manifest_path = plugin_dir.join("plugin.json");
        let json = manifest
            .to_json()
            .map_err(|e| PluginError::Io(e.to_string()))?;
        fs::write(&manifest_path, json)
            .map_err(|e| PluginError::Io(format!("Failed to write manifest: {}", e)))?;

        Ok(())
    }
}

/// The result of [`PluginLoader::install`].
#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub path: PathBuf,
    pub overwritten: bool,
    /// What changed between the replaced version and this one; `None` for a first install.
    pub replaced: Option<ReplacedPlugin>,
}

/// What an install that replaced a plugin changed: the two versions and the commands and
/// tools the new version added or no longer offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacedPlugin {
    pub from_version: String,
    pub to_version: String,
    pub added_commands: Vec<String>,
    pub removed_commands: Vec<String>,
    pub added_tools: Vec<String>,
    pub removed_tools: Vec<String>,
}

impl ReplacedPlugin {
    pub fn between(old: &PluginManifest, new: &PluginManifest) -> Self {
        fn names(listed: &[String], described: impl Iterator<Item = String>) -> BTreeSet<String> {
            listed.iter().cloned().chain(described).collect()
        }
        let old_commands = names(&old.provides_commands, old.commands.keys().cloned());
        let new_commands = names(&new.provides_commands, new.commands.keys().cloned());
        let old_tools = names(&old.provides_tools, old.tools.keys().cloned());
        let new_tools = names(&new.provides_tools, new.tools.keys().cloned());
        let diff = |from: &BTreeSet<String>, to: &BTreeSet<String>| -> Vec<String> {
            to.difference(from).cloned().collect()
        };
        ReplacedPlugin {
            from_version: old.version.clone(),
            to_version: new.version.clone(),
            added_commands: diff(&old_commands, &new_commands),
            removed_commands: diff(&new_commands, &old_commands),
            added_tools: diff(&old_tools, &new_tools),
            removed_tools: diff(&new_tools, &old_tools),
        }
    }
}

/// A plugin id names one folder under the plugin directory. It must be a single plain
/// component: letters, digits, `-`, `_` and inner dots, never starting with a dot, so `.`, `..`
/// and the hidden staging folders cannot be named.
pub fn validate_plugin_id(id: &str) -> bool {
    !id.trim().is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

/// Copy a plugin folder. Files and folders only: a symlink in a plugin is not followed, so a
/// plugin cannot copy files from outside its own folder into the plugin directory.
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&src_path, &dst_path)?;
        } else if ty.is_file() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
