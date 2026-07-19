use agent_room_core::protocol::{
    DispositionAction, EventKind, FindingInput, FindingStatus, Phase, ReceiptRef, ResolutionChoice,
    Room, RoomConfig, ScopeRef, SeatConfig,
};
use agent_room_core::store::RoomStore;
use sha2::{Digest, Sha256};

fn finding(claim: &str) -> FindingInput {
    FindingInput {
        claim: claim.into(),
        severity: "P1".into(),
        rationale: "Evidence must close this claim".into(),
        evidence_refs: vec![],
        proposed_change: Some("Make the proof explicit".into()),
        confidence: Some(0.9),
    }
}

#[test]
fn full_two_seat_round_seals_only_after_both_exits_and_records_context() {
    let root = tempfile::tempdir().unwrap();
    let run = root.path().join("run");
    std::fs::write(
        root.path().join("review.disposition.json"),
        b"{\"deferred\":[\"rollback\"]}\n",
    )
    .unwrap();
    std::fs::write(root.path().join("deferred.json"), b"[\"rollback\"]\n").unwrap();
    std::fs::write(root.path().join("evidence.txt"), b"measured: true\n").unwrap();
    std::fs::write(
        root.path().join("revision.md"),
        b"rollback gate implemented\n",
    )
    .unwrap();
    let artifact_digest = format!("{:x}", Sha256::digest(b"rollback gate implemented\n"));
    let scope_refs = vec![
        ScopeRef::pinned(
            "review.disposition.json",
            &format!("{:x}", Sha256::digest(b"{\"deferred\":[\"rollback\"]}\n")),
        ),
        ScopeRef::pinned(
            "deferred.json",
            &format!("{:x}", Sha256::digest(b"[\"rollback\"]\n")),
        ),
    ];
    let mut room = Room::new(RoomConfig {
        room_id: "full-round".into(),
        mode: "review".into(),
        at_budget: "abstain".into(),
        workspace_root: root.path().to_path_buf(),
        seats: vec![
            SeatConfig::new("reviewer", "Reviewer", "model-a", "reviewer"),
            SeatConfig::new("implementer", "Implementer", "model-b", "implementer"),
            SeatConfig::new("human", "Adrian", "human", "human"),
        ],
    })
    .unwrap();
    let scope_digest = room.calculate_scope_digest(&scope_refs).unwrap();
    let scope = room
        .create_review_scope("human", "scope", scope_refs, &scope_digest)
        .unwrap();
    room.advance_idempotent("human", "to-positions", Phase::Positions)
        .unwrap();
    room.post(
        "reviewer",
        "position-r",
        EventKind::Position,
        "Rollback proof is missing",
        None,
    )
    .unwrap();
    assert!(!room
        .visible_events("implementer", 0)
        .unwrap()
        .iter()
        .any(|event| event.body.contains("missing")));
    room.post(
        "implementer",
        "position-i",
        EventKind::Position,
        "Two claims can be proven",
        None,
    )
    .unwrap();
    room.advance_idempotent("human", "to-debate", Phase::PeerDebate)
        .unwrap();
    assert!(room
        .visible_events("implementer", 0)
        .unwrap()
        .iter()
        .any(|event| event.body.contains("missing")));
    let reviewer_cursor = room.next("reviewer", 0).unwrap().last().unwrap().seq;
    let implementer_cursor = room.next("implementer", 0).unwrap().last().unwrap().seq;

    let folded = room
        .raise("reviewer", "raise-fold", finding("Rollback gate absent"))
        .unwrap();
    let refuted = room
        .raise("reviewer", "raise-refute", finding("Measurement absent"))
        .unwrap();
    let escalated = room
        .raise(
            "reviewer",
            "raise-escalate",
            finding("Rollback artifact too weak"),
        )
        .unwrap();
    room.contest(
        "implementer",
        "contest",
        &folded.finding_id,
        "A revision can close this",
        vec![],
    )
    .unwrap();
    room.advance_idempotent("human", "to-dispositions", Phase::Dispositions)
        .unwrap();
    assert_eq!(
        room.advance_idempotent("human", "blocked", Phase::AuthorReview)
            .unwrap_err()
            .code,
        "open_findings"
    );
    let fold_exit = room
        .dispose(
            "implementer",
            "dispose-fold",
            &folded.finding_id,
            DispositionAction::Folded,
            None,
            vec![ReceiptRef::artifact("revision.md", &artifact_digest)],
        )
        .unwrap();
    let refute_exit = room
        .dispose(
            "implementer",
            "dispose-refute",
            &refuted.finding_id,
            DispositionAction::Refuted,
            None,
            vec![ReceiptRef::file_line("evidence.txt", 1)],
        )
        .unwrap();
    let escalate_exit = room
        .dispose(
            "implementer",
            "dispose-escalate",
            &escalated.finding_id,
            DispositionAction::Refuted,
            None,
            vec![ReceiptRef::file_line("evidence.txt", 1)],
        )
        .unwrap();
    room.advance_idempotent("human", "to-author", Phase::AuthorReview)
        .unwrap();
    room.resolve(
        "reviewer",
        "accept-fold",
        &fold_exit.disposition_id,
        ResolutionChoice::Accept,
        "Artifact closes it",
        vec![],
    )
    .unwrap();
    room.resolve(
        "reviewer",
        "accept-refute",
        &refute_exit.disposition_id,
        ResolutionChoice::Accept,
        "Measurement proves it",
        vec![],
    )
    .unwrap();
    room.resolve(
        "reviewer",
        "recontest",
        &escalate_exit.disposition_id,
        ResolutionChoice::Recontest,
        "That line proves a different claim",
        vec![],
    )
    .unwrap();
    room.advance_idempotent("human", "to-escalation", Phase::Escalation)
        .unwrap();
    room.rule(
        "human",
        "human-rule",
        &escalate_exit.disposition_id,
        DispositionAction::Folded,
        "Require the verified rollback artifact",
        vec![ReceiptRef::artifact("revision.md", &artifact_digest)],
    )
    .unwrap();
    assert_eq!(
        room.finding(&folded.finding_id).unwrap().status,
        FindingStatus::Folded
    );
    assert_eq!(
        room.finding(&refuted.finding_id).unwrap().status,
        FindingStatus::Refuted
    );
    assert_eq!(
        room.finding(&escalated.finding_id).unwrap().status,
        FindingStatus::Folded
    );
    room.advance_idempotent("human", "to-voting", Phase::Voting)
        .unwrap();
    room.vote(
        "reviewer",
        "vote-r",
        &scope.motion_id,
        "approve",
        "All exits have receipts",
    )
    .unwrap();
    room.vote(
        "implementer",
        "vote-i",
        &scope.motion_id,
        "approve",
        "Revision is pinned",
    )
    .unwrap();
    room.advance_idempotent("human", "to-verdict", Phase::Verdict)
        .unwrap();
    assert!(room.open_finding_ids().is_empty());
    let _ = room.next("reviewer", reviewer_cursor).unwrap();
    let _ = room.next("implementer", implementer_cursor).unwrap();
    assert!(room.seat_usage("reviewer").unwrap().context_bytes > 0);
    assert!(room.seat_usage("implementer").unwrap().context_bytes > 0);
    assert_eq!(room.escalation_rate(), 0.0);
    let seal = room.seal_idempotent("human", "seal-1").unwrap();
    let seal_replay = room.seal_idempotent("human", "seal-1").unwrap();
    assert_eq!(seal, seal_replay);
    let mut store = RoomStore::create(&run, &room).unwrap();
    let export = store.export_jsonl(&room).unwrap();
    let ledger_export = store.export_finding_ledger(&room).unwrap();
    assert_eq!(export.sha256, seal.digest);
    assert!(RoomStore::verify_export(&export.path, &seal.digest).unwrap());
    assert!(RoomStore::verify_export(&ledger_export.path, &ledger_export.sha256).unwrap());
    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ledger_export.path).unwrap()).unwrap();
    assert_eq!(ledger["schema_version"], 1);
    assert_eq!(ledger["findings"].as_array().unwrap().len(), 3);
    let statuses = ledger["dispositions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["status"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == "Accepted")
            .count(),
        2
    );
    assert_eq!(
        statuses.iter().filter(|status| **status == "Ruled").count(),
        1
    );
    assert_eq!(ledger["receipts"].as_array().unwrap().len(), 2);
    assert_eq!(store.row_count("findings").unwrap(), 3);
    assert_eq!(store.row_count("contests").unwrap(), 1);
    assert_eq!(store.row_count("receipts").unwrap(), 3);
}
