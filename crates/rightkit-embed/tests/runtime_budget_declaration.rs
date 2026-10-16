//! The held-resource declaration in Cargo.toml must keep covering every file the runtime-budget
//! scanner (`packages/release/runtime-budgets.mjs`) flags in this crate. The scanner itself runs
//! in the release gate; this test pins the shape it requires so an edit fails here first, with no
//! model and on every platform.

const MANIFEST: &str = include_str!("../Cargo.toml");
const COREML: &str = include_str!("../src/coreml.rs");
const TABLE: &str = "[[package.metadata.rightkit.resources]]";

/// `(key, value)` lines of each resources table, in manifest order.
fn resources() -> Vec<Vec<(String, String)>> {
    MANIFEST
        .split(TABLE)
        .skip(1)
        .map(|block| {
            block
                .lines()
                .take_while(|line| !line.trim_start().starts_with('['))
                .filter_map(|line| {
                    let (key, value) = line.split_once('=')?;
                    Some((key.trim().to_owned(), value.trim().to_owned()))
                })
                .collect()
        })
        .collect()
}

fn field<'a>(entry: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entry
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.trim_matches('"'))
        .filter(|v| !v.trim().is_empty())
}

#[test]
fn the_crate_is_declared_held_resource() {
    assert!(MANIFEST.contains("continuous = \"held-resource\""));
}

#[test]
fn every_resource_has_the_required_fields() {
    let entries = resources();
    assert!(entries.len() >= 3, "embedder, e40 and coreml entries");
    for entry in &entries {
        for key in ["api", "file", "scale", "release"] {
            assert!(field(entry, key).is_some(), "resource without {key}: {entry:?}");
        }
        let holds = field(entry, "holds").or_else(|| {
            entry
                .iter()
                .find(|(k, _)| k == "holds")
                .map(|(_, v)| v.as_str())
        });
        let holds = holds.expect("holds");
        assert!(
            ["memory", "thread", "task", "listener", "child"]
                .iter()
                .any(|kind| holds.contains(kind)),
            "holds names no held kind: {holds}"
        );
        let release = field(entry, "release").unwrap();
        assert!(
            !matches!(release.to_lowercase().as_str(), "none" | "never" | "n/a" | "-"),
            "release must name Drop or the function that ends it"
        );
        let file = field(entry, "file").unwrap();
        assert!(file.starts_with("src/"), "{file} must be under src/");
    }
}

#[test]
fn coreml_static_is_listed_with_a_static_note() {
    // The scanner's `resident-static` pattern matches this line; the file must then be listed
    // by a resource that carries a static_note.
    assert!(
        COREML.contains("static COMPILE: std::sync::Mutex<()>"),
        "the compile lock moved: revisit the static_note"
    );
    let entries = resources();
    let coreml: Vec<_> = entries
        .iter()
        .filter(|entry| field(entry, "file") == Some("src/coreml.rs"))
        .collect();
    assert!(!coreml.is_empty(), "no resource lists src/coreml.rs");
    assert!(
        coreml.iter().any(|entry| field(entry, "static_note").is_some()),
        "src/coreml.rs matches resident-static and needs a static_note"
    );
}

#[test]
fn e40_is_listed_for_its_resident_graphs() {
    let entries = resources();
    assert!(entries
        .iter()
        .any(|entry| field(entry, "file") == Some("src/e40.rs")));
}
