//! `OnRefused::ReadOnlyDefaults`: a file newer than the build, or over `max_bytes`, opens as
//! defaults and is never moved, written, backed up or locked. And undeclared keys survive a save.

use rightkit_app_settings::{
    Error, FileFormat, OnRefused, Refusal, Schema, Store, StoreOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::Path;

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
    StoreOptions::new(Schema::new("demo", 2))
}

const NEWER_ENVELOPE: &str = "{\"schemaVersion\":9,\"namespace\":\"demo\",\"values\":{\"theme\":\"future\",\"font_px\":99,\"hologram\":true},\"revision\":4}";

fn only_file(dir: &Path) {
    let names: Vec<_> = fs::read_dir(dir).unwrap().collect();
    assert_eq!(names.len(), 1, "no quarantine, .bak or .lock beside the file");
}

#[test]
fn a_newer_envelope_fails_open_by_default_and_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, NEWER_ENVELOPE).unwrap();
    let err = Store::<Prefs>::open_path(&path, opts()).err().expect("refused");
    assert!(matches!(
        err,
        Error::NewerSchema {
            found: 9,
            supported: 2
        }
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), NEWER_ENVELOPE);
    only_file(dir.path());
}

#[test]
fn a_newer_envelope_opens_as_read_only_defaults_and_stays_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, NEWER_ENVELOPE).unwrap();
    let store =
        Store::<Prefs>::open_path(&path, opts().on_refused(OnRefused::ReadOnlyDefaults)).unwrap();

    assert_eq!(store.get(), Prefs::default());
    assert!(!store.writable());
    assert_eq!(
        store.refusal(),
        Some(Refusal::NewerSchema {
            found: 9,
            supported: 2
        })
    );

    let update = store.update(|p| {
        p.font_px = 20;
        Ok(())
    });
    assert!(matches!(update, Err(Error::NewerSchema { found: 9, .. })));
    assert!(matches!(
        store.patch(&json!({ "theme": "dark" }), None),
        Err(Error::NewerSchema { .. })
    ));
    assert!(matches!(
        store.remove_keys(&["theme"]),
        Err(Error::NewerSchema { .. })
    ));
    // Still the defaults; the file is byte for byte as it was and nothing was created beside it.
    assert_eq!(store.get(), Prefs::default());
    assert_eq!(fs::read_to_string(&path).unwrap(), NEWER_ENVELOPE);
    only_file(dir.path());
}

#[test]
fn a_refused_store_recovers_when_the_file_is_replaced_by_one_the_build_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, NEWER_ENVELOPE).unwrap();
    let store =
        Store::<Prefs>::open_path(&path, opts().on_refused(OnRefused::ReadOnlyDefaults)).unwrap();
    assert!(!store.writable());

    fs::remove_file(&path).unwrap();
    assert!(store.writable());
    store
        .update(|p| {
            p.font_px = 30;
            Ok(())
        })
        .unwrap();
    assert_eq!(store.get().font_px, 30);
}

#[test]
fn a_current_file_is_an_ordinary_writable_store_under_read_only_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let store =
        Store::<Prefs>::open_path(&path, opts().on_refused(OnRefused::ReadOnlyDefaults)).unwrap();
    assert!(store.writable());
    assert_eq!(store.refusal(), None);
    store
        .update(|p| {
            p.theme = "dark".into();
            Ok(())
        })
        .unwrap();
    assert_eq!(store.get().theme, "dark");
}

#[test]
fn a_newer_flat_file_is_found_through_flat_version_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pill-settings.json");
    let text = "{\"schema_version\":5,\"theme\":\"future\",\"nearby\":{\"on\":true}}";
    fs::write(&path, text).unwrap();
    let flat = || {
        opts()
            .format(FileFormat::Flat)
            .flat_version_key("schema_version")
    };

    let err = Store::<Prefs>::open_path(&path, flat()).err().expect("refused");
    assert!(matches!(
        err,
        Error::NewerSchema {
            found: 5,
            supported: 2
        }
    ));

    let store =
        Store::<Prefs>::open_path(&path, flat().on_refused(OnRefused::ReadOnlyDefaults)).unwrap();
    assert_eq!(store.get(), Prefs::default());
    assert!(!store.writable());
    assert!(store
        .update(|p| {
            p.font_px = 1;
            Ok(())
        })
        .is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    only_file(dir.path());
}

