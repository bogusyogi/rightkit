//! Anthropic Messages wire journey: a real daemon, a direct
//! `anthropic_compatible` inference connection, and an independent scripted
//! Anthropic-style SSE server. Covers request shape, streaming of thinking /
//! text / fragmented tool input, usage + prompt-cache accounting, and the
//! tool_use -> tool_result round trip with thinking replay.
//!
//! Retires `coderight-providers` anthropic_client / replay / prompt_cache unit
//! tests. Evidence map: provider-lane streaming + cache atoms, HCTX-001.

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Response, Server};

const MODEL_ID: &str = "anthropic/claude-sonnet-4.5";

struct Anthropic {
    port: u16,
    seen: Arc<Mutex<Vec<(Value, Vec<(String, String)>)>>>,
    _server: Arc<Server>,
}

fn ev(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn turn_one() -> String {
    let input = json!({"path": "."}).to_string();
    let (a, b) = input.split_at(4);
    [
        ev("message_start", json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude","content":[],
            "usage":{"input_tokens":1200,"cache_read_input_tokens":900,"cache_creation_input_tokens":100,"output_tokens":1}}})),
        ev("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}})),
        ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"ANTHROPIC-THINKING-9"}})),
        ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-abc"}})),
        ev("content_block_stop", json!({"type":"content_block_stop","index":0})),
        ev("content_block_start", json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"list_files","input":{}}})),
        ev("content_block_delta", json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":a}})),
        ev("content_block_delta", json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":b}})),
        ev("content_block_stop", json!({"type":"content_block_stop","index":1})),
        ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}})),
        ev("message_stop", json!({"type":"message_stop"})),
    ]
    .concat()
}

fn turn_two() -> String {
    [
        ev("message_start", json!({"type":"message_start","message":{"id":"msg_2","type":"message","role":"assistant","model":"claude","content":[],
            "usage":{"input_tokens":1500,"cache_read_input_tokens":1300,"cache_creation_input_tokens":0,"output_tokens":1}}})),
        ev("content_block_start", json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})),
        ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"anthropic "}})),
        ev("content_block_delta", json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"final answer"}})),
        ev("content_block_stop", json!({"type":"content_block_stop","index":0})),
        ev("message_delta", json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}})),
        ev("message_stop", json!({"type":"message_stop"})),
    ]
    .concat()
}

impl Anthropic {
    fn start() -> Self {
        let server = Arc::new(Server::http("127.0.0.1:0").expect("bind anthropic fixture"));
        let port = match server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a.port(),
            _ => unreachable!(),
        };
        let seen: Arc<Mutex<Vec<(Value, Vec<(String, String)>)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let (s2, seen2) = (server.clone(), seen.clone());
        std::thread::spawn(move || {
            for mut req in s2.incoming_requests() {
                if !req.url().ends_with("/messages") {
                    let _ = req.respond(Response::from_string("not found").with_status_code(404));
                    continue;
                }
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let headers = req
                    .headers()
                    .iter()
                    .map(|h| {
                        (
                            h.field.to_string().to_ascii_lowercase(),
                            h.value.to_string(),
                        )
                    })
                    .collect();
                let n = {
                    let mut g = seen2.lock().unwrap();
                    g.push((serde_json::from_str(&body).unwrap_or(Value::Null), headers));
                    g.len()
                };
                let sse = if n == 1 { turn_one() } else { turn_two() };
                let resp = Response::from_string(sse)
                    .with_header(Header::from_bytes("Content-Type", "text/event-stream").unwrap());
                let _ = req.respond(resp);
            }
        });
        Anthropic {
            port,
            seen,
            _server: server,
        }
    }
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }
    fn requests(&self) -> Vec<(Value, Vec<(String, String)>)> {
        self.seen.lock().unwrap().clone()
    }
}

use std::io::Read;

