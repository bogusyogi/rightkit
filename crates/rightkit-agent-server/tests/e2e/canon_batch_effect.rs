//! Effect-admission & provider-family truncation contracts.

use crate::support::{e2e_api, e2e_cli};
use serde_json::json;
use std::io::Read;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Response, Server};

#[test]
fn eff001_headless_tool_effect_is_admitted_and_settled_once() {
    let provider = e2e_cli::Provider::start(vec![
        e2e_cli::Reply::Chat(e2e_cli::Chat::tool(
            "write",
            "write_file",
            json!({"path":"effect.txt","content":"settled"}),
        )),
        e2e_cli::Reply::Chat(e2e_cli::Chat::text("done")),
    ]);
    let cli = e2e_cli::Cli::new(&provider);
    let out = cli.run(&["--stream-json", "--bypass-approvals"], "write file");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    // FM-034's production TrustPolicy forces bypass sessions into an isolated
    // execution worktree (api_state_impl_04.rs); use the shared root resolver.
    assert_eq!(
        cli.written("effect.txt")
            .unwrap_or_else(|| panic!("missing effect file\n{}", out.diag())),
        "settled",
    );
    let events = out.events();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.to_string().contains("toolCallStarted"))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.to_string().contains("toolResultAdded"))
            .count(),
        1
    );
}

#[test]
fn eff009_incomplete_anthropic_tool_generation_never_reaches_effect() {
    let server = Arc::new(Server::http("127.0.0.1:0").expect("Anthropic fixture listener"));
    let port = match server.server_addr() {
        tiny_http::ListenAddr::IP(addr) => addr.port(),
        _ => unreachable!(),
    };
    let fixture = server.clone();
    let requests = Arc::new(Mutex::new(0usize));
    let seen = requests.clone();
    std::thread::spawn(move || {
        while let Ok(Some(mut request)) = fixture.recv_timeout(std::time::Duration::from_secs(30)) {
            // Connection registration probes /models before inference; only
            // /messages consumes this provider-family truncation fixture.
            if !request.url().ends_with("/messages") {
                let _ = request.respond(Response::from_string("not found").with_status_code(404));
                continue;
            }
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            assert!(
                body.contains("messages"),
                "Anthropic request reaches provider fixture"
            );
            *seen.lock().unwrap() += 1;
            let response = concat!(
                "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
                "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"x\",\"name\":\"write_file\",\"input\":{}}}\n\n",
                "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"truncated.txt\\\",\\\"content\\\":\\\"partial\"\"}}\n\n",
                "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":10}}\n\n",
                "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
            );
            let response = Response::from_string(response)
                .with_header(Header::from_bytes("Content-Type", "text/event-stream").unwrap());
            let _ = request.respond(response);
            break;
        }
    });
    let rig = e2e_api::Rig::start(vec![]);
    let (status, connection) = rig.post("/inference/connections/direct", json!({
        "name":"anthropic-truncation","protocol":"anthropic_compatible","normalizedBaseUrl":format!("http://127.0.0.1:{port}/v1"),
        "authKind":"none","publicHeaders":{},"manualModelIds":["anthropic/claude-sonnet-4.5"],"trustPrivateNetwork":false
    }));
    assert_eq!(status, 200, "direct Anthropic fixture route: {connection}");
    let (status, created) = rig.post("/sessions", json!({
        "id":"anthropic-truncated","workspaceRoot":rig.ws().to_string_lossy(),"title":"truncated","permissionMode":"bypass",
        "inferenceSelection":{"schemaVersion":1,"authority":"direct","connectionId":connection["id"],"protocol":"anthropic_compatible","modelId":"anthropic/claude-sonnet-4.5"}
    }));
    assert_eq!(status, 200, "create direct session: {created}");
    let (status, _) = rig.send("anthropic-truncated", "write a file");
    assert_eq!(status, 200);
    rig.wait("provider request", 30, || {
        (*requests.lock().unwrap() > 0).then_some(())
    });
    assert!(
        !rig.ws().join("truncated.txt").exists(),
        "incomplete Anthropic call never mutates workspace"
    );
    assert!(
        !rig.kinds("anthropic-truncated")
            .iter()
            .any(|kind| kind == "toolCallStarted"),
        "no approval or ToolStarted for incomplete generation"
    );
}
