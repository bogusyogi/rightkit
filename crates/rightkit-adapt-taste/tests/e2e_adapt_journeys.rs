//! End-to-end Adapt journeys over the public API only: transcript evidence -> mined issues ->
//! review -> sealed proposals -> outcome reporting, and the persisted proposal plan store
//! (propose -> approve -> verify -> commit across process restarts), plus the admission/rejection
//! boundaries that keep untrusted text (model output, forged manifests, stale writers) from
//! becoming durable truth.

use std::collections::{BTreeMap, BTreeSet};

use rightkit_adapt_taste::authority::{
    classify_authority_effect, evaluate_origin, AuthorityEffect, Origin, PrecedenceTier,
};
use rightkit_adapt_taste::benchmark::LabelledCase;
use rightkit_adapt_taste::canonical::sha256_hex;
use rightkit_adapt_taste::cli_api::*;
use rightkit_adapt_taste::gates::{
    ContextAdmissionRecord, ContextDecision, CortexAdmissionEnvelope,
};
use rightkit_adapt_taste::insights::recurrence::transition_issue;
use rightkit_adapt_taste::insights::{EventKind, IssueState, TranscriptEventV1};
use rightkit_adapt_taste::manifest::{manifest_hash, payload_sha256, validate_schema};
use rightkit_adapt_taste::model_boundary::{ModelExtractionProposal, ModelProposalError};
use rightkit_adapt_taste::multiwriter::{converge, WriterRecord};
use rightkit_adapt_taste::outcomes::{AdjustedOutcome, Exposure, OutcomeEntryV1, RawOutcome};
use rightkit_adapt_taste::proposal::{build_pending_manifest, Gate1ReviewContextV1};
use rightkit_adapt_taste::proposal_state::{
    ProposalPlanState, ProposalPlanStore, ProposalRisk, ProposalStateError, ProposalVerificationV1,
};
use rightkit_adapt_taste::record::{InfluenceClass, RecordClass, RuleKey};
use rightkit_adapt_taste::remediation::{RemediationEffect, RemediationProposalV1};
use rightkit_adapt_taste::scope::ScopeDimensions;
use rightkit_adapt_taste::seal::{verify_seal, SemanticPayloadV1, SEAL_CONTRACT_VERSION};
use rightkit_adapt_taste::taste::extract_candidates_with_source;

fn ev(id: &str, session: &str, provenance: &str, kind: EventKind, text: &str) -> TranscriptEventV1 {
    TranscriptEventV1 {
        event_id: id.into(),
        session_id: session.into(),
        host: "pi".into(),
        provenance: provenance.into(),
        kind,
        text: text.into(),
        timestamp: Some("2026-08-24T01:00:00Z".into()),
        byte_start: 0,
        byte_end: text.len() as i64,
        call_id: None,
        occurrence: 0,
        evidence_eligible: true,
    }
}

fn user(id: &str, session: &str, text: &str) -> TranscriptEventV1 {
    ev(id, session, "external_user", EventKind::UserMessage, text)
}

fn payload(text: &str) -> SemanticPayloadV1 {
    SemanticPayloadV1 {
        seal_contract_version: SEAL_CONTRACT_VERSION.into(),
        record_kind: "preference".into(),
        category: "verification".into(),
        canonical_text: text.into(),
        scope: "repo-x".into(),
        scope_dimensions: ScopeDimensions::normalize(&BTreeMap::new()).unwrap(),
        authority_tier: PrecedenceTier::ExplicitScopedUserPreference,
        authority_effect: AuthorityEffect::Neutral,
        influence_class: InfluenceClass::Provisional,
        record_class: Some(RecordClass::StandingPreference),
        machine_binding: None,
        source_evidence_digests: vec!["evidence-sha".into()],
        canonical_pool_sha256: "pool-sha".into(),
        admission_policy_version: "adapt.admission.v1".into(),
        validator_receipt_id: "validator-1".into(),
        validator_receipt_sha256: "validator-sha".into(),
        redaction_contract_version: "membrane.redaction.v1".into(),
        provenance_contract_version: "adapt.provenance.v2".into(),
    }
}

