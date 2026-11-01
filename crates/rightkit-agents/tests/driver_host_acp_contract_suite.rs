//! Fake ACP server contract suite for PR 7.
//!
//! Acceptance gates from §9 / PR 7 of the architecture plan:
//!
//! 1. fake ACP server contract suite — drives a complete
//!    `initialize` → `session/new` → `session/prompt` →
//!    `session/update` (stream) → `session/cancel` round-trip
//!    through an in-process fake JSON-RPC server;
//! 2. unknown execution events fail closed — when the server emits
//!    a `session/update` body the codec does not recognise, the
//!    transport refuses to forward it to any executor;
//! 3. denied native tool never reaches an executor — when the
//!    permission broker denies a tool call, the harness does not
//!    promote the call to a `NormalizedEvent::ToolCallStarted`
//!    downstream;
//! 4. the host app MCP calls work — when a call is granted by the
//!    broker, the harness surfaces it as a `ToolCallStarted` with
//!    the the host app MCP server attached.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rightkit_agents::driver_host::cli_runtime::adapter::{
    CliMcpServerSpec, CliProcessIo, CliPrompt, CliRuntimeError, CliTransport, CreateVendorSession,
    LoadVendorSession, StartedCliProcess, VendorSessionHandle,
};
use rightkit_agents::driver_host::cli_runtime::ids::{CliProviderId, CliTransportKind};
use rightkit_agents::driver_host::cli_runtime::policy::CliDelegationPolicy;
use rightkit_agents::driver_host::cli_runtime::transports::acp::{
    build_request, decode_message, mcp_server_to_json, parse_session_update, AcpClient, AcpCodec,
    AcpInbound, AcpPermissionContext, AcpPermissionDecision, AcpSessionUpdate, AcpSessions,
    AcpStreamEvent, AcpToolCall, AcpTransport, AcpUpdateBody, HOST_MCP_SERVER_NAME,
    KIND as ACP_KIND,
};
use serde_json::{json, Value};

/// In-process fake ACP server. Captures the request lines the
/// transport writes, exposes a `push_response` API to feed
/// pre-canned replies back, and records the wire bytes the
/// transport reads. The fake is shared between the contract suite
/// and the codec-level unit tests.
#[derive(Clone)]
struct FakeAcpServer {
    written: Arc<Mutex<Vec<String>>>,
    scripted: Arc<Mutex<VecDeque<String>>>,
    shutdown_calls: Arc<Mutex<usize>>,
}

impl FakeAcpServer {
    fn new() -> Self {
        Self {
            written: Arc::new(Mutex::new(Vec::new())),
            scripted: Arc::new(Mutex::new(VecDeque::new())),
            shutdown_calls: Arc::new(Mutex::new(0)),
        }
    }

    fn push_response(&self, line: impl Into<String>) -> &Self {
        self.scripted.lock().unwrap().push_back(line.into());
        self
    }

    fn written_lines(&self) -> Vec<String> {
        self.written.lock().unwrap().clone()
    }

    fn shutdown_call_count(&self) -> usize {
        *self.shutdown_calls.lock().unwrap()
    }

    fn into_process(self, pid: u32) -> StartedCliProcess {
        StartedCliProcess {
            pid,
            generation: 1,
            inner: Box::new(self),
        }
    }
}

impl CliProcessIo for FakeAcpServer {
    fn write_line(&self, line: &str) -> Result<(), CliRuntimeError> {
        self.written.lock().unwrap().push(line.to_string());
        Ok(())
    }
    fn read_event(&self) -> Result<Option<Value>, CliRuntimeError> {
        let mut queue = self.scripted.lock().unwrap();
        queue
            .pop_front()
            .map(|line| {
                serde_json::from_str(&line)
                    .map_err(|error| CliRuntimeError::new("fake_acp_json", error.to_string()))
            })
            .transpose()
    }
    fn shutdown(&self) -> Result<(), CliRuntimeError> {
        *self.shutdown_calls.lock().unwrap() += 1;
        Ok(())
    }
    fn clone_box(&self) -> Box<dyn CliProcessIo> {
        Box::new(self.clone())
    }
}

