use super::*;

#[test]
fn transcript_message_system_kind() {
    let msg = TranscriptMessage::System {
        id: "m1".into(),
        created_at: "t".into(),
        content: "You are a helpful assistant.".into(),
        pinned: false,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""$kind":"system""#), "got: {json}");
    let rt: TranscriptMessage = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, TranscriptMessage::System { .. }));
}

#[test]
fn transcript_message_user_kind() {
    let msg = TranscriptMessage::User {
        id: "m2".into(),
        created_at: "t".into(),
        content: "Hello!".into(),
        trust: None,
        pinned: false,
        attachments: Vec::new(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""$kind":"user""#), "got: {json}");
    let rt: TranscriptMessage = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, TranscriptMessage::User { .. }));
}

#[test]
fn transcript_message_assistant_kind() {
    let msg = TranscriptMessage::Assistant {
        id: "m3".into(),
        created_at: "t".into(),
        content: "Hi there!".into(),
        reasoning_content: Some("thinking...".into()),
        tool_calls: vec![],
        pinned: false,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""$kind":"assistant""#), "got: {json}");
    let rt: TranscriptMessage = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, TranscriptMessage::Assistant { .. }));
}

#[test]
fn transcript_message_tool_kind() {
    let msg = TranscriptMessage::Tool {
        id: "m4".into(),
        created_at: "t".into(),
        tool_call_id: "tc1".into(),
        summary: "wrote 3 files".into(),
        output: None,
        trust: Some(TrustEnvelope {
            trust: ContentTrust::ToolUntrusted,
            source: "tool:write_file".into(),
            instructions_are_data: true,
            content_hash: "sha256:test".into(),
        }),
        is_error: false,
        pinned: false,
        process_id: None,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""$kind":"tool""#), "got: {json}");
    assert!(json.contains(r#""trust":"toolUntrusted""#), "got: {json}");
    let rt: TranscriptMessage = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        rt,
        TranscriptMessage::Tool {
            trust: Some(envelope),
            ..
        } if envelope.trust == ContentTrust::ToolUntrusted
    ));

    let legacy = r#"{"$kind":"tool","id":"m4","createdAt":"t","toolCallId":"tc1","summary":"wrote 3 files","isError":false,"pinned":false}"#;
    let rt: TranscriptMessage = serde_json::from_str(legacy).unwrap();
    assert!(matches!(rt, TranscriptMessage::Tool { trust: None, .. }));
}

#[test]
fn transcript_message_compact_kind() {
    let msg = TranscriptMessage::Compact {
        id: "m5".into(),
        created_at: "t".into(),
        summary: "10 turns summarized.".into(),
        dropped_count: 10,
        pinned: false,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""$kind":"compact""#), "got: {json}");
    let rt: TranscriptMessage = serde_json::from_str(&json).unwrap();
    assert!(matches!(rt, TranscriptMessage::Compact { .. }));
}

// ── camelCase field name spot-checks ──────────────────────────────────────
