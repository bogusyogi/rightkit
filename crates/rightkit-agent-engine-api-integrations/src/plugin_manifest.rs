//! Portable plugin path & manifest projections.

use crate::plugin_signature::{read_record, SignatureStatus};
use std::path::{Path, PathBuf};

pub fn default_plugin_dir() -> PathBuf {
    // An explicit directory is already the catalog root; only home-derived
    // defaults need the plugins suffix.
    if let Some(dir) = std::env::var_os("RIGHTKIT_PLUGIN_DIR") {
        return PathBuf::from(dir);
    }
    rightkit_app_settings::agent_config::state_root()
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".rightkit")
        })
        .join("plugins")
}

pub fn default_plugin_marketplace_path() -> PathBuf {
    std::env::var_os("RIGHTKIT_PLUGIN_MARKETPLACE")
        .map(PathBuf::from)
        .or_else(|| {
            rightkit_app_settings::agent_config::state_root()
                .map(|root| root.join("marketplace.json"))
        })
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".rightkit")
                .join("marketplace.json")
        })
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

pub fn plugin_manifest_json_with_provenance(
    manifest: &rightkit_agent_plugins::PluginManifest,
    install_dir: &Path,
) -> serde_json::Value {
    let mut value = plugin_manifest_json(manifest);
    let record = read_record(install_dir);
    let status = match record.status {
        SignatureStatus::Signed => "signed",
        SignatureStatus::Unsigned => "unsigned",
        SignatureStatus::Unknown => "unknown",
    };
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "signatureStatus".into(),
            serde_json::Value::String(status.into()),
        );
        object.insert(
            "signatureSignedBy".into(),
            serde_json::Value::String(record.signed_by),
        );
    }
    value
}
