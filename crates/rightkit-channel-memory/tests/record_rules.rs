//! Record-level entry points: `validate_proof` (one proof in isolation) and
//! `check_proof_write` (one proposed status change). Each rule has a passing
//! and a failing case. The document path and the record path must return the
//! same error for the same defect, so the two are compared directly.

use std::fs;
use std::path::PathBuf;

use rightkit_channel_memory::{
    check_proof_write, check_supersession, check_write, parse, validate_proof, Approval, Author,
    ChannelMemory, Decimal, Intent, MemoryError, Proof, ProofStatus, ReasonCode, Scope, Withdrawal,
    WriteRequest,
};
use serde_json::{json, Value};

const LAUNCH: &str = "proof.receipt-launch-week";
const SUCCESSOR: &str = "proof.receipt-launch-week-v2";
const REFUND: &str = "proof.refund-rate-withdrawn";

fn fixture_text(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts-pending/channel-memory/fixtures")
        .join(name);
    fs::read_to_string(path).expect("read fixture")
}

fn load(name: &str) -> ChannelMemory {
    parse(&fixture_text(&format!("valid/{name}"))).expect("valid fixture")
}

fn json_of(name: &str) -> Value {
    serde_json::from_str(&fixture_text(&format!("valid/{name}"))).expect("fixture is JSON")
}

fn proof_of(name: &str, id: &str) -> Proof {
    load(name)
        .proofs
        .into_iter()
        .find(|item| item.id == id)
        .expect("proof in fixture")
}

fn proof_json(name: &str, id: &str) -> Value {
    json_of(name)["proofs"]
        .as_array()
        .expect("proofs")
        .iter()
        .find(|item| item["id"] == id)
        .expect("proof in fixture")
        .clone()
}

fn scope() -> Scope {
    Scope {
        venture: "orthic".into(),
        channel: "legion-notes".into(),
        account: "main".into(),
    }
}

fn refusal(result: Result<(), MemoryError>) -> ReasonCode {
    match result {
        Err(MemoryError::Refused { code, .. }) => code,
        other => panic!("expected a typed refusal, got {other:?}"),
    }
}

fn semantic(result: Result<(), MemoryError>) -> String {
    match result {
        Err(MemoryError::Semantic { rule }) => rule,
        other => panic!("expected a semantic refusal, got {other:?}"),
    }
}

fn withdrawn_copy(prev: &Proof) -> Proof {
    let mut next = prev.clone();
    next.status = ProofStatus::Withdrawn;
    next.approval = None;
    next.withdrawal = Some(Withdrawal {
        withdrawn_by: "adrian".into(),
        withdrawn_at: "2026-10-12T09:00:00Z".into(),
        reason: "Test withdrawal".into(),
    });
    next.permitted_claims.clear();
    next.revision = prev.revision + 1;
    next.supersedes_revision = Some(prev.revision);
    next
}

fn approved_by_adrian() -> Approval {
    Approval {
        approved_by: "adrian".into(),
        approved_at: "2026-10-12T09:00:00Z".into(),
    }
}

// ---- validate_proof ------------------------------------------------------

#[test]
fn a_valid_proof_record_passes_in_isolation() {
    let proof = proof_of("proof-bank-rev2.json", LAUNCH);
    assert!(validate_proof(&proof, &scope()).is_ok());
}

#[test]
fn a_proof_with_an_open_unit_passes_in_isolation() {
    let proof = proof_of("unit-shorts-per-week.json", LAUNCH);
    assert_eq!(proof.units.as_str(), "shorts_per_week");
    assert!(validate_proof(&proof, &scope()).is_ok());
}

#[test]
fn a_malformed_scope_is_refused() {
    let proof = proof_of("proof-bank-rev2.json", LAUNCH);
    let mut bad = scope();
    bad.venture = "Orthic".into();
    assert!(matches!(
        validate_proof(&proof, &bad),
        Err(MemoryError::Schema { .. })
    ));
}

#[test]
fn an_approved_proof_without_an_approver_is_refused() {
    let mut proof = proof_of("proof-bank-rev2.json", LAUNCH);
    proof.approval = None;
    assert!(matches!(
        validate_proof(&proof, &scope()),
        Err(MemoryError::Schema { .. })
    ));
}

#[test]
fn a_superseded_proof_without_a_successor_is_refused() {
    let mut proof = proof_of("proof-bank-rev3-superseded.json", LAUNCH);
    assert_eq!(proof.status, ProofStatus::Superseded);
    proof.superseded_by = None;
    assert!(matches!(
        validate_proof(&proof, &scope()),
        Err(MemoryError::Schema { .. })
    ));
}

