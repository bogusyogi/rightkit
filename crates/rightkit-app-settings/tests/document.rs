//! Frontend-owned settings documents (`Document`): opaque object, frontend-stated version,
//! envelope persistence, revision CAS, downgrade refusal and one-time import.

use rightkit_app_settings::{Document, Error, FileFormat, Schema, StoreOptions};
use serde_json::{json, Value};
use std::fs;

fn path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("view").join("settings.json")
}

#[test]
fn missing_document_is_none_and_put_persists_an_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let p = path(&dir);
    let doc = Document::open_path(&p, "view-right").unwrap();
    assert_eq!(doc.get().unwrap(), None);
    let s = doc.put(3, json!({ "theme": "dark", "n": [1, 2] }), Some(0)).unwrap();
    assert_eq!((s.version, s.revision), (3, 1));
    let got = doc.get().unwrap().unwrap();
    assert_eq!(got, s);
    assert_eq!(got.value["n"], json!([1, 2]));
    let raw: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(raw["schemaVersion"], 3);
    assert_eq!(raw["namespace"], "view-right");
    assert_eq!(raw["values"]["theme"], "dark");
    // A second handle (another process) sees it.
    let other = Document::open_path(&p, "view-right").unwrap();
    assert_eq!(other.get().unwrap().unwrap().value["theme"], "dark");
}

#[test]
fn patch_merges_top_level_keys_and_null_removes() {
    let dir = tempfile::tempdir().unwrap();
    let doc = Document::open_path(path(&dir), "app").unwrap();
    doc.put(1, json!({ "a": 1, "b": { "x": 1, "y": 2 }, "c": true }), None)
        .unwrap();
    let s = doc
        .patch(1, &json!({ "a": 2, "b": { "x": 9 }, "c": null, "d": "new" }), None)
        .unwrap();
    assert_eq!(s.value, json!({ "a": 2, "b": { "x": 9 }, "d": "new" }));
    assert_eq!(s.revision, 2);
    // Patching a missing document starts from empty.
    let dir2 = tempfile::tempdir().unwrap();
    let fresh = Document::open_path(path(&dir2), "app").unwrap();
    assert_eq!(
        fresh.patch(2, &json!({ "k": 1 }), None).unwrap().value,
        json!({ "k": 1 })
    );
    assert!(matches!(
        fresh.patch(2, &json!([1]), None),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        fresh.put(2, json!("not an object"), None),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn unchanged_patch_commits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let doc = Document::open_path(path(&dir), "app").unwrap();
    doc.put(1, json!({ "a": 1 }), None).unwrap();
    let s = doc.patch(1, &json!({ "a": 1 }), None).unwrap();
    assert_eq!(s.revision, 1);
    assert_eq!(doc.get().unwrap().unwrap().revision, 1);
}

#[test]
fn stale_revision_conflicts_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let doc = Document::open_path(path(&dir), "app").unwrap();
    let first = doc.put(1, json!({ "a": 1 }), Some(0)).unwrap();
    doc.put(1, json!({ "a": 2 }), Some(first.revision)).unwrap();
    let err = doc
        .put(1, json!({ "a": 3 }), Some(first.revision))
        .unwrap_err();
    assert!(matches!(err, Error::Conflict { expected: 1, actual: 2 }));
    assert_eq!(doc.get().unwrap().unwrap().value, json!({ "a": 2 }));
    assert!(matches!(
        doc.patch(1, &json!({ "a": 9 }), Some(1)),
        Err(Error::Conflict { .. })
    ));
}

#[test]
fn an_older_frontend_version_never_overwrites_a_newer_document() {
    let dir = tempfile::tempdir().unwrap();
    let doc = Document::open_path(path(&dir), "app").unwrap();
    doc.put(5, json!({ "a": 1 }), None).unwrap();
    let err = doc.put(4, json!({ "a": 2 }), None).unwrap_err();
    assert!(matches!(err, Error::NewerSchema { found: 5, supported: 4 }));
    assert!(matches!(
        doc.patch(4, &json!({ "a": 2 }), None),
        Err(Error::NewerSchema { .. })
    ));
    assert_eq!(doc.get().unwrap().unwrap().value, json!({ "a": 1 }));
    // Raising the version (the frontend's migration write-back) is allowed.
    let s = doc.put(6, json!({ "a": 1, "b": 2 }), None).unwrap();
    assert_eq!(s.version, 6);
}

#[test]
fn namespace_mismatch_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = path(&dir);
    Document::open_path(&p, "one")
        .unwrap()
        .put(1, json!({}), None)
        .unwrap();
    let other = Document::open_path(&p, "two").unwrap();
    assert!(matches!(
        other.put(1, json!({ "a": 1 }), None),
        Err(Error::Invalid(m)) if m.contains("namespace")
    ));
    assert!(matches!(
        Document::open_path(&p, "bad name"),
        Err(Error::BadSchema(_))
    ));
}

