//! M10-O5: `StoreOptions::max_bytes` read size cap.

use rightkit_app_settings::{Error, Schema, Store, StoreOptions};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Prefs {
    theme: String,
    font_px: u32,
}
impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            font_px: 16,
        }
    }
}

fn opts() -> StoreOptions {
    StoreOptions::new(Schema::new("demo", 1))
}

#[test]
fn max_bytes_refuses_an_oversized_file_without_touching_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(4096));
    fs::write(&path, &big).unwrap();
    let err = Store::<Prefs>::open_path(&path, opts().max_bytes(1024))
        .err()
        .expect("oversized file must be refused");
    assert!(matches!(err, Error::Invalid(m) if m.contains("byte limit")));
    assert_eq!(fs::read_to_string(&path).unwrap(), big);
    let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(names.len(), 1, "no quarantine or sidecar created");
}

#[test]
fn max_bytes_allows_a_file_within_the_limit_and_default_is_unlimited() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let store = Store::<Prefs>::open_path(&path, opts().max_bytes(64 * 1024)).unwrap();
    store
        .update(|p| {
            p.font_px = 18;
            Ok(())
        })
        .unwrap();
    assert_eq!(store.get().font_px, 18);
    let unlimited = Store::<Prefs>::open_path(&path, opts()).unwrap();
    assert_eq!(unlimited.get().font_px, 18);
}

#[test]
fn max_bytes_blocks_a_write_after_the_file_grows_past_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let store = Store::<Prefs>::open_path(&path, opts().max_bytes(2048)).unwrap();
    let grown = format!("{{\"pad\":\"{}\"}}", "y".repeat(8192));
    fs::write(&path, &grown).unwrap();
    assert!(store
        .update(|p| {
            p.font_px = 1;
            Ok(())
        })
        .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), grown);
    // get keeps the last known value.
    assert_eq!(store.get().font_px, 16);
}