#[test]
fn a_period_that_ends_before_it_starts_is_refused() {
    let mut proof = proof_of("proof-bank-rev2.json", LAUNCH);
    proof.period.to = "2000-01-01".into();
    assert!(semantic(validate_proof(&proof, &scope())).contains("period ends before it starts"));
}

#[test]
fn a_validity_window_that_ends_before_it_starts_is_refused() {
    let mut proof = proof_of("proof-bank-rev2.json", LAUNCH);
    proof.valid_until = Some("2026-01-01".into());
    assert!(semantic(validate_proof(&proof, &scope())).contains("validUntil is before validFrom"));
}

#[test]
fn a_repeated_claim_id_is_refused() {
    let mut proof = proof_of("proof-bank-rev2.json", LAUNCH);
    let claim = proof.permitted_claims[0].clone();
    proof.permitted_claims.push(claim);
    assert!(semantic(validate_proof(&proof, &scope())).starts_with("E_DUPLICATE_CLAIM"));
}

#[test]
fn a_claim_whose_value_differs_from_its_proof_is_refused() {
    let mut proof = proof_of("proof-bank-rev2.json", LAUNCH);
    proof.permitted_claims[0].value = Decimal::parse("1250.00").unwrap();
    assert!(semantic(validate_proof(&proof, &scope())).contains("value differs"));
}

#[test]
fn a_claim_whose_units_differ_from_its_proof_is_refused() {
    let mut proof = proof_of("proof-bank-rev2.json", LAUNCH);
    proof.permitted_claims[0].units = serde_json::from_value(json!("EUR")).unwrap();
    assert!(semantic(validate_proof(&proof, &scope())).contains("units differs"));
}

#[test]
fn a_bad_decimal_cannot_become_a_proof_record() {
    for text in ["1,240.00", "1e3", "1240.", "NaN"] {
        let mut value = proof_json("proof-bank-rev2.json", LAUNCH);
        value["value"] = json!(text);
        assert!(
            serde_json::from_value::<Proof>(value).is_err(),
            "`{text}` must not become a proof value"
        );
    }
}

#[test]
fn a_unit_outside_the_vocabulary_cannot_become_a_proof_record() {
    for text in [
        "Views",
        "shorts per week",
        "9lives",
        "",
        "x_per_hour",
        "views_per_",
        "views_per",
        "x_per",
        "a_per_per_day",
    ] {
        let mut value = proof_json("proof-bank-rev2.json", LAUNCH);
        value["units"] = json!(text);
        assert!(
            serde_json::from_value::<Proof>(value).is_err(),
            "`{text}` must not become a unit"
        );
    }
}

// ---- document path and record path agree ---------------------------------

#[test]
fn the_same_period_defect_returns_the_same_error_on_both_paths() {
    let mut value = json_of("proof-bank-rev2.json");
    value["proofs"][0]["period"]["to"] = json!("2000-01-01");
    let document = parse(&value.to_string()).expect_err("document refuses the period");

    let mut proof = proof_of("proof-bank-rev2.json", LAUNCH);
    proof.period.to = "2000-01-01".into();
    let record = validate_proof(&proof, &scope()).expect_err("record refuses the period");

    assert_eq!(document, record);
}

#[test]
fn the_same_claim_defect_returns_the_same_error_on_both_paths() {
    let mut value = json_of("proof-bank-rev2.json");
    value["proofs"][0]["permittedClaims"][0]["value"] = json!("1250.00");
    let document = parse(&value.to_string()).expect_err("document refuses the claim");

    let mut proof = proof_of("proof-bank-rev2.json", LAUNCH);
    proof.permitted_claims[0].value = Decimal::parse("1250.00").unwrap();
    let record = validate_proof(&proof, &scope()).expect_err("record refuses the claim");

    assert_eq!(document, record);
}

#[test]
fn a_skill_approval_returns_the_same_code_on_both_paths() {
    let head = load("proof-bank-rev2.json");
    let next_doc = load("proof-bank-rev3-superseded.json");
    let document = refusal(check_write(
        &head,
        &WriteRequest {
            author: Author::Skill,
            intent: Intent::Approve,
            base_revision: 2,
            proposed: next_doc,
        },
    ));

    let prev = proof_of("proof-bank-rev2.json", SUCCESSOR);
    let next = proof_of("proof-bank-rev3-superseded.json", SUCCESSOR);
    let record = refusal(check_proof_write(Some(&prev), &next, Author::Skill));

    assert_eq!(document, ReasonCode::SkillStatusForbidden);
    assert_eq!(record, document);
}