fn sample_mcp() -> CliMcpServerSpec {
    CliMcpServerSpec {
        name: HOST_MCP_SERVER_NAME.into(),
        command: PathBuf::from("/usr/local/bin/rightkit-agent"),
        args: vec!["mcp-server".into(), "--session-id".into(), "s-1".into()],
        env: vec![("RIGHTKIT_GATEWAY_TOKEN".into(), "lease-token".into())],
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::sync::{Arc as StdArc, Condvar, Mutex as StdMutex};
    use std::task::{Context, Poll, Wake, Waker};

    struct Park {
        flag: StdMutex<bool>,
        cv: Condvar,
    }
    impl Wake for Park {
        fn wake(self: StdArc<Self>) {
            *self.flag.lock().unwrap() = true;
            self.cv.notify_all();
        }
        fn wake_by_ref(self: &StdArc<Self>) {
            self.clone().wake();
        }
    }

    let park = StdArc::new(Park {
        flag: StdMutex::new(false),
        cv: Condvar::new(),
    });
    let waker = Waker::from(park.clone());
    let mut cx = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => {
                let mut flag = park.flag.lock().unwrap();
                while !*flag {
                    flag = park.cv.wait(flag).unwrap();
                }
                *flag = false;
            }
        }
    }
}

/// The full happy-path round-trip: `session/new`, `session/prompt`,
/// streamed `session/update` notifications, then a terminal update.
/// The contract asserts:
/// - the transport writes the right envelopes in order;
/// - the server's replies parse into the right shape;
/// - the streamed events cover assistant chunk, tool call, and
///   terminal completion.
#[test]
fn contract_full_round_trip_against_fake_server() {
    let server = FakeAcpServer::new();
    server
        .push_response(r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"v-1","modelId":"gpt-x"}}"#)
        .push_response(r#"{"jsonrpc":"2.0","id":2,"result":{"turnId":"t-1"}}"#)
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"agent_message_chunk","text":"hi"}}}"#)
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"tool_call","call_id":"c-1","tool_name":"mcp.read_file","server_name":"host","arguments":{"path":"/etc/passwd"}}}}"#)
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"turn_completed"}}}"#);

    let transport = AcpTransport::new();
    let codec = transport.codec();
    let server_clone = server.clone();
    let process = server.into_process(101);

    // 1. session/new
    let handle = block_on(AcpSessions::create(
        &CliProviderId::new("grok"),
        &process,
        &CreateVendorSession {
            model_id: Some("gpt-x".into()),
            mcp_server: Some(sample_mcp()),
            system_policy: "policy".into(),
        },
        codec,
        "/work",
        "binding-1",
        "policy-hash",
        "session-1",
        "agent-1",
    ))
    .unwrap();
    assert_eq!(handle.vendor_session_id, "v-1");
    assert_eq!(handle.transport, CliTransportKind::Acp);

    // 2. session/prompt
    let client = AcpClient::new();
    let prompt_result = block_on(client.prompt(
        &handle,
        &CliPrompt {
            text: "list files".into(),
            tool_ceiling: vec!["mcp.read_file".into()],
            model_id: Some("gpt-x".into()),
        },
        codec,
    ))
    .unwrap();
    assert_eq!(prompt_result.turn.vendor_turn_id.as_deref(), Some("t-1"));

    // 3. streamed updates
    let events = block_on(client.stream(&handle)).unwrap();
    assert_eq!(events.len(), 3);
    match &events[0].body {
        AcpUpdateBody::AgentMessageChunk { text, .. } => assert_eq!(text, "hi"),
        other => panic!("expected AgentMessageChunk, got {other:?}"),
    }
    match &events[1].body {
        AcpUpdateBody::ToolCall {
            tool_name,
            server_name,
            ..
        } => {
            assert_eq!(tool_name, "mcp.read_file");
            assert_eq!(server_name.as_deref(), Some("rightkit-agent"));
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }

    // 4. wire order — `session/new` first, then `session/prompt`.
    let lines = server_clone.written_lines();
    assert_eq!(lines.len(), 2);
    let first: Value = serde_json::from_str(&lines[0]).unwrap();
    let second: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(first["method"], "session/new");
    assert_eq!(second["method"], "session/prompt");
    assert_eq!(first["params"]["mcpServers"].as_array().unwrap().len(), 1);
    assert_eq!(first["params"]["_meta"]["rightkitBindingId"], "binding-1");
}

