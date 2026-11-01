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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::MarketplaceEntry;
    use tempfile::TempDir;

    fn entry(id: &str, version: &str) -> MarketplaceEntry {
        let json = format!(
            "{{\"id\":\"{id}\",\"name\":\"{id}\",\"version\":\"{version}\",\"description\":\"d\",\"entry\":\"e\",\"enabled\":true}}"
        );
        MarketplaceEntry {
            manifest: PluginManifest::from_json(&json).unwrap(),
            downloads: 0,
            rating: 0.0,
            source_url: String::new(),
            category: "x".into(),
            tags: vec![],
        }
    }

    fn installed_version(dir: &Path, id: &str) -> Option<String> {
        let s = fs::read_to_string(dir.join(id).join("plugin.json")).ok()?;
        PluginManifest::from_json(&s).ok().map(|m| m.version)
    }

    fn setup() -> (TempDir, TempDir, ScopedPlugins, Marketplace) {
        let u = TempDir::new().unwrap();
        let p = TempDir::new().unwrap();
        let sp = ScopedPlugins::new(u.path().to_path_buf(), p.path().to_path_buf());
        let mut m = Marketplace::new();
        m.register(entry("alpha", "1.0.0"));
        m.register(entry("alpha", "2.0.0"));
        (u, p, sp, m)
    }

    #[test]
    fn install_is_scoped_and_project_shadows_user() {
        let (u, p, sp, m) = setup();
        sp.install(&m, "alpha", Some("1.0.0"), PluginScope::User)
            .unwrap();
        assert_eq!(
            installed_version(u.path(), "alpha").as_deref(),
            Some("1.0.0")
        );
        assert!(installed_version(p.path(), "alpha").is_none());
        assert_eq!(sp.resolve("alpha").unwrap().0, PluginScope::User);
        sp.install(&m, "alpha", None, PluginScope::Project).unwrap();
        assert_eq!(
            installed_version(p.path(), "alpha").as_deref(),
            Some("2.0.0")
        );
        let (scope, manifest) = sp.resolve("alpha").unwrap();
        assert_eq!(scope, PluginScope::Project);
        assert_eq!(manifest.version, "2.0.0");
        assert_eq!(
            installed_version(u.path(), "alpha").as_deref(),
            Some("1.0.0")
        );
    }

    #[test]
    fn failed_upgrade_restores_previous_version_and_leaves_no_debris() {
        let (u, _p, sp, m) = setup();
        sp.install(&m, "alpha", Some("1.0.0"), PluginScope::User)
            .unwrap();
        fs::write(u.path().join("alpha").join("data.txt"), "keep").unwrap();
        let new = entry("alpha", "2.0.0");
        let err = m.install_entry_verified(&new, u.path(), &|_| {
            Err(PluginError::Io("injected verify failure".into()))
        });
        assert!(err.is_err());
        assert_eq!(
            installed_version(u.path(), "alpha").as_deref(),
            Some("1.0.0")
        );
        assert_eq!(
            fs::read_to_string(u.path().join("alpha").join("data.txt")).unwrap(),
            "keep"
        );
        assert!(!u.path().join(".staging").exists());
    }

    #[test]
    fn failed_fresh_install_leaves_nothing() {
        let (u, _p, _sp, m) = setup();
        let new = entry("beta", "1.0.0");
        let err =
            m.install_entry_verified(&new, u.path(), &|_| Err(PluginError::Io("injected".into())));
        assert!(err.is_err());
        assert!(!u.path().join("beta").exists());
        assert!(!u.path().join(".staging").exists());
    }

    #[test]
    fn successful_upgrade_snapshots_previous_for_rollback() {
        let (u, _p, sp, m) = setup();
        sp.install(&m, "alpha", Some("1.0.0"), PluginScope::User)
            .unwrap();
        sp.install(&m, "alpha", Some("2.0.0"), PluginScope::User)
            .unwrap();
        assert_eq!(
            installed_version(u.path(), "alpha").as_deref(),
            Some("2.0.0")
        );
        let loader = PluginLoader::new(u.path().to_path_buf());
        assert_eq!(loader.available_rollback_versions("alpha"), vec!["1.0.0"]);
        loader.rollback("alpha", "1.0.0").unwrap();
        assert_eq!(
            installed_version(u.path(), "alpha").as_deref(),
            Some("1.0.0")
        );
        assert!(!u.path().join(".staging").exists());
    }

    #[test]
    fn uninstall_is_clean_and_scope_local() {
        let (u, p, sp, m) = setup();
        sp.install(&m, "alpha", Some("1.0.0"), PluginScope::User)
            .unwrap();
        sp.install(&m, "alpha", Some("2.0.0"), PluginScope::User)
            .unwrap();
        sp.install(&m, "alpha", Some("1.0.0"), PluginScope::Project)
            .unwrap();
        sp.uninstall("alpha", PluginScope::User).unwrap();
        assert!(!u.path().join("alpha").exists());
        assert!(!u.path().join(".rollback").exists());
        assert_eq!(
            installed_version(p.path(), "alpha").as_deref(),
            Some("1.0.0")
        );
        assert!(matches!(
            sp.uninstall("alpha", PluginScope::User),
            Err(PluginError::NotFound(_))
        ));
    }

    #[test]
    fn invalid_ids_are_rejected_for_install_and_uninstall() {
        let (u, _p, sp, m) = setup();
        assert!(sp.uninstall("../escape", PluginScope::User).is_err());
        let bad = entry("a/b", "1.0.0");
        assert!(m
            .install_entry_verified(&bad, u.path(), &|_| Ok(()))
            .is_err());
    }
}