#[test]
fn anthropic_stream_tool_round_trip_usage_cache_and_thinking_replay() {
    let anthropic = Anthropic::start();
    let rig = Rig::start(vec![]);
    let (status, conn) = rig.post(
        "/inference/connections/direct",
        json!({
            "name": "anthropic-fixture",
            "protocol": "anthropic_compatible",
            "normalizedBaseUrl": anthropic.base(),
            "authKind": "none",
            "publicHeaders": {},
            "manualModelIds": [MODEL_ID],
            "trustPrivateNetwork": false
        }),
    );
    assert_eq!(status, 200, "register direct anthropic connection: {conn}");
    let conn_id = conn["id"].as_str().expect("connection id").to_string();

    // `model` and `inferenceSelection` are mutually exclusive on session create (lifecycle.rs:428).
    let (status, created) = rig.post(
        "/sessions",
        json!({
            "id": "an1",
            "workspaceRoot": rig.ws().to_string_lossy(),
            "title": "e2e an1",
            "permissionMode": "bypass",
            "inferenceSelection": {
                "schemaVersion": 1, "authority": "direct", "connectionId": conn_id,
                "protocol": "anthropic_compatible", "modelId": MODEL_ID
            }
        }),
    );
    assert_eq!(status, 200, "create anthropic session: {created}");
    assert_eq!(rig.send("an1", "list the files").0, 200);
    rig.wait_assistant("an1", "anthropic final answer");

    // --- what hit the wire ---
    let reqs = anthropic.requests();
    assert_eq!(reqs.len(), 2, "one tool_use turn + one continuation");
    let (first, headers) = &reqs[0];
    assert!(
        headers.iter().any(|(k, _)| k == "anthropic-version"),
        "{headers:?}"
    );
    assert_eq!(first["stream"], true);
    assert!(
        first["max_tokens"].as_u64().unwrap_or(0) > 0,
        "Anthropic requires max_tokens: {first}"
    );
    assert!(
        first["model"]
            .as_str()
            .unwrap()
            .contains("claude-sonnet-4.5"),
        "{first}"
    );
    let system = first["system"]
        .as_array()
        .expect("system prompt rides the top-level `system` block array");
    assert!(system.iter().all(|b| b["type"] == "text"), "{system:?}");
    assert_eq!(
        system[0]["cache_control"]["type"], "ephemeral",
        "stable system prefix carries the cache marker"
    );
    let tools = first["tools"].as_array().unwrap();
    assert_eq!(
        tools.last().unwrap()["cache_control"]["type"],
        "ephemeral",
        "tools block ends on a cache breakpoint"
    );
    assert!(
        first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["role"] != "system"),
        "no system role inside messages"
    );
    assert_eq!(first["messages"][0]["role"], "user");
    let tool = &first["tools"][0];
    assert!(
        tool["name"].is_string() && tool["input_schema"]["type"] == "object",
        "Anthropic tool schema shape: {tool}"
    );

    // --- continuation: thinking replayed, tool_result correlated ---
    let second = &reqs[1].0;
    // Prompt-cache prefix stability: system + tools are byte-identical between calls.
    assert_eq!(
        second["system"], first["system"],
        "system prefix must be stable across turns"
    );
    assert_eq!(
        second["tools"], first["tools"],
        "tool block must be stable across turns"
    );
    let msgs = second["messages"].as_array().unwrap();
    let assistant = msgs
        .iter()
        .rev()
        .find(|m| m["role"] == "assistant")
        .unwrap();
    let blocks = assistant["content"]
        .as_array()
        .expect("assistant content blocks");
    assert!(
        blocks
            .iter()
            .any(|b| b["type"] == "thinking" && b["signature"] == "sig-abc"),
        "signed thinking must be replayed verbatim: {assistant}"
    );
    let tu = blocks
        .iter()
        .find(|b| b["type"] == "tool_use")
        .expect("tool_use replayed");
    assert_eq!(tu["id"], "toolu_1");
    assert_eq!(
        tu["input"]["path"], ".",
        "fragmented input_json must be reassembled"
    );
    let result_msg = msgs.last().unwrap();
    assert_eq!(result_msg["role"], "user");
    assert!(
        result_msg["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["type"] == "tool_result" && b["tool_use_id"] == "toolu_1"),
        "{result_msg}"
    );

    // --- what the user observed (durable facts only: deltas are live-only) ---
    let hist = rig.history("an1");
    assert!(
        !hist.iter().any(|e| kind_of(e) == "assistantDelta"),
        "live deltas must not be persisted"
    );
    let (_, transcript) = rig.get("/sessions/an1/transcript");
    let reasoning_persisted = transcript.as_array().unwrap().iter().any(|m| {
        m["$kind"] == "assistant"
            && m["reasoningContent"]
                .as_str()
                .is_some_and(|r| r.contains("ANTHROPIC-THINKING-9"))
    });
    assert!(
        reasoning_persisted,
        "thinking summary must be durable on the assistant message: {transcript}"
    );
    let u = hist
        .iter()
        .filter(|e| kind_of(e) == "usageUpdated")
        .next_back()
        .expect("usage event")
        .clone();
    assert_eq!(
        u["cacheReadTokens"], 2200,
        "cache reads accumulate across calls (900 + 1300): {u}"
    );
    assert_eq!(u["cacheCreationTokens"], 100, "{u}");
}
