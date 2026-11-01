use crate::*;

#[test]
fn artifact_row_round_trip() {
    let row = ArtifactRow {
        id: "a1".into(),
        path: "src/main.rs".into(),
        kind: "file".into(),
        content_hash: "abc123".into(),
        shippable: false,
        producer: "session1".into(),
        provenance: "".into(),
        verdict: Some(ArtifactVerdict {
            passed: true,
            juror: "jury-agent".into(),
            bound_hash: "abc123".into(),
            summary: "looks good".into(),
        }),
        approvals: vec![ArtifactApproval {
            approver: "alice".into(),
            bound_hash: "abc123".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
        }],
        meta: "".into(),
        trust_label: "unreviewed".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
    };
    let json = serde_json::to_string(&row).unwrap();
    assert!(json.contains(r#""contentHash":"abc123""#), "got: {json}");
    assert!(json.contains(r#""boundHash":"abc123""#), "got: {json}");
    let rt: ArtifactRow = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.id, "a1");
    assert!(rt.verdict.is_some());
    assert_eq!(rt.approvals.len(), 1);
}

// ── TaskNode round-trip ───────────────────────────────────────────────────

#[test]
fn task_node_round_trip() {
    let node = TaskNode {
        id: "t1".into(),
        title: "Implement feature".into(),
        deps: vec!["t0".into()],
        done_when: "tests pass".into(),
        done_when_predicates: vec![],
        state: TaskState::Open,
        category: "dev".into(),
        estimated_duration: "2h".into(),
        assigned_role: "coder".into(),
        escalation_path: "".into(),
        open_questions: vec![],
        created_at: "2024-01-01T00:00:00Z".into(),
        updated_at: "2024-01-01T00:00:00Z".into(),
    };
    let json = serde_json::to_string(&node).unwrap();
    assert!(json.contains(r#""doneWhen":"tests pass""#), "got: {json}");
    assert!(json.contains(r#""assignedRole":"coder""#), "got: {json}");
    let rt: TaskNode = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.id, "t1");
    assert_eq!(rt.deps, vec!["t0"]);
}

// ── Op round-trips ────────────────────────────────────────────────────────

#[test]
fn op_submit_user_message_round_trip() {
    let op = Op::SubmitUserMessage {
        op_id: "op1".into(),
        session_id: "s1".into(),
        text: "hello".into(),
    };
    let json = serde_json::to_string(&op).unwrap();
    assert!(
        json.contains(r#""$kind":"submitUserMessage""#),
        "got: {json}"
    );
    let rt: Op = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, Op::SubmitUserMessage { .. }));
}

#[test]
fn op_interrupt_run_round_trip() {
    let op = Op::InterruptRun {
        op_id: "op2".into(),
        session_id: "s1".into(),
    };
    let json = serde_json::to_string(&op).unwrap();
    assert!(json.contains(r#""$kind":"interruptRun""#), "got: {json}");
    let rt: Op = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, Op::InterruptRun { .. }));
}

#[test]
fn op_request_compaction_round_trip() {
    let op = Op::RequestCompaction {
        op_id: "op3".into(),
        session_id: "s1".into(),
    };
    let json = serde_json::to_string(&op).unwrap();
    assert!(
        json.contains(r#""$kind":"requestCompaction""#),
        "got: {json}"
    );
}

#[test]
fn op_inject_steering_message_round_trip() {
    let op = Op::InjectSteeringMessage {
        op_id: "op4".into(),
        session_id: "s1".into(),
        content: "focus on tests".into(),
        injected_at: "t".into(),
        injected_by: "alice".into(),
    };
    let json = serde_json::to_string(&op).unwrap();
    assert!(
        json.contains(r#""$kind":"injectSteeringMessage""#),
        "got: {json}"
    );
    let rt: Op = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, Op::InjectSteeringMessage { .. }));
}

#[test]
fn op_resolve_tool_approval_round_trip() {
    let op = Op::ResolveToolApproval {
        op_id: "op5".into(),
        session_id: "s1".into(),
        tool_call_id: "tc1".into(),
        approved: true,
        reason: Some("looks safe".into()),
    };
    let json = serde_json::to_string(&op).unwrap();
    assert!(
        json.contains(r#""$kind":"resolveToolApproval""#),
        "got: {json}"
    );
    let rt: Op = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, Op::ResolveToolApproval { approved: true, .. }));
}

// ── Op camelCase field spot-checks ────────────────────────────────────────

#[test]
fn op_session_id_camel_case() {
    let op = Op::SubmitUserMessage {
        op_id: "op1".into(),
        session_id: "sess42".into(),
        text: "hi".into(),
    };
    let json = serde_json::to_string(&op).unwrap();
    assert!(json.contains(r#""sessionId":"sess42""#), "got: {json}");
    assert!(json.contains(r#""opId":"op1""#), "got: {json}");
}

#[test]
fn op_tool_call_id_camel_case() {
    let op = Op::ResolveToolApproval {
        op_id: "op5".into(),
        session_id: "s1".into(),
        tool_call_id: "tc99".into(),
        approved: false,
        reason: None,
    };
    let json = serde_json::to_string(&op).unwrap();
    assert!(json.contains(r#""toolCallId":"tc99""#), "got: {json}");
}

// ── OutputRef and ArtifactRef round-trips ─────────────────────────────────

#[test]
fn output_ref_round_trip() {
    let r = OutputRef {
        id: "ref1".into(),
        kind: "shell_stdout".into(),
        size_bytes: 1024,
        content_hash: Some("deadbeef".into()),
        preview: Some("first 80 chars...".into()),
        media_type: "text/plain; charset=utf-8".into(),
        truncated: true,
        retrievable: true,
    };
    let json = serde_json::to_string(&r).unwrap();
    assert!(json.contains(r#""sizeBytes":1024"#), "got: {json}");
    assert!(json.contains(r#""contentHash":"deadbeef""#), "got: {json}");
    assert!(json.contains(r#""truncated":true"#), "got: {json}");
    assert!(json.contains(r#""retrievable":true"#), "got: {json}");
    let rt: OutputRef = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.id, "ref1");
    assert_eq!(rt.size_bytes, 1024);
}

#[test]
fn artifact_ref_round_trip() {
    let r = ArtifactRef {
        id: "aref1".into(),
        kind: "diff".into(),
        title: Some("My patch".into()),
        content_hash: None,
    };
    let json = serde_json::to_string(&r).unwrap();
    assert!(json.contains(r#""kind":"diff""#), "got: {json}");
    let rt: ArtifactRef = serde_json::from_str(&json).unwrap();
    assert_eq!(rt.title, Some("My patch".into()));
    assert_eq!(rt.content_hash, None);
}

// ── Option fields round-trip when None ───────────────────────────────────
