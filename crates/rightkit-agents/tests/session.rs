#![cfg(feature = "session")]
//! Session lifecycle tests against a scripted fake CLI: the test binary re-executes itself
//! (`fake_cli_child`) and plays the vendor's side of the wire. The real `claude` and `codex`
//! are never run.

use std::io::{BufRead, Write};
use std::time::Duration;

use rightkit_agents::claude::ClaudeOptions;
use rightkit_agents::codex::CodexOptions;
use rightkit_agents::session::{
    ApprovalDecision, Backend, CommandOverride, Session, SessionConfig, SessionError, SessionEvent,
};
use rightkit_agents::AgentEvent;
use serde_json::{json, Value};

const KIND_ENV: &str = "RK_FAKE_SESSION_KIND";
const WAIT: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------- the fake CLI (child side)

#[test]
fn fake_cli_child() {
    let Ok(kind) = std::env::var(KIND_ENV) else {
        return;
    };
    // libtest has printed `test fake_cli_child ... ` without a newline; end that line so the
    // first protocol frame stands alone. The session surfaces the banner as non-JSON lines.
    println!();
    match kind.as_str() {
        "claude" => claude_child(),
        "codex" => codex_child(),
        "hang" => loop {
            // Ignores stdin entirely: only a forced tree kill ends it.
            std::thread::sleep(Duration::from_secs(1));
        },
        _ => {}
    }
    std::process::exit(0);
}

fn emit(value: Value) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}

fn stdin_lines() -> impl Iterator<Item = String> {
    let stdin = std::io::stdin();
    std::iter::from_fn(move || {
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim().to_string()),
        }
    })
}

fn claude_child() {
    for line in stdin_lines() {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match value["type"].as_str() {
            Some("user") => {
                emit(json!({"type":"system","subtype":"init","session_id":"sess_f","model":"fake","tools":["Edit"],"permissionMode":"default"}));
                emit(json!({"type":"control_request","request_id":"req_1","request":{"subtype":"can_use_tool","tool_name":"Edit","input":{"file_path":"/w/a.txt"}}}));
            }
            Some("control_response") => {
                emit(json!({"type":"fake_echo","line":line}));
                emit(json!({"type":"result","subtype":"success","is_error":false,"duration_ms":1,"num_turns":1,"total_cost_usd":0.0,"permission_denials":[]}));
            }
            Some("control_request") => emit(json!({"type":"fake_echo","line":line})),
            _ => {}
        }
    }
}

fn codex_child() {
    for line in stdin_lines() {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = value.get("id").cloned();
        let method = value.get("method").and_then(Value::as_str);
        match (id, method) {
            (Some(id), Some("initialize")) => emit(json!({"id":id,"result":{"userAgent":"fake"}})),
            (Some(id), Some("thread/start")) => {
                emit(json!({"id":id,"result":{"thread":{"id":"thr_f"}}}))
            }
            (Some(id), Some("thread/resume")) => {
                let thread = value["params"]["threadId"].clone();
                emit(json!({"id":id,"result":{"thread":{"id":thread}}}))
            }
            (Some(id), Some("turn/start")) => {
                let text = value["params"]["input"][0]["text"].as_str().unwrap_or("");
                emit(json!({"id":id,"result":{"turn":{"id":"turn_f"}}}));
                emit(json!({"method":"turn/started","params":{"threadId":"thr_f","turn":{"id":"turn_f","status":"inProgress"}}}));
                if text.contains("ask") {
                    emit(json!({"jsonrpc":"2.0","id":"srv_1","method":"item/commandExecution/requestApproval","params":{"threadId":"thr_f","turnId":"turn_f","itemId":"i1","command":"rm -rf build"}}));
                } else if text.contains("other") {
                    emit(json!({"jsonrpc":"2.0","id":"srv_x","method":"x/unsupported","params":{}}));
                } else {
                    finish_turn("completed");
                }
            }
            (Some(id), Some("turn/interrupt")) => {
                emit(json!({"id":id,"result":{}}));
                emit(json!({"method":"fake/echo","params":{"line":line}}));
                finish_turn("interrupted");
            }
            (Some(_), None) => {
                // A response to one of our server requests: echo it so the test can see it.
                emit(json!({"method":"fake/echo","params":{"line":line}}));
                if value["id"] == "srv_1" {
                    emit(json!({"method":"serverRequest/resolved","params":{"threadId":"thr_f","requestId":"srv_1"}}));
                    finish_turn("completed");
                }
            }
            _ => {}
        }
    }
}