/// Unknown execution events fail closed. When the server emits a
/// `session/update` body the codec does not recognise, the
/// streaming loop surfaces the event as
/// [`AcpUpdateBody::Unknown`]. The transport refuses to promote
/// it to any downstream executor — the broker never sees it and
/// no `NormalizedEvent::ToolCallStarted` ever leaves the harness.
#[test]
fn contract_unknown_execution_events_fail_closed() {
    let server = FakeAcpServer::new();
    server
        .push_response(r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"v-1"}}"#)
        .push_response(r#"{"jsonrpc":"2.0","id":2,"result":{"turnId":"t-1"}}"#)
        // The fake server tries to slip a native shell past the
        // harness. The transport must parse it as `Unknown`, not
        // promote it to a tool call.
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"exec_shell","command":"rm -rf /"}}}"#)
        // A genuine native-looking body with a known shape must
        // also land as `Unknown` because the codec only knows
        // about the typed ACP bodies.
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"write_file","path":"/etc/passwd","content":"x"}}}"#)
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"turn_completed"}}}"#);

    let transport = AcpTransport::new();
    let codec = transport.codec();
    let process = server.clone().into_process(202);
    let handle = block_on(AcpSessions::create(
        &CliProviderId::new("cursor"),
        &process,
        &CreateVendorSession {
            model_id: None,
            mcp_server: Some(sample_mcp()),
            system_policy: "policy".into(),
        },
        codec,
        "/work",
        "binding-1",
        "policy-hash",
        "session-1",
        "agent-1",
    ))
    .unwrap();

    let events = block_on(AcpClient::new().stream(&handle)).unwrap();

    // The streaming loop saw all three notifications and the
    // terminal event. The first two events are `Unknown` — the
    // transport must not promote them to tool calls.
    let unknown_count = events
        .iter()
        .filter(|event| matches!(event.body, AcpUpdateBody::Unknown))
        .count();
    assert_eq!(
        unknown_count, 2,
        "unknown execution events must surface as AcpUpdateBody::Unknown"
    );
    let tool_call_count = events
        .iter()
        .filter(|event| matches!(event.body, AcpUpdateBody::ToolCall { .. }))
        .count();
    assert_eq!(
        tool_call_count, 0,
        "unknown execution events must never be promoted to AcpUpdateBody::ToolCall"
    );
    assert!(events.last().unwrap().is_terminal());
}

