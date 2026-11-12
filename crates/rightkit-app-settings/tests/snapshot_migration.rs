//! `Store::snapshot` on a writable flat store: the app's flat migration is applied in memory, so a
//! file an older writer put back reads as the migrated value, and nothing is written.

use rightkit_app_settings::{FileFormat, Schema, Store, StoreOptions};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    theme: String,
    font_px: u32,
}

/// A fresh directory under the test build's own temp directory.
fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the test directory");
    dir
}

/// Every file in `dir` with its bytes.
fn files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .expect("list the test directory")
        .map(|entry| {
            let entry = entry.expect("read a directory entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            (name, fs::read(entry.path()).expect("read a file"))
        })
        .collect()
}

/// The older layout stored the theme under `colour`.
fn options() -> StoreOptions {
    StoreOptions::new(Schema::new("probe", 2))
        .format(FileFormat::Flat)
        .flat_migration(|mut map| {
            let Some(colour) = map.remove("colour") else {
                return Ok((map, false));
            };
            map.insert("theme".into(), colour);
            Ok((map, true))
        })
}

#[test]
fn a_writable_store_snapshot_migrates_in_memory_and_writes_nothing() {
    let dir = fresh_dir("snapshot-migration-writable");
    let path = dir.join("settings.json");
    fs::write(&path, br#"{"theme":"dark","font_px":14}"#).expect("seed the file");
    let store = Store::<Prefs>::open_path(&path, options()).expect("open");
    assert_eq!(store.snapshot().expect("snapshot").0.theme, "dark");

    // An older build (or a tool that knows only the old layout) replaces the file.
    fs::write(&path, br#"{"colour":"light","font_px":18}"#).expect("replace the file");
    let before = files(&dir);

    let (value, revision) = store.snapshot().expect("snapshot of the replaced file");
    assert_eq!(
        value,
        Prefs {
            theme: "light".into(),
            font_px: 18
        },
        "the snapshot is the migrated file"
    );
    assert_eq!(
        revision,
        store.revision(),
        "the revision is the stored file's"
    );
    assert_eq!(files(&dir), before, "a snapshot writes nothing");
}

#[test]
fn a_migration_error_fails_the_snapshot_and_writes_nothing() {
    let dir = fresh_dir("snapshot-migration-error");
    let path = dir.join("settings.json");
    fs::write(&path, br#"{"theme":"dark"}"#).expect("seed the file");
    let options = StoreOptions::new(Schema::new("probe", 2))
        .format(FileFormat::Flat)
        .flat_migration(|map| {
            if map.contains_key("poison") {
                return Err(rightkit_app_settings::Error::Invalid("poisoned".into()));
            }
            Ok((map, false))
        });
    let store = Store::<Prefs>::open_path(&path, options).expect("open");
    fs::write(&path, br#"{"poison":true}"#).expect("replace the file");
    let before = files(&dir);
    assert!(
        store.snapshot().is_err(),
        "the migration's error is the snapshot's"
    );
    assert_eq!(files(&dir), before);
}
