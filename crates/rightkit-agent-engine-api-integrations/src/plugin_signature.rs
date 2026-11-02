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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rk-plugin-signature-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_written_record_reads_back() {
        let dir = scratch();
        let record = PluginSignatureRecord::signed(
            "Acme".into(),
            "pk".into(),
            "sig".into(),
            "marketplace".into(),
            "2026-11-02T00:00:00Z".into(),
        );
        write_record(&dir, &record);
        assert_eq!(sidecar_path(&dir), dir.join(".signature.json"));
        let back = read_record(&dir);
        assert_eq!(back.status, SignatureStatus::Signed);
        assert_eq!(back.signed_by, "Acme");
        assert_eq!(back.signing_public_key, "pk");
        assert_eq!(back.signature, "sig");
        assert_eq!(back.install_source, "marketplace");
        assert_eq!(back.installed_at, "2026-11-02T00:00:00Z");
        let raw = std::fs::read_to_string(sidecar_path(&dir)).unwrap();
        assert!(raw.contains("\"status\": \"signed\""), "{raw}");
        assert!(raw.contains("\"signedBy\": \"Acme\""), "{raw}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_corrupt_sidecar_reads_as_unknown() {
        let dir = scratch();
        assert_eq!(read_record(&dir).status, SignatureStatus::Unknown);
        std::fs::write(sidecar_path(&dir), "{ nope").unwrap();
        let record = read_record(&dir);
        assert_eq!(record.status, SignatureStatus::Unknown);
        assert!(record.signed_by.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_unsigned_plugins_are_refused_and_only_under_the_policy() {
        let unsigned = PluginSignatureRecord::unsigned("local".into(), "t".into());
        assert_eq!(unsigned.status, SignatureStatus::Unsigned);
        assert!(unsigned.signed_by.is_empty());
        assert!(is_acceptable(&unsigned, false));
        assert!(!is_acceptable(&unsigned, true));
        let signed = PluginSignatureRecord::signed(
            "A".into(),
            "k".into(),
            "s".into(),
            "m".into(),
            "t".into(),
        );
        assert!(is_acceptable(&signed, true));
        assert!(is_acceptable(&unknown_record(), true));
    }
}
