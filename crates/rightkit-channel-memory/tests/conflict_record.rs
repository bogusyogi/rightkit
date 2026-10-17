//! The conflict record: two retained device bodies from one base revision, with
//! an open or resolved status. These tests cover the record's own API and the
//! rules that bind a record to its status and chosen device.

use std::fs;
use std::path::PathBuf;

use rightkit_channel_memory::{parse, ChannelMemory, ConflictRecord, ConflictStatus, MemoryError};
use serde_json::{json, Value};

const CONFLICT: &str = "conflict.launch-week-v2-1";
const PROOF: &str = "proof.receipt-launch-week-v2";

fn fixture_json() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts-pending/channel-memory/fixtures/valid")
        .join("proof-conflict-open-rev3.json");
    serde_json::from_str(&fs::read_to_string(path).expect("read fixture")).expect("fixture is JSON")
}

fn load(value: &Value) -> Result<ChannelMemory, MemoryError> {
    parse(&value.to_string())
}

#[test]
fn an_open_conflict_reports_open_and_blocks_its_proof_only() {
    let memory = load(&fixture_json()).expect("valid fixture");
    let conflict = memory
        .conflicts
        .iter()
        .find(|item| item.id == CONFLICT)
        .expect("conflict in fixture");
    assert_eq!(conflict.status, ConflictStatus::Open);
    assert_eq!(conflict.status.as_str(), "open");
    assert!(conflict.is_open());
    assert!(conflict.chosen_body().is_none());
    assert_eq!(conflict.bodies.len(), 2);
    assert!(memory.has_open_conflict(PROOF));
    assert_eq!(memory.open_conflicts(PROOF).count(), 1);
    assert!(!memory.has_open_conflict("proof.receipt-launch-week"));
}

#[test]
fn both_bodies_are_kept_and_addressable_by_device() {
    let memory = load(&fixture_json()).expect("valid fixture");
    let conflict = &memory.conflicts[0];
    let a = conflict.body_for("mac-a").expect("body a");
    let b = conflict.body_for("xps-b").expect("body b");
    assert_ne!(a.proof, b.proof);
    assert_eq!(a.proof.revision, conflict.base_revision + 1);
    assert_eq!(b.proof.revision, conflict.base_revision + 1);
    assert!(conflict.body_for("phone-c").is_none());
}

#[test]
fn a_resolved_conflict_exposes_the_chosen_body_and_stops_blocking() {
    let mut value = fixture_json();
    value["conflicts"][0]["status"] = json!("resolved");
    value["conflicts"][0]["chosenDevice"] = json!("xps-b");
    let memory = load(&value).expect("resolved conflict is valid");
    let conflict = &memory.conflicts[0];
    assert_eq!(conflict.status.as_str(), "resolved");
    assert!(!conflict.is_open());
    assert_eq!(conflict.chosen_body().expect("chosen").device, "xps-b");
    assert!(!memory.has_open_conflict(PROOF));
}

#[test]
fn a_resolved_conflict_must_choose_one_of_its_devices() {
    let mut value = fixture_json();
    value["conflicts"][0]["status"] = json!("resolved");
    value["conflicts"][0]["chosenDevice"] = json!("phone-c");
    assert!(matches!(load(&value), Err(MemoryError::Semantic { .. })));
    // No chosen device at all fails the schema.
    let mut value = fixture_json();
    value["conflicts"][0]["status"] = json!("resolved");
    assert!(load(&value).is_err());
}

#[test]
fn an_open_conflict_may_not_name_a_chosen_device() {
    let mut value = fixture_json();
    value["conflicts"][0]["chosenDevice"] = json!("mac-a");
    assert!(load(&value).is_err());
}

#[test]
fn a_conflict_needs_exactly_two_bodies_from_two_devices_and_a_held_proof() {
    let mut one = fixture_json();
    one["conflicts"][0]["bodies"].as_array_mut().unwrap().pop();
    assert!(load(&one).is_err());

    let mut same_device = fixture_json();
    same_device["conflicts"][0]["bodies"][1]["device"] = json!("mac-a");
    assert!(matches!(
        load(&same_device),
        Err(MemoryError::Semantic { .. })
    ));

    let mut foreign = fixture_json();
    foreign["conflicts"][0]["proofId"] = json!("proof.not-held");
    assert!(matches!(load(&foreign), Err(MemoryError::Semantic { .. })));
}

#[test]
fn a_conflict_record_round_trips_through_serde() {
    let memory = load(&fixture_json()).expect("valid fixture");
    let text = serde_json::to_string(&memory.conflicts[0]).expect("serialise");
    let back: ConflictRecord = serde_json::from_str(&text).expect("deserialise");
    assert_eq!(memory.conflicts[0], back);
}
