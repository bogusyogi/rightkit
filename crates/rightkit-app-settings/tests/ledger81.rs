use rightkit_app_settings::{
    read_with_defaults, Error, FileFormat, NullMeans, OnCorrupt, Schema, Store, StoreOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
struct Nick {
    theme: String,
    nick: Option<String>,
}

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

fn nick_schema() -> Schema {
    Schema::new("nicks", 1)
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn read_json(p: &Path) -> Value {
    serde_json::from_slice(&fs::read(p).unwrap()).unwrap()
}

#[test]
fn refuse_on_corrupt_returns_typed_error_and_touches_nothing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    fs::write(&p, b"{ not json").unwrap();
    let opts = StoreOptions::new(nick_schema()).on_corrupt(OnCorrupt::Refuse);
    match Store::<Nick>::open_path(&p, opts) {
        Err(Error::Corrupt(_)) => {}
        Err(other) => panic!("expected Corrupt, got {other}"),
        Ok(_) => panic!("open must refuse a corrupt file"),
    }
    assert_eq!(fs::read(&p).unwrap(), b"{ not json");
    assert_eq!(
        listing(d.path()),
        vec!["settings.json"],
        "nothing moved or written"
    );
}

#[test]
fn refuse_on_corrupt_also_refuses_updates_without_moving_the_file() {
    // Open a healthy store, then corrupt the file under it and try a write.
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    let s = Store::<Nick>::open_path(
        &p,
        StoreOptions::new(nick_schema()).on_corrupt(OnCorrupt::Refuse),
    )
    .unwrap();
    s.update(|v| {
        v.theme = "dark".into();
        Ok(())
    })
    .unwrap();
    fs::write(&p, b"[broken").unwrap();
    let before = listing(d.path());
    let err = s.patch(&json!({"theme": "light"}), None).unwrap_err();
    assert!(matches!(err, Error::Corrupt(_)), "got {err}");
    assert_eq!(fs::read(&p).unwrap(), b"[broken");
    assert_eq!(listing(d.path()), before, "no rename, no new file");
}

#[test]
fn quarantine_is_the_default_and_moves_the_damaged_file_aside() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    fs::write(&p, b"{ not json").unwrap();
    let _s = Store::<Nick>::open_path(&p, StoreOptions::new(nick_schema())).unwrap();
    let names = listing(d.path());
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("settings.json.corrupt-")),
        "damaged file kept aside: {names:?}"
    );
}

#[test]
fn set_null_stores_null_for_a_key_the_file_does_not_hold() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    // Envelope without the `nick` key.
    fs::write(
        &p,
        serde_json::to_vec(&json!({
            "schemaVersion": 1, "namespace": "nicks",
            "values": {"theme": "dark"}, "revision": 1
        }))
        .unwrap(),
    )
    .unwrap();
    let s = Store::<Nick>::open_path(
        &p,
        StoreOptions::new(nick_schema()).null_means(NullMeans::SetNull),
    )
    .unwrap();
    s.patch(&json!({"nick": null}), None).unwrap();
    let doc = read_json(&p);
    assert_eq!(doc["values"]["nick"], Value::Null);
    assert_eq!(doc["values"]["theme"], "dark");
}

#[test]
fn sidecars_off_leaves_exactly_one_file_and_no_lock_or_backup() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    let opts = || StoreOptions::new(nick_schema()).sidecars(false);
    let s = Store::<Nick>::open_path(&p, opts()).unwrap();
    s.update(|v| {
        v.theme = "dark".into();
        Ok(())
    })
    .unwrap();
    s.update(|v| {
        v.theme = "light".into();
        Ok(())
    })
    .unwrap();
    assert_eq!(listing(d.path()), vec!["settings.json"]);
    assert_eq!(read_json(&p)["values"]["theme"], "light");
}

#[test]
fn sidecars_default_keeps_backup_and_lock_files() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    let s = Store::<Nick>::open_path(&p, StoreOptions::new(nick_schema())).unwrap();
    s.update(|v| {
        v.theme = "dark".into();
        Ok(())
    })
    .unwrap();
    s.update(|v| {
        v.theme = "light".into();
        Ok(())
    })
    .unwrap();
    let names = listing(d.path());
    assert!(
        names.contains(&"settings.json.bak".to_string()),
        "{names:?}"
    );
    assert!(
        names.contains(&"settings.json.lock".to_string()),
        "{names:?}"
    );
}

