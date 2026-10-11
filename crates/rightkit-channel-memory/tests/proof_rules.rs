//! Status transitions (contract §3), the revision chain (§6 A3, A4), the
//! controlled write path (§7), conflict records (§6 A5) and banned-move
//! matching. Each rule has a passing and a failing case.

use std::fs;
use std::path::PathBuf;

use rightkit_channel_memory::{
    check_stale_arrival, check_successor, check_transition, check_write, parse, Approval, Author,
    ChannelMemory, Decimal, Intent, MemoryError, Proof, ProofStatus, ReasonCode, WriteRequest,
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

fn typed(value: &Value) -> ChannelMemory {
    parse(&value.to_string())
        .unwrap_or_else(|error| panic!("edited fixture must stay valid: {error}"))
}

fn proof<'a>(memory: &'a ChannelMemory, id: &str) -> &'a Proof {
    memory
        .proofs
        .iter()
        .find(|item| item.id == id)
        .expect("proof in fixture")
}

fn proof_mut<'a>(memory: &'a mut ChannelMemory, id: &str) -> &'a mut Proof {
    memory
        .proofs
        .iter_mut()
        .find(|item| item.id == id)
        .expect("proof in fixture")
}

fn code(result: Result<(), MemoryError>) -> ReasonCode {
    match result {
        Err(MemoryError::Refused { code, .. }) => code,
        other => panic!("expected a typed refusal, got {other:?}"),
    }
}

fn request(author: Author, intent: Intent, base: u32, proposed: ChannelMemory) -> WriteRequest {
    WriteRequest {
        author,
        intent,
        base_revision: base,
        proposed,
    }
}

// ---- status transitions (§3) ---------------------------------------------

#[test]
fn legal_status_moves_between_revisions_are_accepted() {
    let rev2 = load("proof-bank-rev2.json");
    let rev3 = load("proof-bank-rev3-superseded.json");
    // collected -> approved for the successor, approved -> superseded for the original.
    assert!(check_transition(proof(&rev2, SUCCESSOR), proof(&rev3, SUCCESSOR)).is_ok());
    assert!(check_transition(proof(&rev2, LAUNCH), proof(&rev3, LAUNCH)).is_ok());
}

