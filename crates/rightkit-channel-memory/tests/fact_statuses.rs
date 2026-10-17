//! The four fact statuses (collected, approved, withdrawn, superseded) are one
//! list in the schema, the typed model and the wire names. `pending` is retired.

use rightkit_channel_memory::{parse, ProofStatus, SCHEMA_SOURCE};
use serde_json::{json, Value};

#[test]
fn the_schema_enum_is_exactly_the_four_statuses() {
    let schema: Value = serde_json::from_str(SCHEMA_SOURCE).expect("schema is JSON");
    let names: Vec<&str> = schema["$defs"]["proof"]["properties"]["status"]["enum"]
        .as_array()
        .expect("status enum")
        .iter()
        .map(|item| item.as_str().expect("status name"))
        .collect();
    assert_eq!(names, ["collected", "approved", "withdrawn", "superseded"]);
    let typed: Vec<&str> = ProofStatus::ALL.iter().map(|s| s.as_str()).collect();
    assert_eq!(names, typed);
}

#[test]
fn wire_names_round_trip_through_serde_and_from_wire() {
    for status in ProofStatus::ALL {
        let text = serde_json::to_string(&status).expect("serialise");
        assert_eq!(text, format!("\"{}\"", status.as_str()));
        let back: ProofStatus = serde_json::from_str(&text).expect("deserialise");
        assert_eq!(back, status);
        assert_eq!(ProofStatus::from_wire(status.as_str()), Some(status));
    }
}

#[test]
fn the_retired_pending_name_and_other_text_are_refused() {
    for text in ["pending", "Approved", "", "withdrawn "] {
        assert_eq!(ProofStatus::from_wire(text), None, "{text:?}");
        assert!(
            serde_json::from_value::<ProofStatus>(json!(text)).is_err(),
            "{text:?}"
        );
    }
}

#[test]
fn only_withdrawn_and_superseded_are_terminal() {
    let terminal: Vec<ProofStatus> = ProofStatus::ALL
        .into_iter()
        .filter(|status| status.is_terminal())
        .collect();
    assert_eq!(terminal, [ProofStatus::Withdrawn, ProofStatus::Superseded]);
}

#[test]
fn every_status_appears_in_a_valid_fixture_or_is_refused_with_its_name() {
    // The superseded fixture parses, so `superseded` is accepted on the wire.
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/fixtures/channel-memory/valid")
        .join("proof-bank-rev3-superseded.json");
    let text = std::fs::read_to_string(path).expect("fixture");
    let memory = parse(&text).expect("valid");
    let seen: Vec<ProofStatus> = memory.proofs.iter().map(|proof| proof.status).collect();
    assert!(seen.contains(&ProofStatus::Superseded));
    assert!(seen.contains(&ProofStatus::Approved));
}
