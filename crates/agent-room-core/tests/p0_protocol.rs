use agent_room_core::protocol::{
    DispositionAction, EventKind, FindingInput, FindingStatus, Phase, ReceiptKind, ReceiptRef,
    ResolutionChoice, Room, RoomConfig, ScopeRef, SeatBudget, SeatConfig, TimeoutOutcome,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn review_room_in(workspace: &TempDir) -> Room {
    Room::new(RoomConfig {
        room_id: "room-test".into(),
        mode: "review".into(),
        at_budget: "abstain".into(),
        workspace_root: workspace.path().to_path_buf(),
        seats: vec![
            SeatConfig::new("claude", "Claude", "claude", "reviewer"),
            SeatConfig::new("codex", "Codex", "codex", "implementer"),
            SeatConfig::new("adrian", "Adrian", "human", "human"),
        ],
    })
    .expect("valid room")
}

fn review_room() -> (TempDir, Room) {
    let workspace = tempfile::tempdir().unwrap();
    let room = review_room_in(&workspace);
    (workspace, room)
}

fn finding(claim: &str) -> FindingInput {
    FindingInput {
        claim: claim.into(),
        severity: "P1".into(),
        rationale: "This can fail".into(),
        evidence_refs: vec![],
        proposed_change: Some("Add a check".into()),
        confidence: Some(0.9),
    }
}

#[test]
fn positions_are_private_until_atomic_peer_debate_reveal_including_replay() {
    let (_workspace, mut room) = review_room();
    room.advance("adrian", Phase::Positions).unwrap();
    room.post(
        "claude",
        "req-position-1",
        EventKind::Position,
        "The rollback evidence is incomplete",
        None,
    )
    .unwrap();
    assert!(room
        .visible_events("claude", 0)
        .unwrap()
        .iter()
        .any(|e| e.body.contains("rollback")));
    assert!(!room
        .visible_events("codex", 0)
        .unwrap()
        .iter()
        .any(|e| e.body.contains("rollback")));
    assert!(!room
        .visible_events("codex", 0)
        .unwrap()
        .iter()
        .any(|e| e.body.contains("rollback")));
    room.advance("adrian", Phase::PeerDebate).unwrap();
    assert!(room
        .visible_events("codex", 0)
        .unwrap()
        .iter()
        .any(|e| e.body.contains("rollback")));
}

#[test]
fn duplicate_request_applies_once_and_changed_payload_rejects() {
    let (_workspace, mut room) = review_room();
    room.advance("adrian", Phase::Positions).unwrap();
    let first = room
        .post("claude", "same", EventKind::Position, "Position A", None)
        .unwrap();
    let replay = room
        .post("claude", "same", EventKind::Position, "Position A", None)
        .unwrap();
    assert_eq!(first.seq, replay.seq);
    assert_eq!(room.events.len(), 1);
    assert_eq!(
        room.post("claude", "same", EventKind::Position, "Position B", None)
            .unwrap_err()
            .code,
        "request_id_conflict"
    );
}

#[test]
fn folded_and_refuted_findings_require_receipts_and_author_acceptance() {
    let (workspace, mut room) = review_room();
    std::fs::write(workspace.path().join("evidence.txt"), "measured true\n").unwrap();
    std::fs::write(workspace.path().join("revision.md"), "revised plan\n").unwrap();
    let revision_sha = format!("{:x}", Sha256::digest(b"revised plan\n"));
    room.advance("adrian", Phase::Positions).unwrap();
    room.advance("adrian", Phase::PeerDebate).unwrap();
    room.advance("adrian", Phase::Dispositions).unwrap();

    let fold = room
        .raise("claude", "raise-fold", finding("Missing rollback"))
        .unwrap();
    let refute = room
        .raise("claude", "raise-refute", finding("No measurement"))
        .unwrap();
    let fold_exit = room
        .dispose(
            "codex",
            "dispose-fold",
            &fold.finding_id,
            DispositionAction::Folded,
            None,
            vec![ReceiptRef::artifact("revision.md", &revision_sha)],
        )
        .unwrap();
    let refute_exit = room
        .dispose(
            "codex",
            "dispose-refute",
            &refute.finding_id,
            DispositionAction::Refuted,
            None,
            vec![ReceiptRef::file_line("evidence.txt", 1)],
        )
        .unwrap();
    assert_eq!(
        room.finding(&fold.finding_id).unwrap().status,
        FindingStatus::ExitProposed
    );
    room.advance("adrian", Phase::AuthorReview).unwrap();
    room.resolve(
        "claude",
        "resolve-fold",
        &fold_exit.disposition_id,
        ResolutionChoice::Accept,
        "Revision closes it",
        vec![],
    )
    .unwrap();
    assert_eq!(
        room.finding(&fold.finding_id).unwrap().status,
        FindingStatus::Folded
    );
    room.resolve(
        "claude",
        "resolve-refute",
        &refute_exit.disposition_id,
        ResolutionChoice::Accept,
        "Evidence answers it",
        vec![],
    )
    .unwrap();
    assert_eq!(
        room.finding(&refute.finding_id).unwrap().status,
        FindingStatus::Refuted
    );
    room.advance("adrian", Phase::Voting).unwrap();
}

#[test]
fn recontest_reaches_human_ruling_and_open_finding_blocks_verdict() {
    let (workspace, mut room) = review_room();
    std::fs::write(workspace.path().join("evidence.txt"), "weak evidence\n").unwrap();
    std::fs::write(workspace.path().join("revision.md"), "stronger artifact\n").unwrap();
    let revision_sha = format!("{:x}", Sha256::digest(b"stronger artifact\n"));
    room.advance("adrian", Phase::Positions).unwrap();
    room.advance("adrian", Phase::PeerDebate).unwrap();
    room.advance("adrian", Phase::Dispositions).unwrap();
    let item = room
        .raise("claude", "raise", finding("Evidence is weak"))
        .unwrap();
    assert_eq!(
        room.advance("adrian", Phase::AuthorReview)
            .unwrap_err()
            .code,
        "open_findings"
    );
    let exit = room
        .dispose(
            "codex",
            "dispose",
            &item.finding_id,
            DispositionAction::Refuted,
            None,
            vec![ReceiptRef::file_line("evidence.txt", 1)],
        )
        .unwrap();
    room.advance("adrian", Phase::AuthorReview).unwrap();
    room.resolve(
        "claude",
        "recontest",
        &exit.disposition_id,
        ResolutionChoice::Recontest,
        "The line does not prove the claim",
        vec![],
    )
    .unwrap();
    room.advance("adrian", Phase::Escalation).unwrap();
    room.rule(
        "adrian",
        "rule",
        &exit.disposition_id,
        DispositionAction::Folded,
        "Require a stronger artifact",
        vec![ReceiptRef::artifact("revision.md", &revision_sha)],
    )
    .unwrap();
    assert_eq!(
        room.finding(&item.finding_id).unwrap().status,
        FindingStatus::Folded
    );
    room.advance("adrian", Phase::Voting).unwrap();
    room.advance("adrian", Phase::Verdict).unwrap();
    assert!(room.seal("adrian").is_ok());
}

#[test]
fn implementer_timeout_emits_fallback_and_unknown_motion_rejects() {
    let (_workspace, mut room) = review_room();
    room.advance("adrian", Phase::Positions).unwrap();
    room.advance("adrian", Phase::PeerDebate).unwrap();
    room.advance("adrian", Phase::Dispositions).unwrap();
    assert_eq!(
        room.vote("claude", "vote", "missing", "yes", "reason")
            .unwrap_err()
            .code,
        "unknown_motion"
    );
    room.implementer_timeout("adrian").unwrap();
    assert_eq!(room.phase, Phase::AbortedFallback);
    assert!(room
        .events
        .iter()
        .any(|event| event.body == "fallback_required"));
}

#[test]
fn review_scope_motion_is_digest_verified_and_out_of_scope_requires_its_receipt() {
    let (workspace, mut room) = review_room();
    std::fs::write(
        workspace.path().join("review.disposition.json"),
        b"{\"ok\":true}\n",
    )
    .unwrap();
    std::fs::write(workspace.path().join("deferred.json"), b"[\"P1\"]\n").unwrap();
    std::fs::write(
        workspace.path().join("evidence.txt"),
        b"outside pinned scope\n",
    )
    .unwrap();
    let refs = vec![
        ScopeRef::pinned(
            "review.disposition.json",
            &format!("{:x}", Sha256::digest(b"{\"ok\":true}\n")),
        ),
        ScopeRef::pinned(
            "deferred.json",
            &format!("{:x}", Sha256::digest(b"[\"P1\"]\n")),
        ),
    ];
    let digest = room.calculate_scope_digest(&refs).unwrap();
    assert_eq!(
        room.create_review_scope("adrian", "scope-bad", refs.clone(), &"0".repeat(64))
            .unwrap_err()
            .code,
        "scope_digest_mismatch"
    );
    let motion = room
        .create_review_scope("adrian", "scope-good", refs, &digest)
        .unwrap();
    assert_eq!(motion.kind, "review_scope");
    assert_eq!(motion.scope_digest.as_deref(), Some(digest.as_str()));

    room.advance("adrian", Phase::Positions).unwrap();
    room.advance("adrian", Phase::PeerDebate).unwrap();
    room.advance("adrian", Phase::Dispositions).unwrap();
    let item = room
        .raise("claude", "raise-scope", finding("Not part of this phase"))
        .unwrap();
    let fake = ReceiptRef {
        kind: ReceiptKind::ReviewScope,
        locator: "motion_fake".into(),
        line: None,
        digest: Some(digest.clone()),
    };
    assert_eq!(
        room.dispose(
            "codex",
            "scope-refute-bad",
            &item.finding_id,
            DispositionAction::Refuted,
            Some("out_of_scope".into()),
            vec![fake],
        )
        .unwrap_err()
        .code,
        "scope_receipt_invalid"
    );
    let receipt = ReceiptRef::review_scope(&motion.motion_id, &digest);
    room.dispose(
        "codex",
        "scope-refute-good",
        &item.finding_id,
        DispositionAction::Refuted,
        Some("out_of_scope".into()),
        vec![receipt],
    )
    .unwrap();
}

#[test]
fn contest_targets_one_service_finding_and_retries_are_idempotent() {
    let (_workspace, mut room) = review_room();
    room.advance("adrian", Phase::Positions).unwrap();
    room.advance("adrian", Phase::PeerDebate).unwrap();
    let item = room
        .raise("claude", "raise-contested", finding("Unsafe default"))
        .unwrap();
    assert_eq!(
        room.contest("codex", "contest-unknown", "finding_fake", "No", vec![])
            .unwrap_err()
            .code,
        "unknown_finding"
    );
    let first = room
        .contest(
            "codex",
            "contest-one",
            &item.finding_id,
            "The evidence contradicts it",
            vec!["evidence:1".into()],
        )
        .unwrap();
    let replay = room
        .contest(
            "codex",
            "contest-one",
            &item.finding_id,
            "The evidence contradicts it",
            vec!["evidence:1".into()],
        )
        .unwrap();
    assert_eq!(first.contest_id, replay.contest_id);
    assert_eq!(room.contests.len(), 1);
}

#[test]
fn seat_send_wake_and_context_budgets_are_hard_and_measured() {
    let workspace = tempfile::tempdir().unwrap();
    let budget = SeatBudget {
        max_messages: 1,
        max_send_bytes: 32,
        max_wakes: 1,
        max_context_bytes: 8,
    };
    let mut reviewer = SeatConfig::new("reviewer", "Reviewer", "model", "reviewer");
    reviewer.budget = budget.clone();
    let mut peer = SeatConfig::new("peer", "Peer", "model", "moderator");
    peer.budget = budget;
    let mut room = Room::new(RoomConfig {
        room_id: "budget-room".into(),
        mode: "debate".into(),
        at_budget: "abstain".into(),
        workspace_root: workspace.path().to_path_buf(),
        seats: vec![reviewer, peer],
    })
    .unwrap();
    room.advance("peer", Phase::Positions).unwrap();
    room.post("peer", "p1", EventKind::Position, "123456789", None)
        .unwrap();
    assert_eq!(
        room.post("peer", "p2", EventKind::Position, "again", None)
            .unwrap_err()
            .code,
        "message_budget_exhausted"
    );
    room.advance("peer", Phase::PeerDebate).unwrap();
    assert_eq!(
        room.next("reviewer", 0).unwrap_err().code,
        "seat_abstained_at_budget"
    );
    let usage = room.seat_usage("reviewer").unwrap();
    assert_eq!(usage.wakes, 1);
    assert_eq!(usage.context_bytes, 9);
    assert!(usage.abstained);
    assert_eq!(
        room.next("reviewer", 0).unwrap_err().code,
        "wake_budget_exhausted"
    );
}

#[test]
fn summarize_at_context_budget_is_explicit_not_silent() {
    let workspace = tempfile::tempdir().unwrap();
    let mut a = SeatConfig::new("a", "A", "model", "reviewer");
    a.budget.max_context_bytes = 1;
    let b = SeatConfig::new("b", "B", "model", "moderator");
    let mut room = Room::new(RoomConfig {
        room_id: "summary-room".into(),
        mode: "debate".into(),
        at_budget: "summarize-and-continue".into(),
        workspace_root: workspace.path().to_path_buf(),
        seats: vec![a, b],
    })
    .unwrap();
    room.advance("b", Phase::Positions).unwrap();
    room.post("b", "p", EventKind::Position, "peer context", None)
        .unwrap();
    room.advance("b", Phase::PeerDebate).unwrap();
    assert_eq!(
        room.next("a", 0).unwrap_err().code,
        "summary_required_at_budget"
    );
    assert!(!room.seat_usage("a").unwrap().abstained);
}

#[test]
fn author_timeout_gets_one_rewake_then_measured_escalation() {
    let (workspace, mut room) = review_room();
    std::fs::write(workspace.path().join("evidence.txt"), "evidence\n").unwrap();
    room.advance("adrian", Phase::Positions).unwrap();
    room.advance("adrian", Phase::PeerDebate).unwrap();
    room.advance("adrian", Phase::Dispositions).unwrap();
    let item = room
        .raise("claude", "timeout-finding", finding("Author may time out"))
        .unwrap();
    let exit = room
        .dispose(
            "codex",
            "timeout-disposition",
            &item.finding_id,
            DispositionAction::Refuted,
            None,
            vec![ReceiptRef::file_line("evidence.txt", 1)],
        )
        .unwrap();
    room.advance("adrian", Phase::AuthorReview).unwrap();
    assert_eq!(
        room.author_timeout("adrian", "timeout-1", &exit.disposition_id)
            .unwrap(),
        TimeoutOutcome::Rewake
    );
    assert_eq!(room.escalation_rate(), 0.0);
    assert_eq!(
        room.author_timeout("adrian", "timeout-2", &exit.disposition_id)
            .unwrap(),
        TimeoutOutcome::Escalated
    );
    assert_eq!(
        room.finding(&item.finding_id).unwrap().status,
        FindingStatus::Recontested
    );
    assert_eq!(room.escalation_rate(), 1.0);
    assert!(room.failed_by_majority_escalation());
    room.advance("adrian", Phase::Escalation).unwrap();
}

#[test]
fn phase_advancement_is_human_authorized_recorded_and_idempotent() {
    let (_workspace, mut room) = review_room();
    assert_eq!(
        room.advance_idempotent("claude", "phase-1", Phase::Positions)
            .unwrap_err()
            .code,
        "role_forbidden"
    );
    let first = room
        .advance_idempotent("adrian", "phase-1", Phase::Positions)
        .unwrap();
    let replay = room
        .advance_idempotent("adrian", "phase-1", Phase::Positions)
        .unwrap();
    assert_eq!(first.seq, replay.seq);
    assert_eq!(
        room.events
            .iter()
            .filter(|event| event.body == "phase:Positions")
            .count(),
        1
    );
    assert_eq!(
        room.advance_idempotent("adrian", "phase-1", Phase::PeerDebate)
            .unwrap_err()
            .code,
        "request_id_conflict"
    );
}

#[test]
fn unshipped_mode_and_plumbing_fields_reject() {
    let workspace = tempfile::tempdir().unwrap();
    let error = Room::new(RoomConfig {
        room_id: "future".into(),
        mode: "worksplit".into(),
        at_budget: "abstain".into(),
        workspace_root: workspace.path().to_path_buf(),
        seats: vec![SeatConfig::new("adrian", "Adrian", "human", "human")],
    })
    .unwrap_err();
    assert_eq!(error.code, "mode_unshipped");
    let raw = r#"{"claim":"x","severity":"P1","rationale":"r","evidence_refs":[],"raw_response":"secret"}"#;
    assert!(serde_json::from_str::<FindingInput>(raw).is_err());
}

#[test]
fn sealed_room_rejects_new_mutations_but_preserves_exact_replays() {
    let (_workspace, mut room) = review_room();
    let prior = room
        .post(
            "adrian",
            "pre-seal",
            EventKind::Info,
            "recorded before seal",
            None,
        )
        .unwrap();
    for phase in [
        Phase::Positions,
        Phase::PeerDebate,
        Phase::Dispositions,
        Phase::AuthorReview,
        Phase::Voting,
        Phase::Verdict,
    ] {
        room.advance("adrian", phase).unwrap();
    }
    room.seal_idempotent("adrian", "seal-room").unwrap();
    let replay = room
        .post(
            "adrian",
            "pre-seal",
            EventKind::Info,
            "recorded before seal",
            None,
        )
        .unwrap();
    assert_eq!(replay, prior);
    assert_eq!(
        room.post("adrian", "post-seal", EventKind::Info, "must fail", None)
            .unwrap_err()
            .code,
        "room_sealed"
    );
}
