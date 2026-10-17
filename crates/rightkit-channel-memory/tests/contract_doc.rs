//! The published contract doc must stay in step with the schema and the typed
//! model: it names every fact status, conflict status and top-level field, and
//! the crate ships it.

use rightkit_channel_memory::{ConflictStatus, ProofStatus, SCHEMA_SOURCE};
use serde_json::Value;

const DOC: &str = include_str!("../docs/CONTRACT.md");

#[test]
fn the_doc_names_every_fact_status() {
    for status in ProofStatus::ALL {
        assert!(
            DOC.contains(&format!("`{}`", status.as_str())),
            "docs/CONTRACT.md does not name status {}",
            status.as_str()
        );
    }
}

#[test]
fn the_doc_names_both_conflict_statuses() {
    for status in [ConflictStatus::Open, ConflictStatus::Resolved] {
        assert!(DOC.contains(&format!("`{}`", status.as_str())));
    }
}

#[test]
fn the_doc_names_every_top_level_schema_field() {
    let schema: Value = serde_json::from_str(SCHEMA_SOURCE).expect("schema is JSON");
    let fields = schema["properties"].as_object().expect("properties");
    for name in fields.keys() {
        assert!(
            DOC.contains(&format!("`{name}`")),
            "docs/CONTRACT.md does not name top-level field {name}"
        );
    }
}

#[test]
fn the_doc_states_the_contract_name_and_version() {
    assert!(DOC.contains("`channel-memory`"));
    assert!(DOC.contains("`1.0.0`"));
}

#[test]
fn the_manifest_ships_the_doc() {
    let manifest = include_str!("../Cargo.toml");
    assert!(manifest.contains("\"docs/**\""));
}
