//! Per-plugin signature provenance records.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SignatureStatus {
    Signed,
    Unsigned,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSignatureRecord {
    pub status: SignatureStatus,
    #[serde(default)]
    pub signed_by: String,
    #[serde(default)]
    pub signing_public_key: String,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub installed_at: String,
    #[serde(default)]
    pub install_source: String,
}

impl PluginSignatureRecord {
    pub fn signed(
        signed_by: String,
        signing_public_key: String,
        signature: String,
        install_source: String,
        now_iso: String,
    ) -> Self {
        Self {
            status: SignatureStatus::Signed,
            signed_by,
            signing_public_key,
            signature,
            installed_at: now_iso,
            install_source,
        }
    }

    pub fn unsigned(install_source: String, now_iso: String) -> Self {
        Self {
            status: SignatureStatus::Unsigned,
            signed_by: String::new(),
            signing_public_key: String::new(),
            signature: String::new(),
            installed_at: now_iso,
            install_source,
        }
    }
}

pub fn sidecar_path(plugin_install_dir: &Path) -> PathBuf {
    plugin_install_dir.join(".signature.json")
}

pub fn read_record(plugin_install_dir: &Path) -> PluginSignatureRecord {
    let path = sidecar_path(plugin_install_dir);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| unknown_record()),
        Err(_) => unknown_record(),
    }
}

pub fn write_record(plugin_install_dir: &Path, record: &PluginSignatureRecord) {
    let path = sidecar_path(plugin_install_dir);
    match serde_json::to_string_pretty(record) {
        Ok(json) => {
            if let Err(error) = std::fs::write(&path, json) {
                eprintln!(
                    "[plugin-signature] failed to write provenance to {}: {error}",
                    path.display()
                );
            }
        }
        Err(error) => eprintln!("[plugin-signature] serialise record failed: {error}"),
    }
}

pub fn is_acceptable(record: &PluginSignatureRecord, policy_reject_unsigned: bool) -> bool {
    match record.status {
        SignatureStatus::Signed | SignatureStatus::Unknown => true,
        SignatureStatus::Unsigned => !policy_reject_unsigned,
    }
}

fn unknown_record() -> PluginSignatureRecord {
    PluginSignatureRecord {
        status: SignatureStatus::Unknown,
        signed_by: String::new(),
        signing_public_key: String::new(),
        signature: String::new(),
        installed_at: String::new(),
        install_source: String::new(),
    }
}