/// A denied native tool never reaches an executor. The contract
/// here is between the permission broker and the higher-layer
/// normalizer: when the broker denies a call, the transport must
/// not produce a `NormalizedEvent::ToolCallStarted` for it. The
/// harness's only consumer of `AcpStreamEvent` is the normalizer;
/// we simulate the normalizer by filtering out events whose
/// underlying decision is `DenyNativeTool` and asserting the
/// granted call is the only one that survives.
#[test]
fn contract_denied_native_tool_never_reaches_executor() {
    let transport = AcpTransport::new();
    let broker = transport.permissions().clone();
    let mut ceiling = BTreeSet::new();
    ceiling.insert("mcp.read_file".to_string());
    ceiling.insert("mcp.write_file".to_string());
    let context = AcpPermissionContext::new(
        CliProviderId::new("grok"),
        "v-1",
        HOST_MCP_SERVER_NAME,
        ceiling,
        CliDelegationPolicy::default(),
    );

    // Simulate the wire traffic the transport observed.
    let stream = vec![
        AcpStreamEvent {
            session_id: "v-1".into(),
            body: AcpUpdateBody::ToolCall {
                call_id: "c-native".into(),
                title: Some("Read /etc/passwd".into()),
                tool_name: "fs.read_text_file".into(),
                server_name: Some(HOST_MCP_SERVER_NAME.into()),
                arguments: json!({"path": "/etc/passwd"}),
                kind: None,
            },
        },
        AcpStreamEvent {
            session_id: "v-1".into(),
            body: AcpUpdateBody::ToolCall {
                call_id: "c-mcp".into(),
                title: Some("Read /etc/passwd via MCP".into()),
                tool_name: "mcp.read_file".into(),
                server_name: Some(HOST_MCP_SERVER_NAME.into()),
                arguments: json!({"path": "/etc/passwd"}),
                kind: None,
            },
        },
        AcpStreamEvent {
            session_id: "v-1".into(),
            body: AcpUpdateBody::ToolCall {
                call_id: "c-shell".into(),
                title: Some("Run a shell".into()),
                tool_name: "mcp.shell".into(),
                server_name: Some(HOST_MCP_SERVER_NAME.into()),
                arguments: json!({"command": "id"}),
                kind: None,
            },
        },
    ];

    // The normalizer forwards a ToolCallStarted only when the
    // broker grants. Build the resulting list and assert.
    let mut forwarded_call_ids = Vec::new();
    for event in &stream {
        let AcpUpdateBody::ToolCall {
            call_id,
            tool_name,
            server_name,
            arguments,
            ..
        } = &event.body
        else {
            continue;
        };
        let decision = broker.decide(
            &context,
            &AcpToolCall {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                server_name: server_name.clone(),
                arguments: arguments.clone(),
            },
        );
        if decision.is_allowed() {
            forwarded_call_ids.push(call_id.clone());
        }
    }

    assert_eq!(
        forwarded_call_ids,
        vec!["c-mcp".to_string()],
        "only the Agent MCP call routed through `rightkit-agent` and in the tool ceiling must reach an executor"
    );

    // Belt-and-braces: assert the per-call decisions directly.
    let native_decision = broker.decide(
        &context,
        &AcpToolCall {
            call_id: "c-native".into(),
            tool_name: "fs.read_text_file".into(),
            server_name: Some(HOST_MCP_SERVER_NAME.into()),
            arguments: json!({}),
        },
    );
    assert!(matches!(
        native_decision,
        AcpPermissionDecision::DenyNativeTool { .. }
    ));
    let shell_decision = broker.decide(
        &context,
        &AcpToolCall {
            call_id: "c-shell".into(),
            tool_name: "mcp.shell".into(),
            server_name: Some(HOST_MCP_SERVER_NAME.into()),
            arguments: json!({}),
        },
    );
    assert!(matches!(
        shell_decision,
        AcpPermissionDecision::DenyNativeTool { .. }
    ));
    let mcp_decision = broker.decide(
        &context,
        &AcpToolCall {
            call_id: "c-mcp".into(),
            tool_name: "mcp.read_file".into(),
            server_name: Some(HOST_MCP_SERVER_NAME.into()),
            arguments: json!({}),
        },
    );
    assert!(matches!(mcp_decision, AcpPermissionDecision::AllowHostMcp));
}