#[test]
fn pretty_sorted_newline_writes_sorted_pretty_bytes_with_trailing_newline() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    let opts = StoreOptions::new(Schema::new("prefs", 1)).pretty_sorted_newline(true);
    let s = Store::<Prefs>::open_path(&p, opts).unwrap();
    s.patch(&json!({"font_px": 20, "theme": "dark"}), None)
        .unwrap();
    let bytes = fs::read(&p).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.ends_with("}\n"), "trailing newline");
    let pos = |key: &str| text.find(&format!("\"{key}\"")).unwrap();
    assert!(pos("namespace") < pos("revision"));
    assert!(pos("revision") < pos("schemaVersion"));
    assert!(pos("schemaVersion") < pos("values"));
    assert!(pos("font_px") < pos("theme"), "values keys sorted");
}

#[test]
fn pretty_sorted_newline_is_byte_stable_for_equal_values() {
    let run = |sub: &str| {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join(sub).join("settings.json");
        let opts = StoreOptions::new(Schema::new("prefs", 1)).pretty_sorted_newline(true);
        let s = Store::<Prefs>::open_path(&p, opts).unwrap();
        s.patch(&json!({"theme": "dark", "font_px": 18}), None)
            .unwrap();
        fs::read(&p).unwrap()
    };
    assert_eq!(run("a"), run("b"));
}

#[test]
fn default_output_has_no_trailing_newline() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    let s = Store::<Prefs>::open_path(&p, StoreOptions::new(Schema::new("prefs", 1))).unwrap();
    s.patch(&json!({"theme": "dark"}), None).unwrap();
    assert!(!fs::read_to_string(&p).unwrap().ends_with('\n'));
}

#[test]
fn flat_plus_pretty_sorted_rewrites_the_whole_object_sorted() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("flat.json");
    fs::write(&p, b"{\n  \"theme\": \"dark\",\n  \"font_px\": 12\n}\n").unwrap();
    let opts = StoreOptions::new(Schema::new("prefs", 1))
        .format(FileFormat::Flat)
        .pretty_sorted_newline(true);
    let s = Store::<Prefs>::open_path(&p, opts).unwrap();
    s.patch(&json!({"font_px": 14}), None).unwrap();
    let text = fs::read_to_string(&p).unwrap();
    assert!(text.ends_with("}\n"));
    assert!(text.find("font_px").unwrap() < text.find("theme").unwrap());
}

#[test]
fn preview_document_equals_the_committed_document_including_open_keys() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    let opts = StoreOptions::new(Schema::new("prefs", 1))
        .allow_unknown_keys(|k| k.starts_with("signature:"));
    let s = Store::<Prefs>::open_path(&p, opts).unwrap();
    let patch = json!({"theme": "dark", "signature:acct": "sig-1"});
    let before = fs::read(&p).unwrap();
    let preview = s.patch_dry_run(&patch, None).unwrap();
    assert_eq!(fs::read(&p).unwrap(), before, "dry run writes nothing");
    assert_eq!(preview.document["signature:acct"], "sig-1");
    assert_eq!(preview.document["theme"], "dark");
    s.patch(&patch, None).unwrap();
    let stored = read_json(&p);
    assert_eq!(stored["values"], Value::Object(preview.document));
}

#[test]
fn read_with_defaults_on_a_missing_file_writes_nothing_and_uses_the_defaults() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("never").join("settings.json");
    let v: Prefs = read_with_defaults(
        &p,
        FileFormat::Envelope,
        &json!({"theme": "dark", "font_px": 18}),
    )
    .unwrap();
    assert_eq!(
        v,
        Prefs {
            theme: "dark".into(),
            font_px: 18
        }
    );
    assert!(!d.path().join("never").exists(), "no directory is created");
}

