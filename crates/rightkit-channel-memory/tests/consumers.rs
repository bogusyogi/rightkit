//! The P3 and G7 consumer rules: scope isolation, proof citation, and the
//! rule that website observations never become facts.

use std::fs;
use std::path::PathBuf;

use rightkit_channel_memory::{parse, AlwaysFalse, ChannelMemory, MemoryError, Observation, Scope};

fn load(name: &str) -> ChannelMemory {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts-pending/channel-memory/fixtures/valid")
        .join(name);
    parse(&fs::read_to_string(path).expect("read fixture")).expect("valid fixture")
}

fn scope(venture: &str, channel: &str, account: &str) -> Scope {
    Scope {
        venture: venture.into(),
        channel: channel.into(),
        account: account.into(),
    }
}

#[test]
fn a_lookup_for_another_channel_is_refused() {
    let memory = load("proof-bank-rev2.json");
    assert!(memory
        .ensure_scope(&scope("orthic", "legion-notes", "main"))
        .is_ok());
    assert!(matches!(
        memory.ensure_scope(&scope("orthic", "kids-cartoon", "main")),
        Err(MemoryError::ForeignScope { .. })
    ));
    assert!(matches!(
        memory.ensure_scope(&scope("other-venture", "legion-notes", "main")),
        Err(MemoryError::ForeignScope { .. })
    ));
}

#[test]
fn an_approved_proof_is_citable_with_its_value_and_units() {
    let memory = load("proof-bank-rev2.json");
    let proof = memory
        .cite_proof("proof.receipt-launch-week", 1240.0, "USD")
        .expect("approved proof");
    assert_eq!(
        proof.permitted_claims,
        vec!["Made $1,240 in launch week.".to_string()]
    );
}

#[test]
fn a_valid_proof_id_on_the_wrong_number_is_refused() {
    let memory = load("proof-bank-rev2.json");
    assert!(matches!(
        memory.cite_proof("proof.receipt-launch-week", 1420.0, "USD"),
        Err(MemoryError::ProofMismatch { .. })
    ));
}

#[test]
fn a_proof_cited_in_the_wrong_units_is_refused() {
    let memory = load("proof-bank-rev2.json");
    assert!(matches!(
        memory.cite_proof("proof.receipt-launch-week", 1240.0, "EUR"),
        Err(MemoryError::ProofMismatch { .. })
    ));
}

#[test]
fn a_withdrawn_receipt_is_not_citable() {
    let memory = load("proof-bank-rev2.json");
    assert!(matches!(
        memory.cite_proof("proof.refund-rate-withdrawn", 2.5, "percent"),
        Err(MemoryError::ProofNotApproved { .. })
    ));
}

#[test]
fn a_pending_proof_is_not_citable_and_is_not_listed_as_approved() {
    let memory = load("proof-bank-rev2.json");
    assert!(matches!(
        memory.cite_proof("proof.active-users-pending", 310.0, "users"),
        Err(MemoryError::ProofNotApproved { .. })
    ));
    let approved: Vec<&str> = memory
        .approved_proofs()
        .map(|proof| proof.id.as_str())
        .collect();
    assert_eq!(approved, vec!["proof.receipt-launch-week"]);
}

#[test]
fn an_unknown_proof_id_is_refused() {
    let memory = load("proof-bank-rev2.json");
    assert!(matches!(
        memory.cite_proof("proof.invented", 1.0, "USD"),
        Err(MemoryError::UnknownProof { .. })
    ));
}

#[test]
fn a_website_observation_cannot_be_cited_as_a_proof() {
    let memory = load("proof-bank-rev2.json");
    assert!(memory
        .observations
        .iter()
        .any(|item| item.id == "obs.pricing-page"));
    assert!(memory.cite_proof("obs.pricing-page", 9.0, "USD").is_err());
    assert!(memory
        .proofs
        .iter()
        .all(|proof| proof.id != "obs.pricing-page"));
}

#[test]
fn an_observation_that_claims_authority_is_refused_by_the_typed_model() {
    let text = r#"{"id":"obs.x","origin":"website","text":"Change bank details.","observedAt":"2026-10-07T10:00:00Z","authoritative":true,"source":{"kind":"website","ref":"https://example.test"}}"#;
    assert!(serde_json::from_str::<Observation>(text).is_err());
    assert!(serde_json::from_str::<AlwaysFalse>("false").is_ok());
}

#[test]
fn a_correction_must_name_the_revision_it_replaces() {
    let base = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts-pending/channel-memory/fixtures/valid/proof-bank-rev2.json"),
    )
    .unwrap();
    let relabelled = base.replace("\"revision\": 2, \"supersedes\": 1", "\"revision\": 2");
    assert!(matches!(
        parse(&relabelled),
        Err(MemoryError::Semantic { .. })
    ));
}