#[test]
fn approved_content_is_frozen() {
    let rev2 = load("proof-bank-rev2.json");
    let from = proof(&rev2, LAUNCH).clone();
    let mut to = from.clone();
    to.revision = 3;
    to.supersedes_revision = Some(2);
    to.value = Decimal::parse("1250.00").unwrap();
    assert_eq!(
        code(check_transition(&from, &to)),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn withdrawn_is_terminal() {
    let rev2 = load("proof-bank-rev2.json");
    let from = proof(&rev2, REFUND).clone();
    let mut restored = from.clone();
    restored.status = ProofStatus::Collected;
    restored.withdrawal = None;
    restored.revision = 3;
    restored.supersedes_revision = Some(2);
    assert_eq!(
        code(check_transition(&from, &restored)),
        ReasonCode::ProofWithdrawn
    );
}

#[test]
fn superseded_is_terminal() {
    let rev3 = load("proof-bank-rev3-superseded.json");
    let from = proof(&rev3, LAUNCH).clone();
    let mut back = from.clone();
    back.status = ProofStatus::Approved;
    back.superseded_by = None;
    back.approval = Some(Approval {
        approved_by: "adrian".into(),
        approved_at: "2026-10-12T09:00:00Z".into(),
    });
    back.revision = 4;
    back.supersedes_revision = Some(3);
    assert_eq!(
        code(check_transition(&from, &back)),
        ReasonCode::ProofSuperseded
    );
}

#[test]
fn a_collected_proof_cannot_jump_to_superseded() {
    let rev2 = load("proof-bank-rev2.json");
    let from = proof(&rev2, SUCCESSOR).clone();
    let mut to = from.clone();
    to.status = ProofStatus::Superseded;
    to.superseded_by = Some(LAUNCH.into());
    to.revision = 2;
    to.supersedes_revision = Some(1);
    assert_eq!(
        code(check_transition(&from, &to)),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn an_approved_proof_cannot_return_to_collected() {
    let rev2 = load("proof-bank-rev2.json");
    let from = proof(&rev2, LAUNCH).clone();
    let mut to = from.clone();
    to.status = ProofStatus::Collected;
    to.approval = None;
    to.revision = 3;
    to.supersedes_revision = Some(2);
    assert_eq!(
        code(check_transition(&from, &to)),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn a_transition_must_advance_the_proof_revision_by_one() {
    let rev2 = load("proof-bank-rev2.json");
    let from = proof(&rev2, LAUNCH).clone();
    let mut to = from.clone();
    to.status = ProofStatus::Withdrawn;
    to.approval = None;
    to.withdrawal = Some(rightkit_channel_memory::Withdrawal {
        withdrawn_by: "adrian".into(),
        withdrawn_at: "2026-10-12T09:00:00Z".into(),
        reason: "Test withdrawal".into(),
    });
    to.revision = 5;
    to.supersedes_revision = Some(2);
    assert_eq!(
        code(check_transition(&from, &to)),
        ReasonCode::IllegalTransition
    );
}

// ---- revision chain (§6 A3, A4) ------------------------------------------

#[test]
fn a_successor_must_follow_the_head_revision() {
    let head = load("proof-bank-rev2.json");
    let mut next = load("proof-bank-rev3-superseded.json");
    assert!(check_successor(&head, &next).is_ok());
    next.revision = 4;
    assert_eq!(
        code(check_successor(&head, &next)),
        ReasonCode::RevisionConflict
    );
}

#[test]
fn a_successor_cannot_restore_a_withdrawn_proof() {
    // Scenario S-P3-06 in miniature: the withdrawal is the head; a later
    // revision tries to bring the proof back as collected.
    let head = load("proof-bank-rev2.json");
    let mut next = head.clone();
    next.revision = 3;
    next.supersedes = Some(2);
    let refund = proof_mut(&mut next, REFUND);
    refund.status = ProofStatus::Collected;
    refund.withdrawal = None;
    refund.revision = 3;
    refund.supersedes_revision = Some(2);
    assert_eq!(
        code(check_successor(&head, &next)),
        ReasonCode::ProofWithdrawn
    );
}

#[test]
fn a_new_proof_must_start_collected() {
    let head = load("proof-bank-rev2.json");
    let mut next = head.clone();
    next.revision = 3;
    next.supersedes = Some(2);
    let mut fresh = proof(&head, SUCCESSOR).clone();
    fresh.id = "proof.fresh-approved".into();
    fresh.status = ProofStatus::Approved;
    fresh.approval = Some(Approval {
        approved_by: "adrian".into(),
        approved_at: "2026-10-12T09:00:00Z".into(),
    });
    next.proofs.push(fresh);
    assert_eq!(
        code(check_successor(&head, &next)),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn a_withdrawn_proof_stays_withdrawn_when_an_older_revision_arrives() {
    // The head is revision 2 with the refund withdrawn. A revision 1 body that
    // shows it approved must not restore it.
    let head = load("proof-bank-rev2.json");
    let mut stale = head.clone();
    stale.revision = 1;
    stale.supersedes = None;
    let refund = proof_mut(&mut stale, REFUND);
    refund.status = ProofStatus::Approved;
    refund.withdrawal = None;
    refund.approval = Some(Approval {
        approved_by: "adrian".into(),
        approved_at: "2026-02-10T09:00:00Z".into(),
    });
    refund.revision = 1;
    refund.supersedes_revision = None;
    assert_eq!(
        code(check_stale_arrival(&head, &stale)),
        ReasonCode::ProofWithdrawn
    );
}

#[test]
fn an_older_copy_of_the_same_withdrawn_body_is_accepted() {
    let head = load("proof-bank-rev2.json");
    let mut stale = head.clone();
    stale.revision = 1;
    stale.supersedes = None;
    assert!(check_stale_arrival(&head, &stale).is_ok());
}

// ---- controlled writes (§7) ----------------------------------------------

#[test]
fn adrian_supersedes_a_proof_with_an_approved_successor() {
    let head = load("proof-bank-rev2.json");
    let next = load("proof-bank-rev3-superseded.json");
    assert!(check_write(&head, &request(Author::Adrian, Intent::Supersede, 2, next)).is_ok());
}

#[test]
fn a_skill_cannot_approve_a_proof() {
    let head = load("proof-bank-rev2.json");
    let next = load("proof-bank-rev3-superseded.json");
    assert_eq!(
        code(check_write(
            &head,
            &request(Author::Skill, Intent::Approve, 2, next.clone())
        )),
        ReasonCode::SkillStatusForbidden
    );
    // Same status change, labelled as a collection: still refused by the status rule.
    assert_eq!(
        code(check_write(
            &head,
            &request(Author::Skill, Intent::Collect, 2, next)
        )),
        ReasonCode::SkillStatusForbidden
    );
}

#[test]
fn a_skill_cannot_write_authoritative_memory() {
    let head = load("proof-bank-rev2.json");
    let mut value = json_of("proof-bank-rev2.json");
    value["revision"] = json!(3);
    value["supersedes"] = json!(2);
    value["voice"]["tone"] = json!("warm and loud");
    let next = typed(&value);
    assert_eq!(
        code(check_write(
            &head,
            &request(Author::Skill, Intent::Collect, 2, next)
        )),
        ReasonCode::AuthorForbidden
    );
}

#[test]
fn a_skill_may_collect_a_new_proof() {
    let head = load("proof-bank-rev2.json");
    let mut next = head.clone();
    next.revision = 3;
    next.supersedes = Some(2);
    let mut fresh = proof(&head, SUCCESSOR).clone();
    fresh.id = "proof.interview-collected".into();
    next.proofs.push(fresh);
    assert!(check_write(&head, &request(Author::Skill, Intent::Collect, 2, next)).is_ok());
}

#[test]
fn a_skill_may_record_an_observation_that_is_not_authoritative() {
    let head = load("proof-bank-rev2.json");
    let mut value = json_of("proof-bank-rev2.json");
    value["revision"] = json!(3);
    value["supersedes"] = json!(2);
    value["observations"].as_array_mut().unwrap().push(
        json!({"id": "obs.review-page", "origin": "website", "text": "Customers call it fast.",
                     "observedAt": "2026-10-10T10:00:00Z", "authoritative": false,
                     "source": {"kind": "website", "ref": "https://example.test/reviews"}}),
    );
    let next = typed(&value);
    assert!(check_write(&head, &request(Author::Skill, Intent::Observe, 2, next)).is_ok());
}

#[test]
fn an_observation_cannot_become_authoritative_in_a_write() {
    // Scenario S-P3-03 in miniature: the flag is refused before the write is considered.
    let head = load("proof-bank-rev2.json");
    let mut value = json!({
        "author": "skill",
        "intent": "observe",
        "baseRevision": 2,
        "proposed": json_of("proof-bank-rev2.json"),
    });
    value["proposed"]["revision"] = json!(3);
    value["proposed"]["supersedes"] = json!(2);
    value["proposed"]["observations"][0]["authoritative"] = json!(true);
    assert!(serde_json::from_value::<WriteRequest>(value.clone()).is_err());
    assert!(parse(&value["proposed"].to_string()).is_err());
    assert_eq!(head.revision, 2);
}

#[test]
fn a_daemon_cannot_withdraw_a_proof() {
    let head = load("proof-bank-rev2.json");
    let next = load("proof-bank-rev3-superseded.json");
    assert_eq!(
        code(check_write(
            &head,
            &request(Author::Daemon, Intent::Withdraw, 2, next)
        )),
        ReasonCode::AuthorForbidden
    );
}

#[test]
fn a_daemon_may_record_a_conflict_without_touching_proof_status() {
    let head = load("proof-bank-rev2.json");
    let conflict_doc = load("proof-conflict-open-rev3.json");
    let mut next = head.clone();
    next.revision = 3;
    next.supersedes = Some(2);
    next.conflicts = conflict_doc.conflicts.clone();
    assert!(check_write(&head, &request(Author::Daemon, Intent::Conflict, 2, next)).is_ok());
}

#[test]
fn approve_intent_needs_a_status_change_to_approved() {
    let head = load("proof-bank-rev2.json");
    let mut next = head.clone();
    next.revision = 3;
    next.supersedes = Some(2);
    assert_eq!(
        code(check_write(
            &head,
            &request(Author::Adrian, Intent::Approve, 2, next)
        )),
        ReasonCode::IllegalTransition
    );
}

#[test]
fn a_write_against_a_stale_base_is_refused() {
    // Scenario S-P3-06: the device writes from revision 1 after the head moved to 2.
    let head = load("proof-bank-rev2.json");
    let next = load("proof-bank-rev3-superseded.json");
    assert_eq!(
        code(check_write(
            &head,
            &request(Author::Adrian, Intent::Supersede, 1, next)
        )),
        ReasonCode::RevisionConflict
    );
}

#[test]
fn an_edit_cannot_change_approved_content() {
    let head = load("proof-bank-rev2.json");
    let mut value = json_of("proof-bank-rev2.json");
    value["revision"] = json!(3);
    value["supersedes"] = json!(2);
    let launch = value["proofs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|item| item["id"] == LAUNCH)
        .unwrap();
    launch["revision"] = json!(3);
    launch["supersedesRevision"] = json!(2);
    launch["value"] = json!("1250.00");
    launch["permittedClaims"][0]["value"] = json!("1250.00");
    let next = typed(&value);
    assert_eq!(
        code(check_write(
            &head,
            &request(Author::Adrian, Intent::Edit, 2, next)
        )),
        ReasonCode::IllegalTransition
    );
}

// ---- conflict records (§6 A5) --------------------------------------------

#[test]
fn a_conflict_record_keeps_both_bodies_and_its_state() {
    let memory = load("proof-conflict-open-rev3.json");
    let conflict = &memory.conflicts[0];
    assert_eq!(conflict.bodies.len(), 2);
    assert_ne!(conflict.bodies[0].device, conflict.bodies[1].device);
    assert_eq!(
        conflict.status,
        rightkit_channel_memory::ConflictStatus::Open
    );
}

// ---- banned moves --------------------------------------------------------

#[test]
fn a_banned_move_matches_its_phrases_ignoring_case() {
    let memory = load("kids-cartoon-rev1.json");
    let move_ = &memory.banned_moves[0];
    assert!(move_.matches("Stop Scrolling right now"));
    assert!(!move_.matches("A calm narrator reads the story"));
}

#[test]
fn banned_move_phrases_must_be_strings() {
    let mut value = json_of("proof-bank-rev2.json");
    value["bannedMoves"][0]["phrases"] = json!([1]);
    assert!(matches!(
        parse(&value.to_string()),
        Err(MemoryError::Schema { .. })
    ));
}

#[test]
fn a_decimal_in_a_proof_is_exact_not_a_float() {
    let memory = load("proof-bank-rev2.json");
    assert_eq!(proof(&memory, LAUNCH).value.as_str(), "1240.00");
    assert_eq!(
        proof(&memory, LAUNCH).value,
        Decimal::parse("1240").unwrap()
    );
}
