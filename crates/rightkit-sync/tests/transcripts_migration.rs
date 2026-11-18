//! Public migration API exercised through real legacy files & atomic destination writes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rightkit_sync::transcripts::{LegacyFiles, TranscriptSyncProfile};
use rightkit_sync::Ledger;
use serde_json::{json, Value};

static NEXT: AtomicU64 = AtomicU64::new(0);
const ID: &str = "56894b6f-9bbb-56f6-a23e-2b6f377f9490";

struct Files {
    root: PathBuf,
    legacy: LegacyFiles,
    ledger: PathBuf,
    versions: PathBuf,
}

impl Files {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "rightkit-transcript-migration-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self {
            legacy: LegacyFiles {
                tombstones: root.join("cloud-sync-tombstones.json"),
                pending: root.join("cloud-sync-tombstones.pending.json"),
                aliases: root.join("cloud-sync-aliases.json"),
                acks: root.join("cloud-sync-acks.json"),
            },
            ledger: root.join("ledger.json"),
            versions: root.join("versions.json"),
            root,
        }
    }

    fn acks(&self, scope: Option<&str>) -> Vec<u8> {
        let bytes = serde_json::to_vec(&json!({
            "scope": scope,
            "entries": { (ID): { "updated_at_ms": 42 } }
        }))
        .unwrap();
        std::fs::write(&self.legacy.acks, &bytes).unwrap();
        bytes
    }

    fn migrate(
        &self,
        scope: Option<&str>,
    ) -> Result<Option<rightkit_sync::transcripts::MigrationReport>, String> {
        TranscriptSyncProfile::default().migrate_legacy_once(
            &self.legacy,
            scope,
            &self.ledger,
            &self.versions,
        )
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn matching_scope_preserves_ack_seed_and_legacy_bytes() {
    let files = Files::new();
    let original = files.acks(Some("account-A"));
    let report = files.migrate(Some("account-A")).unwrap().unwrap();
    assert_eq!(report.seeded_versions, 1);
    let ledger: Ledger = serde_json::from_slice(&std::fs::read(&files.ledger).unwrap()).unwrap();
    assert_eq!(ledger.scope.as_deref(), Some("account-A"));
    let versions: Value = serde_json::from_slice(&std::fs::read(&files.versions).unwrap()).unwrap();
    assert_eq!(versions[ID]["updated_at_ms"], 42);
    assert_eq!(versions[ID]["hash"], Value::Null);
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), original);
}

#[test]
fn mismatched_scope_excludes_acks_without_blocking_normal_sync() {
    let files = Files::new();
    let original = files.acks(Some("account-A"));
    let versions = b"{\"already-local\":{\"hash\":null,\"updated_at_ms\":17}}";
    std::fs::write(&files.versions, versions).unwrap();
    assert!(files.migrate(Some("account-B")).unwrap().is_none());
    assert!(!files.ledger.exists());
    assert_eq!(std::fs::read(&files.versions).unwrap(), versions);
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), original);
}

#[test]
fn absent_or_empty_scope_evidence_excludes_without_destination_writes() {
    for (stored, authorized) in [
        (None, Some("account-A")),
        (Some(""), Some("account-A")),
        (Some("account-A"), None),
        (Some("account-A"), Some("")),
        (None, None),
    ] {
        let files = Files::new();
        let original = files.acks(stored);
        assert!(files.migrate(authorized).unwrap().is_none());
        assert!(!files.ledger.exists());
        assert!(!files.versions.exists());
        assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), original);
    }
    let files = Files::new();
    let original = serde_json::to_vec(&json!({
        "entries": { (ID): { "updated_at_ms": 42 } }
    }))
    .unwrap();
    std::fs::write(&files.legacy.acks, &original).unwrap();
    assert!(files.migrate(Some("account-A")).unwrap().is_none());
    assert!(!files.ledger.exists());
    assert!(!files.versions.exists());
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), original);
}

#[test]
fn excluded_scope_can_retry_and_success_remains_one_time() {
    let files = Files::new();
    let original = files.acks(Some("account-A"));
    assert!(files.migrate(Some("account-B")).unwrap().is_none());
    assert!(!files.ledger.exists());
    assert!(!files.versions.exists());
    assert_eq!(
        files
            .migrate(Some("account-A"))
            .unwrap()
            .unwrap()
            .seeded_versions,
        1
    );
    let ledger = std::fs::read(&files.ledger).unwrap();
    let versions = std::fs::read(&files.versions).unwrap();
    assert!(files.migrate(Some("account-A")).unwrap().is_none());
    assert_eq!(std::fs::read(&files.ledger).unwrap(), ledger);
    assert_eq!(std::fs::read(&files.versions).unwrap(), versions);
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), original);
}

