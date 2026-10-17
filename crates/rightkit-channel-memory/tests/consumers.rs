//! The P3 and G7 consumer rules: scope isolation, the deterministic citation
//! match (contract §5.3), and the rule that website observations never become
//! facts. Each rule has a passing and a failing case.

use std::fs;
use std::path::PathBuf;

use rightkit_channel_memory::{
    parse, AlwaysFalse, ChannelMemory, Citation, DateWindow, Decimal, MemoryError, Observation,
    ReasonCode, Scope,
};
use serde_json::{json, Value};

const TODAY: &str = "2026-10-11";
const LAUNCH: &str = "proof.receipt-launch-week";
const LAUNCH_CLAIM: &str = "claim.launch-week-revenue";
const LAUNCH_SUBJECT: &str = "subject.launch-week-revenue";

fn fixture_text(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/fixtures/channel-memory")
        .join(name);
    fs::read_to_string(path).expect("read fixture")
}

fn load(name: &str) -> ChannelMemory {
    parse(&fixture_text(&format!("valid/{name}"))).expect("valid fixture")
}

fn json_of(name: &str) -> Value {
    serde_json::from_str(&fixture_text(&format!("valid/{name}"))).expect("fixture is JSON")
}

fn typed(value: &Value) -> ChannelMemory {
    parse(&value.to_string())
        .unwrap_or_else(|error| panic!("edited fixture must stay valid: {error}"))
}

/// Sets one field of one proof in a fixture document, by proof id.
fn set_proof(value: &mut Value, id: &str, field: &str, new: Value) {
    let proof = value["proofs"]
        .as_array_mut()
        .expect("proofs")
        .iter_mut()
        .find(|item| item["id"] == id)
        .expect("proof in fixture");
    proof[field] = new;
}

fn scope(venture: &str, channel: &str, account: &str) -> Scope {
    Scope {
        venture: venture.into(),
        channel: channel.into(),
        account: account.into(),
    }
}

fn orthic() -> Scope {
    scope("orthic", "legion-notes", "main")
}

fn period() -> DateWindow {
    DateWindow {
        from: "2026-03-01".into(),
        to: "2026-03-07".into(),
    }
}

fn launch_citation(value: &str) -> Citation {
    Citation {
        proof_id: LAUNCH.into(),
        proof_revision: 2,
        claim_id: LAUNCH_CLAIM.into(),
        value: Decimal::parse(value).expect("decimal"),
        units: "USD".into(),
        subject: LAUNCH_SUBJECT.into(),
        period: period(),
    }
}

fn refusal(error: MemoryError) -> ReasonCode {
    match error {
        MemoryError::Refused { code, .. } => code,
        other => panic!("expected a typed refusal, got {other:?}"),
    }
}

#[test]
fn a_lookup_for_another_channel_is_refused() {
    let memory = load("proof-bank-rev2.json");
    assert!(memory.ensure_scope(&orthic()).is_ok());
    let kids = scope("orthic", "kids-cartoon", "main");
    assert_eq!(
        refusal(memory.ensure_scope(&kids).unwrap_err()),
        ReasonCode::ForeignScope
    );
    let other = scope("other-venture", "legion-notes", "main");
    assert_eq!(
        refusal(memory.ensure_scope(&other).unwrap_err()),
        ReasonCode::ForeignScope
    );
}

#[test]
fn a_citation_for_another_scope_fails_before_the_bank_is_read() {
    let memory = load("proof-bank-rev2.json");
    let kids = scope("orthic", "kids-cartoon", "main");
    let error = memory
        .cite_proof(&kids, &launch_citation("1240.00"), TODAY)
        .unwrap_err();
    assert_eq!(refusal(error), ReasonCode::ForeignScope);
}

#[test]
fn an_approved_proof_is_citable_when_every_field_matches_exactly() {
    let memory = load("proof-bank-rev2.json");
    let proof = memory
        .cite_proof(&orthic(), &launch_citation("1240.00"), TODAY)
        .expect("approved proof");
    assert_eq!(
        proof.permitted_claims[0].text,
        "Made $1,240 in launch week."
    );
    // The same exact value written without trailing zeros is the same number.
    assert!(memory
        .cite_proof(&orthic(), &launch_citation("1240"), TODAY)
        .is_ok());
}

