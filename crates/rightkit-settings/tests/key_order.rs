//! `KeyOrder`: a file written by one build and rewritten by another keeps its key order, a patch
//! changes only the bytes of what it changes, and `Sorted` writes what the envelope was written as
//! before the option existed.
//!
//! Every expected file is spelled out as text. None is built with `serde_json::to_string_pretty`
//! or `json!`, because the order those produce depends on whether another crate in the build
//! enables serde_json's `preserve_order`.
use rightkit_settings::{FileBackend, FileFormat, KeyOrder, Schema, Store, StoreOptions};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::path::Path;

/// Fields are declared in an order that is not alphabetical, nested ones too.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct Inner {
    yy: u32,
    xx: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct Cfg {
    zeta: String,
    alpha: u32,
    omega: bool,
    beta: u32,
    nested: Inner,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct Lists {
    name: String,
    items: Vec<Inner>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
struct Fmt {
    text: String,
    ratio: f64,
    big: u64,
    list: Vec<f64>,
}

/// Every declared key, in no alphabetical order, as an older build wrote it.
const FLAT_FULL: &str = "{\n  \"zeta\": \"z\",\n  \"omega\": true,\n  \"alpha\": 1,\n  \"nested\": {\n    \"yy\": 5,\n    \"xx\": 6\n  },\n  \"beta\": 2\n}";

/// The same values as an envelope, revision 3.
const ENV_FULL: &str = "{\n  \"schemaVersion\": 1,\n  \"namespace\": \"ko\",\n  \"values\": {\n    \"zeta\": \"z\",\n    \"omega\": true,\n    \"alpha\": 1,\n    \"nested\": {\n      \"yy\": 5,\n      \"xx\": 6\n    },\n    \"beta\": 2\n  },\n  \"revision\": 3\n}";

/// An envelope as 0.2.0 wrote it: sorted at every level. It lacks `omega`.
const ENV_SORTED: &str = "{\n  \"namespace\": \"ko\",\n  \"revision\": 3,\n  \"schemaVersion\": 1,\n  \"values\": {\n    \"alpha\": 1,\n    \"beta\": 2,\n    \"nested\": {\n      \"xx\": 6,\n      \"yy\": 5\n    },\n    \"zeta\": \"z\"\n  }\n}";

fn schema() -> Schema {
    Schema::new("ko", 1)
}

fn flat() -> StoreOptions {
    StoreOptions::new(schema()).format(FileFormat::Flat)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

fn file_with(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    fs::write(&path, text).unwrap();
    (dir, path)
}

#[test]
fn preserve_is_the_default() {
    assert_eq!(KeyOrder::default(), KeyOrder::Preserve);
}

// ---- flat files ------------------------------------------------------------------------------

#[test]
fn a_flat_file_with_odd_key_order_round_trips_byte_identically() {
    let (dir, path) = file_with(FLAT_FULL);
    // A migration that reports a change without making one forces the whole document through the
    // writer.
    let opts = flat().flat_migration(|map| Ok((map, true)));
    let store = Store::<Cfg>::open_path(&path, opts).unwrap();
    assert_eq!(store.get().alpha, 1);
    assert_eq!(read(&path), FLAT_FULL);
    // The write happened: the previous file was kept as the backup.
    assert_eq!(read(&dir.path().join("s.json.bak")), FLAT_FULL);
}

#[test]
fn a_patch_changes_only_the_bytes_of_the_value_it_changes() {
    let (_dir, path) = file_with(FLAT_FULL);
    let store = Store::<Cfg>::open_path(&path, flat()).unwrap();
    assert_eq!(
        read(&path),
        FLAT_FULL,
        "opening a clean file writes nothing"
    );
    store.patch(&json!({"alpha": 9}), None).unwrap();
    assert_eq!(
        read(&path),
        FLAT_FULL.replace("\"alpha\": 1", "\"alpha\": 9")
    );
}

#[test]
fn a_nested_object_keeps_its_key_order_when_one_of_its_values_changes() {
    let (_dir, path) = file_with(FLAT_FULL);
    let store = Store::<Cfg>::open_path(&path, flat()).unwrap();
    store
        .update(|cfg| {
            cfg.nested.xx = 7;
            Ok(())
        })
        .unwrap();
    // `yy` is still declared before `xx` in the file, as it was; the type declares the same order.
    assert_eq!(read(&path), FLAT_FULL.replace("\"xx\": 6", "\"xx\": 7"));
}

#[test]
fn a_nested_object_the_type_orders_differently_keeps_the_files_order() {
    // The file has `xx` before `yy`; `Inner` declares `yy` first.
    let text = FLAT_FULL.replace("\"yy\": 5,\n    \"xx\": 6", "\"xx\": 6,\n    \"yy\": 5");
    let (_dir, path) = file_with(&text);
    let store = Store::<Cfg>::open_path(&path, flat()).unwrap();
    store
        .update(|cfg| {
            cfg.nested.yy = 8;
            Ok(())
        })
        .unwrap();
    assert_eq!(read(&path), text.replace("\"yy\": 5", "\"yy\": 8"));
}

#[test]
fn new_keys_are_appended_in_the_types_field_order_not_alphabetically() {
    let text = "{\n  \"zeta\": \"z\",\n  \"alpha\": 1\n}";
    let (_dir, path) = file_with(text);
    let store = Store::<Cfg>::open_path(&path, flat()).unwrap();
    assert_eq!(read(&path), text, "opening a clean file writes nothing");
    store.patch(&json!({"alpha": 2}), None).unwrap();
    // omega, beta, nested: the type's order. Alphabetical would be beta, nested, omega.
    assert_eq!(
        read(&path),
        "{\n  \"zeta\": \"z\",\n  \"alpha\": 2,\n  \"omega\": false,\n  \"beta\": 0,\n  \"nested\": {\n    \"yy\": 0,\n    \"xx\": 0\n  }\n}"
    );
}

#[test]
fn sorted_writes_new_keys_and_rewritten_values_alphabetically_as_before() {
    let text = "{\n  \"zeta\": \"z\",\n  \"alpha\": 1\n}";
    let (_dir, path) = file_with(text);
    let store = Store::<Cfg>::open_path(&path, flat().key_order(KeyOrder::Sorted)).unwrap();
    store.patch(&json!({"alpha": 2}), None).unwrap();
    // The flat file's own keys keep their place; what the write adds is sorted, nested keys too.
    assert_eq!(
        read(&path),
        "{\n  \"zeta\": \"z\",\n  \"alpha\": 2,\n  \"beta\": 0,\n  \"nested\": {\n    \"xx\": 0,\n    \"yy\": 0\n  },\n  \"omega\": false\n}"
    );
}

#[test]
fn a_new_flat_file_is_in_the_types_field_order_or_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let preserved = dir.path().join("preserve.json");
    Store::<Cfg>::open_path(&preserved, flat()).unwrap();
    assert_eq!(
        read(&preserved),
        "{\n  \"zeta\": \"\",\n  \"alpha\": 0,\n  \"omega\": false,\n  \"beta\": 0,\n  \"nested\": {\n    \"yy\": 0,\n    \"xx\": 0\n  }\n}"
    );
    let sorted = dir.path().join("sorted.json");
    Store::<Cfg>::open_path(&sorted, flat().key_order(KeyOrder::Sorted)).unwrap();
    assert_eq!(
        read(&sorted),
        "{\n  \"alpha\": 0,\n  \"beta\": 0,\n  \"nested\": {\n    \"xx\": 0,\n    \"yy\": 0\n  },\n  \"omega\": false,\n  \"zeta\": \"\"\n}"
    );
}

#[test]
fn objects_inside_arrays_keep_the_key_order_of_the_element_at_the_same_index() {
    let text = "{\n  \"name\": \"n\",\n  \"items\": [\n    {\n      \"yy\": 1,\n      \"xx\": 2\n    },\n    {\n      \"xx\": 4,\n      \"yy\": 3\n    }\n  ]\n}";
    let (_dir, path) = file_with(text);
    let store = Store::<Lists>::open_path(&path, flat()).unwrap();
    assert_eq!(read(&path), text, "opening a clean file writes nothing");
    store
        .update(|lists| {
            lists.items[0].xx = 9;
            Ok(())
        })
        .unwrap();
    assert_eq!(read(&path), text.replace("\"xx\": 2", "\"xx\": 9"));
}

// ---- envelopes -------------------------------------------------------------------------------

#[test]
fn an_envelope_rewritten_without_a_change_differs_only_in_its_counters() {
    let (_dir, path) = file_with(ENV_FULL);
    // Version 2 with a migration that changes nothing: the document is rewritten, nothing else is.
    let schema_v2 = Schema::new("ko", 2).migration(2, |_| Ok(()));
    Store::<Cfg>::open_path(&path, StoreOptions::new(schema_v2)).unwrap();
    assert_eq!(
        read(&path),
        ENV_FULL
            .replace("\"schemaVersion\": 1", "\"schemaVersion\": 2")
            .replace("\"revision\": 3", "\"revision\": 4")
    );
}

#[test]
fn an_envelope_patch_changes_only_the_value_and_the_revision() {
    let (_dir, path) = file_with(ENV_FULL);
    let store = Store::<Cfg>::open_path(&path, StoreOptions::new(schema())).unwrap();
    assert_eq!(read(&path), ENV_FULL, "opening a clean file writes nothing");
    store.patch(&json!({"alpha": 9}), None).unwrap();
    assert_eq!(
        read(&path),
        ENV_FULL
            .replace("\"alpha\": 1", "\"alpha\": 9")
            .replace("\"revision\": 3", "\"revision\": 4")
    );
}

#[test]
fn an_envelope_written_sorted_stays_sorted_and_a_new_key_is_appended() {
    let (_dir, path) = file_with(ENV_SORTED);
    let store = Store::<Cfg>::open_path(&path, StoreOptions::new(schema())).unwrap();
    store.patch(&json!({"alpha": 9}), None).unwrap();
    // `omega` was not in the file. It goes after the sorted keys, not into its sorted place.
    assert_eq!(
        read(&path),
        "{\n  \"namespace\": \"ko\",\n  \"revision\": 4,\n  \"schemaVersion\": 1,\n  \"values\": {\n    \"alpha\": 9,\n    \"beta\": 2,\n    \"nested\": {\n      \"xx\": 6,\n      \"yy\": 5\n    },\n    \"zeta\": \"z\",\n    \"omega\": false\n  }\n}"
    );
}

#[test]
fn sorted_rewrites_an_envelope_alphabetically_at_every_level() {
    let (_dir, path) = file_with(ENV_FULL);
    let opts = StoreOptions::new(schema()).key_order(KeyOrder::Sorted);
    let store = Store::<Cfg>::open_path(&path, opts).unwrap();
    store.patch(&json!({"alpha": 9}), None).unwrap();
    assert_eq!(
        read(&path),
        "{\n  \"namespace\": \"ko\",\n  \"revision\": 4,\n  \"schemaVersion\": 1,\n  \"values\": {\n    \"alpha\": 9,\n    \"beta\": 2,\n    \"nested\": {\n      \"xx\": 6,\n      \"yy\": 5\n    },\n    \"omega\": true,\n    \"zeta\": \"z\"\n  }\n}"
    );
}

#[test]
fn a_new_envelope_is_in_the_types_field_order_or_sorted() {
    let dir = tempfile::tempdir().unwrap();
    let preserved = dir.path().join("preserve.json");
    Store::<Cfg>::open_path(&preserved, StoreOptions::new(schema())).unwrap();
    assert_eq!(
        read(&preserved),
        "{\n  \"schemaVersion\": 1,\n  \"namespace\": \"ko\",\n  \"values\": {\n    \"zeta\": \"\",\n    \"alpha\": 0,\n    \"omega\": false,\n    \"beta\": 0,\n    \"nested\": {\n      \"yy\": 0,\n      \"xx\": 0\n    }\n  },\n  \"revision\": 1\n}"
    );
    let sorted = dir.path().join("sorted.json");
    let opts = StoreOptions::new(schema()).key_order(KeyOrder::Sorted);
    Store::<Cfg>::open_path(&sorted, opts).unwrap();
    assert_eq!(
        read(&sorted),
        "{\n  \"namespace\": \"ko\",\n  \"revision\": 1,\n  \"schemaVersion\": 1,\n  \"values\": {\n    \"alpha\": 0,\n    \"beta\": 0,\n    \"nested\": {\n      \"xx\": 0,\n      \"yy\": 0\n    },\n    \"omega\": false,\n    \"zeta\": \"\"\n  }\n}"
    );
}

#[test]
fn a_legacy_flat_file_adopted_as_an_envelope_keeps_its_key_order() {
    let (_dir, path) = file_with(FLAT_FULL);
    Store::<Cfg>::open_path(&path, StoreOptions::new(schema())).unwrap();
    assert_eq!(
        read(&path),
        ENV_FULL.replace("\"revision\": 3", "\"revision\": 1")
    );
}

#[test]
fn the_file_backend_used_directly_keeps_the_order_too() {
    let (_dir, path) = file_with(ENV_FULL);
    let backend = FileBackend::new(&path, FileFormat::Envelope);
    let store = Store::<Cfg>::with_backend(backend, StoreOptions::new(schema())).unwrap();
    store.patch(&json!({"beta": 7}), None).unwrap();
    assert_eq!(
        read(&path),
        ENV_FULL
            .replace("\"beta\": 2", "\"beta\": 7")
            .replace("\"revision\": 3", "\"revision\": 4")
    );
}

// ---- open keys, pretty_sorted_newline, formatting -------------------------------------------

#[test]
fn open_keys_added_by_separate_patches_are_appended_in_call_order() {
    let dir = tempfile::tempdir().unwrap();
    let opts = |order: KeyOrder| {
        StoreOptions::new(schema())
            .allow_unknown_keys(|key| key.starts_with("sig:"))
            .key_order(order)
    };
    let patch_both = |path: &Path, order: KeyOrder| {
        let store = Store::<Cfg>::open_path(path, opts(order)).unwrap();
        store.patch(&json!({"sig:b": "1"}), None).unwrap();
        store.patch(&json!({"sig:a": "2"}), None).unwrap();
        read(path)
    };
    let kept = patch_both(&dir.path().join("kept.json"), KeyOrder::Preserve);
    let at = |text: &str, key: &str| text.find(key).unwrap();
    assert!(
        at(&kept, "\"nested\"") < at(&kept, "sig:b"),
        "after the declared keys"
    );
    assert!(
        at(&kept, "sig:b") < at(&kept, "sig:a"),
        "call order, not alphabetical"
    );
    let sorted = patch_both(&dir.path().join("sorted.json"), KeyOrder::Sorted);
    assert!(at(&sorted, "sig:a") < at(&sorted, "sig:b"), "alphabetical");
    assert!(at(&sorted, "omega") < at(&sorted, "sig:a"));
    assert!(at(&sorted, "sig:b") < at(&sorted, "zeta"));
}

#[test]
fn pretty_sorted_newline_sorts_whatever_key_order_says() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    let opts = StoreOptions::new(schema())
        .pretty_sorted_newline(true)
        .key_order(KeyOrder::Preserve);
    Store::<Cfg>::open_path(&path, opts).unwrap();
    let text = read(&path);
    assert!(text.find("\"alpha\"").unwrap() < text.find("\"zeta\"").unwrap());
    assert!(text.ends_with("}\n"));
}

#[test]
fn both_orders_write_the_same_numbers_escapes_and_whitespace() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, order: KeyOrder| {
        let path = dir.path().join(name);
        let store =
            Store::<Fmt>::open_path(&path, StoreOptions::new(schema()).key_order(order)).unwrap();
        store
            .update(|fmt| {
                fmt.text = "caf\u{e9} \"q\"\n\t\\".into();
                fmt.ratio = 0.1;
                fmt.big = u64::MAX;
                fmt.list = vec![1.0, 2.5e-7, 1e21];
                Ok(())
            })
            .unwrap();
        // Same lines in some order: the commas that end lines depend on where a line falls.
        let mut lines: Vec<String> = read(&path)
            .lines()
            .map(|line| line.trim_end_matches(',').to_string())
            .collect();
        lines.sort();
        lines
    };
    assert_eq!(
        write("kept.json", KeyOrder::Preserve),
        write("sorted.json", KeyOrder::Sorted)
    );
}