fn finish_turn(status: &str) {
    emit(json!({"method":"item/agentMessage/delta","params":{"threadId":"thr_f","turnId":"turn_f","itemId":"m","delta":"hi"}}));
    emit(json!({"method":"turn/completed","params":{"threadId":"thr_f","turn":{"id":"turn_f","status":status,"durationMs":5}}}));
}

// ---------------------------------------------------------------- the test side

fn config(backend: Backend, kind: &str) -> SessionConfig {
    let mut config = SessionConfig::new(backend);
    config.command = Some(CommandOverride {
        program: std::env::current_exe().expect("current exe"),
        args: ["fake_cli_child", "--exact", "--nocapture", "--test-threads=1"]
            .iter()
            .map(Into::into)
            .collect(),
    });
    config.env = vec![(KIND_ENV.into(), kind.into())];
    config.startup_timeout = WAIT;
    config.teardown_grace = Duration::from_secs(5);
    config
}

fn claude() -> Session {
    Session::spawn(config(Backend::Claude(ClaudeOptions::default()), "claude")).expect("spawn")
}

fn codex() -> Session {
    Session::spawn(config(Backend::Codex(CodexOptions::default()), "codex")).expect("spawn")
}

/// Collect events until `done` matches one (inclusive). Panics on timeout or `Closed`.
fn collect_until(session: &Session, done: impl Fn(&SessionEvent) -> bool) -> Vec<SessionEvent> {
    let mut seen = Vec::new();
    loop {
        let event = session
            .events()
            .recv_timeout(WAIT)
            .unwrap_or_else(|error| panic!("no event ({error}); saw {seen:#?}"));
        let finished = done(&event);
        let closed = event == SessionEvent::Closed;
        seen.push(event);
        if finished {
            return seen;
        }
        assert!(!closed, "stream closed early: {seen:#?}");
    }
}

fn echo_line(event: &SessionEvent) -> Option<&str> {
    match event {
        SessionEvent::Agent(AgentEvent::Unknown { raw }) => raw
            .get("line")
            .or_else(|| raw.pointer("/params/line"))
            .and_then(Value::as_str),
        _ => None,
    }
}

fn is_echo(event: &SessionEvent, needle: &str) -> bool {
    echo_line(event).is_some_and(|line| line.contains(needle))
}

fn is_approval(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Agent(AgentEvent::ApprovalRequested { .. })
    )
}

fn is_result(event: &SessionEvent) -> bool {
    matches!(event, SessionEvent::Agent(AgentEvent::Result { .. }))
}

fn has_resolved(events: &[SessionEvent], wanted: &str) -> bool {
    events.iter().any(|event| {
        matches!(event, SessionEvent::Agent(AgentEvent::ApprovalResolved { id }) if id == wanted)
    })
}

fn clean_shutdown(session: Session) {
    let info = session.shutdown(None).expect("shutdown");
    assert!(!info.forced, "fake exits on stdin close, no force needed");
    assert!(info.status.success());
}

// ---- Claude

