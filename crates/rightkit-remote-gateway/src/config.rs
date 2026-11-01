//! Names the gateway shares with the host app's engine, chosen once at start.
//!
//! The gateway is product-neutral. The few names it must agree on with the app's resident engine
//! (the discovery service, the engine program, the label a command-line process says hello as) and
//! the apps it must never control (the host app's own) come from one [`GatewayConfig`], installed
//! with [`configure`]. Until then [`config`] answers the neutral defaults.

use std::sync::OnceLock;

/// What the gateway needs to know about the host app.
#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// The resident engine's discovery name (default `rightkit`).
    pub engine_service: String,
    /// The resident engine's program name without a platform suffix; it must sit beside the
    /// running executable (default `rightkit-engine`).
    pub engine_executable: String,
    /// The label a process that is not the gateway says hello as when it asks the engine
    /// (default `rightkit-cli`).
    pub default_label: String,
    /// Application ids (bundle ids, AppUserModelIDs) that start with one of these are the host
    /// app's own and are never launched or controlled for an agent. Empty by default.
    pub protected_app_prefixes: Vec<String>,
    /// A program file name starting with this and ending `.exe` is the host app's own on Windows
    /// and is never launched or controlled for an agent.
    pub protected_exe_prefix: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig {
            engine_service: "rightkit".to_string(),
            engine_executable: "rightkit-engine".to_string(),
            default_label: "rightkit-cli".to_string(),
            protected_app_prefixes: Vec::new(),
            protected_exe_prefix: None,
        }
    }
}

static CONFIG: OnceLock<GatewayConfig> = OnceLock::new();

/// Installs the configuration. Only the first call takes effect; returns `false` otherwise (also
/// when the gateway already read it, which fixes the default).
pub fn configure(config: GatewayConfig) -> bool {
    CONFIG.set(config).is_ok()
}

/// The installed configuration, or the neutral default.
pub fn config() -> &'static GatewayConfig {
    CONFIG.get_or_init(GatewayConfig::default)
}
