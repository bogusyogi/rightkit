//! Host configuration: the names an embedding app supplies once at startup.
//!
//! RightKit crates carry no product names. An app that embeds this crate calls [`configure`]
//! before first use to name its settings namespace, its model-store app label, the prefix of its
//! environment overrides and the folder names it keeps on disk. Without a call, the RightKit
//! defaults apply (`rightkit` everywhere, environment prefix `RIGHTKIT`).

use std::sync::OnceLock;

/// The names an embedding app supplies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConfig {
    /// Settings document namespace (`<app data>/<namespace>/settings.json`).
    pub settings_namespace: String,
    /// App label recorded in `rightkit-models` directory locks.
    pub store_app: String,
    /// Prefix of the environment overrides (`<PREFIX>_MODEL_ROOT`, ...). `RIGHTKIT_` names are
    /// read as a fallback whatever the prefix.
    pub env_prefix: String,
    /// Folder name used for the per-machine lock directory (`/tmp/<name>-<uid>` on Unix,
    /// `%PROGRAMDATA%\<name>\runtime` on Windows) and the default workspace folder under home.
    pub machine_dir_name: String,
    /// Reverse-DNS app identifier: the per-user data folder name and the OS credential store
    /// service. Frozen once users have data: changing it orphans their keys and data.
    pub app_identifier: String,
    /// Folder under the content root that holds the host's outputs
    /// (`<content root>/<content_subdir>/local-images`).
    pub content_subdir: String,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            settings_namespace: "rightkit".into(),
            store_app: "rightkit".into(),
            env_prefix: "RIGHTKIT".into(),
            machine_dir_name: "rightkit".into(),
            app_identifier: "dev.rightkit.app".into(),
            content_subdir: "rightkit".into(),
        }
    }
}

static CONFIG: OnceLock<HostConfig> = OnceLock::new();

/// Set the host configuration once. Returns the rejected value when one is already set.
pub fn configure(config: HostConfig) -> Result<(), Box<HostConfig>> {
    CONFIG.set(config).map_err(Box::new)
}

/// The configuration in force (the defaults until [`configure`] is called).
pub fn config() -> &'static HostConfig {
    CONFIG.get_or_init(HostConfig::default)
}

/// The full name of the environment override `suffix` under the host prefix.
pub fn env_name(suffix: &str) -> String {
    format!("{}_{suffix}", config().env_prefix)
}

/// Read the environment override `suffix`: `<PREFIX>_<suffix>`, then `RIGHTKIT_<suffix>`.
pub fn env_var(suffix: &str) -> Option<String> {
    std::env::var(env_name(suffix))
        .ok()
        .or_else(|| std::env::var(format!("RIGHTKIT_{suffix}")).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_product_neutral() {
        let c = HostConfig::default();
        assert_eq!(c.env_prefix, "RIGHTKIT");
        assert_eq!(c.settings_namespace, "rightkit");
    }
}
