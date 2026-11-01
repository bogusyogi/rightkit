//! Portable plugin path & manifest projections.

use std::path::PathBuf;

pub fn default_plugin_dir() -> PathBuf {
    // An explicit directory is already the catalog root; only home-derived
    // defaults need the plugins suffix.
    if let Some(dir) = std::env::var_os("RIGHTKIT_PLUGIN_DIR") {
        return PathBuf::from(dir);
    }
    // No fallback location: a missing state root fails, it never redirects plugins into the
    // working directory.
    rightkit_app_settings::agent_config::state_root()
        .unwrap_or_else(|| {
            eprintln!("rightkit: cannot continue without a state directory; refusing to fall back");
            std::process::exit(2)
        })
        .join("plugins")
}

pub fn plugin_manifest_json(
    manifest: &rightkit_agent_plugins::PluginManifest,
) -> serde_json::Value {
    serde_json::json!({
        "id": manifest.id,
        "name": manifest.name,
        "version": manifest.version,
        "description": manifest.description,
        "entry": manifest.entry,
        "providesTools": manifest.provides_tools,
        "providesCommands": manifest.provides_commands,
        "tools": manifest.tools,
        "commands": manifest.commands,
        "prompts": manifest.prompts,
        "middleware": manifest.middleware,
        "sandbox": manifest.sandbox,
        "enabled": manifest.enabled,
    })
}