#[test]
fn absent_ack_file_excludes_unscoped_deletions() {
    let files = Files::new();
    let tombstones = serde_json::to_vec(&json!([{ "id": ID, "deleted_at_ms": 23 }])).unwrap();
    std::fs::write(&files.legacy.tombstones, &tombstones).unwrap();
    assert!(files.migrate(Some("account-A")).unwrap().is_none());
    assert!(!files.ledger.exists());
    assert_eq!(std::fs::read(&files.legacy.tombstones).unwrap(), tombstones);
    assert!(!files.versions.exists());
}

#[test]
fn failed_ledger_write_retries_without_overwriting_saved_versions() {
    let mut files = Files::new();
    let original = files.acks(Some("account-A"));
    let blocked = files.root.join("blocked-parent");
    std::fs::write(&blocked, b"preserve").unwrap();
    files.ledger = blocked.join("ledger.json");
    assert!(files.migrate(Some("account-A")).is_err());
    assert!(!files.ledger.exists());
    let saved = std::fs::read(&files.versions).unwrap();
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), original);
    std::fs::remove_file(&blocked).unwrap();
    assert_eq!(
        files
            .migrate(Some("account-A"))
            .unwrap()
            .unwrap()
            .seeded_versions,
        0
    );
    let versions: Value = serde_json::from_slice(&std::fs::read(&files.versions).unwrap()).unwrap();
    assert_eq!(versions[ID]["updated_at_ms"], 42);
    assert_eq!(std::fs::read(&files.versions).unwrap(), saved);
    assert!(files.ledger.is_file());
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), original);
}

#[test]
fn matching_ack_scope_imports_unscoped_deletions_and_aliases() {
    let files = Files::new();
    files.acks(Some("account-A"));
    let tombstones = serde_json::to_vec(&json!([{ "id": ID, "deleted_at_ms": 23 }])).unwrap();
    let aliases = serde_json::to_vec(&json!([{ "id": ID, "aliases": ["legacy-name"] }])).unwrap();
    std::fs::write(&files.legacy.tombstones, &tombstones).unwrap();
    std::fs::write(&files.legacy.pending, &tombstones).unwrap();
    std::fs::write(&files.legacy.aliases, &aliases).unwrap();
    let report = files.migrate(Some("account-A")).unwrap().unwrap();
    assert_eq!(report.tombstones, 1);
    assert_eq!(report.pending_tombstones, 1);
    assert_eq!(report.alias_ids, 1);
    assert_eq!(report.seeded_versions, 0);
    let ledger: Ledger = serde_json::from_slice(&std::fs::read(&files.ledger).unwrap()).unwrap();
    assert_eq!(ledger.tombstones.get(ID), Some(&23));
    assert!(ledger.aliases[ID].contains("legacy-name"));
    assert_eq!(std::fs::read(&files.legacy.tombstones).unwrap(), tombstones);
    assert_eq!(std::fs::read(&files.legacy.pending).unwrap(), tombstones);
    assert_eq!(std::fs::read(&files.legacy.aliases).unwrap(), aliases);
}

#[test]
fn mismatched_or_unscoped_acks_never_authorize_unscoped_deletions_or_aliases() {
    for scope in [None, Some("account-A")] {
        let files = Files::new();
        let acks = files.acks(scope);
        let tombstones = serde_json::to_vec(&json!([{ "id": ID, "deleted_at_ms": 23 }])).unwrap();
        let aliases =
            serde_json::to_vec(&json!([{ "id": ID, "aliases": ["other-account"] }])).unwrap();
        std::fs::write(&files.legacy.tombstones, &tombstones).unwrap();
        std::fs::write(&files.legacy.pending, &tombstones).unwrap();
        std::fs::write(&files.legacy.aliases, &aliases).unwrap();
        assert!(files.migrate(Some("account-B")).unwrap().is_none());
        assert!(!files.ledger.exists());
        assert!(!files.versions.exists());
        assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), acks);
        assert_eq!(std::fs::read(&files.legacy.tombstones).unwrap(), tombstones);
        assert_eq!(std::fs::read(&files.legacy.pending).unwrap(), tombstones);
        assert_eq!(std::fs::read(&files.legacy.aliases).unwrap(), aliases);
    }
}