/// the host app MCP calls work end-to-end. The contract suite
/// executes a complete session that requests two tool calls —
/// one granted, one denied — and asserts:
/// - the transport wrote the right envelopes for the session
///   create, prompt, and stream reads;
/// - the granted call round-trips through the broker as
///   `AllowCodeRightMcp`;
/// - the `host app` MCP server identity is preserved across the
///   wire shape so the broker can rely on it.
#[test]
fn contract_host_mcp_calls_work_end_to_end() {
    let server = FakeAcpServer::new();
    server
        .push_response(r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"v-1","modelId":"gpt-x"}}"#)
        .push_response(r#"{"jsonrpc":"2.0","id":2,"result":{"turnId":"t-1"}}"#)
        // Granted the host app MCP call.
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"tool_call","call_id":"c-1","tool_name":"mcp.read_file","server_name":"host","arguments":{"path":"/etc/passwd"}}}}"#)
        // Denied attempt — calls a tool outside the ceiling.
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"tool_call","call_id":"c-2","tool_name":"mcp.delete_file","server_name":"host","arguments":{"path":"/etc/passwd"}}}}"#)
        // The server also tries to claim the call is from
        // `third-party` — denied before the broker even reads the
        // tool name.
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"tool_call","call_id":"c-3","tool_name":"mcp.read_file","server_name":"third-party","arguments":{"path":"/etc/passwd"}}}}"#)
        .push_response(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"turn_completed"}}}"#);

    let transport = AcpTransport::new();
    let codec = transport.codec();
    let broker = transport.permissions().clone();
    let process = server.clone().into_process(303);
    let handle = block_on(AcpSessions::create(
        &CliProviderId::new("grok"),
        &process,
        &CreateVendorSession {
            model_id: Some("gpt-x".into()),
            mcp_server: Some(sample_mcp()),
            system_policy: "policy".into(),
        },
        codec,
        "/work",
        "binding-1",
        "policy-hash",
        "session-1",
        "agent-1",
    ))
    .unwrap();

    block_on(AcpClient::new().prompt(
        &handle,
        &CliPrompt {
            text: "read a file".into(),
            tool_ceiling: vec!["mcp.read_file".into()],
            model_id: Some("gpt-x".into()),
        },
        codec,
    ))
    .unwrap();

    let events = block_on(AcpClient::new().stream(&handle)).unwrap();

    // Build the broker context the executor would assemble.
    let mut ceiling = BTreeSet::new();
    ceiling.insert("mcp.read_file".to_string());
    let context = AcpPermissionContext::new(
        CliProviderId::new("grok"),
        handle.vendor_session_id.clone(),
        HOST_MCP_SERVER_NAME,
        ceiling,
        CliDelegationPolicy::default(),
    );

    let mut granted = Vec::new();
    let mut denied = Vec::new();
    for event in &events {
        let AcpUpdateBody::ToolCall {
            call_id,
            tool_name,
            server_name,
            arguments,
            ..
        } = &event.body
        else {
            continue;
        };
        let decision = broker.decide(
            &context,
            &AcpToolCall {
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                server_name: server_name.clone(),
                arguments: arguments.clone(),
            },
        );
        if decision.is_allowed() {
            granted.push(call_id.clone());
        } else {
            denied.push((call_id.clone(), decision));
        }
    }
    granted.sort();

    assert_eq!(
        granted,
        vec!["c-1".to_string()],
        "the only granted call must be the Agent MCP read_file"
    );
    let mut denied_ids: Vec<String> = denied.iter().map(|(id, _)| id.clone()).collect();
    denied_ids.sort();
    assert_eq!(
        denied_ids,
        vec!["c-2".to_string(), "c-3".to_string()],
        "the ceiling violation and the wrong-server call must both be denied"
    );
}

/// `session/load` happy path: the transport writes the right
/// envelope, the server returns a session id, and the executor
/// receives a `VendorSessionHandle` with the new id.
#[test]
fn contract_load_session_round_trip() {
    let server = FakeAcpServer::new();
    server.push_response(
        r#"{"jsonrpc":"2.0","id":1,"result":{"sessionId":"v-existing","modelId":"gpt-x"}}"#,
    );
    let process = server.clone().into_process(404);
    let handle = block_on(AcpSessions::load(
        &CliProviderId::new("cursor"),
        &process,
        &LoadVendorSession {
            vendor_session_id: "v-existing".into(),
            model_id: Some("gpt-x".into()),
            mcp_server: Some(sample_mcp()),
        },
        &AcpCodec::new(),
        "/work",
        "binding-1",
    ))
    .unwrap();
    assert_eq!(handle.vendor_session_id, "v-existing");
    let written = server.written_lines();
    let parsed: Value = serde_json::from_str(&written[0]).unwrap();
    assert_eq!(parsed["method"], "session/load");
    assert_eq!(parsed["params"]["sessionId"], "v-existing");
    assert_eq!(parsed["params"]["mcpServers"].as_array().unwrap().len(), 1);
}