fn transcript_event(
    kind: &str,
    role: &str,
    event_id: &str,
    text: &str,
) -> rightkit_agent_transcript::TranscriptEventV1 {
    serde_json::from_value(serde_json::json!({
        "eventId":event_id,"rowIndex":1,"byteStart":0,"byteEnd":text.len(),
        "blockIndex":0,"sequence":1,"kind":kind,"role":role,"text":text,
        "classification":"successful_readonly","class":"successful_readonly",
        "projection":"default","host":"pi","sessionId":"session-1",
        "transcriptId":"transcript-1","parserDigest":"parser-1",
        "synthetic":false,"meta":false,"privateReasoningOmitted":false,
        "redacted":false,"flags":{}
    }))
    .unwrap()
}

fn recurring_session_events() -> Vec<TranscriptEventV1> {
    vec![
        user(
            "a1",
            "s1",
            "please run the full test suite before claiming done",
        ),
        user(
            "a2",
            "s2",
            "Please run the FULL test suite before claiming done.",
        ),
        user(
            "a3",
            "s3",
            "please run the full test suite before claiming done!",
        ),
        // A one-off request must never become an issue.
        user("z1", "s1", "rename the widget module to gadgets"),
        // Ignored tool failure followed by a victory claim, in two separate sessions.
        ev("t1", "s4", "assistant", EventKind::ToolCall, "cargo test"),
        ev(
            "t2",
            "s4",
            "tool",
            EventKind::ToolResult,
            "error: test failed",
        ),
        ev(
            "t3",
            "s4",
            "assistant",
            EventKind::AssistantMessage,
            "All done - I verified the fix works.",
        ),
        ev("u1", "s5", "assistant", EventKind::ToolCall, "cargo test"),
        ev(
            "u2",
            "s5",
            "tool",
            EventKind::ToolResult,
            "error: test failed",
        ),
        ev(
            "u3",
            "s5",
            "assistant",
            EventKind::AssistantMessage,
            "All done - I verified the fix works.",
        ),
    ]
}

#[test]
fn insights_journey_mine_review_doctor_and_outcome_reporting() {
    let mine = MineRequest {
        events: recurring_session_events(),
        min_recurrence: 2,
    };
    let mined = handle_mine(&mine);
    assert_eq!(mined.api_version, "adapt.cli.v1");
    assert!(mined.detector_contract_error.is_none());

    // Recurrence across sessions forms issues; the one-off does not.
    let repeated = mined
        .issues
        .iter()
        .find(|i| i.family == "repeated_ask")
        .expect("repeated_ask issue");
    assert!(repeated.recurrence_count >= 3);
    assert_eq!(repeated.state, IssueState::Observed);
    assert!(mined
        .issues
        .iter()
        .any(|i| i.family == "ignored_tool_failure"));
    assert!(
        mined.issues.iter().all(|i| i.recurrence_count >= 2),
        "single episodes must not form issues"
    );
    assert!(
        !mined.remediation_proposals.is_empty(),
        "issues yield review-only remediation proposals"
    );

    // Deterministic and order independent.
    let again = handle_mine(&mine);
    assert_eq!(
        serde_json::to_string(&mined).unwrap(),
        serde_json::to_string(&again).unwrap()
    );
    let mut shuffled = mine.events.clone();
    // Cross-session arrival order must not matter; order within a session is transcript semantics.
    shuffled.sort_by(|a, b| b.session_id.cmp(&a.session_id));
    let reordered = handle_mine(&MineRequest {
        events: shuffled,
        min_recurrence: 2,
    });
    let ids = |r: &MineResponse| {
        r.issues
            .iter()
            .map(|i| i.issue_id.clone())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&mined), ids(&reordered));

    // Review reports state without mutating anything.
    let review = handle_review(&ReviewRequest {
        issue_ids: vec![],
        issues: mined.issues.clone(),
    });
    assert_eq!(review.items.len(), mined.issues.len());
    assert!(review
        .items
        .iter()
        .all(|i| i.state == "observed" && !i.honesty_limit.is_empty()));

    // Lifecycle: illegal jumps refused, legal step allowed.
    assert!(transition_issue(repeated, IssueState::Mitigated).is_err());
    assert!(transition_issue(repeated, IssueState::Recurring).is_ok());

    // Doctor is healthy on mined state and flags a corrupted reference.
    let healthy = handle_doctor(&DoctorRequest {
        issues: mined.issues.clone(),
        episodes: mined.episodes.clone(),
    });
    assert!(
        healthy.healthy,
        "{:?}",
        healthy.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
    );
    let mut broken = mined.issues.clone();
    broken[0].episode_ids.push("ghost-episode".into());
    let sick = handle_doctor(&DoctorRequest {
        issues: broken,
        episodes: mined.episodes.clone(),
    });
    assert!(
        !sick.healthy
            && sick
                .findings
                .iter()
                .any(|f| f.code == "dangling_episode_ref")
    );

    // Outcome reporting: same-signature recurrence reopens, clean exposure stays closed.
    let entry = |issue: &str, raw: RawOutcome, adjusted: AdjustedOutcome| OutcomeEntryV1 {
        entry_id: format!("e-{issue}"),
        issue_id: issue.into(),
        mitigation_proposal_id: "mit-1".into(),
        raw,
        exposure: Exposure {
            opportunities: 10,
            baseline: 10,
        },
        adjusted,
        note: String::new(),
    };
    let report = handle_report(&ReportRequest {
        issue_ids: vec![],
        ledger_entries: vec![
            entry(
                "issue-bad",
                RawOutcome::RecurredSameSignature,
                AdjustedOutcome::Ineffective,
            ),
            entry(
                "issue-good",
                RawOutcome::NoRecurrence,
                AdjustedOutcome::Effective,
            ),
        ],
    });
    assert_eq!(report.reopen_recommended, vec!["issue-bad".to_string()]);
    assert!(report.effectiveness["issue-good"] > report.effectiveness["issue-bad"]);
}