#[test]
fn a_daemon_withdrawal_returns_the_same_code_on_both_paths() {
    let head = load("proof-bank-rev2.json");
    let document = refusal(check_write(
        &head,
        &WriteRequest {
            author: Author::Daemon,
            intent: Intent::Withdraw,
            base_revision: 2,
            proposed: load("proof-bank-rev3-superseded.json"),
        },
    ));

    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let next = withdrawn_copy(&prev);
    let record = refusal(check_proof_write(Some(&prev), &next, Author::Daemon));

    assert_eq!(document, ReasonCode::AuthorForbidden);
    assert_eq!(record, document);
}

// ---- check_proof_write: skill author -------------------------------------

#[test]
fn a_skill_cannot_approve_a_proof() {
    let prev = proof_of("proof-bank-rev2.json", SUCCESSOR);
    let next = proof_of("proof-bank-rev3-superseded.json", SUCCESSOR);
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &next, Author::Skill)),
        ReasonCode::SkillStatusForbidden
    );
}

#[test]
fn a_skill_cannot_withdraw_a_proof() {
    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let next = withdrawn_copy(&prev);
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &next, Author::Skill)),
        ReasonCode::SkillStatusForbidden
    );
}

#[test]
fn a_skill_cannot_supersede_a_proof() {
    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let next = proof_of("proof-bank-rev3-superseded.json", LAUNCH);
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &next, Author::Skill)),
        ReasonCode::SkillStatusForbidden
    );
}

#[test]
fn a_skill_cannot_edit_approved_content() {
    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let mut next = prev.clone();
    next.revision = 3;
    next.supersedes_revision = Some(2);
    next.value = Decimal::parse("1250.00").unwrap();
    next.permitted_claims[0].value = Decimal::parse("1250.00").unwrap();
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &next, Author::Skill)),
        ReasonCode::AuthorForbidden
    );
}

#[test]
fn a_skill_may_collect_a_new_proof() {
    let next = proof_of("proof-bank-rev2.json", SUCCESSOR);
    assert!(check_proof_write(None, &next, Author::Skill).is_ok());
}

// ---- check_proof_write: adrian and the daemon ----------------------------

#[test]
fn adrian_may_approve_a_collected_proof() {
    let prev = proof_of("proof-bank-rev2.json", SUCCESSOR);
    let next = proof_of("proof-bank-rev3-superseded.json", SUCCESSOR);
    assert!(check_proof_write(Some(&prev), &next, Author::Adrian).is_ok());
}

#[test]
fn adrian_may_withdraw_an_approved_proof() {
    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let next = withdrawn_copy(&prev);
    assert!(check_proof_write(Some(&prev), &next, Author::Adrian).is_ok());
}

#[test]
fn adrian_may_supersede_an_approved_proof() {
    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let next = proof_of("proof-bank-rev3-superseded.json", LAUNCH);
    assert!(check_proof_write(Some(&prev), &next, Author::Adrian).is_ok());
}

#[test]
fn the_daemon_cannot_withdraw_a_proof() {
    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let next = withdrawn_copy(&prev);
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &next, Author::Daemon)),
        ReasonCode::AuthorForbidden
    );
}

#[test]
fn a_new_record_must_start_collected() {
    let next = proof_of("proof-bank-rev3-superseded.json", SUCCESSOR);
    assert_eq!(next.status, ProofStatus::Approved);
    assert_eq!(
        refusal(check_proof_write(None, &next, Author::Adrian)),
        ReasonCode::IllegalTransition
    );
}

// ---- supersession (contract §3) ------------------------------------------

/// The document-path code: `parse` keeps `Semantic` and puts the code first.
fn doc_code(error: MemoryError) -> String {
    match error {
        MemoryError::Semantic { rule } => rule.split(':').next().unwrap_or_default().to_string(),
        other => panic!("expected a semantic refusal, got {other:?}"),
    }
}

#[test]
fn a_superseded_proof_and_its_approved_successor_agree() {
    let prev = proof_of("proof-bank-rev3-superseded.json", LAUNCH);
    let successor = proof_of("proof-bank-rev3-superseded.json", SUCCESSOR);
    assert!(check_supersession(&prev, &successor).is_ok());
}

#[test]
fn a_successor_that_is_not_approved_is_refused() {
    // The document path refuses this same defect; see the test below.
    let prev = proof_of("proof-bank-rev3-superseded.json", LAUNCH);
    let collected = proof_of("proof-bank-rev2.json", SUCCESSOR);
    assert_eq!(
        refusal(check_supersession(&prev, &collected)),
        ReasonCode::ProofNotApproved
    );
}