#[test]
fn legacy_flat_file_reads_as_version_zero_and_is_adopted_on_write() {
    let dir = tempfile::tempdir().unwrap();
    let p = path(&dir);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, r#"{"theme":"light"}"#).unwrap();
    let doc = Document::open_path(&p, "app").unwrap();
    let s = doc.get().unwrap().unwrap();
    assert_eq!((s.version, s.value["theme"].clone()), (0, json!("light")));
    let s = doc.patch(1, &json!({ "font": 14 }), None).unwrap();
    assert_eq!(s.version, 1);
    let raw: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(raw["values"], json!({ "theme": "light", "font": 14 }));
}

#[test]
fn import_if_missing_runs_once_and_only_for_a_missing_document() {
    let dir = tempfile::tempdir().unwrap();
    let doc = Document::open_path(path(&dir), "app").unwrap();
    // Nothing to import: nothing stored, and a later import may still run.
    assert!(!doc.import_if_missing(2, || Ok(None)).unwrap());
    assert_eq!(doc.get().unwrap(), None);
    assert!(doc
        .import_if_missing(2, || Ok(Some(json!({ "legacy": true }))))
        .unwrap());
    let s = doc.get().unwrap().unwrap();
    assert_eq!((s.version, s.value), (2, json!({ "legacy": true })));
    // Already present: the builder does not even run.
    let ran = std::cell::Cell::new(false);
    assert!(!doc
        .import_if_missing(2, || {
            ran.set(true);
            Ok(Some(json!({ "legacy": false })))
        })
        .unwrap());
    assert!(!ran.get());
    assert_eq!(doc.get().unwrap().unwrap().value, json!({ "legacy": true }));
    // A failing import writes nothing.
    let dir2 = tempfile::tempdir().unwrap();
    let doc2 = Document::open_path(path(&dir2), "app").unwrap();
    assert!(doc2
        .import_if_missing(1, || Err(Error::Invalid("boom".into())))
        .is_err());
    assert_eq!(doc2.get().unwrap(), None);
}

#[test]
fn damaged_file_is_quarantined_and_backup_is_kept_like_a_store() {
    let dir = tempfile::tempdir().unwrap();
    let p = path(&dir);
    let doc = Document::open_path(&p, "app").unwrap();
    doc.put(1, json!({ "a": 1 }), None).unwrap();
    doc.put(1, json!({ "a": 2 }), None).unwrap();
    assert!(p.with_file_name("settings.json.bak").exists());
    fs::write(&p, "{ not json").unwrap();
    // Reads fall back to the last good backup.
    assert_eq!(doc.get().unwrap().unwrap().value, json!({ "a": 1 }));
    // The next write quarantines the damaged file and commits.
    doc.put(1, json!({ "a": 3 }), None).unwrap();
    let names: Vec<String> = fs::read_dir(p.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().any(|n| n.starts_with("settings.json.corrupt-")));
    assert_eq!(doc.get().unwrap().unwrap().value, json!({ "a": 3 }));
}

#[test]
fn open_path_with_requires_the_envelope_format() {
    let dir = tempfile::tempdir().unwrap();
    let p = path(&dir);
    let flat = StoreOptions::new(Schema::new("app", 1)).format(FileFormat::Flat);
    assert!(matches!(
        Document::open_path_with(&p, &flat),
        Err(Error::Invalid(_))
    ));
    let doc = Document::open_path_with(&p, &StoreOptions::new(Schema::new("app", 1))).unwrap();
    doc.put(7, json!({ "a": 1 }), None).unwrap();
    assert_eq!(doc.get().unwrap().unwrap().version, 7);
}