#[test]
fn claude_approval_allow_round_trip() {
    let session = claude();
    session.send_user_message("go").unwrap();
    let events = collect_until(&session, is_approval);
    assert!(matches!(
        events.last(),
        Some(SessionEvent::Agent(AgentEvent::ApprovalRequested { id, tool, input }))
            if id == "req_1" && tool == "Edit" && input["file_path"] == "/w/a.txt"
    ));
    assert_eq!(session.session_id().as_deref(), Some("sess_f"));
    assert_eq!(session.pending_approvals(), vec!["req_1".to_string()]);

    session.approve("req_1", ApprovalDecision::Allow).unwrap();
    assert!(session.pending_approvals().is_empty());
    let events = collect_until(&session, is_result);
    assert!(has_resolved(&events, "req_1"));
    assert!(events
        .iter()
        .any(|e| is_echo(e, "\"behavior\":\"allow\"") && is_echo(e, "\"request_id\":\"req_1\"")));
    assert!(events.iter().any(|e| is_echo(e, "\"updatedInput\"")));
    clean_shutdown(session);
}

#[test]
fn claude_deny_carries_message_and_interrupt_flag() {
    let session = claude();
    session.send_user_message("go").unwrap();
    collect_until(&session, is_approval);
    session.deny("req_1", Some("not now"), true).unwrap();
    let events = collect_until(&session, is_result);
    assert!(events.iter().any(|e| is_echo(e, "\"behavior\":\"deny\"")
        && is_echo(e, "\"message\":\"not now\"")
        && is_echo(e, "\"interrupt\":true")));
    clean_shutdown(session);
}

#[test]
fn answering_an_unknown_or_already_answered_approval_is_an_error() {
    let session = claude();
    assert!(matches!(
        session.approve("nope", ApprovalDecision::Allow),
        Err(SessionError::UnknownApproval(id)) if id == "nope"
    ));
    session.send_user_message("go").unwrap();
    collect_until(&session, is_approval);
    session.approve("req_1", ApprovalDecision::Allow).unwrap();
    assert!(matches!(
        session.approve("req_1", ApprovalDecision::Allow),
        Err(SessionError::UnknownApproval(_))
    ));
    clean_shutdown(session);
}

#[test]
fn claude_cancel_sends_interrupt_and_releases_pending_approvals() {
    let session = claude();
    session.send_user_message("go").unwrap();
    collect_until(&session, is_approval);
    assert!(session.cancel().unwrap());
    assert!(session.pending_approvals().is_empty());
    let events = collect_until(&session, |e| is_echo(e, "\"subtype\":\"interrupt\""));
    assert!(events.iter().any(|e| is_echo(e, "\"type\":\"control_request\"")));
    clean_shutdown(session);
}

// ---- Codex

#[test]
fn codex_handshake_yields_the_thread_id_before_spawn_returns() {
    let session = codex();
    assert_eq!(session.session_id().as_deref(), Some("thr_f"));
    clean_shutdown(session);
}

#[test]
fn codex_plain_turn_streams_text_and_a_result() {
    let session = codex();
    session.send_user_message("hello").unwrap();
    let events = collect_until(&session, is_result);
    assert!(events.iter().any(
        |e| matches!(e, SessionEvent::Agent(AgentEvent::TextDelta { text }) if text == "hi")
    ));
    assert!(matches!(
        events.last(),
        Some(SessionEvent::Agent(AgentEvent::Result { subtype, .. })) if subtype == "completed"
    ));
    clean_shutdown(session);
}

fn codex_decision(decision: ApprovalDecision, wire: &str) {
    let session = codex();
    session.send_user_message("ask").unwrap();
    let events = collect_until(&session, is_approval);
    assert!(matches!(
        events.last(),
        Some(SessionEvent::Agent(AgentEvent::ApprovalRequested { id, tool, input }))
            if id == "srv_1"
                && tool == "item/commandExecution/requestApproval"
                && input["command"] == "rm -rf build"
    ));
    session.approve("srv_1", decision).unwrap();
    let events = collect_until(&session, is_result);
    assert!(
        events
            .iter()
            .any(|e| is_echo(e, &format!("\"decision\":\"{wire}\""))),
        "no {wire} on the wire: {events:#?}"
    );
    assert!(has_resolved(&events, "srv_1"), "server announces resolution");
    clean_shutdown(session);
}