#[test]
fn precision_gate_admits_only_benchmarked_families() {
    let corpus = || {
        vec![LabelledCase {
            case_id: "frustration".into(),
            events: vec![
                user("a", "s1", "this is so frustrating and annoying"),
                user("b", "s2", "this is so frustrating and annoying"),
            ],
            expected_families: BTreeSet::from(["visible_frustration".to_string()]),
            forbidden_families: BTreeSet::new(),
        }]
    };
    let report = handle_benchmark(&BenchmarkRequest { corpus: corpus() }).report;
    assert_eq!(
        report.report_digest,
        handle_benchmark(&BenchmarkRequest { corpus: corpus() })
            .report
            .report_digest
    );
    let precision = report.by_family["visible_frustration"].precision();

    let proposal = RemediationProposalV1::build(
        "issue",
        "visible_frustration",
        RemediationEffect::ProcessChange,
        "ask clarifying question",
        vec![],
    );
    assert!(proposal.precision_gate(Some(precision)).is_ok());
    assert!(
        proposal.precision_gate(None).is_err(),
        "unmeasured family must be refused"
    );
    assert!(proposal.precision_gate(Some(0.5)).is_err());
}

#[test]
fn plan_store_journey_survives_restart_and_rejects_unsafe_transitions() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("plans.json");

    let mut store = ProposalPlanStore::open(&path).unwrap();
    let plan = store
        .propose(
            "plan-1",
            payload("run focused verification"),
            ProposalRisk::High,
            100,
            300,
            7,
        )
        .unwrap();
    let seal = plan.proposal_seal_sha256.clone();
    assert_eq!(plan.state, ProposalPlanState::Proposed);
    drop(store);

    // Restart: insufficient reviewer risk, stale target version, wrong verification all refused.
    let mut store = ProposalPlanStore::open(&path).unwrap();
    assert_eq!(
        store.approve("plan-1", "reviewer", ProposalRisk::Medium, 110, 7),
        Err(ProposalStateError::RiskApprovalInsufficient)
    );
    assert!(matches!(
        store.approve("plan-1", "reviewer", ProposalRisk::High, 110, 8),
        Err(ProposalStateError::VersionConflict { .. })
    ));
    store
        .approve("plan-1", "reviewer", ProposalRisk::High, 110, 7)
        .unwrap();

    let verification = |plan_id: &str, passed_seal: &str| ProposalVerificationV1 {
        receipt_id: "verify-1".into(),
        plan_id: plan_id.into(),
        proposal_seal_sha256: passed_seal.into(),
        target_version: 7,
        passed: true,
        checked_at: 115,
    };
    assert_eq!(
        store.commit("plan-1", 120, 7, verification("other-plan", &seal)),
        Err(ProposalStateError::VerificationRejected)
    );
    assert!(store
        .commit("plan-1", 120, 7, verification("plan-1", "forged-seal"))
        .is_err());
    store
        .commit("plan-1", 120, 7, verification("plan-1", &seal))
        .unwrap();
    drop(store);

    let reopened = ProposalPlanStore::open(&path).unwrap();
    let committed = reopened.get("plan-1").unwrap();
    assert_eq!(committed.state, ProposalPlanState::Committed);
    assert!(committed.approval.is_some() && committed.verification.is_some());
}

