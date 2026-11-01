use crate::*;

#[test]
fn session_started_title_absent_when_none() {
    let ev = ProtocolEvent::SessionStarted {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        title: None,
    };
    let json = serde_json::to_string(&ev).unwrap();
    let _rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
}

#[test]
fn run_interrupted_reason_absent_when_none() {
    let ev = ProtocolEvent::RunInterrupted {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        reason: None,
    };
    let json = serde_json::to_string(&ev).unwrap();
    let _rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
}

// ── TaskState serialization ───────────────────────────────────────────────

#[test]
fn task_state_variants_serialize() {
    let states = [
        (TaskState::Open, r#""open""#),
        (TaskState::InProgress, r#""inProgress""#),
        (TaskState::Blocked, r#""blocked""#),
        (TaskState::Done, r#""done""#),
        (TaskState::Failed, r#""failed""#),
        (TaskState::Escalated, r#""escalated""#),
        (TaskState::Split, r#""split""#),
        (TaskState::Merged, r#""merged""#),
    ];
    for (state, expected_json) in &states {
        let json = serde_json::to_string(state).unwrap();
        assert_eq!(json, *expected_json);
        let rt: TaskState = serde_json::from_str(&json).unwrap();
        assert_eq!(*state, rt);
    }

    assert!(serde_json::from_str::<TaskState>(r#""InProgress""#).is_err());
}

// ── VerificationIssue camelCase ───────────────────────────────────────────

#[test]
fn verification_issue_camel_case() {
    let issue = VerificationIssue {
        tool: "clippy".into(),
        file_path: "src/main.rs".into(),
        line: Some(10),
        column: Some(5),
        message: "unused import".into(),
        repair_suggestion: "remove it".into(),
    };
    let json = serde_json::to_string(&issue).unwrap();
    assert!(json.contains(r#""filePath":"src/main.rs""#), "got: {json}");
    assert!(
        json.contains(r#""repairSuggestion":"remove it""#),
        "got: {json}"
    );
}

// ── F1 agent rail: roster event + usage rollup ────────────────────────────

#[test]
fn agent_roster_event_serializes_with_kind_tag() {
    let ev = ProtocolEvent::AgentRosterUpdated {
        session_id: "s1".into(),
        seq: 7,
        created_at: "2026-06-19T00:00:00Z".into(),
        agents: vec![AgentRosterRow {
            sub_session_id: "w1".into(),
            role: "reviewer".into(),
            status: "running".into(),
            model: "claude-sonnet-5".into(),
            tokens_in: 100,
            tokens_out: 20,
            cache_read: 0,
            tool_uses: 3,
            elapsed_ms: 1500,
            last_tools: vec!["read_file".into(), "grep".into()],
        }],
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"agentRosterUpdated""#),
        "got: {json}"
    );
    assert!(json.contains(r#""toolUses":3"#), "got: {json}");
    assert!(json.contains(r#""subSessionId":"w1""#), "got: {json}");
    assert!(json.contains(r#""model":"claude-sonnet-5""#), "got: {json}");
}

#[test]
fn session_usage_rollup_sums_agent_rows() {
    let rows = vec![
        AgentRosterRow {
            sub_session_id: "a".into(),
            role: "x".into(),
            status: "done".into(),
            model: String::new(),
            tokens_in: 100,
            tokens_out: 50,
            cache_read: 10,
            tool_uses: 2,
            elapsed_ms: 100,
            last_tools: vec![],
        },
        AgentRosterRow {
            sub_session_id: "b".into(),
            role: "y".into(),
            status: "done".into(),
            model: String::new(),
            tokens_in: 200,
            tokens_out: 60,
            cache_read: 5,
            tool_uses: 1,
            elapsed_ms: 200,
            last_tools: vec![],
        },
    ];
    let r = SessionUsageRollup::from_rows("s1", &rows, "2026-06-19T00:00:00Z");
    assert_eq!(r.tokens_in, 300);
    assert_eq!(r.tokens_out, 110);
    assert_eq!(r.cache_read, 15);
}

// ── F3 pre-gate: check-run event ──────────────────────────────────────────

#[test]
fn check_run_completed_serializes_with_kind_tag() {
    let ev = ProtocolEvent::CheckRunCompleted {
        session_id: "s1".into(),
        seq: 9,
        created_at: "t".into(),
        check_id: "fmt".into(),
        command: "cargo fmt --check".into(),
        exit_code: 1,
        attempts: 1,
        duration_ms: 42,
        passed: false,
        jury_skipped: true,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"checkRunCompleted""#),
        "got: {json}"
    );
    assert!(json.contains(r#""jurySkipped":true"#), "got: {json}");
    assert!(json.contains(r#""checkId":"fmt""#), "got: {json}");
}

// ── F2 goal: progress event ───────────────────────────────────────────────

#[test]
fn goal_progress_updated_serializes_with_kind_tag() {
    let ev = ProtocolEvent::GoalProgressUpdated {
        session_id: "s1".into(),
        seq: 11,
        created_at: "t".into(),
        snapshot: crate::event::GoalSnapshotV1 {
            goal_id: "g1".into(),
            objective: "ship".into(),
            status: "budget-limited".into(),
            started_at: "t0".into(),
            closed_at: None,
            dismissed_at: None,
            decomposition_state: "missing".into(),
            plan_revision: 0,
            completion_percent: None,
            now: None,
            next: None,
            steps: Vec::new(),
            verification_state: "unverified".into(),
            tokens_used: 500,
            budget_tokens: 500,
            continuation_count: 4,
            no_progress_count: 0,
        },
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"goalProgressUpdated""#),
        "got: {json}"
    );
    assert!(json.contains(r#""status":"budget-limited""#), "got: {json}");
    assert!(json.contains(r#""continuationCount":4"#), "got: {json}");
}
