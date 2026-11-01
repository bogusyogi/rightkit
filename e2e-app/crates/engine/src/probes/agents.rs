//! rightkit-agents: (1) the Claude Code stream-json protocol over a recorded session (init, tool use, can_use_tool approval,
//! permission denial, result, an unknown kind) through `StreamParser`, with the host-answer path failing closed on timeout;
//! (2) a real Codex `app-server` JSON-RPC conversation over a loopback TCP socket between `CodexClient` and an in-probe
//! server that plays an approval-bearing turn and checks the decisions it receives; (3) vendor CLI discovery on this PATH,
//! reported as observed (installed or not is data, not a failure).
//!
//! Needs: engine dependency on `rightkit-agents` with features `["claude", "codex"]` (default features are empty and the
//! `claude`/`codex` modules do not exist without them). The `session` feature's `Session::spawn` is not exercised: it needs a
//! vendor-CLI stand-in executable, and a stand-in would not be the vendor CLI; add one only as an engine fake-CLI mode.
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rightkit_agents::claude::{
    answer_can_use_tool, build_args, parse_can_use_tool, permission_allow_frame, user_turn_frame, ClaudeOptions,
    ConfigMode, PermissionAnswer, PermissionDecision, StreamParser, NO_ANSWER_MESSAGE,
};
use rightkit_agents::codex::{CodexClient, Decision, Incoming};
use rightkit_agents::discovery::{self, VendorCli};
use rightkit_agents::AgentEvent;
use serde_json::{json, Value};

const SID: &str = "11111111-2222-3333-4444-555555555555";

fn claude_transcript() -> Vec<String> {
    vec![
        json!({"type":"system","subtype":"init","session_id":SID,"model":"claude-sonnet-5-5","cwd":"/work","tools":["Bash","Read","Edit","Task"],"permissionMode":"default"}),
        json!({"type":"assistant","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"Cleaning."},{"type":"tool_use","id":"toolu_rm","name":"Bash","input":{"command":"rm -rf build"}}]},"parent_tool_use_id":null,"session_id":SID}),
        json!({"type":"control_request","request_id":"req_1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf build"},"tool_use_id":"toolu_rm"}}),
        json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_rm","is_error":true,"content":"Permission to use Bash has been denied."}]},"parent_tool_use_id":null,"session_id":SID}),
        json!({"type":"future_message_kind","x":1}),
        json!({"type":"result","subtype":"success","is_error":false,"duration_ms":1500,"num_turns":1,"result":"I could not run that.","total_cost_usd":0.004,"permission_denials":[{"tool_name":"Bash","tool_use_id":"toolu_rm","tool_input":{"command":"rm -rf build"}}],"session_id":SID}),
    ]
    .into_iter()
    .map(|v| v.to_string())
    .collect()
}