#[test]
fn read_with_options_fill_optional_takes_omitted_fields_from_the_defaults_not_type_default() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    fs::write(
        &p,
        serde_json::to_vec(&json!({
            "schemaVersion": 1, "namespace": "prefs",
            "values": {"theme": "light"}, "revision": 1
        }))
        .unwrap(),
    )
    .unwrap();
    let before = fs::read(&p).unwrap();
    let defaults = json!({"theme": "dark", "font_px": 18});
    let plain: Prefs = read_with_defaults(&p, FileFormat::Envelope, &defaults).unwrap();
    assert_eq!(
        plain,
        Prefs {
            theme: "light".into(),
            font_px: 16
        },
        "omitted optional stays serde's"
    );
    let v: Prefs = rightkit_app_settings::read_with_options(
        &p,
        &defaults,
        rightkit_app_settings::ReadOptions::new(FileFormat::Envelope).fill_optional(true),
    )
    .unwrap();
    assert_eq!(
        v,
        Prefs {
            theme: "light".into(),
            font_px: 18
        },
        "font_px from defaults, not 16"
    );
    assert_eq!(fs::read(&p).unwrap(), before, "file untouched");
    assert_eq!(
        listing(d.path()),
        vec!["settings.json"],
        "no lock, backup or quarantine"
    );
}

#[test]
fn read_with_defaults_falls_back_to_bak_without_quarantining_the_damaged_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    fs::write(&p, b"{ broken").unwrap();
    fs::write(
        d.path().join("settings.json.bak"),
        serde_json::to_vec(&json!({
            "schemaVersion": 1, "namespace": "prefs",
            "values": {"theme": "from-bak"}, "revision": 1
        }))
        .unwrap(),
    )
    .unwrap();
    let v: Prefs = read_with_defaults(
        &p,
        FileFormat::Envelope,
        &json!({"theme": "dark", "font_px": 18}),
    )
    .unwrap();
    assert_eq!(v.theme, "from-bak");
    assert_eq!(fs::read(&p).unwrap(), b"{ broken");
    assert_eq!(
        listing(d.path()),
        vec!["settings.json", "settings.json.bak"],
        "no quarantine file appears"
    );
}

#[test]
fn open_repair_uses_the_defaults_provider_for_invalid_fields() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    fs::write(
        &p,
        serde_json::to_vec(&json!({
            "schemaVersion": 1, "namespace": "prefs",
            "values": {"theme": 5, "font_px": "big"}, "revision": 1
        }))
        .unwrap(),
    )
    .unwrap();
    let opts = StoreOptions::new(Schema::new("prefs", 1))
        .defaults_provider(|| json!({"theme": "dark", "font_px": 18}));
    let s = Store::<Prefs>::open_path(&p, opts).unwrap();
    assert_eq!(
        s.get(),
        Prefs {
            theme: "dark".into(),
            font_px: 18
        }
    );
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Legacy {
    #[serde(alias = "colorTheme")]
    theme: String,
    font_px: u32,
}

impl Default for Legacy {
    fn default() -> Self {
        Self {
            theme: "auto".into(),
            font_px: 16,
        }
    }
}

#[test]
fn legacy_alias_value_survives_open_and_read_with_defaults() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("settings.json");
    // A legacy file holds only the alias key.
    fs::write(
        &p,
        serde_json::to_vec(&json!({
            "schemaVersion": 1, "namespace": "legacy",
            "values": {"colorTheme": "light"}, "revision": 1
        }))
        .unwrap(),
    )
    .unwrap();
    let defaults = json!({"theme": "dark", "font_px": 18});

    let v: Legacy = read_with_defaults(&p, FileFormat::Envelope, &defaults).unwrap();
    assert_eq!(
        v,
        Legacy {
            theme: "light".into(),
            font_px: 16
        },
        "alias value kept"
    );

    let opts = StoreOptions::new(Schema::new("legacy", 1))
        .defaults_provider(move || defaults.clone())
        .fill_optional(true);
    let s = Store::<Legacy>::open_path(&p, opts).unwrap();
    assert_eq!(
        s.get(),
        Legacy {
            theme: "light".into(),
            font_px: 18
        }
    );
}
