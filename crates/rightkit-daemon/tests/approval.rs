//! The approval workflow (PK07-4, PK07-6): binding, scope, expiry, revocation, one claim, audit
//! before effect, uncertain effects and crash recovery. The verifier is a fake that emulates a
//! signature by requiring the proof to carry the exact binding string of the stored request, so
//! "a proof for another request, action, account or revision fails" is exercised without a
//! cryptography dependency; the real profile verifiers (Pulse P-256, Mail sessions) stay in the
//! apps.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rightkit_daemon::approval::{
    redact_detail, AccountScope, ActionProfile, ApprovalClock, ApprovalCoordinator, ApprovalError,
    ApprovalProfile, ApprovalRecord, ApprovalState, ApprovalVerifier, AuditError, AuditEvent,
    AuditKind, AuditRequirement, AuditSink, ClaimRecovery, Decision, EffectOutcome,
    EffectReconciler, MemoryApprovalStore, NewRequest, Reconciled, Verified, VerifiedPrincipal,
    VerifyError, VerifyInput,
};

// ---- fakes ---------------------------------------------------------------------------------

struct Manual(AtomicU64);

impl Manual {
    fn at(ms: u64) -> Arc<Self> {
        Arc::new(Manual(AtomicU64::new(ms)))
    }

    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl ApprovalClock for Manual {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn binding(record: &ApprovalRecord, decision: Decision) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        record.id,
        record.action,
        record.effect_digest,
        record.account.as_deref().unwrap_or("-"),
        record.client.as_deref().unwrap_or("-"),
        record.revision,
        decision.as_str()
    )
}

fn proof(principal: &str, nonce: &str, record: &ApprovalRecord, decision: Decision) -> Vec<u8> {
    format!("{principal};{nonce};{}", binding(record, decision)).into_bytes()
}

struct FakeVerifier {
    principals: BTreeMap<String, AccountScope>,
}

impl ApprovalVerifier for FakeVerifier {
    fn verify(&self, input: &VerifyInput<'_>) -> Result<Verified, VerifyError> {
        let text = std::str::from_utf8(input.proof).map_err(|_| VerifyError::new("not text"))?;
        let mut parts = text.splitn(3, ';');
        let (principal, nonce, bound) = match (parts.next(), parts.next(), parts.next()) {
            (Some(p), Some(n), Some(b)) => (p, n, b),
            _ => return Err(VerifyError::new("malformed proof")),
        };
        if bound != binding(input.record, input.decision) {
            return Err(VerifyError::new("proof is bound to another request"));
        }
        let scope = self
            .principals
            .get(principal)
            .ok_or_else(|| VerifyError::new("unknown principal"))?
            .clone();
        Ok(Verified::new(VerifiedPrincipal::new(principal, scope))
            .with_nonce(nonce, input.now_ms + 120_000))
    }
}

#[derive(Default)]
struct Rows {
    rows: Mutex<Vec<AuditEvent>>,
    fail_on: Mutex<Option<AuditKind>>,
}

impl Rows {
    fn kinds(&self) -> Vec<AuditKind> {
        self.rows.lock().unwrap().iter().map(|e| e.kind).collect()
    }

    fn fail_on(&self, kind: Option<AuditKind>) {
        *self.fail_on.lock().unwrap() = kind;
    }
}

impl AuditSink for Rows {
    fn append(&self, event: &AuditEvent) -> Result<(), AuditError> {
        if *self.fail_on.lock().unwrap() == Some(event.kind) {
            return Err(AuditError::new("audit store is locked"));
        }
        self.rows.lock().unwrap().push(event.clone());
        Ok(())
    }
}

struct Reconcile {
    answer: Reconciled,
    asked: AtomicU64,
}

impl EffectReconciler for Reconcile {
    fn reconcile(&self, _record: &ApprovalRecord) -> Reconciled {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer
    }
}