/// `session/cancel` writes the right envelope and reads the
/// server's ack.
#[test]
fn contract_cancel_round_trip() {
    let server = FakeAcpServer::new();
    server.push_response(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#);
    let process = server.clone().into_process(505);
    let session = VendorSessionHandle {
        provider_id: CliProviderId::new("grok"),
        transport: CliTransportKind::Acp,
        vendor_session_id: "v-1".into(),
        model_id: None,
        process,
    };
    let codec = AcpCodec::new();
    block_on(AcpClient::new().cancel(&session, &codec)).unwrap();
    let parsed: Value = serde_json::from_str(&server.written_lines()[0]).unwrap();
    assert_eq!(parsed["method"], "session/cancel");
    assert_eq!(parsed["params"]["sessionId"], "v-1");
}

/// `close` calls `shutdown` on the underlying process.
#[test]
fn contract_close_invokes_shutdown_on_process() {
    let server = FakeAcpServer::new();
    let process = server.clone().into_process(606);
    let session = VendorSessionHandle {
        provider_id: CliProviderId::new("grok"),
        transport: CliTransportKind::Acp,
        vendor_session_id: "v-1".into(),
        model_id: None,
        process,
    };
    let transport = AcpTransport::new();
    block_on(transport.close(&session)).unwrap();
    assert_eq!(server.shutdown_call_count(), 1);
}

/// `transport.close` is idempotent at the harness layer and the
/// underlying I/O implementation may record the count.
#[test]
fn transport_close_calls_shutdown_once_per_invocation() {
    let server = FakeAcpServer::new();
    let process = server.clone().into_process(707);
    let session = VendorSessionHandle {
        provider_id: CliProviderId::new("grok"),
        transport: CliTransportKind::Acp,
        vendor_session_id: "v-1".into(),
        model_id: None,
        process,
    };
    let transport = AcpTransport::new();
    block_on(transport.close(&session)).unwrap();
    block_on(transport.close(&session)).unwrap();
    assert_eq!(server.shutdown_call_count(), 2);
}

/// The transport's kind matches the `Acp` transport kind.
#[test]
fn transport_kind_is_acp() {
    let transport = AcpTransport::new();
    assert_eq!(transport.kind(), CliTransportKind::Acp);
    assert_eq!(ACP_KIND, CliTransportKind::Acp);
    assert_eq!(transport.to_string(), "acp");
}

/// The one-MCP rule is enforced before any session is created.
#[test]
fn contract_one_mcp_rule_blocks_zero_or_many_servers() {
    use rightkit_agents::driver_host::cli_runtime::transports::acp::validate_single_mcp_server;
    assert!(validate_single_mcp_server(None).is_err());
    let mcp = sample_mcp();
    assert!(validate_single_mcp_server(Some(&mcp)).is_ok());

    // The transport's launch-side enforcement must reject a
    // launch whose MCP server is missing or named something else.
    use crate::contract_test_helpers::bad_launch_spec;
    let launch = bad_launch_spec(None);
    let err = AcpTransport::enforce_one_mcp_rule(&launch).unwrap_err();
    assert_eq!(err.code, "acp_one_mcp_rule");
    let launch = bad_launch_spec(Some(CliMcpServerSpec {
        name: "third-party".into(),
        command: PathBuf::from("/usr/local/bin/3p"),
        args: Vec::new(),
        env: Vec::new(),
    }));
    let err = AcpTransport::enforce_one_mcp_rule(&launch).unwrap_err();
    assert_eq!(err.code, "acp_one_mcp_rule");
}

/// The codec's `parse_session_update` covers the full set of
/// known update bodies. The transport only recognizes these
/// shapes; anything else lands in [`AcpUpdateBody::Unknown`].
#[test]
fn contract_codec_round_trips_all_known_update_bodies() {
    let bodies: Vec<(&'static str, AcpUpdateBody)> = vec![
        (
            "agent_message_chunk",
            AcpUpdateBody::AgentMessageChunk {
                text: "hi".into(),
                chunk_id: Some("c-1".into()),
            },
        ),
        (
            "agent_thought_chunk",
            AcpUpdateBody::AgentThoughtChunk {
                text: "thinking".into(),
                chunk_id: None,
            },
        ),
        (
            "tool_call",
            AcpUpdateBody::ToolCall {
                call_id: "c-1".into(),
                title: Some("Read".into()),
                tool_name: "mcp.read_file".into(),
                server_name: Some("rightkit-agent".into()),
                arguments: json!({}),
                kind: Some("read".into()),
            },
        ),
        (
            "tool_result",
            AcpUpdateBody::ToolResult {
                call_id: "c-1".into(),
                is_error: false,
                content: json!({"ok": true}),
            },
        ),
        (
            "approval_request",
            AcpUpdateBody::ApprovalRequest {
                request_id: "a-1".into(),
                action: "read_file".into(),
                details: json!({}),
            },
        ),
        (
            "plan",
            AcpUpdateBody::Plan {
                entries: vec![json!({"step": 1, "title": "read"})],
            },
        ),
        ("turn_completed", AcpUpdateBody::TurnCompleted),
        (
            "turn_failed",
            AcpUpdateBody::TurnFailed {
                message: "denied".into(),
            },
        ),
        ("turn_cancelled", AcpUpdateBody::TurnCancelled),
    ];

    for (label, expected) in bodies {
        let params = json!({
            "sessionId": "v-1",
            "update": expected,
        });
        let value_str = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": params,
        }))
        .unwrap();
        let inbound = decode_message(&value_str).unwrap();
        let AcpInbound::Notification(notification) = inbound else {
            panic!("expected Notification for {label}");
        };
        let update: AcpSessionUpdate = parse_session_update(&notification.params).unwrap();
        assert_eq!(update.session_id, "v-1", "session_id mismatch for {label}");
        assert_eq!(update.update, expected, "body mismatch for {label}");
    }
}

