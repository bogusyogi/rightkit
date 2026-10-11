//! `conflicts_with` and the document path must agree: `parse` refuses a pair of
//! approved proofs with `E_PROOF_CONFLICT_SCOPE` exactly when `conflicts_with`
//! returns true. One table of pairs drives both.

use std::fs;
use std::path::PathBuf;

use rightkit_channel_memory::{conflicts_with, parse, MemoryError, Proof, ReasonCode};
use serde_json::{json, Value};

const A: &str = "proof.receipt-launch-week";
const B: &str = "proof.pair-b";

fn fixture_value(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts-pending/channel-memory/fixtures/valid")
        .join(name);
    serde_json::from_str(&fs::read_to_string(path).expect("read fixture")).expect("fixture is JSON")
}

/// The approved LAUNCH proof, as JSON, from the rev2 fixture.
fn launch_json() -> Value {
    fixture_value("proof-bank-rev2.json")["proofs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == A)
        .unwrap()
        .clone()
}

/// A second proof derived from LAUNCH, approved, with its own id and claim.
fn pair_b() -> Value {
    let mut b = launch_json();
    b["id"] = json!(B);
    b["revision"] = json!(1);
    b["supersedesRevision"] = Value::Null;
    b["permittedClaims"][0]["claimId"] = json!("claim.pair-b");
    b
}

/// Claims mirror their proof, so a changed field updates the first claim when
/// the proof has one. A withdrawn or superseded proof has no claims.
fn first_claim(b: &mut Value) -> Option<&mut Value> {
    b["permittedClaims"].get_mut(0)
}

fn set_value(b: &mut Value, value: &str) {
    b["value"] = json!(value);
    if let Some(claim) = first_claim(b) {
        claim["value"] = json!(value);
    }
}

fn set_units(b: &mut Value, units: &str) {
    b["units"] = json!(units);
    if let Some(claim) = first_claim(b) {
        claim["units"] = json!(units);
    }
}

fn set_subject(b: &mut Value, subject: &str) {
    b["subject"] = json!(subject);
    if let Some(claim) = first_claim(b) {
        claim["subject"] = json!(subject);
    }
}

fn set_period(b: &mut Value, from: &str, to: &str) {
    b["period"] = json!({"from": from, "to": to});
    if let Some(claim) = first_claim(b) {
        claim["period"] = json!({"from": from, "to": to});
    }
}

/// The schema forbids a null `approval`; the field is absent unless approved.
fn remove_key(b: &mut Value, key: &str) {
    b.as_object_mut().unwrap().remove(key);
}

fn as_withdrawn(b: &mut Value) {
    b["status"] = json!("withdrawn");
    remove_key(b, "approval");
    b["withdrawal"] =
        json!({"withdrawnBy": "adrian", "withdrawnAt": "2026-10-12T09:00:00Z", "reason": "Test"});
    b["permittedClaims"] = json!([]);
}

fn as_superseded(b: &mut Value) {
    b["status"] = json!("superseded");
    remove_key(b, "approval");
    b["supersededBy"] = json!(A);
    b["permittedClaims"] = json!([]);
}

fn as_collected(b: &mut Value) {
    b["status"] = json!("collected");
    remove_key(b, "approval");
}

/// The rev2 document with LAUNCH and `b` as its only proofs.
fn document_with(b: &Value) -> String {
    let mut doc = fixture_value("proof-bank-rev2.json");
    doc["proofs"] = json!([launch_json(), b]);
    doc.to_string()
}

fn typed(value: &Value) -> Proof {
    serde_json::from_value(value.clone()).expect("proof")
}

/// Document-path code for a pair: `None` when `parse` accepts it.
fn document_code(b: &Value) -> Option<String> {
    match parse(&document_with(b)) {
        Ok(_) => None,
        Err(MemoryError::Semantic { rule }) => {
            Some(rule.split(':').next().unwrap_or_default().to_string())
        }
        Err(other) => panic!("unexpected refusal: {other:?}"),
    }
}

fn table() -> Vec<(&'static str, Value, bool)> {
    let mut rows = Vec::new();

    let mut conflicting = pair_b();
    set_value(&mut conflicting, "1250.00");
    rows.push(("conflicting value, both approved", conflicting, true));

    let mut same_value = pair_b();
    set_value(&mut same_value, "1240");
    rows.push(("same value written differently", same_value, false));

    let mut subject = pair_b();
    set_subject(&mut subject, "subject.other");
    set_value(&mut subject, "1250.00");
    rows.push(("different subject", subject, false));

    let mut period = pair_b();
    set_period(&mut period, "2026-08-01", "2026-08-31");
    set_value(&mut period, "1250.00");
    rows.push(("different period", period, false));

    let mut units = pair_b();
    set_units(&mut units, "EUR");
    set_value(&mut units, "1250.00");
    rows.push(("different units", units, false));

    let mut withdrawn = pair_b();
    as_withdrawn(&mut withdrawn);
    set_value(&mut withdrawn, "1250.00");
    rows.push(("one withdrawn", withdrawn, false));

    let mut superseded = pair_b();
    as_superseded(&mut superseded);
    set_value(&mut superseded, "1250.00");
    rows.push(("one superseded", superseded, false));

    let mut collected = pair_b();
    as_collected(&mut collected);
    set_value(&mut collected, "1250.00");
    rows.push(("one collected", collected, false));

    rows
}

#[test]
fn conflicts_with_and_the_document_path_agree_on_every_pair() {
    let a = typed(&launch_json());
    for (name, b_json, expected) in table() {
        let b = typed(&b_json);
        assert_eq!(conflicts_with(&a, &b), expected, "conflicts_with: {name}");
        assert_eq!(conflicts_with(&b, &a), expected, "symmetric: {name}");
        let code = document_code(&b_json);
        if expected {
            assert_eq!(
                code.as_deref(),
                Some(ReasonCode::ProofConflictScope.as_str()),
                "document path: {name}"
            );
        } else {
            assert_eq!(code, None, "document path must accept a valid pair: {name}");
        }
    }
}

#[test]
fn a_proof_never_conflicts_with_itself() {
    let a = typed(&launch_json());
    assert!(!conflicts_with(&a, &a));
}

#[test]
fn the_document_refusal_carries_the_new_code_first() {
    let mut b = pair_b();
    set_value(&mut b, "1250.00");
    let error = parse(&document_with(&b)).expect_err("document refuses the pair");
    match error {
        MemoryError::Semantic { rule } => {
            assert!(rule.starts_with("E_PROOF_CONFLICT_SCOPE: "), "{rule}");
            assert!(rule.contains(A) && rule.contains(B), "{rule}");
        }
        other => panic!("expected a semantic refusal, got {other:?}"),
    }
}
