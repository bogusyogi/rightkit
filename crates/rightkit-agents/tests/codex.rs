#![cfg(feature = "codex")]

mod common;

use rightkit_agents::codex::{
    parse_line, CodexClient, Decision, Incoming, APPROVAL_REQUESTS, KNOWN_NOTIFICATIONS,
    PINNED_CODEX_VERSION,
};
use rightkit_agents::AgentEvent;
use serde_json::Value;

#[test]
fn fake_cli_child() {
    common::run_child_if_requested();
}

fn schema() -> Value {
    let text = std::fs::read_to_string(common::fixture_path("codex/schema-0.161.0.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("schema fixture lacks {key}"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn schema_snapshot_is_pinned_and_covers_what_the_client_uses() {
    let schema = schema();
    assert_eq!(schema["codex_cli_version"], PINNED_CODEX_VERSION);
    let hash = schema["sha256"].as_str().unwrap();
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(schema["file_count"].as_u64().unwrap() > 0);

    let notifications = strings(&schema, "server_notifications");
    for method in KNOWN_NOTIFICATIONS {
        assert!(
            notifications.iter().any(|m| m == method),
            "notification {method} is not in the pinned schema"
        );
    }
    let server_requests = strings(&schema, "server_requests");
    for method in APPROVAL_REQUESTS {
        assert!(
            server_requests.iter().any(|m| m == method),
            "server request {method} is not in the pinned schema"
        );
    }
    let client_requests = strings(&schema, "client_requests");
    for method in [
        "initialize",
        "thread/start",
        "thread/resume",
        "turn/start",
        "turn/steer",
        "turn/interrupt",
    ] {
        assert!(
            client_requests.iter().any(|m| m == method),
            "client request {method} is not in the pinned schema"
        );
    }
}

#[test]
fn every_fixture_line_classifies() {
    for line in common::fixture_lines("codex/approvals.ndjson") {
        parse_line(&line).unwrap_or_else(|e| panic!("{line}: {e}"));
    }
    assert!(parse_line("{}").is_err());
    assert!(parse_line("nope").is_err());
}

#[test]
fn approval_server_requests_carry_an_event() {
    let line = common::fixture_lines("codex/approvals.ndjson")
        .into_iter()
        .find(|l| l.contains("commandExecution/requestApproval"))
        .unwrap();
    match parse_line(&line).unwrap() {
        Incoming::ServerRequest {
            id,
            event:
                Some(AgentEvent::ApprovalRequested {
                    id: eid,
                    tool,
                    input,
                }),
            ..
        } => {
            assert_eq!(id, "srv_1");
            assert_eq!(eid, "srv_1");
            assert_eq!(tool, "item/commandExecution/requestApproval");
            assert_eq!(input["command"], "rm -rf build");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn client_runs_a_recorded_conversation_with_approvals_end_to_end() {
    let (mut child, stdout) = common::spawn_fake("codex", "codex/approvals.ndjson");
    let stdin = child.stdin.take().unwrap();
    let mut client = CodexClient::new(stdout, stdin);

    client.initialize("rightkit-agents-test", "0.0.0").unwrap();
    let thread = client.thread_start(serde_json::json!({})).unwrap();
    assert_eq!(thread, "thr_1");
    let _turn = client.turn_start(&thread, "clean the build").unwrap();

    let mut events = Vec::new();
    let mut echoes = Vec::new();
    let mut backlog = client.drain_pending();
    loop {
        let incoming = if backlog.is_empty() {
            match client.next_incoming().unwrap() {
                Some(incoming) => incoming,
                None => break,
            }
        } else {
            backlog.remove(0)
        };
        match incoming {
            Incoming::ServerRequest { id, event, .. } => {
                events.extend(event);
                client.respond_approval(&id, Decision::Decline).unwrap();
            }
            Incoming::Notification {
                method,
                events: derived,
            } => {
                if method == "fake/echo" {
                    echoes.push(derived);
                } else {
                    events.extend(derived);
                }
            }
            Incoming::Response { .. } => {}
        }
        if echoes.len() == 2 {
            break;
        }
    }
    drop(client);
    child.wait().unwrap();

    let echoed = format!("{echoes:?}");
    assert!(
        echoed.contains("decline"),
        "decision was not written: {echoed}"
    );
    let count = |pred: &dyn Fn(&AgentEvent) -> bool| events.iter().filter(|e| pred(e)).count();
    assert!(matches!(
        events.first(),
        Some(AgentEvent::TextDelta { .. }) | Some(AgentEvent::Init { .. })
    ));
    assert_eq!(
        count(&|e| matches!(e, AgentEvent::ApprovalRequested { .. })),
        2
    );
    assert_eq!(
        count(&|e| matches!(e, AgentEvent::ApprovalResolved { id } if id == "srv_1")),
        1
    );
    assert_eq!(
        count(&|e| matches!(e, AgentEvent::SubagentStarted { .. })),
        1
    );
    assert_eq!(
        count(&|e| matches!(e, AgentEvent::SubagentFinished { .. })),
        1
    );
    assert_eq!(count(&|e| matches!(e, AgentEvent::Compacted)), 1);
    assert_eq!(count(&|e| matches!(e, AgentEvent::Unknown { .. })), 1);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolResult { id, is_error: true, .. } if id == "item_cmd"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::Usage { usage } if usage.input == 120 && usage.output == 40 && usage.cache_read == 60
    )));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Result { is_error: false, subtype, duration_ms: Some(2500), .. }) if subtype == "completed"
    ));
}
