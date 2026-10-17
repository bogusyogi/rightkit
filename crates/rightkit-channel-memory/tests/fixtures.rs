//! Every fixture under contracts/fixtures/channel-memory lands in the stage its
//! name promises: valid fixtures parse, schema breaks fail the schema pass,
//! semantic breaks pass the schema and fail the semantic pass, and version
//! breaks are refused before any schema work.

use std::fs;
use std::path::PathBuf;

use rightkit_channel_memory::{parse, MemoryError};

fn fixture_dir(category: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/fixtures/channel-memory")
        .join(category)
}

fn fixtures(category: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = fs::read_dir(fixture_dir(category))
        .unwrap_or_else(|error| panic!("read {category}: {error}"))
        .map(|entry| {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&path).expect("read fixture"))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {category}");
    files
}

#[test]
fn valid_fixtures_parse_into_the_typed_model() {
    for (name, text) in fixtures("valid") {
        let memory = parse(&text).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(memory.contract, "channel-memory", "{name}");
        assert_eq!(memory.contract_version, "1.0.0", "{name}");
    }
}

#[test]
fn valid_fixtures_round_trip_through_serde() {
    for (name, text) in fixtures("valid") {
        let memory = parse(&text).unwrap();
        let again = parse(&serde_json::to_string(&memory).unwrap())
            .unwrap_or_else(|error| panic!("{name} round trip: {error}"));
        assert_eq!(memory, again, "{name}");
    }
}

#[test]
fn schema_breaks_fail_the_schema_pass() {
    for (name, text) in fixtures("invalid") {
        match parse(&text) {
            Err(MemoryError::Schema { .. }) => {}
            other => panic!("{name}: expected a schema refusal, got {other:?}"),
        }
    }
}

#[test]
fn semantic_breaks_pass_the_schema_and_fail_the_semantic_pass() {
    for (name, text) in fixtures("semantic-invalid") {
        match parse(&text) {
            Err(MemoryError::Semantic { .. }) => {}
            other => panic!("{name}: expected a semantic refusal, got {other:?}"),
        }
    }
}

#[test]
fn incompatible_versions_are_refused_before_schema_work() {
    for (name, text) in fixtures("version-rejected") {
        match parse(&text) {
            Err(MemoryError::UnsupportedVersion { .. }) => {}
            other => panic!("{name}: expected a version refusal, got {other:?}"),
        }
    }
}

#[test]
fn the_plan_facts_hold_on_the_valid_fixtures() {
    let kids = parse(&fixtures("valid")[0].1).expect("kids fixture");
    assert_eq!(kids.cadence.posts_per_day, 5, "five a day (P4 line 100)");
    assert!(matches!(
        kids.cadence.batch,
        rightkit_channel_memory::Batch::Overnight
    ));
    assert!(kids.licence_scope.monetised);
    assert_eq!(
        kids.licence_scope.asset_rights,
        rightkit_channel_memory::AssetRights::CommercialSafe,
        "monetised channels need commercial-safe assets (P4 line 98)"
    );
    assert!(
        kids.identity_bindings.is_some(),
        "identity bindings (G7 line 88)"
    );
}
