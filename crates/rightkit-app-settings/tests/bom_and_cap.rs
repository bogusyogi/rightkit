//! `StoreOptions::utf8_bom` / `ReadOptions::utf8_bom` (a leading UTF-8 BOM is read and written
//! when the option is on, and is damaged JSON when it is off) and `ReadOptions::max_bytes` (the
//! free read refuses an oversized file before reading it).

use rightkit_app_settings::{
    read_with_options, Error, FileFormat, OnCorrupt, ReadOptions, Schema, Store, StoreOptions,
};
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

const BOM: &[u8] = b"\xEF\xBB\xBF";

fn opts() -> StoreOptions {
    StoreOptions::new(Schema::new("demo", 1))
}

fn with_bom(json: &str) -> Vec<u8> {
    let mut bytes = BOM.to_vec();
    bytes.extend_from_slice(json.as_bytes());
    bytes
}

#[test]
fn a_bom_file_opens_and_is_written_back_with_a_bom_when_the_option_is_on() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = concat!(
        r#"{"schemaVersion":1,"namespace":"demo","#,
        r#""values":{"theme":"dark","font_px":12},"revision":3}"#
    );
    fs::write(&path, with_bom(json)).unwrap();

    let store = Store::<Prefs>::open_path(&path, opts().utf8_bom(true)).unwrap();
    assert_eq!(store.get().theme, "dark");
    assert_eq!(store.get().font_px, 12);

    store
        .update(|p| {
            p.font_px = 20;
            Ok(())
        })
        .unwrap();
    let written = fs::read(&path).unwrap();
    assert!(written.starts_with(BOM), "write keeps the byte order mark");
    let text = std::str::from_utf8(&written[BOM.len()..]).unwrap();
    let doc: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(doc["values"]["font_px"], 20);
    assert_eq!(doc["values"]["theme"], "dark");
}

#[test]
fn without_the_option_a_bom_file_is_damaged_json_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{"schemaVersion":1,"namespace":"demo","values":{"theme":"dark"},"revision":1}"#;
    let original = with_bom(json);
    fs::write(&path, &original).unwrap();

    let store = Store::<Prefs>::open_path(&path, opts().on_corrupt(OnCorrupt::Refuse));
    assert!(matches!(store, Err(Error::Corrupt(_))));
    assert_eq!(fs::read(&path).unwrap(), original, "the file is untouched");
}

#[test]
fn read_with_options_accepts_a_bom_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let json = r#"{"schemaVersion":1,"namespace":"demo","values":{"theme":"light"},"revision":1}"#;
    fs::write(&path, with_bom(json)).unwrap();
    let defaults = serde_json::to_value(Prefs::default()).unwrap();

    let read: Prefs = read_with_options(
        &path,
        &defaults,
        ReadOptions::new(FileFormat::Envelope).utf8_bom(true),
    )
    .unwrap();
    assert_eq!(read.theme, "light");
    assert_eq!(
        fs::read(&path).unwrap(),
        with_bom(json),
        "a read writes nothing"
    );
}

#[test]
fn read_with_options_refuses_an_oversized_file_before_reading_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let big = format!("{{\"pad\":\"{}\"}}", "x".repeat(4096));
    fs::write(&path, &big).unwrap();
    let defaults = serde_json::to_value(Prefs::default()).unwrap();

    let err = read_with_options::<Prefs>(
        &path,
        &defaults,
        ReadOptions::new(FileFormat::Flat).max_bytes(1024),
    )
    .expect_err("oversized file must be refused");
    assert!(matches!(err, Error::Invalid(m) if m.contains("byte limit")));
    assert_eq!(fs::read_to_string(&path).unwrap(), big);
    let names: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(names.len(), 1, "no quarantine or sidecar created");
}

#[test]
fn read_with_options_without_a_cap_reads_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, r#"{"theme":"light","font_px":14}"#).unwrap();
    let defaults = serde_json::to_value(Prefs::default()).unwrap();

    let read: Prefs = read_with_options(
        &path,
        &defaults,
        ReadOptions::new(FileFormat::Flat).max_bytes(1 << 20),
    )
    .unwrap();
    assert_eq!(read.font_px, 14);
}
