//! Plugin install scopes (user, project), clean uninstall, and scoped lookup.
//!
//! Installs and upgrades are atomic (see `Marketplace::install_entry_verified`):
//! a failed install or upgrade restores the previous version, and a failed fresh
//! install leaves nothing behind.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::PluginError;
use crate::loader::PluginLoader;
use crate::manifest::PluginManifest;
use crate::marketplace::Marketplace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginScope {
    User,
    Project,
}

impl PluginScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginScope::User => "user",
            PluginScope::Project => "project",
        }
    }
}

pub(crate) fn valid_plugin_id(id: &str) -> bool {
    !id.trim().is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
}

/// The user-level and project-level plugin roots.
#[derive(Debug, Clone)]
pub struct ScopedPlugins {
    user_dir: PathBuf,
    project_dir: PathBuf,
}

impl ScopedPlugins {
    pub fn new(user_dir: PathBuf, project_dir: PathBuf) -> Self {
        Self {
            user_dir,
            project_dir,
        }
    }

    pub fn dir(&self, scope: PluginScope) -> &Path {
        match scope {
            PluginScope::User => &self.user_dir,
            PluginScope::Project => &self.project_dir,
        }
    }

    /// Install (or upgrade to) `version`, or the latest when `None`, at `scope`.
    pub fn install(
        &self,
        marketplace: &Marketplace,
        id: &str,
        version: Option<&str>,
        scope: PluginScope,
    ) -> Result<(), PluginError> {
        let dir = self.dir(scope);
        match version {
            Some(v) => marketplace.install_version(id, v, dir),
            None => marketplace.install(id, dir),
        }
    }

    /// Remove the plugin and its rollback history from `scope`. Other scopes
    /// are untouched. Errors with `NotFound` when nothing is installed there.
    pub fn uninstall(&self, id: &str, scope: PluginScope) -> Result<(), PluginError> {
        if !valid_plugin_id(id) {
            return Err(PluginError::Io(format!("Invalid plugin id '{}'", id)));
        }
        let dir = self.dir(scope);
        let plugin_dir = dir.join(id);
        let rollback_dir = dir.join(".rollback").join(id);
        if !plugin_dir.exists() && !rollback_dir.exists() {
            return Err(PluginError::NotFound(format!(
                "Plugin {} not installed at {} scope",
                id,
                scope.as_str()
            )));
        }
        for p in [&plugin_dir, &rollback_dir] {
            if p.exists() {
                fs::remove_dir_all(p).map_err(|e| {
                    PluginError::Io(format!("Failed to remove {}: {}", p.display(), e))
                })?;
            }
        }
        // Drop the now-empty rollback root.
        let _ = fs::remove_dir(dir.join(".rollback"));
        Ok(())
    }

    /// Which scope serves `id`: project shadows user.
    pub fn resolve(&self, id: &str) -> Option<(PluginScope, PluginManifest)> {
        for scope in [PluginScope::Project, PluginScope::User] {
            let mut loader = PluginLoader::new(self.dir(scope).to_path_buf());
            if loader.discover().is_ok() {
                if let Some(m) = loader.get(id) {
                    return Some((scope, m.clone()));
                }
            }
        }
        None
    }
}