fn claude_part() -> Result<Value, String> {
    let mut parser = StreamParser::new();
    let mut events = Vec::new();
    for line in claude_transcript() {
        events.extend(parser.parse_line(&line).map_err(|e| format!("claude line did not parse: {e:?}"))?);
    }
    if !matches!(&events[0], AgentEvent::Init { session_id, model: Some(m), permission_mode: Some(p), .. } if session_id == SID && m == "claude-sonnet-5-5" && p == "default") {
        return Err(format!("first event is not the typed Init: {:?}", events[0]));
    }
    let approval = events.iter().find_map(|e| match e {
        AgentEvent::ApprovalRequested { id, tool, input } => Some((id.clone(), tool.clone(), input.clone())),
        _ => None,
    });
    if approval != Some(("req_1".into(), "Bash".into(), json!({"command": "rm -rf build"}))) {
        return Err(format!("can_use_tool did not become an ApprovalRequested: {approval:?}"));
    }
    if !events.iter().any(|e| matches!(e, AgentEvent::ToolUse { id, name, .. } if id == "toolu_rm" && name == "Bash"))
        || !events.iter().any(|e| matches!(e, AgentEvent::ToolResult { id, is_error: true, .. } if id == "toolu_rm"))
        || !events.iter().any(|e| matches!(e, AgentEvent::Unknown { raw } if raw["type"] == "future_message_kind"))
    {
        return Err("tool use / tool result / unknown-kind events missing".into());
    }
    match events.last() {
        Some(AgentEvent::Result { is_error: false, num_turns: Some(1), duration_ms: Some(1500), subtype, permission_denials, .. })
            if subtype == "success" && permission_denials.len() == 1 && permission_denials[0].tool == "Bash" => {}
        other => return Err(format!("final Result wrong: {other:?}")),
    }
    if parser.parse_line("not json").is_ok() {
        return Err("a non-JSON line parsed".into());
    }

    // Host approval path: the CanUseTool parsed from the wire, answered over a channel; no answer fails closed.
    let request = parse_can_use_tool(&serde_json::from_str::<Value>(&claude_transcript()[2]).unwrap()).ok_or("no CanUseTool parsed")?;
    let (tx, rx) = mpsc::channel();
    tx.send(PermissionAnswer::Allow).unwrap();
    let allow = answer_can_use_tool(&request, Some(rx), Duration::from_secs(1));
    if allow != (PermissionDecision::Allow { updated_input: request.input.clone() }) {
        return Err("a host Allow did not keep the original input".into());
    }
    let frame = allow.frame(&request.request_id);
    let sent: Value = serde_json::from_str(frame.trim_end()).map_err(|e| format!("answer frame is not json: {e}"))?;
    if sent["response"]["request_id"] != "req_1" || sent["response"]["response"]["behavior"] != "allow" || frame != permission_allow_frame("req_1", &request.input) {
        return Err(format!("allow frame wrong: {sent}"));
    }
    let started = Instant::now();
    let (_keep, rx2) = mpsc::channel::<PermissionAnswer>();
    let silent = answer_can_use_tool(&request, Some(rx2), Duration::from_millis(50));
    if silent != (PermissionDecision::Deny { message: NO_ANSWER_MESSAGE.into() }) || started.elapsed() > Duration::from_secs(2) {
        return Err("no host answer did not fail closed within its timeout".into());
    }

    // Writer and argv.
    let turn: Value = serde_json::from_str(user_turn_frame("fix the build").trim_end()).map_err(|e| e.to_string())?;
    if turn["type"] != "user" || turn["message"]["content"] != "fix the build" {
        return Err("user turn frame wrong".into());
    }
    let args = build_args(&ClaudeOptions { config: ConfigMode::Bare, model: Some("sonnet".into()), host_approvals: true, ..ClaudeOptions::default() })
        .map_err(|e| format!("build_args: {e}"))?;
    let has = |a: &str| args.iter().any(|x| x == a);
    if !has("stream-json") || !has("--bare") || !has("--permission-prompt-tool") {
        return Err(format!("argv missing stream-json/--bare/--permission-prompt-tool: {args:?}"));
    }
    if build_args(&ClaudeOptions { model: Some("--dangerously-skip-permissions".into()), ..ClaudeOptions::default() }).is_ok() {
        return Err("an option-shaped model reached argv".into());
    }
    Ok(json!({"events": events.len(), "denials": 1, "argv": args.len()}))
}

/// The in-probe `codex app-server`: JSON lines over a socket, one approval-bearing turn. Returns the decisions it received.
fn codex_server(stream: TcpStream) -> Result<Vec<String>, String> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut out = stream;
    let mut send = |v: Value| -> Result<(), String> { writeln!(out, "{v}").and_then(|_| out.flush()).map_err(|e| format!("server write: {e}")) };
    let mut decisions = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).map_err(|e| format!("server read: {e}"))? == 0 {
            return Ok(decisions);
        }
        let msg: Value = serde_json::from_str(line.trim()).map_err(|e| format!("client sent non-json: {e}"))?;
        let (id, method) = (msg.get("id").cloned(), msg["method"].as_str().unwrap_or("").to_string());
        match (method.as_str(), id) {
            ("initialize", Some(id)) => send(json!({"id": id, "result": {"userAgent": "codex/0.161.0"}}))?,
            ("thread/start", Some(id)) => {
                send(json!({"method": "thread/started", "params": {"thread": {"id": "thr_1", "model": "gpt-5.6"}}}))?;
                send(json!({"id": id, "result": {"thread": {"id": "thr_1"}}}))?;
            }
            ("turn/start", Some(id)) => {
                if msg["params"]["threadId"] != "thr_1" || msg["params"]["input"][0]["text"] != "clean the build" {
                    return Err(format!("turn/start params wrong: {}", msg["params"]));
                }
                send(json!({"method": "turn/started", "params": {"threadId": "thr_1", "turn": {"id": "turn_1", "status": "inProgress"}}}))?;
                send(json!({"id": id, "result": {}}))?;
                send(json!({"method": "item/agentMessage/delta", "params": {"threadId": "thr_1", "turnId": "turn_1", "itemId": "i0", "delta": "Run"}}))?;
                send(json!({"method": "item/started", "params": {"threadId": "thr_1", "turnId": "turn_1", "item": {"type": "commandExecution", "id": "item_cmd", "command": "rm -rf build", "cwd": "/work", "status": "inProgress"}}}))?;
                for (rid, method, params) in [
                    ("srv_1", "item/commandExecution/requestApproval", json!({"threadId": "thr_1", "turnId": "turn_1", "itemId": "item_cmd", "command": "rm -rf build", "cwd": "/work"})),
                    ("srv_2", "item/fileChange/requestApproval", json!({"threadId": "thr_1", "turnId": "turn_1", "itemId": "item_fc"})),
                ] {
                    send(json!({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}))?;
                    let mut answer = String::new();
                    if reader.read_line(&mut answer).map_err(|e| format!("server read answer: {e}"))? == 0 {
                        return Err("client hung up before answering an approval".into());
                    }
                    let a: Value = serde_json::from_str(answer.trim()).map_err(|e| e.to_string())?;
                    if a["id"] != rid {
                        return Err(format!("approval answered for the wrong id: {a}"));
                    }
                    decisions.push(a["result"]["decision"].as_str().unwrap_or("?").to_string());
                    send(json!({"method": "serverRequest/resolved", "params": {"threadId": "thr_1", "requestId": rid}}))?;
                }
                send(json!({"method": "thread/tokenUsage/updated", "params": {"threadId": "thr_1", "turnId": "turn_1", "tokenUsage": {"total": {"inputTokens": 120, "outputTokens": 40, "cachedInputTokens": 60, "cacheWriteInputTokens": 0, "reasoningOutputTokens": 5, "totalTokens": 160}}}}))?;
                send(json!({"method": "some/future/notification", "params": {"x": 1}}))?;
                send(json!({"method": "turn/completed", "params": {"threadId": "thr_1", "turn": {"id": "turn_1", "status": "completed", "durationMs": 2500}}}))?;
                return Ok(decisions);
            }
            _ => {}
        }
    }
}