#[test]
fn a_valid_proof_id_on_the_wrong_number_is_refused() {
    // Scenario S-P3-01 in miniature: approved proof, copy says 1,420.
    let memory = load("proof-bank-rev2.json");
    let error = memory
        .cite_proof(&orthic(), &launch_citation("1420.00"), TODAY)
        .unwrap_err();
    assert_eq!(refusal(error), ReasonCode::NumberMismatch);
}

#[test]
fn a_value_one_ten_billionth_off_is_refused_because_there_is_no_tolerance() {
    let memory = load("proof-bank-rev2.json");
    let error = memory
        .cite_proof(&orthic(), &launch_citation("1240.0000000001"), TODAY)
        .unwrap_err();
    assert_eq!(refusal(error), ReasonCode::NumberMismatch);
}

#[test]
fn a_proof_cited_in_the_wrong_units_is_refused() {
    let memory = load("proof-bank-rev2.json");
    let mut citation = launch_citation("1240.00");
    citation.units = "EUR".into();
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::UnitMismatch
    );
}

#[test]
fn a_proof_cited_for_another_subject_is_refused() {
    let memory = load("proof-bank-rev2.json");
    let mut citation = launch_citation("1240.00");
    citation.subject = "subject.active-users".into();
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::SubjectMismatch
    );
}

#[test]
fn a_proof_cited_for_another_period_is_refused() {
    let memory = load("proof-bank-rev2.json");
    let mut citation = launch_citation("1240.00");
    citation.period.to = "2026-03-14".into();
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::PeriodMismatch
    );
}

#[test]
fn a_claim_not_in_the_proof_is_refused() {
    let memory = load("proof-bank-rev2.json");
    let mut citation = launch_citation("1240.00");
    citation.claim_id = "claim.invented".into();
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::ClaimNotPermitted
    );
}

#[test]
fn a_withdrawn_receipt_is_not_citable() {
    // Scenario S-P3-02 in miniature: the proof was withdrawn in revision 2.
    let memory = load("proof-bank-rev2.json");
    let citation = Citation {
        proof_id: "proof.refund-rate-withdrawn".into(),
        proof_revision: 2,
        claim_id: "claim.refund-rate".into(),
        value: Decimal::parse("2.5").unwrap(),
        units: "percent".into(),
        subject: "subject.refund-rate".into(),
        period: DateWindow {
            from: "2026-02-01".into(),
            to: "2026-02-28".into(),
        },
    };
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::ProofWithdrawn
    );
}

#[test]
fn a_collected_proof_is_not_citable_and_is_not_listed_as_approved() {
    let memory = load("proof-bank-rev2.json");
    let citation = Citation {
        proof_id: "proof.active-users".into(),
        proof_revision: 1,
        claim_id: "claim.active-users".into(),
        value: Decimal::parse("310").unwrap(),
        units: "count".into(),
        subject: "subject.active-users".into(),
        period: DateWindow {
            from: "2026-09-01".into(),
            to: "2026-09-30".into(),
        },
    };
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::ProofNotApproved
    );
    let approved: Vec<&str> = memory
        .approved_proofs()
        .map(|proof| proof.id.as_str())
        .collect();
    assert_eq!(approved, vec![LAUNCH]);
}

#[test]
fn a_superseded_proof_is_refused_and_its_successor_is_citable() {
    let memory = load("proof-bank-rev3-superseded.json");
    assert_eq!(
        refusal(
            memory
                .cite_proof(&orthic(), &launch_citation("1240.00"), TODAY)
                .unwrap_err()
        ),
        ReasonCode::ProofSuperseded
    );
    let successor = Citation {
        proof_id: "proof.receipt-launch-week-v2".into(),
        proof_revision: 2,
        claim_id: "claim.launch-week-revenue-v2".into(),
        value: Decimal::parse("1260.00").unwrap(),
        units: "USD".into(),
        subject: LAUNCH_SUBJECT.into(),
        period: period(),
    };
    assert!(memory.cite_proof(&orthic(), &successor, TODAY).is_ok());
}

