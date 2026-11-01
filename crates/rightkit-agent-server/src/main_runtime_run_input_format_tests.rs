fn parse_line(json: &str) -> Result<RunInputEvent, serde_json::Error> {
    serde_json::from_str::<RunInputEvent>(json)
}

#[test]
fn round_trip_user_message() {
    let original = RunInputEvent::UserMessage {
        text: "hi".to_string(),
        nonce: Some("n-1".to_string()),
        trust: None,
    };
    let wire = serde_json::to_string(&original).expect("serialize");
    assert!(wire.contains("\"type\":\"user_message\""), "got {wire}");
    assert!(wire.contains("\"text\":\"hi\""));
    let recovered: RunInputEvent = serde_json::from_str(&wire).expect("deserialize");
    assert_eq!(recovered, original);
    let parsed = parse_line(r#"{"type":"user_message","text":"hi","nonce":"abc"}"#)
        .expect("parse user_message");
    assert_eq!(
        parsed,
        RunInputEvent::UserMessage {
            text: "hi".to_string(),
            nonce: Some("abc".to_string()),
            trust: None,
        }
    );
    // `trust: null` deserializes to `None` and the field is omitted on
    // re-serialization (verified by the round-trip via `wire` above).
    let parsed_null_trust =
        parse_line(r#"{"type":"user_message","text":"hi","nonce":"n-1","trust":null}"#)
            .expect("parse user_message with explicit null trust");
    let RunInputEvent::UserMessage { text, nonce, trust } = parsed_null_trust else {
        panic!("expected UserMessage");
    };
    assert_eq!(text, "hi");
    assert_eq!(nonce.as_deref(), Some("n-1"));
    assert!(trust.is_none());
}

#[test]
fn round_trip_tool_result_and_control() {
    let tool = RunInputEvent::ToolResult {
        tool_call_id: "call_1".to_string(),
        summary: "ok".to_string(),
        is_error: false,
        process_id: None,
    };
    let tool_wire = serde_json::to_string(&tool).unwrap();
    assert!(tool_wire.contains("\"type\":\"tool_result\""));
    assert!(tool_wire.contains("\"tool_call_id\":\"call_1\""));
    assert!(tool_wire.contains("\"is_error\":false"));
    assert_eq!(
        serde_json::from_str::<RunInputEvent>(&tool_wire).unwrap(),
        tool
    );
    // is_error=true round-trips and the optional process_id field is
    // skipped-on-None and read-when-present (primitive field path).
    let parsed_tool = parse_line(
            r#"{"type":"tool_result","tool_call_id":"x","summary":"err","is_error":true,"process_id":"p-7"}"#,
        )
        .expect("parse tool_result with process_id");
    assert_eq!(
        parsed_tool,
        RunInputEvent::ToolResult {
            tool_call_id: "x".to_string(),
            summary: "err".to_string(),
            is_error: true,
            process_id: Some("p-7".to_string()),
        }
    );

    // Control round-trips for both verbs.
    let abort = RunInputEvent::Control {
        action: ControlAction::Abort,
        reason: Some("user pressed ctrl-c".to_string()),
    };
    let abort_wire = serde_json::to_string(&abort).unwrap();
    assert!(abort_wire.contains("\"type\":\"control\""));
    assert!(abort_wire.contains("\"action\":\"abort\""));
    assert_eq!(
        serde_json::from_str::<RunInputEvent>(&abort_wire).unwrap(),
        abort
    );

    let ping = parse_line(r#"{"type":"control","action":"ping"}"#).expect("parse control ping");
    assert_eq!(
        ping,
        RunInputEvent::Control {
            action: ControlAction::Ping,
            reason: None,
        }
    );
}

#[test]
fn small_jsonl_script_drives_loop_in_order() {
    // Realistic 5-line script: lead user_message seeds the loop, the rest
    // are accounted for. Verifies (a) physical line numbers preserved, (b)
    // first user_message seeds the initial prompt, (c) abort detected at
    // its physical line, (d) summary collapses the run-facing fields
    // without losing order.
    let script = "\
{\"type\":\"user_message\",\"text\":\"first turn\"}\n\
{\"type\":\"user_message\",\"text\":\"follow-up\"}\n\
{\"type\":\"control\",\"action\":\"ping\"}\n\
{\"type\":\"tool_result\",\"tool_call_id\":\"c1\",\"summary\":\"ok\"}\n\
{\"type\":\"control\",\"action\":\"abort\",\"reason\":\"enough\"}\n";
    let events = drain_jsonl_script(Cursor::new(script)).expect("drain clean script");
    assert_eq!(
        events.iter().map(|(l, _)| l).copied().collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5]
    );
    let (init_line, init_text) = pick_initial_prompt(&events).expect("initial prompt");
    assert_eq!(init_line, 1);
    assert_eq!(init_text, "first turn");
    let (abort_line, reason) = detect_abort(&events).expect("abort detected");
    assert_eq!(abort_line, 5);
    assert_eq!(reason.as_deref(), Some("enough"));
    let (prompt, summary) = summarize_script(&events).expect("summary");
    assert_eq!(prompt, "first turn");
    assert_eq!(summary.event_count, 5);
    assert_eq!(summary.initial_prompt_line, 1);
    assert_eq!(summary.abort_line, Some(5));
    assert_eq!(summary.trailing_event_count, 4);

    // Drive hook also yields events in physical order — guard the binding
    // used by the bin caller that takes FnMut closures.
    let mut seen_lines: Vec<usize> = Vec::new();
    let mut seen_kinds: Vec<&'static str> = Vec::new();
    drive_jsonl_script(Cursor::new(script), |line, ev| {
        seen_lines.push(line);
        seen_kinds.push(match ev {
            RunInputEvent::UserMessage { .. } => "user_message",
            RunInputEvent::ToolResult { .. } => "tool_result",
            RunInputEvent::Control { .. } => "control",
        });
    })
    .expect("drive ok");
    assert_eq!(seen_lines, vec![1, 2, 3, 4, 5]);
    assert_eq!(
        seen_kinds,
        vec![
            "user_message",
            "user_message",
            "control",
            "tool_result",
            "control"
        ]
    );
}

#[test]
fn malformed_line_rejected_with_line_number() {
    // Line 3 is intentionally bad: missing the discriminator.
    let script = "\
{\"type\":\"user_message\",\"text\":\"one\"}\n\
{\"type\":\"tool_result\",\"tool_call_id\":\"c\",\"summary\":\"ok\"}\n\
{\"type\":\"oops\"}\n\
{\"type\":\"control\",\"action\":\"abort\"}\n";
    let err = drain_jsonl_script(Cursor::new(script)).expect_err("malformed script should error");
    assert_eq!(err.line, 3, "physical line 3 must be tagged");
    assert_eq!(err.raw, "{\"type\":\"oops\"}");
    let rendered = err.to_string();
    assert!(
        rendered.contains("line 3"),
        "operator-facing message must cite the bad line: {rendered}"
    );
    assert!(rendered.contains("oops"));
    // ParseError implements std::error::Error — needed for clap error chains.
    fn assert_error<E: std::error::Error>(_: &E) {}
    assert_error(&err);
}

#[test]
fn script_without_user_message_is_a_usage_contract() {
    // Edge case: valid JSONL but no user_message. Parser still drains
    // cleanly; the absence surfaces as pick_initial_prompt = None, which
    // run_session will map to CliExitCode::Usage. Pin that contract here
    // so the bin's translation doesn't regress.
    let script = "\
{\"type\":\"control\",\"action\":\"abort\"}\n\
{\"type\":\"tool_result\",\"tool_call_id\":\"c\",\"summary\":\"ok\"}\n";
    let events =
        drain_jsonl_script(Cursor::new(script)).expect("drain still ok if no user_message");
    assert!(pick_initial_prompt(&events).is_none());
    let abort = detect_abort(&events).expect("abort still detected");
    assert_eq!(abort.0, 1);
    assert!(summarize_script(&events).is_none());
}

#[test]
fn blank_lines_and_tabs_in_whitespace_are_skipped() {
    // Blank lines must NOT bump event_count but DO advance physical
    // line counting — verified by putting the first event on physical
    // line 3 with two leading blanks. This protects downstream
    // operators who hand-write JSONL with section breaks.
    let script = "\n\n{\"type\":\"user_message\",\"text\":\"hi\"}\n\n";
    let events = drain_jsonl_script(Cursor::new(script)).expect("drain with blanks");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, 3, "first event should be at physical line 3");
}

#[test]
fn unknown_discriminator_is_rejected_not_silently_dropped() {
    // Forward-compatibility assertion: unknown `type` values are a
    // parse error (line number points at the bad line) rather than a
    // silent drop. This guards silent schema drift.
    let script = "{\"type\":\"future_event\",\"v\":1}\n";
    let err = drain_jsonl_script(Cursor::new(script)).expect_err("unknown type must reject");
    assert_eq!(err.line, 1);
    assert!(
        err.message.contains("future_event") || err.message.contains("unknown"),
        "diagnostic must mention the bad tag: {}",
        err.message
    );
}

#[test]
fn live_reader_emits_before_eof_and_closes_after_drain() {
    let mut input = spawn_jsonl_reader(
        Cursor::new(
            br#"{"type":"user_message","text":"first"}
{"type":"control","action":"ping"}
"#
            .to_vec(),
        ),
        2,
    );
    let first = input
        .blocking_recv()
        .expect("first event before EOF")
        .expect("valid first event");
    assert_eq!(first.0, 1);
    assert!(matches!(first.1, RunInputEvent::UserMessage { .. }));
    let second = input
        .blocking_recv()
        .expect("second event")
        .expect("valid second event");
    assert_eq!(second.0, 2);
    assert!(matches!(second.1, RunInputEvent::Control { .. }));
    assert!(input.blocking_recv().is_none(), "EOF must close stream");
}

#[test]
fn live_reader_rejects_oversized_unterminated_line() {
    let oversized = vec![b'x'; MAX_JSONL_LINE_BYTES + 1];
    let mut input = spawn_jsonl_reader(Cursor::new(oversized), 1);
    let error = input
        .blocking_recv()
        .expect("oversize error")
        .expect_err("oversize line must be refused");
    assert_eq!(error.line, 1);
    assert!(error.message.contains("exceeds"));
}

#[test]
fn live_reader_preserves_utf8_across_reader_buffer_boundaries() {
    let script = r#"{"type":"user_message","text":"café"}
"#
    .as_bytes();
    let reader = std::io::BufReader::with_capacity(1, Cursor::new(script));
    let mut input = spawn_jsonl_reader(reader, 1);
    let event = input
        .blocking_recv()
        .expect("UTF-8 event")
        .expect("valid UTF-8 event");
    let RunInputEvent::UserMessage { text, .. } = event.1 else {
        panic!("expected user message");
    };
    assert_eq!(text, "café");
}
