use rightkit_logs::{
    BundleMetadata, Consent, ConsentStore, EnqueueError, EventRecord, JsonlStore,
    JsonlStoreConfig, Level, LogSource, Outbox, OutboxConfig,
};
use serde_json::json;

fn metadata() -> BundleMetadata {
    BundleMetadata {
        app_name: "Example".into(),
        app_version: "1".into(),
        build: "test".into(),
        os: "test".into(),
        settings: json!({}),
    }
}

#[test]
fn consumer_stems_retention_and_declared_bundle_sources() {
    let dir = tempfile::tempdir().unwrap();
    // CodeRight & ViewRight both keep events.jsonl at 4 MiB.
    let config = JsonlStoreConfig::from_stem(dir.path(), "events", 4 * 1024 * 1024).unwrap();
    assert_eq!(config.path, dir.path().join("events.jsonl"));
    assert_eq!(config.generations, 1);
    assert_eq!(config.max_file_bytes, 4 * 1024 * 1024);
    let mut config = config;
    config.generations = 2;
    config.max_file_bytes = 1;
    // CodeRight names this source renderer; ViewRight names it app.
    config.bundle_sources = vec![LogSource::new("renderer", &config.path)];
    config.collection.redaction.additional_key_fragments.insert("path".into());
    let store = JsonlStore::from_config(&config);
    for n in 0..4 {
        store.append(
            &EventRecord::new(n, Level::Info, format!("event_{n}"))
                .with_field("source_path", json!("private-diagnostic-path")),
        ).unwrap();
    }
    assert!(dir.path().join("events.jsonl.1").exists());
    assert!(dir.path().join("events.jsonl.2").exists());
    assert!(!dir.path().join("events.jsonl.3").exists());
    let unrelated = dir.path().join("private.jsonl");
    std::fs::write(&unrelated, "{\"event\":\"not_declared\"}\n").unwrap();
    let bundle = config.collect(metadata());
    assert_eq!(bundle.sources.len(), 1);
    assert_eq!(bundle.sources[0].name, "renderer");
    assert!(!serde_json::to_string(&bundle).unwrap().contains("private-diagnostic-path"));
    store.clear().unwrap();
    assert!(!config.path.exists());
    assert!(!dir.path().join("events.jsonl.1").exists());
    assert!(!dir.path().join("events.jsonl.2").exists());
    assert!(unrelated.exists());
}

#[test]
fn stems_cannot_escape_host_directory() {
    for stem in ["", ".", "..", "../raw", "a/b", "a\\b", "C:raw", "a\0b"] {
        assert!(JsonlStoreConfig::from_stem("logs", stem, 1).is_err(), "{stem:?}");
    }
}

#[test]
fn cached_consent_rechecks_replacement_deletion_and_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("consent.json");
    let cached = ConsentStore::new(&path).with_cache(true);
    let other = ConsentStore::new(&path);
    other.grant("v1").unwrap();
    assert!(cached.is_granted());
    assert!(cached.clone().is_granted());
    other.grant("v2").unwrap();
    assert!(cached.status().is_granted_for("v2"));
    other.revoke().unwrap();
    assert!(!cached.is_granted());
    other.grant("v1").unwrap();
    assert!(cached.is_granted());
    std::fs::write(&path, b"{").unwrap();
    assert_eq!(cached.status(), Consent::Unset);
    assert!(cached.last_read_error().is_some());
    std::fs::remove_file(&path).unwrap();
    assert_eq!(cached.status(), Consent::Unset);
    assert_eq!(cached.last_read_error(), None);
    other.grant("v1").unwrap();
    assert!(cached.is_granted());
    cached.invalidate_cache();
    assert!(cached.is_granted());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(cached.status(), Consent::Unset);
    assert!(cached.last_read_error().is_some());
}

const REVOKE_PATH: &str = "RK_LOGS_CONFIG_CHILD_REVOKE_PATH";

#[test]
fn cached_child_revoke() {
    if let Some(path) = std::env::var_os(REVOKE_PATH) {
        ConsentStore::new(path).revoke().unwrap();
    }
}

#[test]
fn outbox_with_cached_store_observes_cross_process_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let consent = ConsentStore::new(dir.path().join("consent.json")).with_cache(true);
    consent.grant("v1").unwrap();
    let outbox = Outbox::open(dir.path().join("outbox.json"), consent, OutboxConfig::default());
    outbox.enqueue("before", json!({"count": 1})).unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cached_child_revoke", "--nocapture"])
        .env(REVOKE_PATH, outbox.consent().path())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(outbox.enqueue("after", json!({})), Err(EnqueueError::NoConsent));
    assert_eq!(outbox.len(), 1);
    outbox.purge().unwrap();
    assert!(outbox.is_empty());
}