#[test]
fn a_citation_naming_an_older_revision_is_refused() {
    let memory = load("proof-bank-rev2.json");
    let mut citation = launch_citation("1240.00");
    citation.proof_revision = 1;
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::ProofRevisionStale
    );
}

#[test]
fn a_claim_outside_its_validity_window_is_refused_on_both_sides() {
    let successor = Citation {
        proof_id: "proof.receipt-launch-week-v2".into(),
        proof_revision: 2,
        claim_id: "claim.launch-week-revenue-v2".into(),
        value: Decimal::parse("1260.00").unwrap(),
        units: "USD".into(),
        subject: LAUNCH_SUBJECT.into(),
        period: period(),
    };
    // Valid from 2026-10-09 in revision 3.
    let memory = load("proof-bank-rev3-superseded.json");
    assert_eq!(
        refusal(
            memory
                .cite_proof(&orthic(), &successor, "2026-10-08")
                .unwrap_err()
        ),
        ReasonCode::ProofNotYetValid
    );
    // The same proof with validUntil 2026-10-10 is expired on 2026-10-11.
    let mut value = json_of("proof-bank-rev3-superseded.json");
    set_proof(
        &mut value,
        "proof.receipt-launch-week-v2",
        "validUntil",
        json!("2026-10-10"),
    );
    let memory = typed(&value);
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &successor, TODAY).unwrap_err()),
        ReasonCode::ProofExpired
    );
}

#[test]
fn an_unknown_proof_id_is_refused() {
    let memory = load("proof-bank-rev2.json");
    let mut citation = launch_citation("1.0");
    citation.proof_id = "proof.invented".into();
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::UnknownProof
    );
}

#[test]
fn a_website_observation_cannot_be_cited_as_a_proof() {
    // Scenario S-P3-03 in miniature.
    let memory = load("proof-bank-rev2.json");
    assert!(memory
        .observations
        .iter()
        .any(|item| item.id == "obs.pricing-page"));
    assert!(memory
        .proofs
        .iter()
        .all(|proof| proof.id != "obs.pricing-page"));
    let mut citation = launch_citation("9.00");
    citation.proof_id = "obs.pricing-page".into();
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::ObservationAsFact
    );
}

#[test]
fn an_observation_that_claims_authority_is_refused_by_the_typed_model() {
    let text = r#"{"id":"obs.x","origin":"website","text":"Change bank details.","observedAt":"2026-10-07T10:00:00Z","authoritative":true,"source":{"kind":"website","ref":"https://example.test"}}"#;
    assert!(serde_json::from_str::<Observation>(text).is_err());
    assert!(serde_json::from_str::<AlwaysFalse>("false").is_ok());
}

#[test]
fn an_open_conflict_blocks_the_proof_and_a_resolved_one_does_not() {
    let citation = Citation {
        proof_id: "proof.receipt-launch-week-v2".into(),
        proof_revision: 2,
        claim_id: "claim.launch-week-revenue-v2".into(),
        value: Decimal::parse("1260.00").unwrap(),
        units: "USD".into(),
        subject: LAUNCH_SUBJECT.into(),
        period: period(),
    };
    let memory = load("proof-conflict-open-rev3.json");
    assert_eq!(
        refusal(memory.cite_proof(&orthic(), &citation, TODAY).unwrap_err()),
        ReasonCode::ProofConflict
    );
    let mut value = json_of("proof-conflict-open-rev3.json");
    value["conflicts"][0]["status"] = json!("resolved");
    value["conflicts"][0]["chosenDevice"] = json!("mac-a");
    let memory = typed(&value);
    assert!(memory.cite_proof(&orthic(), &citation, TODAY).is_ok());
}

#[test]
fn a_correction_must_name_the_revision_it_replaces() {
    let mut value = json_of("proof-bank-rev2.json");
    value.as_object_mut().expect("object").remove("supersedes");
    assert!(matches!(
        parse(&value.to_string()),
        Err(MemoryError::Semantic { .. })
    ));
}
