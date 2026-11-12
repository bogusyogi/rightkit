//! A pre-0.7 entry's earlier revisions survive its migration, and an installed model is not
//! replaced while a running app holds a runtime lease.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rightkit_models::hash::sha256_file;
use rightkit_models::store::{FileRecord, RevisionRecord, State};
use rightkit_models::suite::lease;
use rightkit_models::{
    encode_revision_key, ErrorCode, FileSpec, InstallOptions, ModelSpec, RevisionSpec, Store,
};

const ID: &str = "keep-test-model";

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn record(revision: &str, file: &Path) -> RevisionRecord {
    RevisionRecord {
        revision: revision.into(),
        quant: None,
        installed_at_unix: 1,
        files: vec![FileRecord {
            path: "weights.bin".into(),
            bytes: fs::metadata(file).unwrap().len(),
            sha256: sha256_file(file).unwrap(),
            shared: None,
        }],
    }
}

#[test]
fn migration_keeps_the_old_layout_and_reports_it() {
    let suite = tempfile::tempdir().unwrap();
    let root = suite.path().join("models");
    let dir = root.join(ID);
    let old_key = encode_revision_key("r0", None)
        .unwrap()
        .as_str()
        .to_string();
    let key = encode_revision_key("r1", None)
        .unwrap()
        .as_str()
        .to_string();
    let old_file = dir.join("revisions").join(&old_key).join("weights.bin");
    let file = dir.join("revisions").join(&key).join("weights.bin");
    write(&old_file, b"earlier weights");
    write(&file, b"current weights");
    let state = State {
        id: ID.into(),
        active: Some(key.clone()),
        history: vec![old_key.clone()],
        revisions: BTreeMap::from([
            (old_key.clone(), record("r0", &old_file)),
            (key.clone(), record("r1", &file)),
        ]),
    };
    write(
        &dir.join("state.json"),
        serde_json::to_string(&state).unwrap().as_bytes(),
    );

    let store = Store::new(&root, "keep-test");
    assert!(store.recover(ID).unwrap(), "the old entry migrates");
    // The active revision is the suite entry now.
    assert_eq!(
        fs::read(dir.join("weights.bin")).unwrap(),
        b"current weights"
    );
    assert!(dir.join("manifest.json").is_file());
    assert!(!dir.join("state.json").exists());
    // The rest of the old directory is kept, not deleted, and reported.
    let kept = store.legacy_kept().unwrap();
    assert_eq!(kept, vec![root.join(".legacy-kept").join(ID)]);
    assert_eq!(
        fs::read(kept[0].join("revisions").join(&old_key).join("weights.bin")).unwrap(),
        b"earlier weights"
    );
    assert!(kept[0].join("state.json").is_file());
    // A second recover changes nothing and keeps the report.
    assert!(!store.recover(ID).unwrap());
    assert_eq!(store.legacy_kept().unwrap(), kept);
    // Only a listed entry can be removed; removing it frees the space and empties the report.
    let outside = suite.path().join("elsewhere");
    fs::create_dir_all(&outside).unwrap();
    assert_eq!(
        store.remove_legacy_kept(&outside).unwrap_err().code,
        ErrorCode::InvalidSpec
    );
    assert_eq!(
        store.remove_legacy_kept(&root.join(ID)).unwrap_err().code,
        ErrorCode::InvalidSpec
    );
    assert!(outside.is_dir() && dir.join("weights.bin").is_file());
    store.remove_legacy_kept(&kept[0]).unwrap();
    assert!(store.legacy_kept().unwrap().is_empty());
    assert!(!root.join(".legacy-kept").exists());
    assert_eq!(
        fs::read(dir.join("weights.bin")).unwrap(),
        b"current weights"
    );
}

fn spec(revision: &str, payload: &Path) -> ModelSpec {
    ModelSpec::new(
        ID,
        vec![RevisionSpec {
            revision: revision.into(),
            files: vec![FileSpec {
                path: "weights.bin".into(),
                url: "https://models.invalid/weights.bin".into(),
                bytes: Some(fs::metadata(payload).unwrap().len()),
                sha256: Some(sha256_file(payload).unwrap()),
                ..FileSpec::default()
            }],
            ..RevisionSpec::default()
        }],
    )
}

#[test]
fn a_model_in_use_is_not_replaced() {
    let suite = tempfile::tempdir().unwrap();
    let root = suite.path().join("models");
    let store = Store::new(&root, "keep-test");
    let v1 = suite.path().join("v1");
    let v2 = suite.path().join("v2");
    write(&v1.join("weights.bin"), b"first weights");
    write(&v2.join("weights.bin"), b"second weights");
    let opts = InstallOptions::default();
    store
        .adopt_flat_dir(&spec("r1", &v1.join("weights.bin")), &opts, &v1)
        .unwrap();
    let installed = root.join(ID).join("weights.bin");
    assert_eq!(fs::read(&installed).unwrap(), b"first weights");

    // A running app holds a model loaded: the replacement refuses and leaves the entry as it was.
    let held = lease::try_acquire_at_root(suite.path(), lease::PARAKEET_MODEL_FAMILY, "other-app")
        .unwrap()
        .expect("the lease is free");
    let error = store
        .adopt_flat_dir(&spec("r2", &v2.join("weights.bin")), &opts, &v2)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Busy);
    assert!(error.message.contains("in use"), "{}", error.message);
    assert_eq!(fs::read(&installed).unwrap(), b"first weights");
    assert_eq!(
        store.state(ID).unwrap().active.as_deref(),
        Some(encode_revision_key("r1", None).unwrap().as_str())
    );

    // Released: the replacement goes through.
    drop(held);
    store
        .adopt_flat_dir(&spec("r2", &v2.join("weights.bin")), &opts, &v2)
        .unwrap();
    assert_eq!(fs::read(&installed).unwrap(), b"second weights");
}

#[test]
fn a_store_outside_the_suite_has_no_leases_and_gets_none() {
    // An app's own model folder: no `runtime/model-use/` beside it. A replacement goes through
    // and no lease folder appears next to the store.
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("models");
    let store = Store::new(&root, "keep-test");
    let v1 = home.path().join("v1");
    let v2 = home.path().join("v2");
    write(&v1.join("weights.bin"), b"first weights");
    write(&v2.join("weights.bin"), b"second weights");
    let opts = InstallOptions::default();
    store
        .adopt_flat_dir(&spec("r1", &v1.join("weights.bin")), &opts, &v1)
        .unwrap();
    store
        .adopt_flat_dir(&spec("r2", &v2.join("weights.bin")), &opts, &v2)
        .unwrap();
    assert_eq!(
        fs::read(root.join(ID).join("weights.bin")).unwrap(),
        b"second weights"
    );
    assert!(!home.path().join("runtime").exists());
}
