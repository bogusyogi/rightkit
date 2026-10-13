#![cfg(feature = "claude")]

mod common;

use std::io::{BufRead, Write};

use rightkit_agents::claude::{
    build_args, interrupt_frame, user_turn_frame, ClaudeOptions, ConfigMode, StreamParser,
};
use rightkit_agents::AgentEvent;

#[test]
fn fake_cli_child() {
    common::run_child_if_requested();
}

fn parse_fixture(name: &str) -> Vec<AgentEvent> {
    let mut parser = StreamParser::new();
    common::fixture_lines(name)
        .iter()
        .flat_map(|line| parser.parse_line(line).expect("fixture line parses"))
        .collect()
}

#[test]
fn basic_session_maps_init_text_tool_usage_and_result() {
    let events = parse_fixture("claude/basic.ndjson");
    assert!(matches!(
        &events[0],
        AgentEvent::Init { session_id, model: Some(model), tools, permission_mode: Some(mode) }
            if session_id == "11111111-2222-3333-4444-555555555555"
                && model == "claude-sonnet-5-5"
                && tools.contains(&"Task".to_string())
                && mode == "default"
    ));
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["Hel", "lo"]);
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ToolUse { id, name, .. } if id == "toolu_1" && name == "Read")
    ));
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { id, is_error: false, .. } if id == "toolu_1")
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Usage { usage } if usage.input == 10 && usage.output == 5
            && usage.cache_read == 100 && usage.cache_write == 20
    )));
    // An unrecognised message kind degrades to Unknown instead of failing.
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Unknown { raw } if raw["type"] == "future_message_kind")));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Result { is_error: false, num_turns: Some(2), duration_ms: Some(4200), subtype, .. })
            if subtype == "success"
    ));
}

#[test]
fn subagent_calls_are_derived_from_task_tool_use_and_parent_ids() {
    let events = parse_fixture("claude/subagent.ndjson");
    let started = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SubagentStarted { id, description: Some(d), .. } if id == "toolu_task" && d == "Survey the repo"))
        .expect("subagent started");
    let finished = events
        .iter()
        .position(|e| matches!(e, AgentEvent::SubagentFinished { id, is_error: false } if id == "toolu_task"))
        .expect("subagent finished");
    assert!(started < finished);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolUse { id, parent_tool_use_id: Some(p), .. } if id == "toolu_inner" && p == "toolu_task"
    )));
    // The inner tool result must not close the subagent.
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::SubagentFinished { .. }))
            .count(),
        1
    );
}

#[test]
fn permission_denials_are_carried_on_the_result() {
    let events = parse_fixture("claude/permission_denial.ndjson");
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { id, is_error: true, .. } if id == "toolu_rm")
    ));
    match events.last() {
        Some(AgentEvent::Result {
            permission_denials, ..
        }) => {
            assert_eq!(permission_denials.len(), 1);
            assert_eq!(permission_denials[0].tool, "Bash");
            assert_eq!(
                permission_denials[0].tool_use_id.as_deref(),
                Some("toolu_rm")
            );
            assert_eq!(permission_denials[0].input["command"], "rm -rf build");
        }
        other => panic!("expected Result, got {other:?}"),
    }
}

#[test]
fn budget_hit_is_an_error_result_with_cost() {
    let events = parse_fixture("claude/budget_hit.ndjson");
    match events.last() {
        Some(AgentEvent::Result {
            is_error,
            subtype,
            cost_usd: Some(cost),
            ..
        }) => {
            assert!(*is_error);
            assert_eq!(subtype, "error_max_budget_usd");
            assert!(*cost > 1.0);
        }
        other => panic!("expected Result, got {other:?}"),
    }
}

#[test]
fn can_use_tool_control_request_becomes_an_approval_request() {
    let events = parse_fixture("claude/control_approval.ndjson");
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ApprovalRequested { id, tool, input } if id == "req_1" && tool == "Edit" && input["file_path"] == "/work/a.txt"
    )));
}

#[test]
fn malformed_lines_are_errors_and_unknown_kinds_are_not() {
    let mut parser = StreamParser::new();
    assert!(parser.parse_line("not json").is_err());
    assert!(parser.parse_line("[1]").is_err());
    assert!(parser.parse_line(r#"{"no":"type"}"#).is_err());
    assert!(parser.parse_line(r#"{"type":"brand_new"}"#).is_ok());
}

#[test]
fn events_round_trip_through_serde() {
    for event in parse_fixture("claude/subagent.ndjson") {
        let text = serde_json::to_string(&event).unwrap();
        let back: AgentEvent = serde_json::from_str(&text).unwrap();
        assert_eq!(back, event);
    }
}

#[test]
fn writer_frames_are_single_json_lines() {
    let turn = user_turn_frame("hi \"there\"\nnext");
    assert!(turn.ends_with('\n') && turn.matches('\n').count() == 1);
    let value: serde_json::Value = serde_json::from_str(turn.trim_end()).unwrap();
    assert_eq!(value["type"], "user");
    assert_eq!(value["message"]["content"], "hi \"there\"\nnext");
    let interrupt: serde_json::Value =
        serde_json::from_str(interrupt_frame("r1").trim_end()).unwrap();
    assert_eq!(interrupt["request"]["subtype"], "interrupt");
    assert_eq!(interrupt["request_id"], "r1");
}

#[test]
fn argv_full_and_bare_modes() {
    let full = build_args(&ClaudeOptions::default()).unwrap();
    assert!(full.starts_with(&["--print".to_string(), "--input-format".to_string()]));
    assert!(full.contains(&"--verbose".to_string()));
    assert!(!full.contains(&"--bare".to_string()));

    let bare = build_args(&ClaudeOptions {
        config: ConfigMode::Bare,
        model: Some("sonnet".into()),
        resume_session_id: Some("abc".into()),
        allowed_tools: vec!["Read".into(), "Bash(right-ctl pulse *)".into()],
        disallowed_tools: vec!["WebFetch".into()],
        permission_mode: Some("default".into()),
        max_budget_usd: Some(2.5),
        append_system_prompt: Some("be brief".into()),
        ..ClaudeOptions::default()
    })
    .unwrap();
    for expected in [
        "--bare",
        "--model",
        "sonnet",
        "--resume",
        "abc",
        "--max-budget-usd",
        "2.5",
    ] {
        assert!(bare.contains(&expected.to_string()), "missing {expected}");
    }
    let allowed = bare.iter().position(|a| a == "--allowedTools").unwrap();
    assert_eq!(bare[allowed + 2], "Bash(right-ctl pulse *)");
}

#[test]
fn argv_rejects_session_id_with_resume() {
    let result = build_args(&ClaudeOptions {
        session_id: Some("a".into()),
        resume_session_id: Some("b".into()),
        ..ClaudeOptions::default()
    });
    assert!(result.is_err());
}

#[test]
fn fake_cli_replays_a_fixture_over_stdio() {
    let (mut child, stdout) = common::spawn_fake("claude", "claude/subagent.ndjson");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(user_turn_frame("go").as_bytes()).unwrap();
    stdin.flush().unwrap();
    let mut parser = StreamParser::new();
    let mut events = Vec::new();
    for line in stdout.lines() {
        events.extend(parser.parse_line(&line.unwrap()).unwrap());
    }
    drop(stdin);
    child.wait().unwrap();
    assert!(matches!(events.first(), Some(AgentEvent::Init { .. })));
    assert!(matches!(events.last(), Some(AgentEvent::Result { .. })));
}