fn codex_part() -> Result<Value, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let server = std::thread::spawn(move || -> Result<Vec<String>, String> {
        let (stream, _) = listener.accept().map_err(|e| format!("accept: {e}"))?;
        codex_server(stream)
    });
    let stream = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let mut client = CodexClient::new(BufReader::new(stream.try_clone().map_err(|e| e.to_string())?), stream);

    let init = client.initialize("rke2e", "0.0.0").map_err(|e| format!("initialize: {e}"))?;
    if init["userAgent"] != "codex/0.161.0" {
        return Err(format!("initialize result wrong: {init}"));
    }
    let thread = client.thread_start(json!({})).map_err(|e| format!("thread/start: {e}"))?;
    if thread != "thr_1" {
        return Err(format!("thread id {thread:?}"));
    }
    client.turn_start(&thread, "clean the build").map_err(|e| format!("turn/start: {e}"))?;

    let mut events: Vec<AgentEvent> = Vec::new();
    let mut backlog = client.drain_pending();
    let mut approvals = 0;
    loop {
        let incoming = if backlog.is_empty() {
            match client.next_incoming().map_err(|e| format!("next_incoming: {e}"))? {
                Some(i) => i,
                None => break,
            }
        } else {
            backlog.remove(0)
        };
        match incoming {
            Incoming::ServerRequest { id, event, .. } => {
                events.extend(event);
                approvals += 1;
                client
                    .respond_approval(&id, if approvals == 1 { Decision::Decline } else { Decision::Accept })
                    .map_err(|e| format!("respond_approval: {e}"))?;
            }
            Incoming::Notification { events: derived, .. } => events.extend(derived),
            Incoming::Response { .. } => {}
        }
        if matches!(events.last(), Some(AgentEvent::Result { .. })) {
            break;
        }
    }
    drop(client);
    let decisions = server.join().map_err(|_| "codex server thread panicked")??;
    if decisions != ["decline", "accept"] {
        return Err(format!("the server received decisions {decisions:?}, expected [decline, accept]"));
    }
    let n = |f: &dyn Fn(&AgentEvent) -> bool| events.iter().filter(|e| f(e)).count();
    if n(&|e| matches!(e, AgentEvent::ApprovalRequested { .. })) != 2
        || n(&|e| matches!(e, AgentEvent::ApprovalResolved { id } if id == "srv_1")) != 1
        || !events.iter().any(|e| matches!(e, AgentEvent::ApprovalRequested { id, input, .. } if id == "srv_1" && input["command"] == "rm -rf build"))
    {
        return Err(format!("approval events wrong: {events:?}"));
    }
    if !events.iter().any(|e| matches!(e, AgentEvent::Usage { usage } if usage.input == 120 && usage.output == 40 && usage.cache_read == 60)) {
        return Err("token usage did not map".into());
    }
    if n(&|e| matches!(e, AgentEvent::Unknown { .. })) != 1 {
        return Err("the unknown notification did not degrade to one Unknown event".into());
    }
    match events.last() {
        Some(AgentEvent::Result { is_error: false, subtype, duration_ms: Some(2500), .. }) if subtype == "completed" => {}
        other => return Err(format!("turn result wrong: {other:?}")),
    }
    Ok(json!({"decisions": decisions, "events": events.len()}))
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    let claude = claude_part().map_err(|e| format!("claude protocol: {e}"))?;
    let codex = codex_part().map_err(|e| format!("codex app-server: {e}"))?;
    let status = |k: VendorCli| {
        let s = discovery::probe(k, Duration::from_secs(5));
        json!({"installed": s.installed, "path": s.path.map(|p| p.display().to_string()), "version": s.version, "signed_in": s.signed_in})
    };
    Ok(json!({
        "claude_protocol": claude,
        "codex_rpc": codex,
        "discovery": {"claude": status(VendorCli::Claude), "codex": status(VendorCli::Codex)},
    }))
}