fn digest(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn profile() -> ApprovalProfile {
    ApprovalProfile::new()
        .action(
            "send",
            ActionProfile::new(60_000)
                .audit(AuditRequirement::BeforeEffect)
                .requires_account(true),
        )
        .action("delete", ActionProfile::new(60_000))
        .action(
            "unlock",
            ActionProfile::new(60_000).max_ttl_ms(120_000).requires_client(true),
        )
        .action(
            "idempotent",
            ActionProfile::new(60_000)
                .audit(AuditRequirement::BeforeEffect)
                .reconcilable(true),
        )
}

fn verifier() -> Arc<FakeVerifier> {
    let only = |a: &str| AccountScope::Only(BTreeSet::from([a.to_string()]));
    Arc::new(FakeVerifier {
        principals: BTreeMap::from([
            ("owner".to_string(), AccountScope::Any),
            ("alice".to_string(), only("acct-a")),
            ("bob".to_string(), only("acct-b")),
        ]),
    })
}

struct Rig {
    coordinator: ApprovalCoordinator,
    store: MemoryApprovalStore,
    clock: Arc<Manual>,
    rows: Arc<Rows>,
}

fn rig() -> Rig {
    let store = MemoryApprovalStore::new();
    let clock = Manual::at(1_000_000);
    let rows = Arc::new(Rows::default());
    let coordinator = ApprovalCoordinator::new(profile(), Arc::new(store.clone()), verifier())
        .unwrap()
        .with_audit(rows.clone())
        .with_clock(clock.clone());
    Rig {
        coordinator,
        store,
        clock,
        rows,
    }
}

fn send(rig: &Rig, n: u8, account: &str) -> ApprovalRecord {
    rig.coordinator
        .request(NewRequest::new("send", digest(n), "agent-1").account(account))
        .unwrap()
}

fn approve(rig: &Rig, record: &ApprovalRecord, who: &str, nonce: &str) -> ApprovalRecord {
    rig.coordinator
        .decide(
            &record.id,
            Decision::Approve,
            record.revision,
            &proof(who, nonce, record, Decision::Approve),
        )
        .unwrap()
}

// ---- the happy path ------------------------------------------------------------------------

#[test]
fn a_request_is_approved_claimed_once_and_settled_with_every_row_audited() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    assert_eq!(pending.state, ApprovalState::Pending);
    assert_eq!(pending.revision, 1);
    let approved = approve(&rig, &pending, "alice", "n1");
    assert_eq!(approved.state, ApprovalState::Approved);
    assert_eq!(approved.revision, 2);
    assert_eq!(approved.decided_by.as_deref(), Some("alice"));

    let token = rig.coordinator.claim(&approved.id).unwrap();
    assert_eq!(token.effect_digest, digest(1));
    assert_eq!(token.account.as_deref(), Some("acct-a"));
    let settled = rig
        .coordinator
        .settle(&token, EffectOutcome::Succeeded, "sent=1")
        .unwrap();
    assert_eq!(settled.state, ApprovalState::Settled);
    assert_eq!(settled.outcome, Some(EffectOutcome::Succeeded));

    assert_eq!(
        rig.rows.kinds(),
        [
            AuditKind::Requested,
            AuditKind::Approved,
            AuditKind::Claimed,
            AuditKind::Succeeded
        ]
    );
    let rows = rig.rows.rows.lock().unwrap();
    assert!(rows[1].line().contains(&format!("approval={}", pending.id)));
    assert!(rows[1].line().contains("actor=alice"));
}

// ---- binding, replay, scope ----------------------------------------------------------------