#[test]
fn a_flat_file_at_or_below_the_version_is_read_and_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pill-settings.json");
    fs::write(&path, "{\"schema_version\":2,\"theme\":\"dark\"}").unwrap();
    let store = Store::<Prefs>::open_path(
        &path,
        opts()
            .format(FileFormat::Flat)
            .flat_version_key("schema_version")
            .on_refused(OnRefused::ReadOnlyDefaults),
    )
    .unwrap();
    assert!(store.writable());
    assert_eq!(store.get().theme, "dark");
}

#[test]
fn an_oversized_file_fails_open_by_default_and_opens_read_only_on_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(4096));
    fs::write(&path, &big).unwrap();

    let err = Store::<Prefs>::open_path(&path, opts().max_bytes(1024))
        .err()
        .expect("refused");
    assert!(matches!(err, Error::Invalid(m) if m.contains("byte limit")));

    let store = Store::<Prefs>::open_path(
        &path,
        opts()
            .max_bytes(1024)
            .on_refused(OnRefused::ReadOnlyDefaults),
    )
    .unwrap();
    assert_eq!(store.get(), Prefs::default());
    assert!(!store.writable());
    assert!(matches!(
        store.refusal(),
        Some(Refusal::Oversized { limit: 1024, .. })
    ));
    assert!(matches!(
        store.update(|p| {
            p.font_px = 1;
            Ok(())
        }),
        Err(Error::Invalid(m)) if m.contains("byte limit")
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), big);
    only_file(dir.path());
}

/// The hub writes keys the notch does not read; a save by a build that does not declare them keeps
/// them, in the order the file had them.
#[test]
fn undeclared_keys_survive_a_save_in_flat_and_in_envelope_with_allow_unknown_keys() {
    let dir = tempfile::tempdir().unwrap();

    let flat_path = dir.path().join("flat.json");
    fs::write(
        &flat_path,
        "{\"theme\":\"a\",\"nearby\":{\"on\":true},\"agent\":\"x\",\"font_px\":10}",
    )
    .unwrap();
    let store =
        Store::<Prefs>::open_path(&flat_path, opts().format(FileFormat::Flat)).unwrap();
    store
        .update(|p| {
            p.theme = "b".into();
            Ok(())
        })
        .unwrap();
    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(&flat_path).unwrap()).unwrap();
    assert_eq!(doc["theme"], "b");
    assert_eq!(doc["nearby"], json!({ "on": true }));
    assert_eq!(doc["agent"], "x");

    let env_path = dir.path().join("env.json");
    let env = "{\"schemaVersion\":2,\"namespace\":\"demo\",\"values\":{\"theme\":\"a\",\"remote_tools\":[1,2],\"font_px\":10},\"revision\":1}";
    fs::write(&env_path, env).unwrap();
    let store =
        Store::<Prefs>::open_path(&env_path, opts().allow_unknown_keys(|_| false)).unwrap();
    store
        .update(|p| {
            p.font_px = 11;
            Ok(())
        })
        .unwrap();
    let text = fs::read_to_string(&env_path).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["values"]["remote_tools"], json!([1, 2]));
    assert_eq!(doc["values"]["font_px"], 11);
    // KeyOrder::Preserve (default): the keys keep the order the file had them in.
    let at = |k: &str| text.find(k).unwrap();
    assert!(at("\"theme\"") < at("\"remote_tools\"") && at("\"remote_tools\"") < at("\"font_px\""));
}