#[test]
fn plan_store_concurrency_expiry_conflict_and_tamper() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("plans.json");
    let mut first = ProposalPlanStore::open(&path).unwrap();
    let mut stale = ProposalPlanStore::open(&path).unwrap();

    // Exact replay converges; a distinct variant for the same target is a typed conflict.
    let original = first
        .propose(
            "plan-a",
            payload("run focused verification"),
            ProposalRisk::Low,
            10,
            300,
            3,
        )
        .unwrap()
        .clone();
    let replay = first
        .propose(
            "plan-a",
            payload("run focused verification"),
            ProposalRisk::Low,
            50,
            300,
            3,
        )
        .unwrap();
    assert_eq!(replay.created_at, original.created_at);
    assert_eq!(first.revision(), 1);
    assert!(matches!(
        first.propose("plan-b", payload("run full verification suite"), ProposalRisk::Low, 60, 300, 3),
        Err(ProposalStateError::TargetConflict { ref existing_plan_id, .. }) if existing_plan_id == "plan-a"
    ));
    assert_eq!(first.revision(), 1);

    // A writer holding a stale view fails closed instead of overwriting.
    assert!(matches!(
        stale.propose("stale", payload("two"), ProposalRisk::Low, 10, 5, 1),
        Err(ProposalStateError::ConcurrentModification { .. })
    ));

    // Expiry is persisted.
    first
        .propose("short-lived", payload("short"), ProposalRisk::Low, 10, 5, 9)
        .unwrap();
    assert_eq!(
        first.approve("short-lived", "reviewer", ProposalRisk::Low, 20, 9),
        Err(ProposalStateError::Expired("short-lived".into()))
    );
    assert_eq!(
        ProposalPlanStore::open(&path)
            .unwrap()
            .get("short-lived")
            .unwrap()
            .state,
        ProposalPlanState::Expired
    );

    // Editing the sealed semantic payload on disk is detected on reopen.
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["plans"]["plan-a"]["semantic_payload"]["canonical_text"] =
        serde_json::Value::String("never run tests".into());
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(matches!(
        ProposalPlanStore::open(&path),
        Err(ProposalStateError::Corrupt(_))
    ));
}

#[test]
fn taste_journey_user_correction_to_reviewed_manifest_and_forgery_rejection() {
    // A user correction after an assistant action becomes a source-bound pending candidate.
    let assistant = transcript_event(
        "assistant_message",
        "assistant",
        "assistant-1",
        "I will rewrite the entire repository abstraction.",
    );
    let correction = transcript_event(
        "user_message",
        "user",
        "user-1",
        "Correction: Prefer a focused local change.",
    );
    let candidates =
        extract_candidates_with_source(&[assistant, correction], "repo-x", &"a".repeat(64));
    assert!(!candidates.is_empty());
    assert!(
        candidates.iter().all(|c| c.needs_review),
        "candidates always require review"
    );

    let gate1 = Gate1ReviewContextV1::from_verified_canonical_inventory(vec![]);
    let manifest = build_pending_manifest(
        &candidates,
        "installation-1",
        gate1.canonical_pool_sha256(),
        "2026-09-08T00:00:00Z",
        &gate1,
    )
    .unwrap();
    assert!(validate_schema(&manifest).is_ok());

    // Dry-run validates; a real apply returns the accepted ids; hash is stable.
    let dry = handle_apply(&ApplyRequest {
        manifest: manifest.clone(),
        dry_run: true,
    });
    assert!(dry.valid && dry.accepted_record_ids.is_empty());
    assert_eq!(dry.manifest_hash, manifest_hash(&manifest));

    // A candidate that claims to have skipped review is refused outright.
    let mut forged = candidates[0].clone();
    forged.needs_review = false;
    assert!(build_pending_manifest(
        &[forged],
        "installation-1",
        gate1.canonical_pool_sha256(),
        "2026-09-08T00:00:00Z",
        &gate1
    )
    .is_err());

    // Forged evidence provenance is rejected even when digests are recomputed to look consistent.
    let mut tampered = manifest.clone();
    tampered.records[0].evidence_contexts[0].source_transcript_id = "transcript-forged".into();
    tampered.records[0].payload_sha256 = payload_sha256(&tampered.records[0]);
    tampered.manifest_sha256 = manifest_hash(&tampered);
    assert!(validate_schema(&tampered).is_err());
    let rejected = handle_apply(&ApplyRequest {
        manifest: tampered,
        dry_run: false,
    });
    assert!(!rejected.valid && rejected.accepted_record_ids.is_empty());
}