#[test]
fn a_proof_for_one_request_cannot_answer_another_action_account_or_revision() {
    let rig = rig();
    let first = send(&rig, 1, "acct-a");
    let other_account = send(&rig, 1, "acct-b");
    let other_action = rig
        .coordinator
        .request(NewRequest::new("delete", digest(1), "agent-1").account("acct-a"))
        .unwrap();
    let proof_for_first = proof("owner", "n1", &first, Decision::Approve);

    // Same digest, other account.
    let err = rig
        .coordinator
        .decide(&other_account.id, Decision::Approve, 1, &proof_for_first)
        .unwrap_err();
    assert!(matches!(err, ApprovalError::Unverified(_)), "{err}");
    // Same account and digest, other action.
    let err = rig
        .coordinator
        .decide(&other_action.id, Decision::Approve, 1, &proof_for_first)
        .unwrap_err();
    assert!(matches!(err, ApprovalError::Unverified(_)), "{err}");
    // A proof for approving does not deny.
    let err = rig
        .coordinator
        .decide(&first.id, Decision::Deny, 1, &proof_for_first)
        .unwrap_err();
    assert!(matches!(err, ApprovalError::Unverified(_)), "{err}");
    // A decision for a revision the approver did not see.
    let err = rig
        .coordinator
        .decide(&first.id, Decision::Approve, 7, &proof_for_first)
        .unwrap_err();
    assert!(
        matches!(err, ApprovalError::StaleRevision { expected: 7, found: 1 }),
        "{err}"
    );
    // None of that changed anything.
    for r in [&first, &other_account, &other_action] {
        assert_eq!(
            rig.coordinator.get(&r.id).unwrap().unwrap().state,
            ApprovalState::Pending
        );
    }
    // The right proof still works.
    assert_eq!(
        rig.coordinator
            .decide(&first.id, Decision::Approve, 1, &proof_for_first)
            .unwrap()
            .state,
        ApprovalState::Approved
    );
}

#[test]
fn a_nonce_is_accepted_once_across_requests() {
    let rig = rig();
    let first = send(&rig, 1, "acct-a");
    let second = send(&rig, 2, "acct-a");
    approve(&rig, &first, "alice", "same-nonce");
    let err = rig
        .coordinator
        .decide(
            &second.id,
            Decision::Approve,
            1,
            &proof("alice", "same-nonce", &second, Decision::Approve),
        )
        .unwrap_err();
    assert!(matches!(err, ApprovalError::ReplayedNonce), "{err}");
    assert_eq!(
        rig.coordinator.get(&second.id).unwrap().unwrap().state,
        ApprovalState::Pending
    );
}

#[test]
fn a_principal_decides_only_for_its_accounts_and_only_when_named() {
    let rig = rig();
    let for_a = send(&rig, 1, "acct-a");
    let err = rig
        .coordinator
        .decide(
            &for_a.id,
            Decision::Approve,
            1,
            &proof("bob", "n1", &for_a, Decision::Approve),
        )
        .unwrap_err();
    assert!(matches!(err, ApprovalError::ScopeDenied), "{err}");

    // An account-restricted principal never decides an account-less request.
    let no_account = rig
        .coordinator
        .request(NewRequest::new("delete", digest(2), "agent-1"))
        .unwrap();
    let err = rig
        .coordinator
        .decide(
            &no_account.id,
            Decision::Approve,
            1,
            &proof("alice", "n2", &no_account, Decision::Approve),
        )
        .unwrap_err();
    assert!(matches!(err, ApprovalError::ScopeDenied), "{err}");

    // A request that names its approvers refuses everyone else, even a valid in-scope principal.
    let named = rig
        .coordinator
        .request(
            NewRequest::new("send", digest(3), "agent-1")
                .account("acct-a")
                .approvers(["owner"]),
        )
        .unwrap();
    let err = rig
        .coordinator
        .decide(
            &named.id,
            Decision::Approve,
            1,
            &proof("alice", "n3", &named, Decision::Approve),
        )
        .unwrap_err();
    assert!(matches!(err, ApprovalError::NotAnApprover), "{err}");
    approve(&rig, &named, "owner", "n4");
}

// ---- expiry, revocation, cancellation ------------------------------------------------------