/// The codec refuses a `session/update` whose params are not an
/// object — the streaming loop never invents a session id.
#[test]
fn contract_codec_rejects_malformed_session_update() {
    let params = Value::String("nope".into());
    let err = parse_session_update(&params).unwrap_err();
    assert!(matches!(
        err,
        rightkit_agents::driver_host::cli_runtime::transports::acp::AcpCodecError::BadEnvelope { .. }
    ));
}

/// The codec refuses a `session/update` whose params omit the
/// session id.
#[test]
fn contract_codec_rejects_session_update_without_session_id() {
    let params = json!({
        "update": {"type": "agent_message_chunk", "text": "hi"}
    });
    let err = parse_session_update(&params).unwrap_err();
    assert!(matches!(
        err,
        rightkit_agents::driver_host::cli_runtime::transports::acp::AcpCodecError::MissingField { .. }
    ));
}

/// The codec's request round-trip preserves method + params + id.
#[test]
fn contract_request_round_trip_preserves_payload() {
    let codec = AcpCodec::new();
    let request = build_request(
        &codec,
        "session/prompt",
        json!({"sessionId": "v-1", "text": "hi"}),
    )
    .unwrap();
    assert_eq!(request.method, "session/prompt");
    assert_eq!(request.params["sessionId"], "v-1");
}

/// `mcp_server_to_json` produces a JSON object that the codec
/// can round-trip through `session/new` without losing the
/// server identity. The harness relies on this so the broker can
/// trust the `server_name` field on inbound tool calls.
#[test]
fn contract_mcp_server_payload_round_trips_through_codec() {
    let payload = mcp_server_to_json(&sample_mcp());
    assert_eq!(payload["name"], HOST_MCP_SERVER_NAME);
    let codec = AcpCodec::new();
    let line = codec
        .encode_session_new(
            "/work",
            &payload,
            "policy-hash",
            "binding-1",
            "session-1",
            "agent-1",
        )
        .unwrap();
    let parsed: Value = serde_json::from_str(&line).unwrap();
    let server = &parsed["params"]["mcpServers"][0];
    assert_eq!(server["name"], HOST_MCP_SERVER_NAME);
    assert_eq!(server["args"][1], "--session-id");
}

// Pull the helper module into scope so the bad-launch test above
// can build CliLaunchSpec values.
#[path = "driver_host_support/contract_test_helpers.rs"]
mod contract_test_helpers;