#[test]
fn a_record_that_is_not_superseded_has_no_supersession_to_check() {
    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let successor = proof_of("proof-bank-rev3-superseded.json", SUCCESSOR);
    assert_eq!(
        refusal(check_supersession(&prev, &successor)),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn a_successor_with_another_id_than_the_one_named_is_refused() {
    let prev = proof_of("proof-bank-rev3-superseded.json", LAUNCH);
    let other = proof_of("proof-bank-rev2.json", "proof.active-users");
    assert_eq!(
        refusal(check_supersession(&prev, &other)),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn a_proof_cannot_supersede_itself() {
    let mut prev = proof_of("proof-bank-rev3-superseded.json", LAUNCH);
    prev.superseded_by = Some(LAUNCH.into());
    let successor = prev.clone();
    assert_eq!(
        refusal(check_supersession(&prev, &successor)),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn the_document_and_record_paths_return_the_same_code_for_an_unapproved_successor() {
    let text = fixture_text("semantic-invalid/superseded-successor-not-approved.json");
    let document = parse(&text).expect_err("document refuses the successor");
    let doc = doc_code(document);

    let value: Value = serde_json::from_str(&text).unwrap();
    let prev: Proof = serde_json::from_value(
        value["proofs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["status"] == "superseded")
            .unwrap()
            .clone(),
    )
    .unwrap();
    let successor: Proof = serde_json::from_value(
        value["proofs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == SUCCESSOR)
            .unwrap()
            .clone(),
    )
    .unwrap();
    let record = refusal(check_supersession(&prev, &successor));

    assert_eq!(doc, ReasonCode::ProofNotApproved.as_str());
    assert_eq!(doc, record.as_str());
}

#[test]
fn a_document_naming_an_absent_successor_is_refused_with_unknown_proof() {
    let mut value = json_of("proof-bank-rev3-superseded.json");
    let proofs = value["proofs"].as_array_mut().unwrap();
    proofs.retain(|item| item["id"] != SUCCESSOR);
    let document = parse(&value.to_string()).expect_err("document refuses the absent successor");
    assert_eq!(doc_code(document), ReasonCode::UnknownProof.as_str());
}

// ---- check_proof_write: withdrawn is terminal ----------------------------

#[test]
fn a_write_out_of_withdrawn_is_refused_for_adrian() {
    let prev = proof_of("proof-bank-rev2.json", REFUND);
    let mut next = prev.clone();
    next.status = ProofStatus::Collected;
    next.withdrawal = None;
    next.revision = 3;
    next.supersedes_revision = Some(2);
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &next, Author::Adrian)),
        ReasonCode::ProofWithdrawn
    );
}

#[test]
fn a_write_out_of_withdrawn_is_refused_for_a_skill() {
    let prev = proof_of("proof-bank-rev2.json", REFUND);
    let mut next = prev.clone();
    next.status = ProofStatus::Collected;
    next.withdrawal = None;
    next.revision = 3;
    next.supersedes_revision = Some(2);
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &next, Author::Skill)),
        ReasonCode::ProofWithdrawn
    );
}

#[test]
fn a_stale_revision_cannot_resurrect_a_withdrawn_proof() {
    // The head withdrew the refund at revision 2. A revision 1 body that shows
    // it approved must not restore it, even when Adrian writes it.
    let prev = proof_of("proof-bank-rev2.json", REFUND);
    let mut stale = prev.clone();
    stale.status = ProofStatus::Approved;
    stale.withdrawal = None;
    stale.approval = Some(approved_by_adrian());
    stale.revision = 1;
    stale.supersedes_revision = None;
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &stale, Author::Adrian)),
        ReasonCode::ProofWithdrawn
    );
}

#[test]
fn an_approved_proof_cannot_have_its_content_changed_by_adrian() {
    let prev = proof_of("proof-bank-rev2.json", LAUNCH);
    let mut next = prev.clone();
    next.revision = 3;
    next.supersedes_revision = Some(2);
    next.value = Decimal::parse("1250.00").unwrap();
    next.permitted_claims[0].value = Decimal::parse("1250.00").unwrap();
    assert_eq!(
        refusal(check_proof_write(Some(&prev), &next, Author::Adrian)),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn a_record_write_runs_the_record_rules() {
    let prev = proof_of("proof-bank-rev2.json", SUCCESSOR);
    let mut next = proof_of("proof-bank-rev3-superseded.json", SUCCESSOR);
    next.period.to = "2000-01-01".into();
    assert!(
        semantic(check_proof_write(Some(&prev), &next, Author::Adrian))
            .contains("period ends before it starts")
    );
}

#[test]
fn a_bad_decimal_cannot_reach_a_record_write() {
    let mut value = proof_json("proof-bank-rev3-superseded.json", SUCCESSOR);
    value["value"] = json!("1,260.00");
    assert!(serde_json::from_value::<Proof>(value).is_err());
}