#[test]
fn an_expired_request_cannot_be_decided_or_claimed() {
    let rig = rig();
    let pending = rig
        .coordinator
        .request(NewRequest::new("send", digest(1), "agent-1").account("acct-a").ttl_ms(1_000))
        .unwrap();
    rig.clock.advance(1_000);
    let err = rig
        .coordinator
        .decide(
            &pending.id,
            Decision::Approve,
            1,
            &proof("alice", "n1", &pending, Decision::Approve),
        )
        .unwrap_err();
    assert!(matches!(err, ApprovalError::Expired(_)), "{err}");
    assert_eq!(
        rig.coordinator.get(&pending.id).unwrap().unwrap().state,
        ApprovalState::Expired
    );

    // Approved, then expired before the claim.
    let second = rig
        .coordinator
        .request(NewRequest::new("send", digest(2), "agent-1").account("acct-a").ttl_ms(5_000))
        .unwrap();
    let approved = approve(&rig, &second, "alice", "n2");
    rig.clock.advance(5_000);
    let err = rig.coordinator.claim(&approved.id).unwrap_err();
    assert!(matches!(err, ApprovalError::Expired(_)), "{err}");

    // A sweep expires what nobody touched.
    let third = send(&rig, 3, "acct-a");
    rig.clock.advance(60_000);
    assert_eq!(rig.coordinator.sweep_expired().unwrap(), 1);
    assert_eq!(
        rig.coordinator.get(&third.id).unwrap().unwrap().state,
        ApprovalState::Expired
    );
}

#[test]
fn revocation_before_the_claim_ends_the_approval() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    let approved = approve(&rig, &pending, "alice", "n1");
    let revoked = rig
        .coordinator
        .decide(
            &approved.id,
            Decision::Revoke,
            approved.revision,
            &proof("alice", "n2", &approved, Decision::Revoke),
        )
        .unwrap();
    assert_eq!(revoked.state, ApprovalState::Revoked);
    let err = rig.coordinator.claim(&approved.id).unwrap_err();
    assert!(
        matches!(err, ApprovalError::InvalidState { state: ApprovalState::Revoked, .. }),
        "{err}"
    );
}

#[test]
fn removing_an_account_revokes_its_open_requests_but_not_a_claim_in_flight() {
    let rig = rig();
    let a1 = send(&rig, 1, "acct-a");
    let a2 = send(&rig, 2, "acct-a");
    let other = send(&rig, 3, "acct-b");
    let claimed = send(&rig, 4, "acct-a");
    let approved = approve(&rig, &a1, "alice", "n1");
    let in_flight = approve(&rig, &claimed, "alice", "n2");
    let token = rig.coordinator.claim(&in_flight.id).unwrap();

    assert_eq!(rig.coordinator.revoke_account("acct-a", "account removed").unwrap(), 2);
    for id in [&approved.id, &a2.id] {
        assert_eq!(
            rig.coordinator.get(id).unwrap().unwrap().state,
            ApprovalState::Revoked
        );
    }
    assert_eq!(
        rig.coordinator.get(&other.id).unwrap().unwrap().state,
        ApprovalState::Pending
    );
    assert!(matches!(
        rig.coordinator.claim(&approved.id).unwrap_err(),
        ApprovalError::InvalidState { .. }
    ));
    // The claim in flight is left alone and can still settle.
    rig.coordinator
        .settle(&token, EffectOutcome::Succeeded, "")
        .unwrap();
}

#[test]
fn only_the_requester_cancels_and_a_cancelled_request_cannot_be_claimed() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    assert!(matches!(
        rig.coordinator.cancel(&pending.id, "someone-else").unwrap_err(),
        ApprovalError::NotRequester
    ));
    let approved = approve(&rig, &pending, "alice", "n1");
    let cancelled = rig.coordinator.cancel(&approved.id, "agent-1").unwrap();
    assert_eq!(cancelled.state, ApprovalState::Cancelled);
    assert!(matches!(
        rig.coordinator.claim(&approved.id).unwrap_err(),
        ApprovalError::InvalidState { .. }
    ));
}

#[test]
fn a_pending_request_cannot_be_claimed_and_a_denied_one_never_can() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    assert!(matches!(
        rig.coordinator.claim(&pending.id).unwrap_err(),
        ApprovalError::NotApproved(_)
    ));
    let denied = rig
        .coordinator
        .decide(
            &pending.id,
            Decision::Deny,
            1,
            &proof("alice", "n1", &pending, Decision::Deny),
        )
        .unwrap();
    assert_eq!(denied.state, ApprovalState::Denied);
    assert!(rig.coordinator.claim(&pending.id).is_err());
}

// ---- one execution claim -------------------------------------------------------------------

