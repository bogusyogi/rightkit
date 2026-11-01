use std::fs;
use std::path::PathBuf;

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

            if path.is_dir() {
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
        let plugin_dir = self.dir.join(&manifest.id);
        fs::create_dir_all(&plugin_dir)
            .map_err(|e| PluginError::Io(format!("Failed to create plugin dir: {}", e)))?;

        let manifest_path = plugin_dir.join("plugin.json");
        let json = manifest
            .to_json()
            .map_err(|e| PluginError::Io(e.to_string()))?;
        fs::write(&manifest_path, json)
            .map_err(|e| PluginError::Io(format!("Failed to write manifest: {}", e)))?;

        Ok(())
    }
}

fn validate_plugin_id(id: &str) -> bool {
    !id.trim().is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
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
    fn test_loader_versions() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let plugin_dir = temp_dir.path().join("plugins");

        fs::create_dir(&plugin_dir).expect("Failed to create plugin dir");

        for version in ["1.0.0", "1.1.0", "2.0.0"] {
            let plugin_dir = plugin_dir.join(format!("plugin1-v{}", version));
            fs::create_dir(&plugin_dir).expect("Failed to create plugin dir");

            let manifest = PluginManifest {
                id: "plugin1".to_string(),
                name: "Plugin 1".to_string(),
                version: version.to_string(),
                description: "First plugin".to_string(),
                entry: "/path/to/plugin.wasm".to_string(),
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
                plugin_dir.join("plugin.json"),
                manifest.to_json().expect("to_json failed"),
            )
            .expect("Failed to write manifest");
        }

        let mut loader = PluginLoader::new(temp_dir.path().join("plugins"));
        loader.discover().expect("discover failed");

        let versions = loader.versions("plugin1");
        assert_eq!(versions.len(), 3);
        assert_eq!(versions, vec!["1.0.0", "1.1.0", "2.0.0"]);
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