#[test]
fn current_scope_qualified_files_import_without_foreign_ack_seeds() {
    let mut files = Files::new();
    let acks = files.acks(Some("account-A"));
    let profile = TranscriptSyncProfile::default();
    files.legacy.tombstones = files.root.join(profile.legacy_scoped_file_name(
        "account-B",
        Some("account-A"),
        &profile.files.legacy_tombstones,
    ));
    files.legacy.aliases = files.root.join(profile.legacy_scoped_file_name(
        "account-B",
        Some("account-A"),
        &profile.files.legacy_aliases,
    ));
    let tombstones = serde_json::to_vec(&json!([{ "id": ID, "deleted_at_ms": 23 }])).unwrap();
    let aliases =
        serde_json::to_vec(&json!([{ "id": ID, "aliases": ["current-account"] }])).unwrap();
    std::fs::write(&files.legacy.tombstones, &tombstones).unwrap();
    std::fs::write(&files.legacy.aliases, &aliases).unwrap();
    assert!(files.migrate(Some("account-C")).unwrap().is_none());
    let report = files.migrate(Some("account-B")).unwrap().unwrap();
    assert_eq!(report.tombstones, 1);
    assert_eq!(report.alias_ids, 1);
    assert_eq!(report.seeded_versions, 0);
    let ledger: Ledger = serde_json::from_slice(&std::fs::read(&files.ledger).unwrap()).unwrap();
    assert_eq!(ledger.scope.as_deref(), Some("account-B"));
    assert!(!files.versions.exists());
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), acks);
}

#[test]
fn existing_newer_or_older_version_entries_are_never_overwritten() {
    for timestamp in [7, 70] {
        let files = Files::new();
        files.acks(Some("account-A"));
        let original = serde_json::to_vec_pretty(&json!({
            (ID): { "hash": "local-content", "updated_at_ms": timestamp },
            "unrelated": { "hash": "keep", "updated_at_ms": 99 }
        }))
        .unwrap();
        std::fs::write(&files.versions, &original).unwrap();
        assert_eq!(
            files
                .migrate(Some("account-A"))
                .unwrap()
                .unwrap()
                .seeded_versions,
            0
        );
        assert_eq!(std::fs::read(&files.versions).unwrap(), original);
    }
}

#[test]
fn valid_existing_versions_merge_only_missing_seed_ids() {
    let files = Files::new();
    files.acks(Some("account-A"));
    let original = json!({ "unrelated": { "hash": "keep", "updated_at_ms": 99 } });
    std::fs::write(&files.versions, serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(
        files
            .migrate(Some("account-A"))
            .unwrap()
            .unwrap()
            .seeded_versions,
        1
    );
    let versions: Value = serde_json::from_slice(&std::fs::read(&files.versions).unwrap()).unwrap();
    assert_eq!(versions["unrelated"], original["unrelated"]);
    assert_eq!(versions[ID]["updated_at_ms"], 42);
}

#[test]
fn malformed_existing_versions_refuse_before_marker_and_retry_after_repair() {
    let files = Files::new();
    let acks = files.acks(Some("account-A"));
    let malformed = b"{unfinished";
    std::fs::write(&files.versions, malformed).unwrap();
    let error = files.migrate(Some("account-A")).unwrap_err();
    assert!(error.contains("parse existing cloud sync versions"));
    assert_eq!(std::fs::read(&files.versions).unwrap(), malformed);
    assert!(!files.ledger.exists());
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), acks);
    std::fs::write(&files.versions, b"{}").unwrap();
    assert_eq!(
        files
            .migrate(Some("account-A"))
            .unwrap()
            .unwrap()
            .seeded_versions,
        1
    );
    let versions: Value = serde_json::from_slice(&std::fs::read(&files.versions).unwrap()).unwrap();
    assert_eq!(versions[ID]["updated_at_ms"], 42);
    assert!(files.ledger.exists());
}

#[test]
fn unreadable_existing_versions_refuse_before_marker_and_retry_after_repair() {
    let files = Files::new();
    let acks = files.acks(Some("account-A"));
    std::fs::create_dir(&files.versions).unwrap();
    let child = files.versions.join("preserved");
    std::fs::write(&child, b"keep").unwrap();
    let error = files.migrate(Some("account-A")).unwrap_err();
    assert!(error.contains("read existing cloud sync versions"));
    assert!(files.versions.is_dir());
    assert_eq!(std::fs::read(&child).unwrap(), b"keep");
    assert!(!files.ledger.exists());
    assert_eq!(std::fs::read(&files.legacy.acks).unwrap(), acks);
    let preserved = files.root.join("preserved-version-directory");
    std::fs::rename(&files.versions, &preserved).unwrap();
    assert_eq!(
        files
            .migrate(Some("account-A"))
            .unwrap()
            .unwrap()
            .seeded_versions,
        1
    );
    assert_eq!(std::fs::read(preserved.join("preserved")).unwrap(), b"keep");
    assert!(files.ledger.is_file());
}