#[test]
fn exactly_one_of_many_concurrent_claimants_gets_the_token() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    let approved = approve(&rig, &pending, "alice", "n1");
    let wins = AtomicU64::new(0);
    let refusals = Mutex::new(Vec::new());
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                barrier.wait();
                match rig.coordinator.claim(&approved.id) {
                    Ok(_) => {
                        wins.fetch_add(1, Ordering::SeqCst);
                    }
                    Err(e) => refusals.lock().unwrap().push(e),
                }
            });
        }
    });
    assert_eq!(wins.load(Ordering::SeqCst), 1);
    let refusals = refusals.into_inner().unwrap();
    assert_eq!(refusals.len(), 7);
    assert!(
        refusals
            .iter()
            .all(|e| matches!(e, ApprovalError::AlreadyClaimed(_) | ApprovalError::Conflict(_))),
        "{refusals:?}"
    );
    // And never again.
    assert!(matches!(
        rig.coordinator.claim(&approved.id).unwrap_err(),
        ApprovalError::AlreadyClaimed(_)
    ));
}

#[test]
fn a_token_settles_once_and_only_the_token_on_record() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    let approved = approve(&rig, &pending, "alice", "n1");
    let token = rig.coordinator.claim(&approved.id).unwrap();
    rig.coordinator
        .settle(&token, EffectOutcome::Failed, "send failed")
        .unwrap();
    // A second settle of the same token, from any thread, is refused.
    assert!(matches!(
        rig.coordinator
            .settle(&token, EffectOutcome::Succeeded, "")
            .unwrap_err(),
        ApprovalError::StaleClaim(_)
    ));
    assert_eq!(
        rig.coordinator.get(&approved.id).unwrap().unwrap().outcome,
        Some(EffectOutcome::Failed)
    );
}

// ---- audit before effect -------------------------------------------------------------------

#[test]
fn a_failed_claim_row_means_no_token_and_the_approval_is_still_usable() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    let approved = approve(&rig, &pending, "alice", "n1");
    rig.rows.fail_on(Some(AuditKind::Claimed));
    let err = rig.coordinator.claim(&approved.id).unwrap_err();
    assert!(matches!(err, ApprovalError::AuditFailed(_)), "{err}");
    let after = rig.coordinator.get(&approved.id).unwrap().unwrap();
    assert_eq!(after.state, ApprovalState::Approved);
    assert!(after.claim_id.is_none());

    rig.rows.fail_on(None);
    let token = rig.coordinator.claim(&approved.id).expect("claim after audit recovers");
    rig.coordinator
        .settle(&token, EffectOutcome::Succeeded, "")
        .unwrap();
}

#[test]
fn a_failed_outcome_row_blocks_success_until_it_is_written() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    let approved = approve(&rig, &pending, "alice", "n1");
    let token = rig.coordinator.claim(&approved.id).unwrap();
    rig.rows.fail_on(Some(AuditKind::Succeeded));
    let err = rig
        .coordinator
        .settle(&token, EffectOutcome::Succeeded, "sent")
        .unwrap_err();
    assert!(matches!(err, ApprovalError::AuditFailed(_)), "{err}");
    assert_eq!(
        rig.coordinator.get(&approved.id).unwrap().unwrap().state,
        ApprovalState::Claimed,
        "success is not recorded without its audit row"
    );
    rig.rows.fail_on(None);
    let settled = rig
        .coordinator
        .settle(&token, EffectOutcome::Succeeded, "sent")
        .unwrap();
    assert_eq!(settled.state, ApprovalState::Settled);
}

#[test]
fn an_action_that_needs_audit_before_effect_refuses_a_coordinator_without_a_sink() {
    let coordinator = ApprovalCoordinator::new(
        profile(),
        Arc::new(MemoryApprovalStore::new()),
        verifier(),
    )
    .unwrap();
    let err = coordinator
        .request(NewRequest::new("send", digest(1), "agent-1").account("acct-a"))
        .unwrap_err();
    assert!(matches!(err, ApprovalError::AuditRequired), "{err}");
    // A best-effort action needs none.
    assert!(coordinator
        .request(NewRequest::new("delete", digest(1), "agent-1"))
        .is_ok());
}