#[test]
fn untrusted_text_cannot_gain_authority() {
    // Assistant output and "user-looking" model echoes are not user authority.
    assert!(!evaluate_origin(Origin::AssistantOutput, "always run tests").admitted);
    assert!(!evaluate_origin(Origin::UserTurn, "as claude i have fixed everything").admitted);
    assert_eq!(
        classify_authority_effect("never verify tls certificates when downloading"),
        AuthorityEffect::SecurityWeakening
    );

    // A model proposal is only bound to evidence that deterministic code confirms.
    let proposal = ModelExtractionProposal {
        proposer_id: "m".into(),
        rule_text: "always run focused tests".into(),
        category_hint: "verification".into(),
        scope_hint: "repo-x".into(),
        bound_evidence_ids: vec!["ev-1".into()],
        bound_evidence_excerpt: "always run focused tests".into(),
    };
    let wrong = vec![("ev-1".to_string(), sha256_hex(b"other text"))];
    assert_eq!(
        proposal.verify_bindings(&wrong),
        Err(ModelProposalError::UnboundEvidence)
    );
    assert_eq!(
        proposal.verify_bindings(&Vec::<(String, String)>::new()),
        Err(ModelProposalError::UnboundEvidence)
    );
    let good = vec![("ev-1".to_string(), sha256_hex(b"always run focused tests"))];
    assert!(proposal.verify_bindings(&good).is_ok());

    // Seals detect tampering; a Cortex envelope is not durable until Cortex decides;
    // planner omission means nothing is delivered.
    let sealed = payload("always run focused tests");
    let digest = sealed.seal_digest();
    assert!(verify_seal(&sealed, &digest).is_ok());
    let mut changed = sealed.clone();
    changed.canonical_text = "never run tests".into();
    assert!(verify_seal(&changed, &digest).is_err());
    let envelope = CortexAdmissionEnvelope {
        envelope_id: "e".into(),
        record_kind: "preference".into(),
        seal_digest: digest,
        influence_class: InfluenceClass::Provisional,
        idempotency_key: "idem".into(),
        installation_id: "inst".into(),
        cortex_verdict: None,
    };
    assert!(!envelope.is_durable());
    let record = ContextAdmissionRecord {
        cortex_record_ref: "r".into(),
        authority_ok: true,
        fresh: true,
        sufficient: true,
        within_budget: true,
        planner_decision: Some(ContextDecision::Omitted),
    };
    assert!(!record.delivered());
}

#[test]
fn concurrent_writers_converge_regardless_of_arrival_order() {
    let rec = |w: &str, l: u64, d: &str, t: PrecedenceTier| WriterRecord {
        writer_id: w.into(),
        lamport: l,
        rule_key: RuleKey::new("workflow", "always run tests"),
        payload_digest: d.into(),
        precedence_tier: t,
    };
    let set = vec![
        rec("w1", 1, "d1", PrecedenceTier::ExplicitGlobalUserPreference),
        rec("w2", 2, "d2", PrecedenceTier::ExplicitGlobalUserPreference),
        rec("w3", 3, "d3", PrecedenceTier::ProvisionalCandidate),
    ];
    let mut reversed = set.clone();
    reversed.reverse();
    let key = RuleKey::new("workflow", "always run tests");
    let (forward, backward) = (converge(&set), converge(&reversed));
    assert_eq!(
        forward.winner(&key).map(|(d, _)| d),
        backward.winner(&key).map(|(d, _)| d)
    );
    assert_eq!(forward.conflicts_for(&key), backward.conflicts_for(&key));
    // Provisional candidates never outrank explicit user preferences.
    assert_ne!(
        forward.winner(&key).map(|(d, _)| d.to_string()),
        Some("d3".to_string())
    );
}
