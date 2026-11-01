#[test]
fn render_handoff_payload_keeps_stable_json() {
    let payload = serde_json::json!({
        "kind": "coderight.session_handoff.v1",
        "sessionId": "s1",
    });

    let rendered = render_handoff_payload(&payload);

    assert!(rendered.contains(r#""kind": "coderight.session_handoff.v1""#));
    assert!(rendered.contains(r#""sessionId": "s1""#));
}

#[test]
fn app_open_handoff_hint_points_to_explicit_machine_payload() {
    let hint = render_app_open_handoff_hint("s1");

    assert!(hint.contains("coderight app handoff"));
    assert!(hint.contains("s1"));
    assert!(!hint.contains("accessToken"));
}

#[test]
fn render_cli_stream_event_wraps_protocol_event_contract() {
    let event = rightkit_agent_engine_protocol::ProtocolEvent::AssistantDelta {
        session_id: "s1".to_string(),
        seq: 7,
        created_at: "2026-06-24T01:02:03Z".to_string(),
        text: "hello".to_string(),
        is_reasoning: false,
    };

    let rendered = render_cli_stream_event(&event);
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../data/engine__bins__coderight__tests__fixtures__cli-json__stream-event.v1.json"
    ))
    .expect("parse stream event fixture");
    assert_eq!(rendered, fixture);

    assert_eq!(rendered["schema_version"], "coderight.cli.stream_event.v1");
    assert_eq!(rendered["event"], "assistantDelta");
    assert_eq!(rendered["seq"], 7);
    assert_eq!(rendered["session_id"], "s1");
    assert_eq!(rendered["ts"], "2026-06-24T01:02:03Z");
    assert_eq!(rendered["data"]["$kind"], "assistantDelta");
    assert_eq!(rendered["data"]["text"], "hello");
}

// B-16: toolOutputDelta is a live-only projection; `run --stream-json`
// must render it as its own live event from the conduit subscription
// (it is never in the journal).
#[test]
fn render_cli_stream_event_emits_live_tool_output_delta() {
    let event = rightkit_agent_engine_protocol::ProtocolEvent::ToolOutputDelta {
        session_id: "s1".to_string(),
        seq: 4,
        created_at: "2026-06-24T01:02:03Z".to_string(),
        call_id: "c1".to_string(),
        stream: "stdout".to_string(),
        text: "line\n".to_string(),
    };
    assert!(!event.is_durable_fact());

    let rendered = render_cli_stream_event(&event);

    assert_eq!(rendered["event"], "toolOutputDelta");
    assert_eq!(rendered["data"]["callId"], "c1");
    assert_eq!(rendered["data"]["text"], "line\n");
}

#[test]
fn render_cli_stream_event_adds_typed_error_for_failures() {
    let event = rightkit_agent_engine_protocol::ProtocolEvent::RunFailed {
        session_id: "s1".to_string(),
        seq: 8,
        created_at: "2026-06-24T01:02:04Z".to_string(),
        error: "401 Unauthorized".to_string(),
        kind: "auth".to_string(),
        hint: "check your API key".to_string(),
    };

    let rendered = render_cli_stream_event(&event);

    assert_eq!(rendered["event"], "runFailed");
    assert_eq!(rendered["error"]["class"], "auth");
    assert_eq!(rendered["error"]["exit_code"], 3);
    assert_eq!(rendered["error"]["message"], "401 Unauthorized");
}

#[test]
fn cli_stream_event_data_accepts_wrapped_and_legacy_events() {
    let raw = serde_json::json!({"$kind": "usageUpdated", "promptTokens": 3});
    let wrapped = serde_json::json!({
        "schema_version": "coderight.cli.stream_event.v1",
        "data": raw.clone(),
    });

    assert_eq!(cli_stream_event_data(&raw)["$kind"], "usageUpdated");
    assert_eq!(cli_stream_event_data(&wrapped)["$kind"], "usageUpdated");
}

#[test]
fn parse_sse_json_event_kind_reads_data_lines() {
    assert_eq!(
        parse_sse_json_event_kind(
            r#"data: {"$kind":"assistantMessageCompleted","sessionId":"s1"}"#
        ),
        Some("assistantMessageCompleted".to_string())
    );
    assert_eq!(
        parse_sse_json_event_kind(r#"data: {"$kind":"runInterrupted"}"#),
        Some("runInterrupted".to_string())
    );
}

#[test]
fn parse_sse_json_event_kind_ignores_non_payload_lines() {
    assert_eq!(parse_sse_json_event_kind("event: message"), None);
    assert_eq!(parse_sse_json_event_kind("data: [DONE]"), None);
    assert_eq!(parse_sse_json_event_kind(""), None);
}

#[test]
fn terminal_sse_exit_code_marks_conductor_stop_as_failure() {
    assert_eq!(
        terminal_sse_exit_code(
            r#"data: {"$kind":"assistantMessageCompleted","message":{"content":"Conductor run exceeded the 2-minute wall-clock limit and was stopped."}}"#
        ),
        Some(1)
    );
    assert_eq!(
        terminal_sse_exit_code(
            r#"data: {"$kind":"assistantMessageCompleted","message":{"content":"Dry run -- would execute 1 task(s)."}} "#
        ),
        Some(0)
    );
    assert_eq!(
        terminal_sse_exit_code(
            r#"data: {"$kind":"assistantMessageCompleted","message":{"content":"Accepted (GDS 1.00)"}}"#
        ),
        Some(0)
    );
    assert_eq!(
        terminal_sse_exit_code(
            r#"data: {"$kind":"assistantMessageCompleted","message":{"content":"Rejected (GDS 0.00): 1 task(s) failed or unroutable (t1)."}}"#
        ),
        Some(1)
    );
    assert_eq!(
        terminal_sse_exit_code(
            r#"data: {"$kind":"assistantMessageCompleted","message":{"content":"PartiallyComplete (GDS 1.00): independent verification failed."}}"#
        ),
        Some(1)
    );
}