#[test]
fn best_effort_audit_failures_are_counted_and_change_nothing() {
    let rig = rig();
    rig.rows.fail_on(Some(AuditKind::Requested));
    let record = rig
        .coordinator
        .request(NewRequest::new("delete", digest(1), "agent-1"))
        .unwrap();
    assert_eq!(rig.coordinator.audit_failures(), 1);
    assert_eq!(record.state, ApprovalState::Pending);
}

// ---- uncertain effects and crashes ---------------------------------------------------------

#[test]
fn an_effect_that_reports_uncertainty_settles_as_outcome_unknown() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    let approved = approve(&rig, &pending, "alice", "n1");
    let token = rig.coordinator.claim(&approved.id).unwrap();
    let settled = rig
        .coordinator
        .settle(&token, EffectOutcome::OutcomeUnknown, "timeout after submit")
        .unwrap();
    assert_eq!(settled.outcome, Some(EffectOutcome::OutcomeUnknown));
    assert_eq!(rig.rows.kinds().last(), Some(&AuditKind::OutcomeUnknown));
}

#[test]
fn a_crash_after_the_claim_is_outcome_unknown_and_never_a_second_execution() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    let approved = approve(&rig, &pending, "alice", "n1");
    let old_token = rig.coordinator.claim(&approved.id).unwrap();

    // The process dies after the claim and before settle. A new coordinator over the durable
    // store starts up and resolves what it finds.
    let restarted = ApprovalCoordinator::new(profile(), Arc::new(rig.store.clone()), verifier())
        .unwrap()
        .with_audit(rig.rows.clone())
        .with_clock(rig.clock.clone());
    let resolved = restarted.recover_claims(ClaimRecovery::All, None).unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].state, ApprovalState::Settled);
    assert_eq!(resolved[0].outcome, Some(EffectOutcome::OutcomeUnknown));
    // The claim is spent: no new token, and the dead handler's token is stale.
    assert!(matches!(
        restarted.claim(&approved.id).unwrap_err(),
        ApprovalError::AlreadyClaimed(_)
    ));
    assert!(matches!(
        restarted
            .settle(&old_token, EffectOutcome::Succeeded, "")
            .unwrap_err(),
        ApprovalError::StaleClaim(_)
    ));
    assert_eq!(rig.rows.kinds().last(), Some(&AuditKind::OutcomeUnknown));
}

#[test]
fn only_claims_old_enough_are_recovered_at_runtime() {
    let rig = rig();
    let pending = send(&rig, 1, "acct-a");
    let approved = approve(&rig, &pending, "alice", "n1");
    let token = rig.coordinator.claim(&approved.id).unwrap();
    rig.clock.advance(10);
    assert!(rig
        .coordinator
        .recover_claims(ClaimRecovery::OlderThan(100), None)
        .unwrap()
        .is_empty());
    rig.clock.advance(200);
    assert_eq!(
        rig.coordinator
            .recover_claims(ClaimRecovery::OlderThan(100), None)
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        rig.coordinator
            .settle(&token, EffectOutcome::Succeeded, "")
            .unwrap_err(),
        ApprovalError::StaleClaim(_)
    ));
}

#[test]
fn a_reconcilable_effect_is_asked_and_other_actions_never_are() {
    for (answer, outcome) in [
        (Reconciled::Applied, EffectOutcome::Succeeded),
        (Reconciled::NotApplied, EffectOutcome::Failed),
        (Reconciled::Unknown, EffectOutcome::OutcomeUnknown),
    ] {
        let rig = rig();
        let idem = rig
            .coordinator
            .request(NewRequest::new("idempotent", digest(1), "agent-1"))
            .unwrap();
        let approved = approve(&rig, &idem, "owner", "n1");
        rig.coordinator.claim(&approved.id).unwrap();
        // A non-reconcilable claim alongside it.
        let plain = send(&rig, 2, "acct-a");
        let plain_approved = approve(&rig, &plain, "alice", "n2");
        rig.coordinator.claim(&plain_approved.id).unwrap();

        let reconciler = Reconcile {
            answer,
            asked: AtomicU64::new(0),
        };
        let resolved = rig
            .coordinator
            .recover_claims(ClaimRecovery::All, Some(&reconciler))
            .unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(reconciler.asked.load(Ordering::SeqCst), 1);
        let by_id = |id: &str| resolved.iter().find(|r| r.id == id).unwrap().outcome;
        assert_eq!(by_id(&approved.id), Some(outcome));
        assert_eq!(
            by_id(&plain_approved.id),
            Some(EffectOutcome::OutcomeUnknown),
            "an effect that cannot reconcile is never guessed at"
        );
    }
}

