use crate::*;

#[test]
fn camel_case_session_id_field() {
    let ev = ProtocolEvent::SessionStarted {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        title: None,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""sessionId":"s1""#), "got: {json}");
    assert!(json.contains(r#""createdAt":"t""#), "got: {json}");
}

#[test]
fn camel_case_is_reasoning_field() {
    let ev = ProtocolEvent::AssistantDelta {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        text: "hi".into(),
        is_reasoning: true,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""isReasoning":true"#), "got: {json}");
}

#[test]
fn camel_case_tool_call_id_field() {
    let ev = ProtocolEvent::ToolApprovalResolved {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        tool_call_id: "tc1".into(),
        approved: false,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""toolCallId":"tc1""#), "got: {json}");
}

#[test]
fn camel_case_risk_label_field() {
    let ev = ProtocolEvent::ToolApprovalRequested {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        call: ToolCall {
            id: "tc1".into(),
            name: "write_file".into(),
            arguments_json: "{}".into(),
        },
        risk_label: "high".into(),
        proposed_change: None,
        policy_source: None,
        middleware_decisions: vec![],
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""riskLabel":"high""#), "got: {json}");
}

#[test]
fn camel_case_prompt_tokens_field() {
    let ev = ProtocolEvent::UsageUpdated {
        session_id: "s1".into(),
        seq: 1,
        created_at: "t".into(),
        prompt_tokens: 42,
        completion_tokens: 7,
        cost_usd: 0.0,
        context_tokens: 49,
        context_percent: 1.0,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        cost_known: true,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""promptTokens":42"#), "got: {json}");
    assert!(json.contains(r#""completionTokens":7"#), "got: {json}");
    assert!(json.contains(r#""costUsd":0.0"#), "got: {json}");
    assert!(json.contains(r#""contextTokens":49"#), "got: {json}");
    assert!(json.contains(r#""contextPercent":1.0"#), "got: {json}");
}

#[test]
fn usage_updated_cost_known_defaults_true_for_legacy_payloads() {
    // Older senders that predate the `cost_known` field must still decode,
    // with cost treated as known (their prior, assumed-correct behavior).
    let legacy_json = r#"{
        "$kind": "usageUpdated",
        "sessionId": "s1",
        "seq": 1,
        "createdAt": "t",
        "promptTokens": 10,
        "completionTokens": 5,
        "costUsd": 0.01,
        "contextTokens": 15,
        "contextPercent": 0.1
    }"#;
    let ev: ProtocolEvent = serde_json::from_str(legacy_json).unwrap();
    match ev {
        ProtocolEvent::UsageUpdated { cost_known, .. } => assert!(cost_known),
        other => panic!("expected UsageUpdated, got {other:?}"),
    }
}

#[test]
fn camel_case_arguments_json_field() {
    let tc = ToolCall {
        id: "tc1".into(),
        name: "fn".into(),
        arguments_json: r#"{"x":1}"#.into(),
    };
    let json = serde_json::to_string(&tc).unwrap();
    assert!(json.contains(r#""argumentsJson":"#), "got: {json}");
}

#[test]
fn camel_case_reasoning_content_field() {
    let msg = TranscriptMessage::Assistant {
        id: "m1".into(),
        created_at: "t".into(),
        content: "hi".into(),
        reasoning_content: Some("reasoning".into()),
        tool_calls: vec![],
        pinned: false,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(
        json.contains(r#""reasoningContent":"reasoning""#),
        "got: {json}"
    );
}

#[test]
fn camel_case_dropped_count_field() {
    let msg = TranscriptMessage::Compact {
        id: "m1".into(),
        created_at: "t".into(),
        summary: "summary".into(),
        dropped_count: 5,
        pinned: false,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""droppedCount":5"#), "got: {json}");
}

// ── ArtifactRow round-trip ────────────────────────────────────────────────
