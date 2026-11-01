// Thin daemon-shell re-export for plugin signature provenance.

pub(crate) use rightkit_agent_engine_api_integrations::plugin_signature::*;

#[cfg(test)]
mod signature_provenance_tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "coderight-plugin-sig-test-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_sidecar_is_unknown() {
        let dir = temp_dir("missing");
        assert_eq!(read_record(&dir).status, SignatureStatus::Unknown);
    }

    #[test]
    fn write_then_read_signed_round_trips() {
        let dir = temp_dir("round-trip");
        let record = PluginSignatureRecord::signed(
            "publisher-1".into(),
            "pubkey-1".into(),
            "sig-1".into(),
            "local".into(),
            "2026-07-04T00:00:00Z".into(),
        );
        write_record(&dir, &record);
        let read = read_record(&dir);
        assert_eq!(read.status, SignatureStatus::Signed);
        assert_eq!(read.signed_by, "publisher-1");
        assert_eq!(read.signing_public_key, "pubkey-1");
        assert_eq!(read.install_source, "local");
    }

    #[test]
    fn write_then_read_unsigned_round_trips() {
        let dir = temp_dir("unsigned");
        write_record(
            &dir,
            &PluginSignatureRecord::unsigned("local".into(), "2026-07-04T00:00:00Z".into()),
        );
        assert_eq!(read_record(&dir).status, SignatureStatus::Unsigned);
    }

    #[test]
    fn accept_policy_signed_always_ok() {
        let record = PluginSignatureRecord::signed(
            "x".into(),
            "k".into(),
            "s".into(),
            "local".into(),
            "t".into(),
        );
        assert!(is_acceptable(&record, true));
        assert!(is_acceptable(&record, false));
    }

    #[test]
    fn reject_unsigned_only_when_policy_says() {
        let record = PluginSignatureRecord::unsigned("local".into(), "t".into());
        assert!(is_acceptable(&record, false));
        assert!(!is_acceptable(&record, true));
    }
}