// ---- request validation, redaction, profile as data ----------------------------------------

#[test]
fn recovery_cannot_report_reconciled_success_without_required_audit() {
    let rig = rig();
    let pending = rig.coordinator
        .request(NewRequest::new("idempotent", digest(1), "agent-1"))
        .unwrap();
    let approved = approve(&rig, &pending, "owner", "recovery-audit");
    rig.coordinator.claim(&approved.id).unwrap();
    let reconciler = Reconcile { answer: Reconciled::Applied, asked: AtomicU64::new(0) };
    rig.rows.fail_on(Some(AuditKind::Succeeded));
    assert!(matches!(
        rig.coordinator.recover_claims(ClaimRecovery::All, Some(&reconciler)),
        Err(ApprovalError::AuditFailed(_))
    ));
    assert_eq!(rig.coordinator.get(&pending.id).unwrap().unwrap().state, ApprovalState::Claimed);
    rig.rows.fail_on(None);
    let recovered = rig.coordinator.recover_claims(ClaimRecovery::All, Some(&reconciler)).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].outcome, Some(EffectOutcome::Succeeded));
    assert!(rig.rows.kinds().contains(&AuditKind::Succeeded));
    assert!(rig.coordinator.claim(&pending.id).is_err());
}

#[test]
fn requests_are_validated_against_the_declared_profile() {
    let rig = rig();
    let c = &rig.coordinator;
    let bad = |new: NewRequest| c.request(new).unwrap_err();
    assert!(matches!(
        bad(NewRequest::new("launch-missiles", digest(1), "a")),
        ApprovalError::UnknownAction(_)
    ));
    assert!(matches!(
        bad(NewRequest::new("delete", "NOTHEX", "a")),
        ApprovalError::Invalid(_)
    ));
    assert!(matches!(
        bad(NewRequest::new("delete", digest(0xab).to_uppercase(), "a")),
        ApprovalError::Invalid(_)
    ));
    assert!(matches!(
        bad(NewRequest::new("send", digest(1), "a")),
        ApprovalError::Invalid(_)
    ));
    assert!(matches!(
        bad(NewRequest::new("unlock", digest(1), "a")),
        ApprovalError::Invalid(_)
    ));
    assert!(matches!(
        bad(NewRequest::new("delete", digest(1), "a").ttl_ms(60_001)),
        ApprovalError::Invalid(_)
    ));
    assert!(matches!(
        bad(NewRequest::new("delete", digest(1), "a").ttl_ms(0)),
        ApprovalError::Invalid(_)
    ));
    assert!(matches!(
        bad(NewRequest::new("delete", digest(1), "a").id("has space")),
        ApprovalError::Invalid(_)
    ));
    let ok = c
        .request(NewRequest::new("delete", digest(1), "a").id("req-1"))
        .unwrap();
    assert_eq!(ok.id, "req-1");
    assert!(matches!(
        bad(NewRequest::new("delete", digest(2), "a").id("req-1")),
        ApprovalError::Duplicate(_)
    ));
    // A client-bound action with its client is fine.
    assert!(c
        .request(NewRequest::new("unlock", digest(1), "a").client("chat:1"))
        .is_ok());
}

