//! Config loading: TOML parsing, layered merging, and env overrides.

use std::collections::HashMap;

use thiserror::Error;

use crate::agent_config::merge::merge;
use crate::agent_config::types::HostConfig;

/// Errors that can occur while loading configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to parse TOML: {0}")]
    Toml(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Loads and combines configuration from multiple layers.
#[derive(Debug, Default, Clone)]
pub struct ConfigLoader;

impl ConfigLoader {
    /// Create a new loader.
    pub fn new() -> Self {
        ConfigLoader
    }

    /// Parse a single TOML string into a [`CodeRightConfig`].
    pub fn load_str(&self, toml_str: &str) -> Result<HostConfig, ConfigError> {
        toml::from_str(toml_str).map_err(|e| ConfigError::Toml(e.to_string()))
    }

    /// Fold [`merge`] over all layers, starting from defaults.
    /// Earlier layers are overridden by later ones (lists accumulate).
    pub fn load_layered(&self, layers: Vec<HostConfig>) -> HostConfig {
        layers.into_iter().fold(HostConfig::default(), merge)
    }

    /// Load the standard two-file stack:
    ///   1. `<state root>/config.toml` (user-global)
    ///   2. `<state-dir>/config.toml` (workspace, relative to cwd)
    ///
    /// Missing files are silently skipped. Returns merged config.
    pub fn load_files_default(&self) -> HostConfig {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        self.load_files_for_workspace(&cwd)
    }

    /// Load the standard user + explicit workspace stack:
    ///   1. `<state root>/config.toml` (user-global)
    ///   2. `<workspace>/<state-dir>/config.toml`
    ///
    /// Missing files are skipped. An invalid file is moved aside with its bytes kept and
    /// reported on stderr (see [`Self::load_layer_file`]); it never silently falls back.
    pub fn load_files_for_workspace(&self, workspace: &std::path::Path) -> HostConfig {
        let mut layers = vec![];

        // User-global config.
        if let Some(root) = crate::agent_config::state_root() {
            if let Some(cfg) = self.load_layer_file(&root.join("config.toml")) {
                layers.push(cfg);
            }
        }

        // Workspace config (<workspace>/<state-dir>/config.toml).
        let ws_path = workspace.join(".rightkit").join("config.toml");
        if let Some(mut cfg) = self.load_layer_file(&ws_path) {
            // Rules 13 and 58: a repository's `[hooks]` run commands, so they take effect only
            // in a trusted workspace.
            if !crate::agent_config::is_workspace_trusted(workspace) {
                cfg.hooks = crate::agent_config::HooksConfig::default();
            }
            layers.push(cfg);
        }

        self.load_layered(layers)
    }

    /// One layer file. A missing file contributes nothing. A file that cannot be read or parsed is renamed aside with its original bytes
    /// kept (`<name>.corrupt-<unix secs>`), so the user's text is never overwritten and
    /// defaults apply, with a notice on stderr. Renaming fails closed: if the file cannot be
    /// moved, it is kept where it is and the notice says so.
    fn load_layer_file(&self, path: &std::path::Path) -> Option<HostConfig> {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                eprintln!(
                    "warning: config {} is unreadable ({error}); defaults apply for this layer",
                    path.display()
                );
                return None;
            }
        };
        match self.load_str(&content) {
            Ok(cfg) => Some(cfg),
            Err(error) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let aside = path.with_file_name(format!(
                    "{}.corrupt-{stamp}",
                    path.file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default()
                ));
                match std::fs::rename(path, &aside) {
                    Ok(()) => eprintln!(
                        "warning: config {} is invalid ({error}); moved aside to {} and defaults apply",
                        path.display(),
                        aside.display()
                    ),
                    Err(rename_error) => eprintln!(
                        "warning: config {} is invalid ({error}) and could not be moved aside ({rename_error}); it was left in place and defaults apply",
                        path.display()
                    ),
                }
                None
            }
        }
    }

    /// Apply environment-variable overrides as scalar overrides.
    ///
    /// The env map is passed in (rather than read from the process) so the
    /// function is fully testable. Recognized keys:
    /// `RIGHTKIT_MODEL`, `RIGHTKIT_PERMISSION_MODE`,
    /// `RIGHTKIT_WEB_SEARCH_PROVIDER`, `RIGHTKIT_WEB_SEARCH_API_KEY`,
    /// and `RIGHTKIT_TELEGRAM_TOKEN`.
    pub fn apply_env(
        &self,
        mut config: HostConfig,
        env_vars: &HashMap<String, String>,
    ) -> HostConfig {
        if let Some(v) = env_vars.get("RIGHTKIT_MODEL") {
            if !v.is_empty() {
                config.model.default_model = v.clone();
            }
        }
        if let Some(v) = env_vars.get("RIGHTKIT_PERMISSION_MODE") {
            if !v.is_empty() {
                config.permissions.mode = v.clone();
            }
        }
        if let Some(v) = env_vars.get("RIGHTKIT_WEB_SEARCH_PROVIDER") {
            if !v.is_empty() {
                config.tools.web_search_provider = Some(v.clone());
            }
        }
        if let Some(v) = env_vars.get("RIGHTKIT_WEB_SEARCH_API_KEY") {
            if !v.is_empty() {
                config.tools.web_search_api_key = Some(v.clone());
            }
        }
        if let Some(v) = env_vars.get("RIGHTKIT_TELEGRAM_TOKEN") {
            if !v.is_empty() {
                config.remote.telegram_token = Some(v.clone());
            }
        }
        if let Some(v) = env_vars.get("RIGHTKIT_CLI_ACQUISITION_V1") {
            if let Some(enabled) = parse_bool(v) {
                config.features.cli_acquisition_v1 = Some(enabled);
            }
        }
        config
    }

    /// Persist a config to the user-global `<state root>/config.toml`.
    /// Creates the directory if it does not exist.
    pub fn save_user(&self, config: &HostConfig) -> Result<(), ConfigError> {
        let dir = crate::agent_config::state_root()
            .ok_or_else(|| ConfigError::Io("cannot determine the Agent state directory".into()))?;
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let path = dir.join("config.toml");
        let content =
            toml::to_string_pretty(config).map_err(|e| ConfigError::Toml(e.to_string()))?;
        crate::atomic_write(&path, content.as_bytes())
            .map_err(|e| ConfigError::Io(e.to_string()))?;
        Ok(())
    }

    /// Read-modify-write of the user-global `<state root>/config.toml` under a cross-process lock
    /// (`config.toml.lock` beside it, [`rightkit_fs::FileLock`], waiting up to five seconds), so two
    /// processes saving settings at once cannot lose each other's update. The edit starts from the
    /// merged stack [`Self::load_files_default`] returns and the result is saved with
    /// [`Self::save_user`]. Returns the saved config.
    pub fn update_user(
        &self,
        edit: impl FnOnce(&mut HostConfig),
    ) -> Result<HostConfig, ConfigError> {
        self.update_user_from(|loader| Ok(loader.load_files_default()), edit)
    }

    /// [`Self::update_user`] with an explicit starting point read under the same lock (for callers
    /// that edit the user file alone rather than the merged stack). Every writer of the user
    /// config goes through one of the two, so no save can be lost to a concurrent one.
    ///
    /// The wait can last up to five seconds and blocks the calling thread: call it from a blocking
    /// context (`spawn_blocking`), never directly on an async worker.
    pub fn update_user_from(
        &self,
        base: impl FnOnce(&Self) -> Result<HostConfig, ConfigError>,
        edit: impl FnOnce(&mut HostConfig),
    ) -> Result<HostConfig, ConfigError> {
        let dir = crate::agent_config::state_root()
            .ok_or_else(|| ConfigError::Io("cannot determine the Agent state directory".into()))?;
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let _lock = lock_config(&dir, CONFIG_LOCK_WAIT)?;
        let mut config = base(self)?;
        edit(&mut config);
        self.save_user(&config)?;
        Ok(config)
    }

    /// Read-modify-write of `<workspace>/<state-dir>/config.toml` under its own cross-process lock
    /// (`config.toml.lock` beside it, the same lock kind and wait as [`Self::update_user_from`]).
    /// `base` reads the starting config under the lock. Every writer of the workspace config goes
    /// through this, so no save can be lost to a concurrent one.
    ///
    /// The wait can last up to five seconds and blocks the calling thread: call it from a blocking
    /// context (`spawn_blocking`), never directly on an async worker.
    pub fn update_workspace_from(
        &self,
        workspace: &std::path::Path,
        base: impl FnOnce() -> Result<HostConfig, ConfigError>,
        edit: impl FnOnce(&mut HostConfig),
    ) -> Result<HostConfig, ConfigError> {
        self.update_workspace_from_waiting(workspace, CONFIG_LOCK_WAIT, base, edit)
    }

    fn update_workspace_from_waiting(
        &self,
        workspace: &std::path::Path,
        wait: std::time::Duration,
        base: impl FnOnce() -> Result<HostConfig, ConfigError>,
        edit: impl FnOnce(&mut HostConfig),
    ) -> Result<HostConfig, ConfigError> {
        let dir = workspace.join(".rightkit");
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let _lock = lock_config(&dir, wait)?;
        let mut config = base()?;
        edit(&mut config);
        self.save_workspace(workspace, &config)?;
        Ok(config)
    }

    /// Persist a config to `<workspace>/<state-dir>/config.toml`.
    /// Creates the directory if it does not exist. A read-modify-write goes through
    /// [`Self::update_workspace_from`], which holds the file's lock; this alone takes none.
    pub fn save_workspace(
        &self,
        workspace: &std::path::Path,
        config: &HostConfig,
    ) -> Result<(), ConfigError> {
        let dir = workspace.join(".rightkit");
        std::fs::create_dir_all(&dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let path = dir.join("config.toml");
        let content =
            toml::to_string_pretty(config).map_err(|e| ConfigError::Toml(e.to_string()))?;
        crate::atomic_write(&path, content.as_bytes())
            .map_err(|e| ConfigError::Io(e.to_string()))?;
        Ok(())
    }
}