#[test]
fn codex_allow_maps_to_accept() {
    codex_decision(ApprovalDecision::Allow, "accept");
}

#[test]
fn codex_allow_for_session_maps_to_accept_for_session() {
    codex_decision(ApprovalDecision::AllowForSession, "acceptForSession");
}

#[test]
fn codex_deny_maps_to_decline() {
    codex_decision(
        ApprovalDecision::Deny {
            message: Some("no".into()),
            interrupt: false,
        },
        "decline",
    );
}

#[test]
fn codex_deny_with_interrupt_maps_to_cancel() {
    codex_decision(
        ApprovalDecision::Deny {
            message: None,
            interrupt: true,
        },
        "cancel",
    );
}

#[test]
fn codex_cancel_releases_approvals_then_interrupts_the_turn() {
    let session = codex();
    assert!(!session.cancel().unwrap(), "no turn yet, nothing to interrupt");
    session.send_user_message("ask").unwrap();
    collect_until(&session, is_approval);
    assert!(session.cancel().unwrap());
    assert!(session.pending_approvals().is_empty());
    let events = collect_until(&session, |e| {
        matches!(e, SessionEvent::Agent(AgentEvent::Result { subtype, .. }) if subtype == "interrupted")
    });
    assert!(events.iter().any(|e| is_echo(e, "\"decision\":\"cancel\"")));
    assert!(events
        .iter()
        .any(|e| is_echo(e, "turn/interrupt") && is_echo(e, "turn_f")));
    clean_shutdown(session);
}

#[test]
fn codex_refuses_server_requests_it_does_not_implement() {
    let session = codex();
    session.send_user_message("other").unwrap();
    let events = collect_until(&session, |e| is_echo(e, "-32601"));
    assert!(events.iter().any(|e| matches!(
        e,
        SessionEvent::Agent(AgentEvent::Unknown { raw }) if raw["method"] == "x/unsupported"
    )));
    assert!(session.pending_approvals().is_empty());
    clean_shutdown(session);
}

#[test]
fn codex_attach_resumes_an_existing_thread() {
    let options = CodexOptions {
        resume_thread_id: Some("thr_old".into()),
        ..CodexOptions::default()
    };
    let session = Session::spawn(config(Backend::Codex(options), "codex")).expect("spawn");
    assert_eq!(session.session_id().as_deref(), Some("thr_old"));
    clean_shutdown(session);
}

// ---- lifecycle

#[test]
fn startup_failure_is_reported_and_leaves_no_session() {
    let mut cfg = config(Backend::Codex(CodexOptions::default()), "exit");
    cfg.startup_timeout = Duration::from_secs(10);
    assert!(matches!(
        Session::spawn(cfg),
        Err(SessionError::Startup(_))
    ));
}

#[test]
fn conflicting_claude_options_are_rejected_before_spawn() {
    let options = ClaudeOptions {
        session_id: Some("a".into()),
        resume_session_id: Some("b".into()),
        ..ClaudeOptions::default()
    };
    assert!(matches!(
        Session::spawn(config(Backend::Claude(options), "claude")),
        Err(SessionError::Config(_))
    ));
}

#[test]
fn shutdown_force_kills_a_child_that_ignores_stdin_close() {
    let mut session =
        Session::spawn(config(Backend::Claude(ClaudeOptions::default()), "hang")).expect("spawn");
    assert!(session.pid() > 0);
    assert!(session.exit_status().unwrap().is_none(), "still running");
    let info = session.shutdown(Some(Duration::from_millis(300))).unwrap();
    assert!(info.forced);
}

#[test]
fn stream_reports_closed_when_the_cli_exits() {
    let session = Session::spawn(config(Backend::Claude(ClaudeOptions::default()), "exit"))
        .expect("spawn");
    collect_until(&session, |e| *e == SessionEvent::Closed);
    session.shutdown(None).unwrap();
}