#[test]
fn open_requests_are_bounded() {
    let store = MemoryApprovalStore::new();
    let coordinator = ApprovalCoordinator::new(
        ApprovalProfile::new()
            .action("delete", ActionProfile::new(60_000))
            .max_open(2),
        Arc::new(store),
        verifier(),
    )
    .unwrap();
    for n in 0..2 {
        coordinator
            .request(NewRequest::new("delete", digest(n), "a"))
            .unwrap();
    }
    assert!(matches!(
        coordinator
            .request(NewRequest::new("delete", digest(9), "a"))
            .unwrap_err(),
        ApprovalError::TooManyOpen(2)
    ));
}

#[test]
fn free_text_never_reaches_the_record_or_the_audit_rows() {
    let rig = rig();
    let record = rig
        .coordinator
        .request(
            NewRequest::new("delete", digest(1), "agent-1")
                .detail("to=bob@example.com subject=Quarterly numbers"),
        )
        .unwrap();
    assert_eq!(record.detail, "detail=withheld");
    assert_eq!(redact_detail("count=3 reason_code=moved"), "count=3 reason_code=moved");
    assert_eq!(redact_detail("secret: hunter2"), "detail=withheld");
    assert_eq!(redact_detail(&"a".repeat(500)), "detail=withheld");
    assert!(rig
        .rows
        .rows
        .lock()
        .unwrap()
        .iter()
        .all(|e| !e.line().contains("example.com")));
}

#[test]
fn the_profile_is_data_and_a_bad_one_is_refused() {
    let p = profile();
    let text = serde_json::to_string(&p).unwrap();
    let back: ApprovalProfile = serde_json::from_str(&text).unwrap();
    assert_eq!(back, p);
    assert!(ApprovalCoordinator::new(
        back,
        Arc::new(MemoryApprovalStore::new()),
        verifier()
    )
    .is_ok());

    assert!(ApprovalProfile::new()
        .try_action("x", ActionProfile::new(0))
        .is_err());
    assert!(ApprovalProfile::new()
        .try_action("x", ActionProfile::new(10).max_ttl_ms(5))
        .is_err());
    assert!(ApprovalProfile::new()
        .try_action("has space", ActionProfile::new(10))
        .is_err());
    let zero_open: ApprovalProfile =
        serde_json::from_str(r#"{"actions":{},"max_open":0}"#).unwrap();
    assert!(ApprovalCoordinator::new(
        zero_open,
        Arc::new(MemoryApprovalStore::new()),
        verifier()
    )
    .is_err());
}

#[test]
fn records_round_trip_through_json_for_database_backed_stores() {
    let rig = rig();
    let record = send(&rig, 1, "acct-a");
    let text = serde_json::to_string(&record).unwrap();
    let back: ApprovalRecord = serde_json::from_str(&text).unwrap();
    assert_eq!(back, record);
}

#[test]
fn a_failed_decision_audit_keeps_the_exact_pending_revision() {
    let rig = rig();
    let pending = send(&rig, 31, "acct-a");
    rig.rows.fail_on(Some(AuditKind::Approved));
    let error = rig.coordinator.decide(
        &pending.id, Decision::Approve, pending.revision,
        &proof("alice", "audit-failed", &pending, Decision::Approve),
    ).unwrap_err();
    assert!(matches!(error, ApprovalError::AuditFailed(_)));
    let after = rig.coordinator.get(&pending.id).unwrap().unwrap();
    assert_eq!(after.state, ApprovalState::Pending);
    assert_eq!(after.revision, pending.revision);
    assert!(after.decided_by.is_none());
    // Verified nonces are single-use even when the subsequent audit append fails.
    rig.rows.fail_on(None);
    assert_eq!(approve(&rig, &after, "alice", "audit-retry").state, ApprovalState::Approved);
}

#[test]
fn a_failed_request_audit_does_not_insert_a_pending_request() {
    let rig = rig();
    rig.rows.fail_on(Some(AuditKind::Requested));
    let error = rig.coordinator.request(
        NewRequest::new("send", digest(32), "agent-1").account("acct-a").id("audit-refused")
    ).unwrap_err();
    assert!(matches!(error, ApprovalError::AuditFailed(_)));
    assert!(rig.coordinator.get("audit-refused").unwrap().is_none());
}
