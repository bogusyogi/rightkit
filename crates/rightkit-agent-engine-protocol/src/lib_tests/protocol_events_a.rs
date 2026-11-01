use super::*;

#[test]
fn protocol_event_session_started_kind() {
    let ev = ProtocolEvent::SessionStarted {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        title: None,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"sessionStarted""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::SessionStarted { .. }));
}

#[test]
fn protocol_event_session_title_updated_kind() {
    let ev = ProtocolEvent::SessionTitleUpdated {
        session_id: "s1".into(),
        seq: 2,
        created_at: "t".into(),
        title: "Fix Rust Guardrails".into(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"sessionTitleUpdated""#));
    assert!(json.contains(r#""title":"Fix Rust Guardrails""#));
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::SessionTitleUpdated { .. }));
}

#[test]
fn protocol_event_meta_accessors_share_common_fields() {
    let ev = ProtocolEvent::ToolApprovalResolved {
        session_id: "s-meta".into(),
        seq: 42,
        created_at: "2026-06-28T00:00:00Z".into(),
        tool_call_id: "tc1".into(),
        approved: true,
    };

    let meta = ev.meta();
    assert_eq!(meta.session_id, "s-meta");
    assert_eq!(meta.seq, 42);
    assert_eq!(meta.created_at, "2026-06-28T00:00:00Z");
    assert_eq!(ev.session_id(), "s-meta");
    assert_eq!(ev.seq(), 42);
    assert_eq!(ev.created_at(), "2026-06-28T00:00:00Z");

    let json = serde_json::to_value(&ev).unwrap();
    assert_eq!(json["sessionId"], "s-meta");
    assert!(json.get("meta").is_none(), "wire shape must stay flat");
}

#[test]
fn protocol_event_assistant_delta_kind() {
    let ev = ProtocolEvent::AssistantDelta {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        text: "hi".into(),
        is_reasoning: false,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"assistantDelta""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::AssistantDelta { .. }));
}

#[test]
fn protocol_event_user_message_submitted_nonce_is_optional() {
    let ev = ProtocolEvent::UserMessageSubmitted {
        session_id: "s1".into(),
        seq: 2,
        created_at: "t".into(),
        text: "hello".into(),
        nonce: Some("nonce-123".into()),
        trust: None,
        attachments: Vec::new(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"userMessageSubmitted""#),
        "got: {json}"
    );
    assert!(json.contains(r#""nonce":"nonce-123""#), "got: {json}");

    let legacy = r#"{"$kind":"userMessageSubmitted","sessionId":"s1","seq":2,"createdAt":"t","text":"hello"}"#;
    let rt: ProtocolEvent = serde_json::from_str(legacy).unwrap();
    assert!(matches!(
        rt,
        ProtocolEvent::UserMessageSubmitted {
            nonce: None,
            attachments,
            ..
        } if attachments.is_empty()
    ));
}

#[test]
fn protocol_event_assistant_message_completed_kind() {
    let ev = ProtocolEvent::AssistantMessageCompleted {
        session_id: "s1".into(),
        seq: 2,
        created_at: "t".into(),
        message: assistant_msg(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"assistantMessageCompleted""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        rt,
        ProtocolEvent::AssistantMessageCompleted { .. }
    ));
}

#[test]
fn protocol_event_tool_call_started_kind() {
    let ev = ProtocolEvent::ToolCallStarted {
        session_id: "s1".into(),
        seq: 3,
        created_at: "t".into(),
        call: make_tool_call(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"toolCallStarted""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::ToolCallStarted { .. }));
}

#[test]
fn protocol_event_tool_approval_requested_kind() {
    let ev = ProtocolEvent::ToolApprovalRequested {
        session_id: "s1".into(),
        seq: 4,
        created_at: "t".into(),
        call: ToolCall {
            id: "tc1".into(),
            name: "write_file".into(),
            arguments_json: "{}".into(),
        },
        risk_label: "high".into(),
        proposed_change: Some("+ foo".into()),
        policy_source: None,
        middleware_decisions: vec![MiddlewareDecisionEvidence {
            middleware_id: "builtin.observe-approvals".into(),
            title: "Observe approvals".into(),
            phase: "approvalRequested".into(),
            effect: "observe".into(),
            decision: "emitObservation".into(),
            reason: "Approval prompt emitted".into(),
        }],
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"toolApprovalRequested""#),
        "got: {json}"
    );
    assert!(json.contains(r#""middlewareDecisions""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::ToolApprovalRequested { .. }));
}

#[test]
fn protocol_event_tool_approval_resolved_kind() {
    let ev = ProtocolEvent::ToolApprovalResolved {
        session_id: "s1".into(),
        seq: 5,
        created_at: "t".into(),
        tool_call_id: "tc1".into(),
        approved: true,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"toolApprovalResolved""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::ToolApprovalResolved { .. }));
}

#[test]
fn protocol_event_tool_result_added_kind() {
    let ev = ProtocolEvent::ToolResultAdded {
        session_id: "s1".into(),
        seq: 6,
        created_at: "t".into(),
        result: tool_result_msg(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""$kind":"toolResultAdded""#), "got: {json}");
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::ToolResultAdded { .. }));
}

#[test]
fn protocol_event_verification_finished_kind() {
    let ev = ProtocolEvent::VerificationFinished {
        session_id: "s1".into(),
        seq: 7,
        created_at: "t".into(),
        summary: "all good".into(),
        passed: true,
        tool: "cargo test".into(),
        issue_count: 0,
        issues: vec![],
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(
        json.contains(r#""$kind":"verificationFinished""#),
        "got: {json}"
    );
    let rt: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, ProtocolEvent::VerificationFinished { .. }));
}
