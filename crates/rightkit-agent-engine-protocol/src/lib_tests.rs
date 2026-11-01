use super::*;

// ── Helpers ───────────────────────────────────────────────────────────────

fn make_tool_call() -> ToolCall {
    ToolCall {
        id: "tc1".into(),
        name: "read_file".into(),
        arguments_json: "{}".into(),
    }
}

fn assistant_msg() -> TranscriptMessage {
    TranscriptMessage::Assistant {
        id: "m1".into(),
        created_at: "t".into(),
        content: "hello".into(),
        reasoning_content: None,
        tool_calls: vec![],
        pinned: false,
    }
}

fn tool_result_msg() -> TranscriptMessage {
    TranscriptMessage::Tool {
        id: "m2".into(),
        created_at: "t".into(),
        tool_call_id: "tc1".into(),
        summary: "ok".into(),
        output: None,
        trust: Some(TrustEnvelope {
            trust: ContentTrust::ToolUntrusted,
            source: "tool:read_file".into(),
            instructions_are_data: true,
            content_hash: "sha256:test".into(),
        }),
        is_error: false,
        pinned: false,
        process_id: None,
    }
}

// ── ProtocolEvent $kind field tests ──────────────────────────────────────

mod protocol_events_a;
mod protocol_events_b;
mod transcript_messages;
mod wire_shapes;