/// How long a config writer waits for another writer of the same file.
const CONFIG_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The cross-process lock beside a `config.toml` (`config.toml.lock` in `dir`). The default lock kind
/// also excludes other threads of this process.
fn lock_config(
    dir: &std::path::Path,
    wait: std::time::Duration,
) -> Result<rightkit_fs::FileLock, ConfigError> {
    rightkit_fs::FileLock::try_lock_with(
        &dir.join("config.toml.lock"),
        rightkit_fs::LockKind::default(),
        wait,
    )
    .map_err(|e| ConfigError::Io(format!("config.toml is locked by another process: {e}")))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bool_accepts_common_spellings() {
        for value in ["1", "true", "TRUE", "yes", "on"] {
            assert_eq!(parse_bool(value), Some(true));
        }
        for value in ["0", "false", "FALSE", "no", "off"] {
            assert_eq!(parse_bool(value), Some(false));
        }
        assert_eq!(parse_bool("maybe"), None);
    }

    #[test]
    fn apply_env_ignores_empty_and_invalid_values() {
        let loader = ConfigLoader::new();
        let mut config = HostConfig::default();
        config.model.default_model = "kept-model".to_string();
        config.features.cli_acquisition_v1 = Some(false);
        let mut env = HashMap::new();
        env.insert("RIGHTKIT_MODEL".to_string(), String::new());
        env.insert(
            "RIGHTKIT_CLI_ACQUISITION_V1".to_string(),
            "maybe".to_string(),
        );

        let out = loader.apply_env(config, &env);

        assert_eq!(out.model.default_model, "kept-model");
        assert_eq!(out.features.cli_acquisition_v1, Some(false));
    }

    #[test]
    fn load_files_for_workspace_moves_invalid_config_aside_keeping_bytes() {
        let _guard = crate::agent_config::env_var_test_lock();
        let loader = ConfigLoader::new();
        let prior_home = std::env::var_os(rightkit_daemon::ENV_SUITE_ROOT);
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var(rightkit_daemon::ENV_SUITE_ROOT, home.path());
        }
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join(".rightkit");
        std::fs::create_dir_all(&config_dir).unwrap();
        let original = "not = = toml";
        std::fs::write(config_dir.join("config.toml"), original).unwrap();

        let _ = loader.load_files_for_workspace(dir.path());

        unsafe {
            match prior_home {
                Some(value) => std::env::set_var(rightkit_daemon::ENV_SUITE_ROOT, value),
                None => std::env::remove_var(rightkit_daemon::ENV_SUITE_ROOT),
            }
        }

        assert!(!config_dir.join("config.toml").exists());
        let aside: Vec<_> = std::fs::read_dir(&config_dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("config.toml.corrupt-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), original);
    }

    #[test]
    fn load_files_for_workspace_skips_invalid_workspace_config() {
        let _guard = crate::agent_config::env_var_test_lock();
        let loader = ConfigLoader::new();
        let prior_home = std::env::var_os(rightkit_daemon::ENV_SUITE_ROOT);
        let home = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var(rightkit_daemon::ENV_SUITE_ROOT, home.path());
        }
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join(".rightkit");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("config.toml"), "not = = toml").unwrap();
        let baseline_dir = tempfile::tempdir().unwrap();

        let out = loader.load_files_for_workspace(dir.path());
        let baseline = loader.load_files_for_workspace(baseline_dir.path());

        unsafe {
            match prior_home {
                Some(value) => std::env::set_var(rightkit_daemon::ENV_SUITE_ROOT, value),
                None => std::env::remove_var(rightkit_daemon::ENV_SUITE_ROOT),
            }
        }

        assert_eq!(out, baseline);
    }

    fn read_workspace(workspace: &std::path::Path) -> Result<HostConfig, ConfigError> {
        match std::fs::read_to_string(workspace.join(".rightkit").join("config.toml")) {
            Ok(text) => ConfigLoader::new().load_str(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HostConfig::default()),
            Err(e) => Err(ConfigError::Io(e.to_string())),
        }
    }

    /// Concurrent read-modify-writes of the workspace file lose nothing: each of eight threads adds
    /// its own toolset, and all eight are in the file afterwards.
    #[test]
    fn update_workspace_serialises_concurrent_writers() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let workspace = workspace.clone();
                std::thread::spawn(move || {
                    ConfigLoader::new()
                        .update_workspace_from(
                            &workspace,
                            || read_workspace(&workspace),
                            |config| {
                                // Widen the read-modify-write window so an unlocked writer would lose updates.
                                std::thread::sleep(std::time::Duration::from_millis(20));
                                config.toolsets.insert(
                                    format!("t{n}"),
                                    crate::agent_config::ToolsetConfig::default(),
                                );
                            },
                        )
                        .expect("every writer gets the lock in turn")
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let saved = read_workspace(&workspace).unwrap();
        for n in 0..8 {
            assert!(
                saved.toolsets.contains_key(&format!("t{n}")),
                "t{n} was lost: {:?}",
                saved.toolsets.keys()
            );
        }
    }

    /// A held lock refuses the writer after its wait, with the typed error, and writes nothing.
    #[test]
    fn update_workspace_refuses_while_the_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let config_dir = workspace.join(".rightkit");
        std::fs::create_dir_all(&config_dir).unwrap();
        let _held = lock_config(&config_dir, std::time::Duration::from_millis(200)).unwrap();
        let err = ConfigLoader::new()
            .update_workspace_from_waiting(
                &workspace,
                std::time::Duration::from_millis(150),
                || Ok(HostConfig::default()),
                |config| {
                    config
                        .toolsets
                        .insert("late".into(), crate::agent_config::ToolsetConfig::default());
                },
            )
            .unwrap_err();
        assert!(err.to_string().contains("locked"), "{err}");
        assert!(
            !config_dir.join("config.toml").exists(),
            "a refused writer wrote nothing"
        );
    }
}
