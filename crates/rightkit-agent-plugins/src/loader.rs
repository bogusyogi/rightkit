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

    /// Install the plugin in `source` under the plugin directory (contract R7, R9, R34).
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
        Ok(InstalledPlugin {
            manifest,
            path: destination,
            overwritten,
        })
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
}

/// A plugin id names one folder under the plugin directory (contract D10). It must be a single
/// plain component: letters, digits, `-`, `_` and inner dots, never starting with a dot, so
/// `.`, `..` and the hidden staging folders cannot be named.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_loader_discover() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let plugin_dir = temp_dir.path().join("plugins");

        fs::create_dir(&plugin_dir).expect("Failed to create plugin dir");

        let plugin1_dir = plugin_dir.join("plugin1");
        fs::create_dir(&plugin1_dir).expect("Failed to create plugin1 dir");

        let manifest1 = PluginManifest {
            id: "plugin1".to_string(),
            name: "Plugin 1".to_string(),
            version: "1.0.0".to_string(),
            description: "First plugin".to_string(),
            entry: "/path/to/plugin1.wasm".to_string(),
            provides_tools: vec!["tool1".to_string()],
            provides_commands: vec![],
            tools: std::collections::BTreeMap::new(),
            commands: std::collections::BTreeMap::new(),
            prompts: Vec::new(),
            middleware: Vec::new(),
            ui_slots: Vec::new(),
            sandbox: crate::manifest::PluginSandboxPolicy::default(),
            enabled: true,
        };

        fs::write(
            plugin1_dir.join("plugin.json"),
            manifest1.to_json().expect("to_json failed"),
        )
        .expect("Failed to write manifest");

        let mut loader = PluginLoader::new(plugin_dir.clone());
        let count = loader.discover().expect("discover failed");

        assert_eq!(count, 1);
        assert!(loader.get("plugin1").is_some());
    }

    #[test]
    fn test_loader_enable_disable() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let plugin_dir = temp_dir.path().join("plugins");

        fs::create_dir(&plugin_dir).expect("Failed to create plugin dir");

        let plugin1_dir = plugin_dir.join("plugin1");
        fs::create_dir(&plugin1_dir).expect("Failed to create plugin1 dir");

        let manifest1 = PluginManifest {
            id: "plugin1".to_string(),
            name: "Plugin 1".to_string(),
            version: "1.0.0".to_string(),
            description: "First plugin".to_string(),
            entry: "/path/to/plugin1.wasm".to_string(),
            provides_tools: vec![],
            provides_commands: vec![],
            tools: std::collections::BTreeMap::new(),
            commands: std::collections::BTreeMap::new(),
            prompts: Vec::new(),
            middleware: Vec::new(),
            ui_slots: Vec::new(),
            sandbox: crate::manifest::PluginSandboxPolicy::default(),
            enabled: true,
        };

        fs::write(
            plugin1_dir.join("plugin.json"),
            manifest1.to_json().expect("to_json failed"),
        )
        .expect("Failed to write manifest");

        let mut loader = PluginLoader::new(plugin_dir);
        loader.discover().expect("discover failed");

        loader.disable("plugin1").expect("disable failed");
        assert!(!loader.get("plugin1").expect("get failed").enabled);

        loader.enable("plugin1").expect("enable failed");
        assert!(loader.get("plugin1").expect("get failed").enabled);
    }

    #[test]
    fn test_loader_enabled_plugins() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let plugin_dir = temp_dir.path().join("plugins");

        fs::create_dir(&plugin_dir).expect("Failed to create plugin dir");

        for (id, enabled) in [("plugin1", true), ("plugin2", false)] {
            let plugin_subdir = plugin_dir.join(id);
            fs::create_dir(&plugin_subdir).expect("Failed to create plugin dir");

            let manifest = PluginManifest {
                id: id.to_string(),
                name: format!("Plugin {}", id),
                version: "1.0.0".to_string(),
                description: "A plugin".to_string(),
                entry: "/path/to/plugin.wasm".to_string(),
                provides_tools: vec![],
                provides_commands: vec![],
                tools: std::collections::BTreeMap::new(),
                commands: std::collections::BTreeMap::new(),
                prompts: Vec::new(),
                middleware: Vec::new(),
                ui_slots: Vec::new(),
                sandbox: crate::manifest::PluginSandboxPolicy::default(),
                enabled,
            };

            fs::write(
                plugin_subdir.join("plugin.json"),
                manifest.to_json().expect("to_json failed"),
            )
            .expect("Failed to write manifest");
        }

        let mut loader = PluginLoader::new(plugin_dir);
        loader.discover().expect("discover failed");

        let enabled = loader.enabled_plugins();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].id, "plugin1");
    }
}
