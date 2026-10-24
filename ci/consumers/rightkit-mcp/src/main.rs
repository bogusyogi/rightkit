// rightkit-mcp consumed by the exact registry pin in Cargo.toml, outside the workspace, with default features
// (stdio only, which is what pulse selects; coderight also selects "advanced-http", not proven here).
//
// Proves the shared MCP client, server host and trust gate end to end WITHOUT a process or a socket:
// the client is given a ProcessLauncher that returns in-memory pipes served by a ServerHost, so
// McpClient speaks real newline-delimited JSON-RPC to a real ServerHost.
//   * names and types: sanitize_tool_name and parse_qualified_name, ToolDescriptor, ToolResult,
//     Content, PromptMessage wire shapes, mutating_tools, protocol version constants, McpError text,
//     RawJson and to_wire_string (bytes preserved), McpEvent and LifecycleKind.
//   * config: parse_mcp_json with ${VAR} and ${VAR:-default} expansion, sorted entries, every refusal.
//   * trust: fingerprint is an independently computed SHA-256 over the canonical launch description
//     (environment values included, never shown in a PendingApproval); TrustStore decide, approve,
//     deny, gate, gate_stdio, credential_write_allowed, foreign fingerprints, save and try_load,
//     try_load_migrating; a changed environment value needs re-approval.
//   * server host (dispatch, no transport): initialize negotiation and overrides, ping, unknown
//     methods, tools/list and tools/call with ReadOnlyPolicy admission, tool-level and RPC errors,
//     resources, prompts, batches (Process, Reject, Ignore, per protocol version), id and initialized
//     policies, strict request validation, response limits, response hook and transform, RawJson
//     passthrough, dispatch_async with an AsyncRequestHandler, events.
//   * stdio serving over in-memory buffers: replies per line, a parse error reply, an oversize line
//     refused with serving continuing, FailFast ending with InvalidData.
//   * optional provider hooks: list_resource_templates, read_resource_with_context with
//     accessGrants, CallHandle::notify_progress_number and notify over serve_stdio_concurrent_on
//     (inert under dispatch), and a custom AdmissionHook (request fields, Deny, expose).
//   * client: connect through the trust gate (NotApproved and Denied spawn nothing, a changed spec
//     needs re-approval), LaunchRequest contents, list_tools, call_tool, resources, prompts, ping,
//     session ids, per-call options and a pre-cancelled call, cancelling a call in flight, progress
//     notifications through on_notification, lifecycle events with pid, close; protocol_versions
//     refusal before any launch; session-bound elicitation against a hand-written in-memory server
//     (default handler, relatedRequestId, set and clear_elicitation_handler, handler error);
//     ConnectionPool trust refusal, catalog skipped list, events; HTTP extensions are refused
//     with a Config error because the advanced-http feature is off.
//   * LeasedPool: single-flight acquire, epochs, idle eviction, cooldown after a failure, cache
//     lifetime, snapshot. EndpointOriginPolicy resolution and CancellationToken are checked as pure
//     logic.
// Public API only.
// Does not prove: a real child process (DefaultProcessLauncher, LaunchedProcess::from_child), the
// process-stdio entry points serve_stdio, serve_stdio_io and serve_stdio_concurrent (the in-memory
// *_on variants are proven instead), streamable HTTP or legacy SSE clients, the "advanced-http"
// feature, ServerHost::serve_http (it binds a loopback socket), an approved workspace server in a
// ConnectionPool (the pool spawns without a launcher), exact-json-ids, with_cancelled_response, or
// AuthProvider on a live HTTP connection.
// State: one small trust file under a per-process directory in the OS temp directory, removed at the
// end. Worker threads serve the in-memory pipes and are joined by close. No network, no process
// spawn, no wall-clock waits beyond sub-second polls, no real user data.
use rightkit_mcp::config::parse_mcp_json;
use rightkit_mcp::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::{self, JoinHandle};
use std::time::Duration;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("rk-mcp-consumer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create consumer temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..10_000 {
        if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        thread::yield_now();
    }
    panic!("future did not complete");
}

#[cfg(unix)]
fn success() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

#[cfg(windows)]
fn success() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}

/// A one-direction in-memory byte pipe. Closing it gives the reader end of file and fails writes.
#[derive(Default)]
struct Pipe {
    state: Mutex<(VecDeque<u8>, bool)>,
    ready: Condvar,
}

impl Pipe {
    fn close(&self) {
        self.state.lock().unwrap().1 = true;
        self.ready.notify_all();
    }

    fn closed(&self) -> bool {
        self.state.lock().unwrap().1
    }
}

struct PipeReader(Arc<Pipe>);
struct PipeWriter(Arc<Pipe>);

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut state = self.0.state.lock().unwrap();
        loop {
            if !state.0.is_empty() {
                let n = out.len().min(state.0.len());
                for slot in &mut out[..n] {
                    *slot = state.0.pop_front().unwrap();
                }
                return Ok(n);
            }
            if state.1 {
                return Ok(0);
            }
            state = self.0.ready.wait(state).unwrap();
        }
    }
}

impl Write for PipeWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut state = self.0.state.lock().unwrap();
        if state.1 {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        state.0.extend(data.iter().copied());
        self.0.ready.notify_all();
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// The "process" the launcher hands back: it owns the pipes and the thread serving them.
struct FakeChild {
    pipes: [Arc<Pipe>; 2],
    server: Option<JoinHandle<()>>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl ProcessChild for FakeChild {
    fn id(&self) -> u32 {
        4242
    }

    fn kill(&mut self) -> io::Result<()> {
        self.log.lock().unwrap().push("kill");
        for pipe in &self.pipes {
            pipe.close();
        }
        Ok(())
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Ok(self.pipes.iter().all(|p| p.closed()).then(success))
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        self.log.lock().unwrap().push("wait");
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        Ok(success())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Serving {
    Sequential,
    Concurrent,
}

struct Launcher {
    requests: Arc<Mutex<Vec<LaunchRequest>>>,
    log: Arc<Mutex<Vec<&'static str>>>,
    launcher: Arc<dyn ProcessLauncher>,
}

/// A launcher that serves `host` on in-memory pipes instead of spawning a program.
fn launcher(host: ServerHost, serving: Serving) -> Launcher {
    launcher_with(move |reader, writer| {
        let _ = match serving {
            Serving::Sequential => host.serve_stdio_on(BufReader::new(reader), writer),
            Serving::Concurrent => host
                .serve_stdio_concurrent_on(BufReader::new(reader), writer)
                .map_err(McpError::from),
        };
    })
}

/// A launcher that runs `serve` on its own thread against the in-memory pipes of each launch.
fn launcher_with(
    serve: impl Fn(PipeReader, PipeWriter) + Send + Sync + Clone + 'static,
) -> Launcher {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let (seen, events) = (Arc::clone(&requests), Arc::clone(&log));
    let launcher = move |request: &LaunchRequest| -> Result<LaunchedProcess> {
        seen.lock().unwrap().push(request.clone());
        let (to_server, to_client) = (Arc::new(Pipe::default()), Arc::new(Pipe::default()));
        let (reader, writer) = (
            PipeReader(Arc::clone(&to_server)),
            PipeWriter(Arc::clone(&to_client)),
        );
        let serve = serve.clone();
        let server = thread::spawn(move || serve(reader, writer));
        Ok(LaunchedProcess {
            child: Box::new(FakeChild {
                pipes: [Arc::clone(&to_server), Arc::clone(&to_client)],
                server: Some(server),
                log: Arc::clone(&events),
            }),
            stdin: Box::new(PipeWriter(to_server)),
            stdout: Box::new(PipeReader(to_client)),
        })
    };
    Launcher {
        requests,
        log,
        launcher: Arc::new(launcher),
    }
}

#[derive(Clone, Default)]
struct Demo {
    started: Arc<AtomicBool>,
    saw_cancel: Arc<AtomicBool>,
}

impl ToolProvider for Demo {
    fn list(&self) -> Vec<ToolDescriptor> {
        let object = |properties: Value| json!({"type": "object", "properties": properties});
        let read_only = |name: &str, description: &str, schema: Value| {
            ToolDescriptor::new(name, description, schema).read_only(true)
        };
        vec![
            read_only(
                "echo",
                "Echo a message",
                object(json!({"message": {"type": "string"}})),
            ),
            read_only(
                "add",
                "Add two numbers",
                object(json!({"a": {"type": "number"}, "b": {"type": "number"}})),
            ),
            ToolDescriptor::new(
                "write_marker",
                "Writes a marker (mutating)",
                object(json!({"path": {"type": "string"}})),
            ),
            read_only("secret_tool", "Hidden by policy", object(json!({}))),
            read_only("fail", "Reports a tool-level failure", object(json!({}))),
            read_only("rpc_fail", "Fails with error data", object(json!({}))),
            read_only("raw", "Returns raw JSON", object(json!({}))),
            read_only("progress", "Reports progress", object(json!({}))),
            read_only("wait_for_cancel", "Runs until cancelled", object(json!({}))),
        ]
    }

    fn call(&self, name: &str, args: Value) -> Result<ToolResult> {
        match name {
            "echo" => Ok(ToolResult::text(format!(
                "echo: {}",
                args["message"].as_str().unwrap_or("")
            ))),
            "add" => Ok(ToolResult::json(
                json!({"sum": args["a"].as_f64().unwrap_or(0.0) + args["b"].as_f64().unwrap_or(0.0)}),
            )),
            "write_marker" => Ok(ToolResult::text(format!(
                "wrote {}",
                args["path"].as_str().unwrap_or("")
            ))),
            "secret_tool" => Ok(ToolResult::text("secret")),
            "fail" => Ok(ToolResult::error("tool-level failure")),
            "rpc_fail" => Err(McpError::RpcWithData {
                code: -32050,
                message: "custom failure".to_owned(),
                data: json!({"detail": "kept"}),
            }),
            "raw" => Ok(ToolResult::raw_json(
                RawJson::new(r#"{"z":1.0,"a":1E2}"#).unwrap(),
            )),
            other => Err(McpError::ToolNotFound(other.to_owned())),
        }
    }

    fn call_with_handle(&self, name: &str, args: Value, call: &CallHandle) -> Result<ToolResult> {
        match name {
            "progress" => {
                let sent = call.notify_progress(1.0, Some(2.0), Some("half"));
                Ok(ToolResult::text(format!("progress sent: {sent}")))
            }
            "wait_for_cancel" => {
                self.started.store(true, Ordering::SeqCst);
                for _ in 0..5_000 {
                    if call.is_cancelled() {
                        self.saw_cancel.store(true, Ordering::SeqCst);
                        break;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                Ok(ToolResult::text("stopped"))
            }
            _ => self.call(name, args),
        }
    }

    fn resources(&self) -> Vec<ResourceDescriptor> {
        vec![ResourceDescriptor {
            uri: "demo://readme".to_owned(),
            name: "readme".to_owned(),
            description: String::new(),
            mime_type: "text/plain".to_owned(),
        }]
    }

    fn read_resource(&self, uri: &str) -> Result<ResourceContents> {
        if uri == "demo://readme" {
            Ok(ResourceContents {
                uri: uri.to_owned(),
                mime_type: "text/plain".to_owned(),
                text: Some("hello resource".to_owned()),
                blob: None,
            })
        } else {
            Err(McpError::ResourceNotFound(uri.to_owned()))
        }
    }

    fn prompts(&self) -> Vec<PromptDescriptor> {
        vec![PromptDescriptor {
            name: "greet".to_owned(),
            description: String::new(),
            arguments: vec![PromptArgument {
                name: "who".to_owned(),
                description: String::new(),
                required: true,
            }],
        }]
    }

    fn get_prompt(&self, name: &str, args: Value) -> Result<Vec<PromptMessage>> {
        if name == "greet" {
            Ok(vec![PromptMessage {
                role: "user".to_owned(),
                text: format!("Hello {}", args["who"].as_str().unwrap_or("world")),
            }])
        } else {
            Err(McpError::ToolNotFound(name.to_owned()))
        }
    }
}

/// A provider that opts in to the optional hooks the default `Demo` leaves alone: resource templates,
/// a context-aware resource read, whole-number progress and a free-form server notification.
struct Extras;

impl ToolProvider for Extras {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![
            ToolDescriptor::new(
                "exact_progress",
                "Reports whole-number progress",
                json!({"type": "object"}),
            )
            .read_only(true),
            ToolDescriptor::new(
                "forbidden",
                "Refused by the hook",
                json!({"type": "object"}),
            )
            .read_only(true),
            ToolDescriptor::new(
                "hidden",
                "Not exposed by the hook",
                json!({"type": "object"}),
            )
            .read_only(true),
        ]
    }

    fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
        match name {
            "forbidden" | "hidden" => Ok(ToolResult::text("reached the provider")),
            other => Err(McpError::ToolNotFound(other.to_owned())),
        }
    }

    fn call_with_handle(&self, name: &str, args: Value, call: &CallHandle) -> Result<ToolResult> {
        match name {
            "exact_progress" => {
                let progress = call.notify_progress_number(
                    serde_json::Number::from(2u64),
                    Some(serde_json::Number::from(4u64)),
                    Some("half"),
                );
                let custom = call.notify("notifications/message", json!({"level": "info"}));
                Ok(ToolResult::text(format!("sent: {progress} {custom}")))
            }
            _ => self.call(name, args),
        }
    }

    fn list_resource_templates(
        &self,
        _request: &Value,
        _context: &RequestContext,
    ) -> Option<Result<Value>> {
        Some(Ok(json!({
            "resourceTemplates": [{"uriTemplate": "demo://notes/{id}", "name": "note"}]
        })))
    }

    fn read_resource_with_context(&self, uri: &str, context: &ReadContext<'_>) -> Result<Value> {
        Ok(json!({
            "contents": [{"uri": uri, "text": context.access_grants.join(",")}],
            "method": context.request["method"],
            "transport": format!("{:?}", context.context.transport),
        }))
    }
}

/// A custom admission hook: refuses one tool by name, hides another, and records what it was asked.
struct NamedPolicy {
    seen: Mutex<Vec<(String, bool, Option<String>)>>,
}

impl AdmissionHook for NamedPolicy {
    fn admit(&self, req: &AdmissionRequest) -> Admission {
        self.seen.lock().unwrap().push((
            req.tool.to_owned(),
            req.descriptor.is_some(),
            req.session.map(str::to_owned),
        ));
        assert_eq!(req.transport, ServerTransport::Stdio);
        if req.tool == "forbidden" || req.args.get("deny").is_some() {
            Admission::Deny(format!("{} refused by policy", req.tool))
        } else {
            Admission::Allow
        }
    }

    fn expose(&self, tool: &ToolDescriptor) -> bool {
        tool.name != "hidden"
    }
}

/// Output buffer shared with a serving thread, so the test can read what was written.
#[derive(Clone, Default)]
struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn optional_provider_hooks() {
    let host = ServerHost::new("extras", "0.0.1", Arc::new(Extras));
    let templates = ask(&host, "resources/templates/list", json!({}));
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"], "demo://notes/{id}",
        "a provider that opts in answers resources/templates/list"
    );
    let read = ask(
        &host,
        "resources/read",
        json!({"uri": "demo://notes/7", "accessGrants": ["a", 5, "b", null]}),
    );
    assert_eq!(read["result"]["contents"][0]["uri"], "demo://notes/7");
    assert_eq!(
        read["result"]["contents"][0]["text"], "a,b",
        "string grants only, in order"
    );
    assert_eq!(read["result"]["method"], "resources/read");
    assert_eq!(read["result"]["transport"], "Stdio");
    let ungranted = ask(&host, "resources/read", json!({"uri": "demo://notes/8"}));
    assert_eq!(ungranted["result"]["contents"][0]["text"], "");

    // Without a transport that can write outside a reply the call handle is inert.
    let inert = ask(
        &host,
        "tools/call",
        json!({"name": "exact_progress", "arguments": {}, "_meta": {"progressToken": "tok"}}),
    );
    assert_eq!(inert["result"]["content"][0]["text"], "sent: false false");

    // Concurrent stdio serving over in-memory buffers carries both notifications before the reply,
    // and a whole-number progress is written as an integer.
    let request = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": "exact_progress", "arguments": {}, "_meta": {"progressToken": "tok"}}
    });
    let output = SharedBuffer::default();
    host.serve_stdio_concurrent_on(
        Cursor::new(format!("{request}\n").into_bytes()),
        output.clone(),
    )
    .expect("serving ends when input closes");
    let written = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = written.lines().collect();
    assert_eq!(lines.len(), 3, "two notifications and one reply: {written}");
    let progress: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(progress["method"], "notifications/progress");
    assert_eq!(
        progress["params"],
        json!({"progressToken": "tok", "progress": 2, "total": 4, "message": "half"})
    );
    assert!(
        lines[0].contains("\"progress\":2,") && lines[0].contains("\"total\":4}"),
        "whole numbers keep their integer spelling: {}",
        lines[0]
    );
    let custom: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(custom["method"], "notifications/message");
    assert_eq!(custom["params"], json!({"level": "info"}));
    let reply: Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["result"]["content"][0]["text"], "sent: true true");

    // A custom AdmissionHook sees the request and can refuse a call or hide a tool.
    let policy = Arc::new(NamedPolicy {
        seen: Mutex::default(),
    });
    let guarded =
        ServerHost::new("extras", "0.0.1", Arc::new(Extras)).with_admission(policy.clone());
    let listed = ask(&guarded, "tools/list", json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["exact_progress", "forbidden"],
        "expose hides one tool"
    );
    let refused = ask(
        &guarded,
        "tools/call",
        json!({"name": "forbidden", "arguments": {}}),
    );
    assert_eq!(refused["result"]["isError"], true);
    assert_eq!(refused["result"]["structuredContent"]["denied"], true);
    assert!(refused["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("forbidden refused by policy"));
    let by_args = ask(
        &guarded,
        "tools/call",
        json!({"name": "exact_progress", "arguments": {"deny": true}}),
    );
    assert_eq!(by_args["result"]["structuredContent"]["denied"], true);
    assert!(by_args["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("exact_progress refused by policy"));
    let allowed = ask(
        &guarded,
        "tools/call",
        json!({"name": "exact_progress", "arguments": {}}),
    );
    assert_eq!(allowed["result"]["isError"], false);
    assert_eq!(allowed["result"]["content"][0]["text"], "sent: false false");
    let unexposed = ask(
        &guarded,
        "tools/call",
        json!({"name": "hidden", "arguments": {}}),
    );
    assert_eq!(unexposed["result"]["structuredContent"]["denied"], true);
    assert!(unexposed["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("tool 'hidden' is not exposed"));
    let seen = policy.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            ("forbidden".to_owned(), true, None),
            ("exact_progress".to_owned(), true, None),
            ("exact_progress".to_owned(), true, None),
        ],
        "the hook was asked about each exposed tool and never about the hidden one"
    );
}

fn host() -> ServerHost {
    ServerHost::new("demo", "1.2.3", Arc::new(Demo::default())).with_admission(Arc::new(
        ReadOnlyPolicy {
            allow_mutate: false,
            blocked: vec!["secret_tool".to_owned()],
        },
    ))
}

fn frame(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
}

fn ask(host: &ServerHost, method: &str, params: Value) -> Value {
    host.dispatch(&frame(method, params)).expect("a reply")
}

fn error_of(reply: &Value) -> (i64, String) {
    (
        reply["error"]["code"].as_i64().expect("an error code"),
        reply["error"]["message"].as_str().unwrap_or("").to_owned(),
    )
}

fn names_and_types() {
    assert_eq!(
        sanitize_tool_name("My Server", "Read File!"),
        "mcp__my_server__read_file_"
    );
    assert_eq!(sanitize_tool_name("a--b", "c  d"), "mcp__a_b__c_d");
    assert_eq!(
        parse_qualified_name("mcp__srv__tool"),
        Some(("srv".to_owned(), "tool".to_owned()))
    );
    for bad in [
        "mcp__srv",
        "other__srv__tool",
        "mcp____tool",
        "mcp__srv__",
        "",
    ] {
        assert_eq!(parse_qualified_name(bad), None, "{bad:?}");
    }

    let tool = ToolDescriptor::new("echo", "Echo", json!({"type": "object"})).read_only(true);
    assert!(!tool.is_mutating());
    let wire = serde_json::to_value(&tool).unwrap();
    assert_eq!(wire["inputSchema"], json!({"type": "object"}));
    assert_eq!(wire["annotations"], json!({"readOnlyHint": true}));
    let minimal: ToolDescriptor = serde_json::from_value(json!({"name": "x"})).unwrap();
    assert_eq!(
        minimal.input_schema,
        json!({"type": "object"}),
        "a missing schema defaults to an object"
    );
    assert!(
        minimal.is_mutating(),
        "an unannotated tool counts as mutating"
    );
    let tools = vec![tool, minimal];
    assert_eq!(mutating_tools(&tools), ["x"]);

    assert_eq!(ToolResult::text("hi").text_content(), "hi");
    let error = ToolResult::error("bad");
    assert!(error.is_error);
    let value = ToolResult::json(json!({"n": 1}));
    assert_eq!(value.structured, Some(json!({"n": 1})));
    assert_eq!(value.text_content(), r#"{"n":1}"#);
    let wire = value.to_value();
    assert_eq!(wire["isError"], false);
    assert_eq!(wire["structuredContent"], json!({"n": 1}));
    assert_eq!(ToolResult::from_value(&wire), value);
    let mixed = ToolResult::from_value(&json!({
        "content": [
            {"type": "text", "text": "a"},
            {"type": "image", "data": "AAAA", "mimeType": "image/png"},
            {"type": "audio", "data": "BBBB"},
            {"type": "text", "text": "b"}
        ]
    }));
    assert_eq!(mixed.text_content(), "a\nb");
    assert_eq!(
        mixed.content[1],
        Content::Image {
            data: "AAAA".to_owned(),
            mime_type: "image/png".to_owned()
        }
    );
    assert!(matches!(mixed.content[2], Content::Other(_)));
    assert_eq!(
        Content::from_value(&mixed.content[1].to_value()),
        mixed.content[1]
    );

    let message = PromptMessage {
        role: "assistant".to_owned(),
        text: "hi".to_owned(),
    };
    assert_eq!(
        message.to_value(),
        json!({"role": "assistant", "content": {"type": "text", "text": "hi"}})
    );
    assert_eq!(PromptMessage::from_value(&message.to_value()), message);
    assert_eq!(PROTOCOL_VERSION, "2025-03-26");
    assert_eq!(
        SUPPORTED_VERSIONS,
        ["2025-06-18", "2025-03-26", "2024-11-05"]
    );

    for (error, text) in [
        (McpError::Transport("x".into()), "transport error: x"),
        (McpError::Protocol("x".into()), "protocol error: x"),
        (
            McpError::Rpc {
                code: -32601,
                message: "no".into(),
            },
            "rpc error -32601: no",
        ),
        (McpError::ToolNotFound("t".into()), "not found: t"),
        (
            McpError::ResourceNotFound("u".into()),
            "resource not found: u",
        ),
        (McpError::Denied("why".into()), "denied: why"),
        (McpError::PoolFull(4), "connection pool full (4 in use)"),
        (McpError::Timeout, "request timed out"),
        (McpError::Cancelled, "request cancelled"),
        (McpError::Closed, "connection closed"),
        (McpError::Unauthorized, "unauthorized"),
        (McpError::Config("x".into()), "config error: x"),
        (
            McpError::ResponseTooLarge { limit: 9 },
            "response exceeded 9 bytes",
        ),
    ] {
        assert_eq!(error.to_string(), text);
    }
    assert!(
        matches!(McpError::from(io::Error::other("disk")), McpError::Io(m) if m.contains("disk"))
    );
    let as_error: &dyn std::error::Error = &McpError::Closed;
    assert!(!as_error.to_string().is_empty());

    let secret = Secret::new("tok-123");
    assert_eq!(secret.expose(), "tok-123");
    assert!(
        !format!("{secret:?}{secret}").contains("tok-123"),
        "a secret never prints itself"
    );
    struct Provider;
    impl AuthProvider for Provider {
        fn bearer(&self, server: &str) -> Result<Option<Secret>> {
            Ok((server == "known").then(|| Secret::new("tok")))
        }

        fn on_unauthorized(&self, _server: &str, _www_authenticate: Option<&str>) -> Result<bool> {
            Ok(false)
        }
    }
    assert_eq!(Provider.bearer("known").unwrap().unwrap().expose(), "tok");
    assert!(Provider.bearer("other").unwrap().is_none());
}

fn raw_json_and_events() {
    let text = r#"{"z":1.0,"a":1E2,"u":"é😀\/"}"#;
    let raw = RawJson::new(text).unwrap();
    assert_eq!(raw.get(), text);
    assert_eq!(raw, RawJson::from_slice(text.as_bytes()).unwrap());
    assert_eq!(
        RawJson::from_serialize(&json!({"a": 1})).unwrap().get(),
        r#"{"a":1}"#
    );
    assert_eq!(raw.clone().into_inner().get(), text);
    for bad in ["{\"a\":", "{} {}", ""] {
        assert!(RawJson::new(bad).is_err(), "{bad:?}");
    }
    assert!(RawJson::from_slice(&[0xff]).is_err());
    let tree = json!({"x": [1, raw.to_value()], "y": raw.to_value()});
    assert_eq!(
        to_wire_string(&tree),
        format!(r#"{{"x":[1,{text}],"y":{text}}}"#)
    );
    let plain = json!({"b": [1, 2.5, "x\n", null], "a": {"k": true}});
    assert_eq!(to_wire_string(&plain), plain.to_string());

    for (kind, label) in [
        (LifecycleKind::Start, "start"),
        (LifecycleKind::Stop, "stop"),
        (LifecycleKind::Crash, "crash"),
        (LifecycleKind::Denied, "denied"),
    ] {
        assert_eq!(kind.label(), label);
    }
    let mut event = McpEvent::new(LifecycleKind::Denied, "mcpc-9", "refused");
    event.server = Some("srv".to_owned());
    event.tool = Some("write".to_owned());
    event.transport = Some("stdio");
    let json = event.to_json();
    assert_eq!(json["event"], "denied");
    assert_eq!(
        (json["session_id"].as_str(), json["reason"].as_str()),
        (Some("mcpc-9"), Some("refused"))
    );
    assert_eq!(
        (
            json["server"].as_str(),
            json["tool"].as_str(),
            json["transport"].as_str()
        ),
        (Some("srv"), Some("write"), Some("stdio"))
    );
    assert!(json["pid"].is_null());
    assert_eq!(event.timestamp_ms(), json["ts_ms"].as_u64().unwrap());
}

fn mcp_json_config() {
    let env: HashMap<String, String> = [("ROOT".to_owned(), "/work".to_owned())].into();
    let text = r#"{"mcpServers": {
        "zeta": {"url": "https://mcp.example/${PATH_PART:-mcp}"},
        "alpha": {"command": "my-mcp-server", "args": ["--root", "${ROOT}", "--mode=${MODE:-safe}"],
                  "env": {"TOKEN_FILE": "${ROOT}/token"}, "cwd": "/work"}
    }}"#;
    let entries = parse_mcp_json(text, ServerSource::User, &env).unwrap();
    assert_eq!(
        entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        ["alpha", "zeta"],
        "sorted by name"
    );
    assert_eq!(
        entries[0].spec,
        ServerSpec::Stdio {
            command: "my-mcp-server".to_owned(),
            args: vec![
                "--root".to_owned(),
                "/work".to_owned(),
                "--mode=safe".to_owned()
            ],
            env: BTreeMap::from([("TOKEN_FILE".to_owned(), "/work/token".to_owned())]),
            cwd: Some(PathBuf::from("/work")),
        }
    );
    assert_eq!(
        entries[1].spec,
        ServerSpec::Http {
            url: "https://mcp.example/mcp".to_owned()
        }
    );
    assert_eq!(entries[0].source, ServerSource::User);
    let workspace = parse_mcp_json(text, ServerSource::Workspace("ws".to_owned()), &env).unwrap();
    assert_eq!(
        workspace[0].source,
        ServerSource::Workspace("ws".to_owned())
    );
    assert!(parse_mcp_json("{}", ServerSource::User, &env)
        .unwrap()
        .is_empty());
    for (bad, why) in [
        (
            r#"{"mcpServers": {"a": {"command": "x ${MISSING}"}}}"#,
            "an unset variable",
        ),
        (
            r#"{"mcpServers": {"a": {"command": "x ${UNTERMINATED"}}}"#,
            "an unterminated reference",
        ),
        (
            r#"{"mcpServers": {"a": {"url": "https://x", "type": "sse"}}}"#,
            "legacy SSE",
        ),
        (r#"{"mcpServers": {"a": {}}}"#, "neither command nor url"),
        ("not json", "invalid JSON"),
    ] {
        assert!(
            matches!(
                parse_mcp_json(bad, ServerSource::User, &env),
                Err(McpError::Config(_))
            ),
            "must refuse {why}"
        );
    }
    assert!(matches!(
        parse_mcp_json(r#"{"mcpServers": {"a": {"command": "${NOPE}"}}}"#, ServerSource::User, &env),
        Err(McpError::Config(m)) if m == "variable 'NOPE' is not set"
    ));
}

fn stdio_spec() -> ServerSpec {
    ServerSpec::Stdio {
        command: "my-mcp-server".to_owned(),
        args: vec!["--root".to_owned(), "/work".to_owned()],
        env: BTreeMap::from([
            ("TOKEN_FILE".to_owned(), "/x".to_owned()),
            ("PATH".to_owned(), "/usr/bin".to_owned()),
        ]),
        cwd: None,
    }
}

fn trust(dir: &TempDir) {
    // The fingerprint is the SHA-256 of the canonical (sorted-key) launch description, version 2.
    let http = ServerSpec::Http {
        url: "https://mcp.example/mcp".to_owned(),
    };
    assert_eq!(
        fingerprint(&http),
        "sha256:75e111769eeff56e631d3bb6a1cc5c8419a7768d6f2f3307d9e11e2e85b42399"
    );
    assert_eq!(
        fingerprint(&stdio_spec()),
        "sha256:655f2898ee28b79360df56296d2e0bedfbd52855201b7b871a9ba38526e0385a"
    );
    let with_cwd = match stdio_spec() {
        ServerSpec::Stdio {
            command, args, env, ..
        } => ServerSpec::Stdio {
            command,
            args,
            env,
            cwd: Some(PathBuf::from("/work")),
        },
        other => other,
    };
    assert_eq!(
        fingerprint(&with_cwd),
        "sha256:ab4ef59421a679b1554cc4a0e0e597a44e1348d67ea17aca95d5f9635da3b033"
    );
    let changed_env = match stdio_spec() {
        ServerSpec::Stdio {
            command,
            args,
            mut env,
            cwd,
        } => {
            env.insert("PATH".to_owned(), "/opt/evil".to_owned());
            ServerSpec::Stdio {
                command,
                args,
                env,
                cwd,
            }
        }
        other => other,
    };
    assert_ne!(
        fingerprint(&changed_env),
        fingerprint(&stdio_spec()),
        "an environment value is part of the identity"
    );

    let mut store = TrustStore::new();
    let spec = stdio_spec();
    assert_eq!(store.decide("ws", "srv", &spec), TrustDecision::Pending);
    assert_eq!(
        store.gate(&ServerSource::User, "srv", &spec),
        TrustDecision::Approved,
        "user servers are exempt"
    );
    let workspace = ServerSource::Workspace("ws".to_owned());
    assert_eq!(store.gate(&workspace, "srv", &spec), TrustDecision::Pending);
    assert!(!store.credential_write_allowed(&workspace, "srv", &spec));
    assert!(store.credential_write_allowed(&ServerSource::User, "srv", &spec));
    store.approve("ws", "srv", &spec);
    assert_eq!(store.decide("ws", "srv", &spec), TrustDecision::Approved);
    assert_eq!(
        store.gate_stdio("ws", "srv", &spec),
        TrustDecision::Approved
    );
    assert!(store.credential_write_allowed(&workspace, "srv", &spec));
    assert_eq!(
        store.decide("ws", "srv", &changed_env),
        TrustDecision::Pending,
        "a changed spec needs re-approval"
    );
    assert!(!store.credential_write_allowed(&workspace, "srv", &changed_env));
    assert_eq!(
        store.decide("other-ws", "srv", &spec),
        TrustDecision::Pending,
        "approvals are per workspace"
    );
    assert_eq!(
        store.gate_stdio("ws", "srv", &http),
        TrustDecision::Pending,
        "the stdio gate never approves HTTP"
    );
    store.deny("ws", "srv", &spec);
    assert_eq!(store.decide("ws", "srv", &spec), TrustDecision::Denied);

    // What a user is asked to approve names env variables, never their values.
    let pending = store.pending_approval("ws", "srv", &spec);
    assert_eq!(
        (pending.command.as_str(), pending.args.len()),
        ("my-mcp-server", 2)
    );
    assert_eq!(pending.env_keys, ["PATH", "TOKEN_FILE"]);
    assert_eq!(pending.fingerprint, fingerprint(&spec));
    assert!(!serde_json::to_string(&pending)
        .unwrap()
        .contains("/usr/bin"));
    let refused = McpError::NotApproved(Box::new(pending));
    assert!(refused
        .to_string()
        .starts_with("server 'srv' in workspace 'ws' is not approved (fingerprint sha256:"));

    // A host that keeps its own content hash.
    let mut own = TrustStore::new();
    assert!(own.approve_with_fingerprint("ws", "srv", "host-hash-1"));
    assert!(
        !own.approve_with_fingerprint("ws", "srv", ""),
        "an empty fingerprint is ignored"
    );
    assert_eq!(
        own.decide_with_fingerprint("ws", "srv", "host-hash-1"),
        TrustDecision::Approved
    );
    assert_eq!(
        own.decide_with_fingerprint("ws", "srv", "host-hash-2"),
        TrustDecision::Pending
    );
    assert_eq!(
        own.decide_with_fingerprint("ws", "srv", ""),
        TrustDecision::Pending
    );
    assert_eq!(
        own.decide("ws", "srv", &spec),
        TrustDecision::Pending,
        "a foreign fingerprint never satisfies decide"
    );
    assert!(own.deny_with_fingerprint("ws", "srv", "host-hash-1"));
    assert_eq!(
        own.decide_with_fingerprint("ws", "srv", "host-hash-1"),
        TrustDecision::Denied
    );

    // Persistence: a missing file is an empty store, a corrupt one is an error, never an empty store.
    let path = dir.0.join("trust").join("trust.json");
    assert_eq!(
        TrustStore::try_load(&path)
            .unwrap()
            .decide("ws", "srv", &spec),
        TrustDecision::Pending
    );
    let mut approved = TrustStore::new();
    approved.approve("ws", "srv", &spec);
    approved.save(&path).unwrap();
    assert_eq!(
        TrustStore::try_load(&path)
            .unwrap()
            .decide("ws", "srv", &spec),
        TrustDecision::Approved
    );
    std::fs::write(&path, b"{ not json").unwrap();
    assert_eq!(
        TrustStore::try_load(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    let foreign = dir.0.join("trust").join("foreign.json");
    std::fs::write(
        &foreign,
        br#"{"approvals": [{"w": "ws", "s": "srv", "h": "their-hash", "ok": true}]}"#,
    )
    .unwrap();
    let migrate = |document: &Value| -> Option<Vec<ImportedTrust>> {
        let rows = document.get("approvals")?.as_array()?;
        Some(
            rows.iter()
                .map(|row| ImportedTrust {
                    workspace: row["w"].as_str().unwrap().to_owned(),
                    server: row["s"].as_str().unwrap().to_owned(),
                    fingerprint: row["h"].as_str().unwrap().to_owned(),
                    approved: row["ok"].as_bool().unwrap(),
                })
                .collect(),
        )
    };
    let migrated = TrustStore::try_load_migrating(&foreign, migrate).unwrap();
    assert_eq!(
        migrated.decide_with_fingerprint("ws", "srv", "their-hash"),
        TrustDecision::Approved
    );
    assert_eq!(
        migrated.decide("ws", "srv", &spec),
        TrustDecision::Pending,
        "an imported record fails closed in decide"
    );
    assert_eq!(
        TrustStore::try_load_migrating(&foreign, |_| None)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        TrustStore::try_load_migrating(&dir.0.join("trust").join("absent.json"), |_| None).is_ok()
    );
    approved.save(&path).unwrap();
    assert_eq!(
        TrustStore::try_load_migrating(&path, |_| panic!("a native store is not migrated"))
            .unwrap()
            .decide("ws", "srv", &spec),
        TrustDecision::Approved
    );
}

fn leased_pool() {
    let pool: LeasedPool<String, String> = LeasedPool::new(LeasedPoolConfig::default());
    let created = AtomicUsize::new(0);
    let create = || {
        created.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new("conn".to_owned()))
    };
    let first = pool.acquire("a".to_owned(), create).unwrap();
    let second = pool.acquire("a".to_owned(), create).unwrap();
    assert_eq!(
        created.load(Ordering::SeqCst),
        1,
        "a cached value is shared"
    );
    assert_eq!((first.epoch(), second.epoch()), (0, 0));
    assert_eq!(&*first, "conn");
    assert_eq!(*first.value(), "conn");
    assert_eq!(pool.len(), 1);
    assert!(!pool.is_empty());
    assert!(pool.is_current(&"a".to_owned(), &first));
    let rows = pool.snapshot();
    assert_eq!(
        (rows.len(), rows[0].state, rows[0].leases),
        (1, EntryState::Ready, 2)
    );
    assert_eq!(
        pool.evict_idle(Duration::ZERO),
        0,
        "leased values are never evicted"
    );

    assert_eq!(pool.bump_epoch(&"a".to_owned()), 1);
    assert!(
        !pool.is_current(&"a".to_owned(), &first),
        "a lease out keeps working and reports its old epoch"
    );
    let third = pool.acquire("a".to_owned(), create).unwrap();
    assert_eq!(
        (created.load(Ordering::SeqCst), third.epoch()),
        (2, 1),
        "the next acquire reconnects"
    );
    assert_eq!(pool.bump_epoch_on_reload(), 1);
    assert_eq!(pool.current_epoch(&"a".to_owned()), 2);
    drop((first, second, third));
    assert_eq!(
        pool.evict_idle(Duration::ZERO),
        1,
        "an idle value is evicted"
    );
    assert!(pool.is_empty());
    let _kept = pool.acquire("b".to_owned(), create).unwrap();
    assert!(pool.remove(&"b".to_owned()));
    assert!(!pool.remove(&"b".to_owned()));

    // Single flight: concurrent first use connects once.
    let shared: Arc<LeasedPool<u8, u32>> = Arc::new(LeasedPool::new(LeasedPoolConfig::default()));
    let connects = Arc::new(AtomicUsize::new(0));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let (shared, connects) = (Arc::clone(&shared), Arc::clone(&connects));
            thread::spawn(move || {
                shared
                    .acquire(1, || {
                        connects.fetch_add(1, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(20));
                        Ok(Arc::new(7))
                    })
                    .map(|lease| *lease)
            })
        })
        .collect();
    for worker in workers {
        assert_eq!(worker.join().unwrap().unwrap(), 7);
    }
    assert_eq!(connects.load(Ordering::SeqCst), 1);

    // A failed connect cools down without calling create again.
    let cooling: LeasedPool<u8, u32> = LeasedPool::new(LeasedPoolConfig {
        reconnect_cooldown: Duration::from_secs(60),
        cache_lifetime: None,
    });
    assert!(cooling
        .acquire(1, || Err(McpError::Transport("down".to_owned())))
        .is_err());
    let again = cooling.acquire(1, || panic!("create must not run inside the cooldown"));
    assert!(
        matches!(again, Err(McpError::Transport(m)) if m.contains("cooling down") && m.contains("down"))
    );
    assert_eq!(cooling.snapshot()[0].state, EntryState::CoolingDown);
    assert!(
        cooling.remove(&1),
        "removing the failure record ends the cooldown"
    );
    assert!(cooling.acquire(1, || Ok(Arc::new(1))).is_ok());

    // A cached value past its lifetime is reconnected.
    let short: LeasedPool<u8, u32> = LeasedPool::new(LeasedPoolConfig {
        reconnect_cooldown: Duration::ZERO,
        cache_lifetime: Some(Duration::ZERO),
    });
    let counted = AtomicUsize::new(0);
    for _ in 0..2 {
        short
            .acquire(1, || {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(1))
            })
            .unwrap();
    }
    assert_eq!(counted.load(Ordering::SeqCst), 2);
}

fn pure_client_logic() {
    let defaults = ClientOptions::default();
    assert_eq!(defaults.request_timeout, Duration::from_secs(60));
    assert_eq!(defaults.client_name, "rightkit-mcp");
    assert!(
        defaults.connect_timeout.is_none()
            && defaults.on_event.is_none()
            && defaults.auth.is_none()
    );
    assert_eq!(
        ClientOptions::default()
            .connect_timeout(Duration::from_secs(5))
            .connect_timeout,
        Some(Duration::from_secs(5))
    );
    let extensions = ClientExtensions::default();
    assert!(!extensions.legacy_sse && !extensions.allow_loopback);
    assert!(
        extensions.launcher.is_none()
            && extensions.elicitation.is_none()
            && extensions.protocol_versions.is_none()
    );
    assert_eq!(
        extensions.response_limits,
        ResponseLimits {
            max_response_bytes: 16 * 1024 * 1024,
            max_frame_bytes: 16 * 1024 * 1024
        }
    );
    assert_eq!(
        extensions.endpoint_origin_policy,
        EndpointOriginPolicy::default()
    );
    let _ = DefaultProcessLauncher;

    let policy = EndpointOriginPolicy::default();
    assert_eq!(
        policy
            .resolve_endpoint("https://EXAMPLE.com:443/sse", "/messages?sid=1")
            .unwrap(),
        "https://example.com/messages?sid=1"
    );
    assert_eq!(
        policy
            .resolve_endpoint("https://example.com/dir/sse", "../messages")
            .unwrap(),
        "https://example.com/messages"
    );
    for cross in [
        "https://evil.test/messages",
        "https://example.com:444/messages",
        "http://example.com/messages",
        "//evil.test/messages",
    ] {
        assert!(
            matches!(
                policy.resolve_endpoint("https://example.com/sse", cross),
                Err(McpError::CrossOriginEndpoint { .. })
            ),
            "{cross}"
        );
    }
    for malformed in [
        "ftp://example.com/m",
        "https://user:secret@example.com/m",
        "https://example.com/m#frag",
    ] {
        assert!(
            matches!(
                policy.resolve_endpoint("https://example.com/sse", malformed),
                Err(McpError::Protocol(_))
            ),
            "{malformed}"
        );
    }
    let allowed = EndpointOriginPolicy {
        allowed_origins: vec!["https://OTHER.test:443".to_owned()],
    };
    assert_eq!(
        allowed
            .resolve_endpoint("https://example.com/sse", "https://other.test/m")
            .unwrap(),
        "https://other.test/m"
    );
    assert!(allowed
        .resolve_endpoint("https://example.com/sse", "https://other.test:444/m")
        .is_err());
    assert!(EndpointOriginPolicy::allow_any_origin_insecure()
        .resolve_endpoint("https://example.com/sse", "https://anywhere.test/m")
        .is_ok());

    let token = CancellationToken::new();
    let clone = token.clone();
    assert_eq!(token, clone);
    assert_ne!(token, CancellationToken::new());
    let mut waiting = std::pin::pin!(token.cancelled());
    let mut context = Context::from_waker(Waker::noop());
    assert!(waiting.as_mut().poll(&mut context).is_pending());
    clone.cancel();
    assert!(token.is_cancelled());
    assert!(
        waiting.as_mut().poll(&mut context).is_ready(),
        "cancellation is sticky and wakes every waiter"
    );
    block_on(token.cancelled());

    let lenient = CallOptions::default();
    assert!(
        lenient.session_id.is_none()
            && lenient.headers.is_empty()
            && lenient.cancellation.is_none()
    );
    assert!(matches!(ServerTransport::Stdio, ServerTransport::Stdio));
    assert_eq!(RequestContext::default().transport, ServerTransport::Stdio);
    let _ = HttpServeOptions {
        port: 0,
        token: None,
    };
}

fn server_dispatch() {
    let host = host();
    let init = ask(
        &host,
        "initialize",
        json!({"protocolVersion": "2025-06-18"}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(
        init["result"]["serverInfo"],
        json!({"name": "demo", "version": "1.2.3"})
    );
    assert_eq!(
        init["result"]["capabilities"],
        json!({"tools": {}, "resources": {}, "prompts": {}})
    );
    assert_eq!(init["id"], 1);
    assert_eq!(
        ask(
            &host,
            "initialize",
            json!({"protocolVersion": "1999-01-01"})
        )["result"]["protocolVersion"],
        "2025-03-26"
    );
    let described = host
        .clone()
        .with_instructions("Be brief")
        .with_initialize_overrides(InitializeOverrides {
            server_title: Some("Demo".to_owned()),
            server_info: Some(json!({"vendor": "acme"})),
            capabilities: Some(json!({"logging": {}})),
            default_protocol_version: Some("2024-11-05".to_owned()),
            ..InitializeOverrides::default()
        });
    let info = ask(
        &described,
        "initialize",
        json!({"protocolVersion": "1999-01-01"}),
    );
    assert_eq!(info["result"]["instructions"], "Be brief");
    assert_eq!(
        info["result"]["serverInfo"],
        json!({"name": "demo", "version": "1.2.3", "vendor": "acme", "title": "Demo"})
    );
    assert_eq!(info["result"]["capabilities"]["logging"], json!({}));
    assert_eq!(info["result"]["protocolVersion"], "2024-11-05");

    // Negotiation against a configured list, and an explicit supportedProtocolVersions extension.
    let versioned = host
        .clone()
        .with_protocol_versions(vec!["2025-03-26".to_owned(), "2025-11-25".to_owned()]);
    let chosen = ask(
        &versioned,
        "initialize",
        json!({"protocolVersion": "2025-03-26", "supportedProtocolVersions": ["2025-11-25", "2024-11-05"]}),
    );
    assert_eq!(chosen["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        error_of(&ask(
            &versioned,
            "initialize",
            json!({"supportedProtocolVersions": ["2020-01-01"]})
        )),
        (-32602, "no common protocol version".to_owned())
    );
    assert_eq!(
        error_of(&ask(
            &host.clone().with_protocol_versions(Vec::new()),
            "initialize",
            json!({})
        ))
        .0,
        -32602
    );
    let verbatim = host.clone().with_options(ServerOptions {
        initialize_result: Some(json!({"fixed": true})),
        ..ServerOptions::default()
    });
    assert_eq!(
        ask(&verbatim, "initialize", json!({}))["result"],
        json!({"fixed": true})
    );

    assert_eq!(ask(&host, "ping", json!({}))["result"], json!({}));
    assert!(
        host.dispatch(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .is_none(),
        "a notification gets no reply"
    );
    assert_eq!(
        error_of(&ask(&host, "foo/bar", json!({}))),
        (-32601, "method not found: foo/bar".to_owned())
    );
    let membrane = host.clone().with_options(ServerOptions {
        method_not_found_message: Some("Method not found".to_owned()),
        ..ServerOptions::default()
    });
    assert_eq!(
        error_of(&ask(&membrane, "foo/bar", json!({}))).1,
        "Method not found"
    );
    assert_eq!(
        error_of(&ask(&host, "resources/templates/list", json!({}))).0,
        -32601,
        "templates stay unsupported unless a provider opts in"
    );

    // Tools: admission hides and denies; tool-level failures are results, RPC failures are errors.
    let listed = ask(&host, "tools/list", json!({}));
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"echo") && names.contains(&"write_marker"));
    assert!(
        !names.contains(&"secret_tool"),
        "a blocked tool is not exposed"
    );
    let echoed = ask(
        &host,
        "tools/call",
        json!({"name": "echo", "arguments": {"message": "hi"}}),
    );
    assert_eq!(echoed["result"]["content"][0]["text"], "echo: hi");
    assert_eq!(echoed["result"]["isError"], false);
    let sum = ask(
        &host,
        "tools/call",
        json!({"name": "add", "arguments": {"a": 2, "b": 3}}),
    );
    assert_eq!(
        sum["result"]["structuredContent"]["sum"].as_f64(),
        Some(5.0)
    );
    let denied = ask(
        &host,
        "tools/call",
        json!({"name": "write_marker", "arguments": {"path": "x"}}),
    );
    assert_eq!(denied["result"]["isError"], true);
    assert_eq!(denied["result"]["structuredContent"]["denied"], true);
    assert!(denied["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("denied: tool 'write_marker' is mutating"));
    let hidden = ask(&host, "tools/call", json!({"name": "secret_tool"}));
    assert_eq!(hidden["result"]["structuredContent"]["denied"], true);
    assert_eq!(
        ask(&host, "tools/call", json!({"name": "fail"}))["result"]["isError"],
        true
    );
    let unknown = ask(&host, "tools/call", json!({"name": "nope"}));
    assert_eq!(
        unknown["result"]["structuredContent"]["denied"], true,
        "a policy that admits only read-only tools refuses a tool it cannot describe"
    );
    assert_eq!(
        error_of(&ask(&host, "tools/call", json!({}))),
        (-32602, "missing tool name".to_owned())
    );
    let failed = ask(&host, "tools/call", json!({"name": "rpc_fail"}));
    assert_eq!(error_of(&failed), (-32050, "custom failure".to_owned()));
    assert_eq!(failed["error"]["data"], json!({"detail": "kept"}));
    let wire = to_wire_string(&ask(&host, "tools/call", json!({"name": "raw"})));
    assert!(
        wire.contains(r#""structuredContent":{"z":1.0,"a":1E2}"#),
        "RawJson reaches the wire byte for byte: {wire}"
    );
    let open = ServerHost::new("open", "1", Arc::new(Demo::default()));
    assert_eq!(
        error_of(&ask(&open, "tools/call", json!({"name": "nope"}))),
        (-32602, "unknown: nope".to_owned())
    );
    assert_eq!(
        ask(
            &open,
            "tools/call",
            json!({"name": "write_marker", "arguments": {"path": "p"}})
        )["result"]["content"][0]["text"],
        "wrote p"
    );

    // Resources and prompts.
    assert_eq!(
        ask(&host, "resources/list", json!({}))["result"]["resources"][0]["uri"],
        "demo://readme"
    );
    assert_eq!(
        ask(&host, "resources/read", json!({"uri": "demo://readme"}))["result"]["contents"][0]
            ["text"],
        "hello resource"
    );
    let missing = ask(&host, "resources/read", json!({"uri": "demo://nope"}));
    assert_eq!(
        error_of(&missing),
        (-32002, "Resource not found".to_owned())
    );
    assert_eq!(missing["error"]["data"], json!({"uri": "demo://nope"}));
    assert_eq!(
        ask(&host, "prompts/list", json!({}))["result"]["prompts"][0]["arguments"][0]["required"],
        true
    );
    assert_eq!(
        ask(
            &host,
            "prompts/get",
            json!({"name": "greet", "arguments": {"who": "Ada"}})
        )["result"]["messages"][0]["content"]["text"],
        "Hello Ada"
    );
    assert_eq!(
        error_of(&ask(&host, "prompts/get", json!({"name": "nope"}))).0,
        -32602
    );
}

fn server_policies() {
    let host = host();
    let ping = |id: i64| json!({"jsonrpc": "2.0", "id": id, "method": "ping"});
    let batch =
        json!([ping(1), ping(2), {"jsonrpc": "2.0", "method": "notifications/initialized"}]);
    let replies = host.dispatch(&batch).unwrap();
    assert_eq!(
        replies
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [1, 2],
        "replies in order, notifications skipped"
    );
    assert!(host.dispatch(&json!([])).is_none());

    let batches = |policy: BatchPolicy| {
        host.clone().with_options(ServerOptions {
            batch_policy: policy,
            ..ServerOptions::default()
        })
    };
    let rejected = batches(BatchPolicy::Reject).dispatch(&batch).unwrap();
    assert_eq!(
        (rejected["error"]["code"].as_i64(), &rejected["id"]),
        (Some(-32600), &Value::Null)
    );
    assert!(batches(BatchPolicy::Ignore).dispatch(&batch).is_none());
    let per_version = host
        .clone()
        .with_batch_policy_for_version("2025-06-18", BatchPolicy::Reject);
    let at = |version: &str| RequestContext {
        metadata: json!({"protocolVersion": version}),
        ..RequestContext::default()
    };
    assert!(per_version
        .dispatch_with_context(&batch, &at("2025-06-18"))
        .unwrap()
        .get("error")
        .is_some());
    assert!(
        per_version
            .dispatch_with_context(&batch, &at("2025-03-26"))
            .unwrap()
            .is_array(),
        "other versions keep the default"
    );

    let null_ids = host.clone().with_options(ServerOptions {
        id_policy: IdPolicy::NullResponse,
        ..ServerOptions::default()
    });
    let nameless = json!({"jsonrpc": "2.0", "method": "ping"});
    assert!(host.dispatch(&nameless).is_none());
    assert_eq!(null_ids.dispatch(&nameless).unwrap()["id"], Value::Null);
    let initialized = json!({"jsonrpc": "2.0", "id": 3, "method": "notifications/initialized"});
    assert_eq!(error_of(&host.dispatch(&initialized).unwrap()).0, -32601);
    let ignoring = host.clone().with_options(ServerOptions {
        initialized_policy: InitializedPolicy::Ignore,
        ..ServerOptions::default()
    });
    assert!(ignoring.dispatch(&initialized).is_none());

    let strict = host
        .clone()
        .with_request_validation(RequestValidation::Strict);
    for (bad, id) in [
        (json!(7), Value::Null),
        (json!({"jsonrpc": "2.0", "id": 5}), json!(5)),
        (
            json!({"jsonrpc": "2.0", "id": true, "method": "ping"}),
            Value::Null,
        ),
        (
            json!({"jsonrpc": "2.0", "id": 6, "method": "ping", "params": []}),
            json!(6),
        ),
    ] {
        let reply = strict.dispatch(&bad).unwrap();
        assert_eq!(
            error_of(&reply),
            (-32600, "Invalid Request".to_owned()),
            "{bad}"
        );
        assert_eq!(reply["id"], id);
    }
    assert_eq!(
        error_of(&strict.dispatch(&json!([])).unwrap()).0,
        -32600,
        "an empty batch is invalid in strict mode"
    );
    assert!(host.dispatch(&json!(7)).is_none(), "lenient mode drops it");
    assert!(strict.dispatch(&ping(9)).unwrap().get("result").is_some());

    // Response limits, hook and transform.
    let small = host.clone().with_response_limits(ResponseLimits {
        max_response_bytes: 120,
        max_frame_bytes: 120,
    });
    let context = RequestContext::default();
    assert!(small
        .dispatch_checked(&ping(1), &context)
        .unwrap()
        .is_some());
    assert!(small
        .dispatch_checked(&json!({"jsonrpc": "2.0", "method": "ping"}), &context)
        .unwrap()
        .is_none());
    assert!(matches!(
        small.dispatch_checked(&frame("tools/list", json!({})), &context),
        Err(McpError::ResponseTooLarge { limit: 120 })
    ));
    let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let observed = host
        .clone()
        .with_response_hook(move |info| sink.lock().unwrap().push(info.method.map(str::to_owned)))
        .with_response_transform(|info, result| {
            if info.method == Some("ping") {
                result["stamped"] = json!(true);
            }
        });
    assert_eq!(
        observed.dispatch(&ping(1)).unwrap()["result"],
        json!({"stamped": true})
    );
    let _ = observed.dispatch(&json!([ping(2)]));
    let rewritten = host.clone().with_request_transform(|ctx, result| {
        result["answered"] = ctx.request()["id"].clone();
    });
    assert_eq!(
        rewritten.dispatch(&ping(41)).unwrap()["result"]["answered"],
        41
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [Some("ping".to_owned()), Some("ping".to_owned())]
    );

    // Admission denials and serving runs are reported as content-free events.
    let events: Arc<Mutex<Vec<McpEvent>>> = Arc::default();
    let sink = Arc::clone(&events);
    let reporting = host
        .clone()
        .on_event(move |event| sink.lock().unwrap().push(event.clone()));
    let _ = reporting.dispatch(&frame(
        "tools/call",
        json!({"name": "write_marker", "arguments": {"path": "secret-path"}}),
    ));
    let recorded = events.lock().unwrap().clone();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        (
            recorded[0].kind,
            recorded[0].tool.as_deref(),
            recorded[0].transport,
            recorded[0].server.as_deref()
        ),
        (
            LifecycleKind::Denied,
            Some("write_marker"),
            Some("stdio"),
            Some("demo")
        )
    );
    assert!(
        !recorded[0].to_json().to_string().contains("secret-path"),
        "events never carry tool arguments"
    );

    // Async dispatch: a handler answers its own method; None falls back to the legacy dispatcher.
    struct AsyncEcho;
    impl AsyncRequestHandler for AsyncEcho {
        fn handle<'a>(
            &'a self,
            request: &'a Value,
            _: &'a RequestContext,
        ) -> std::pin::Pin<Box<dyn Future<Output = Option<Result<Value>>> + Send + 'a>> {
            Box::pin(
                async move { (request["method"] == "echo").then(|| Ok(request["params"].clone())) },
            )
        }
    }
    let asynchronous = host.clone().with_async_handler(Arc::new(AsyncEcho));
    let reply = block_on(
        asynchronous
            .dispatch_async(&json!({"id": "original-id", "method": "echo", "params": {"k": 1}})),
    )
    .unwrap();
    assert_eq!(
        (reply["id"].as_str(), &reply["result"]),
        (Some("original-id"), &json!({"k": 1}))
    );
    assert_eq!(
        block_on(asynchronous.dispatch_async(&ping(8))).unwrap()["result"],
        json!({})
    );
    let batch = block_on(
        asynchronous.dispatch_async(&json!([{"id": 1, "method": "echo", "params": 1}, ping(2)])),
    )
    .unwrap();
    assert_eq!(batch.as_array().unwrap().len(), 2);
    assert!(
        block_on(asynchronous.dispatch_checked_async(&ping(3), &context))
            .unwrap()
            .is_some()
    );
}

fn serving_stdio() {
    let host = host();
    let input = format!(
        "{}\nnot json\n\n{}\n",
        frame("initialize", json!({"protocolVersion": "2025-03-26"})),
        frame("tools/list", json!({}))
    );
    let mut output = Vec::new();
    host.serve_stdio_on(Cursor::new(input.into_bytes()), &mut output)
        .unwrap();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "a blank line gets no reply");
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "demo");
    assert_eq!(
        (lines[1]["error"]["code"].as_i64(), &lines[1]["id"]),
        (Some(-32700), &Value::Null)
    );
    assert!(lines[1]["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("parse error"));
    assert!(lines[2]["result"]["tools"].is_array());

    // An oversize line is answered and drained; serving continues.
    let capped = host.clone().with_max_stdio_frame_bytes(64);
    let input = format!(
        "{}\n{}\n",
        "x".repeat(200),
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})
    );
    let mut output = Vec::new();
    capped
        .serve_stdio_on(Cursor::new(input.into_bytes()), &mut output)
        .unwrap();
    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        error_of(&lines[0]),
        (-32600, "request frame exceeds 64 byte limit".to_owned())
    );
    assert_eq!(lines[1]["id"], 2);

    // FailFast ends serving with the original InvalidData error.
    let strict = host.clone().with_options(ServerOptions {
        stdio_input_mode: StdioInputMode::FailFast,
        ..ServerOptions::default()
    });
    let failure = strict
        .serve_stdio_on_io(Cursor::new(b"not json\n".to_vec()), Vec::new())
        .unwrap_err();
    assert_eq!(failure.kind(), io::ErrorKind::InvalidData);
    assert!(
        matches!(strict.serve_stdio_on(Cursor::new(b"not json\n".to_vec()), Vec::new()), Err(McpError::IoError(e)) if e.kind() == io::ErrorKind::InvalidData)
    );

    let events: Arc<Mutex<Vec<(LifecycleKind, String)>>> = Arc::default();
    let sink = Arc::clone(&events);
    let reporting = host.on_event(move |e| sink.lock().unwrap().push((e.kind, e.reason.clone())));
    reporting
        .serve_stdio_on(Cursor::new(Vec::new()), Vec::new())
        .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        [
            (LifecycleKind::Start, "serving stdio".to_owned()),
            (LifecycleKind::Stop, "input closed".to_owned())
        ]
    );
}

fn connect(
    host: ServerHost,
    serving: Serving,
    trust: &TrustStore,
    source: &ServerSource,
    spec: ServerSpec,
) -> (Result<McpClient>, Launcher, Arc<Mutex<Vec<McpEvent>>>) {
    let harness = launcher(host, serving);
    let events: Arc<Mutex<Vec<McpEvent>>> = Arc::default();
    let sink = Arc::clone(&events);
    let options = ClientOptions::default().on_event(move |e| sink.lock().unwrap().push(e.clone()));
    let extensions = ClientExtensions {
        launcher: Some(Arc::clone(&harness.launcher)),
        ..ClientExtensions::default()
    };
    let client =
        McpClient::connect_with_extensions("demo", source, trust, spec, options, extensions);
    (client, harness, events)
}

fn client_session() {
    let (client, harness, events) = connect(
        host(),
        Serving::Sequential,
        &TrustStore::new(),
        &ServerSource::User,
        stdio_spec(),
    );
    let client = client.expect("a user server connects without approval");
    assert_eq!(
        harness.requests.lock().unwrap().as_slice(),
        [LaunchRequest {
            program: "my-mcp-server".to_owned(),
            args: vec!["--root".to_owned(), "/work".to_owned()],
            env: BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin".to_owned()),
                ("TOKEN_FILE".to_owned(), "/x".to_owned())
            ]),
            cwd: None,
        }],
        "the launcher gets exactly the configured program, arguments, environment and cwd"
    );
    assert_eq!(client.server_name(), "demo");
    assert_eq!(client.protocol_version(), "2025-03-26");
    let session = client
        .session_id()
        .unwrap()
        .expect("a stdio connection has a local routing session");
    assert!(session.starts_with("mcpc-"));

    let tools = client.list_tools().unwrap();
    assert!(tools.iter().any(|t| t.name == "echo" && !t.is_mutating()));
    assert!(tools
        .iter()
        .any(|t| t.name == "write_marker" && t.is_mutating()));
    assert!(!tools.iter().any(|t| t.name == "secret_tool"));
    let echoed = client.call_tool("echo", json!({"message": "hi"})).unwrap();
    assert_eq!(
        (echoed.text_content().as_str(), echoed.is_error),
        ("echo: hi", false)
    );
    let sum = client.call_tool("add", json!({"a": 2, "b": 3})).unwrap();
    assert_eq!(sum.structured, Some(json!({"sum": 5.0})));
    let denied = client
        .call_tool("write_marker", json!({"path": "x"}))
        .unwrap();
    assert!(denied.is_error && denied.structured.as_ref().unwrap()["denied"] == true);
    assert!(client.call_tool("fail", json!({})).unwrap().is_error);
    assert!(
        client.call_tool("nope", json!({})).unwrap().is_error,
        "the read-only policy refuses a tool it cannot describe"
    );
    assert!(matches!(
        client.request_with_options("tools/call", json!({}), &CallOptions::default()),
        Err(McpError::Rpc { code: -32602, .. })
    ));
    assert!(
        matches!(client.call_tool("rpc_fail", json!({})), Err(McpError::RpcWithData { code: -32050, data, .. }) if data == json!({"detail": "kept"}))
    );
    assert_eq!(client.list_resources().unwrap()[0].uri, "demo://readme");
    assert_eq!(
        client
            .read_resource("demo://readme")
            .unwrap()
            .text
            .as_deref(),
        Some("hello resource")
    );
    assert!(matches!(
        client.read_resource("demo://nope"),
        Err(McpError::RpcWithData { code: -32002, .. })
    ));
    assert_eq!(client.list_prompts().unwrap()[0].name, "greet");
    assert_eq!(
        client.get_prompt("greet", json!({"who": "Ada"})).unwrap()[0].text,
        "Hello Ada"
    );
    client.ping().unwrap();

    // Per-call options: a routing session, and a call cancelled before it starts.
    let options = CallOptions {
        session_id: Some("caller-session".to_owned()),
        ..CallOptions::default()
    };
    assert_eq!(
        client
            .call_tool_with_options("echo", json!({"message": "again"}), &options)
            .unwrap()
            .text_content(),
        "echo: again"
    );
    assert_eq!(
        client
            .request_with_options("ping", json!({}), &options)
            .unwrap(),
        json!({})
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let options = CallOptions {
        cancellation: Some(cancelled),
        ..CallOptions::default()
    };
    assert!(matches!(
        client.call_tool_with_options("echo", json!({}), &options),
        Err(McpError::Cancelled)
    ));
    client
        .set_elicitation_handler(&session, Arc::new(|_, _| Ok(json!({"action": "decline"}))))
        .unwrap();
    client.clear_elicitation_handler(&session).unwrap();

    {
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            (
                events[0].kind,
                events[0].pid,
                events[0].transport,
                events[0].server.as_deref()
            ),
            (
                LifecycleKind::Start,
                Some(4242),
                Some("stdio"),
                Some("demo")
            )
        );
    }
    client.close();
    assert_eq!(
        harness.log.lock().unwrap().as_slice(),
        ["kill", "wait"],
        "close kills and reaps the child"
    );
    let events = events.lock().unwrap();
    assert_eq!(
        (events.len(), events[1].kind, events[1].reason.as_str()),
        (2, LifecycleKind::Stop, "closed")
    );
}

fn client_trust_gate() {
    let workspace = ServerSource::Workspace("ws".to_owned());
    let spec = stdio_spec();
    let mut store = TrustStore::new();

    let (client, harness, events) = connect(
        host(),
        Serving::Sequential,
        &store,
        &workspace,
        spec.clone(),
    );
    match client {
        Err(McpError::NotApproved(pending)) => {
            assert_eq!(
                (
                    pending.workspace.as_str(),
                    pending.server.as_str(),
                    pending.fingerprint.as_str()
                ),
                ("ws", "demo", fingerprint(&spec).as_str())
            );
        }
        other => panic!(
            "an unapproved workspace server must not connect: {:?}",
            other.err()
        ),
    }
    assert!(
        harness.requests.lock().unwrap().is_empty(),
        "nothing was launched"
    );
    assert!(events.lock().unwrap().is_empty());

    store.deny("ws", "demo", &spec);
    let (client, harness, _) = connect(
        host(),
        Serving::Sequential,
        &store,
        &workspace,
        spec.clone(),
    );
    assert!(matches!(client, Err(McpError::Denied(_))));
    assert!(harness.requests.lock().unwrap().is_empty());

    store.approve("ws", "demo", &spec);
    let (client, harness, _) = connect(
        host(),
        Serving::Sequential,
        &store,
        &workspace,
        spec.clone(),
    );
    client.expect("an approved server connects").close();
    assert_eq!(harness.requests.lock().unwrap().len(), 1);

    let changed = match spec {
        ServerSpec::Stdio {
            command,
            args,
            mut env,
            cwd,
        } => {
            env.insert("PATH".to_owned(), "/somewhere/else".to_owned());
            ServerSpec::Stdio {
                command,
                args,
                env,
                cwd,
            }
        }
        other => other,
    };
    let (client, harness, _) = connect(host(), Serving::Sequential, &store, &workspace, changed);
    assert!(
        matches!(client, Err(McpError::NotApproved(_))),
        "a changed environment value needs re-approval"
    );
    assert!(harness.requests.lock().unwrap().is_empty());

    // Configuration refused before any launch.
    let (client, harness, _) = connect(
        host(),
        Serving::Sequential,
        &store,
        &ServerSource::User,
        stdio_spec(),
    );
    drop(client);
    let versions = ClientExtensions {
        protocol_versions: Some(vec![String::new()]),
        launcher: Some(Arc::clone(&harness.launcher)),
        ..ClientExtensions::default()
    };
    let before = harness.requests.lock().unwrap().len();
    let refused = McpClient::connect_with_extensions(
        "demo",
        &ServerSource::User,
        &store,
        stdio_spec(),
        ClientOptions::default(),
        versions,
    );
    assert!(matches!(refused, Err(McpError::Config(_))));
    assert_eq!(harness.requests.lock().unwrap().len(), before);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let refused = McpClient::connect_with_extensions_cancellable(
        "demo",
        &ServerSource::User,
        &store,
        stdio_spec(),
        ClientOptions::default(),
        ClientExtensions {
            launcher: Some(Arc::clone(&harness.launcher)),
            ..ClientExtensions::default()
        },
        &cancelled,
    );
    assert!(matches!(refused, Err(McpError::Cancelled)));
    assert_eq!(harness.requests.lock().unwrap().len(), before);
}

fn client_notifications_and_cancellation() {
    // Concurrent serving carries progress notifications and cancellation.
    let provider = Demo::default();
    let served = ServerHost::new("demo", "1.2.3", Arc::new(provider.clone()));
    let harness = launcher(served, Serving::Concurrent);
    let notifications: Arc<Mutex<Vec<ServerNotification>>> = Arc::default();
    let sink = Arc::clone(&notifications);
    let options =
        ClientOptions::default().on_notification(move |n| sink.lock().unwrap().push(n.clone()));
    let extensions = ClientExtensions {
        launcher: Some(Arc::clone(&harness.launcher)),
        ..ClientExtensions::default()
    };
    let client = Arc::new(
        McpClient::connect_with_extensions(
            "demo",
            &ServerSource::User,
            &TrustStore::new(),
            stdio_spec(),
            options,
            extensions,
        )
        .unwrap(),
    );

    let progress = client
        .request_with_options(
            "tools/call",
            json!({"name": "progress", "arguments": {}, "_meta": {"progressToken": "tok"}}),
            &CallOptions::default(),
        )
        .unwrap();
    assert_eq!(
        ToolResult::from_value(&progress).text_content(),
        "progress sent: true"
    );
    let received = notifications.lock().unwrap().clone();
    assert_eq!(received.len(), 1);
    assert_eq!(
        (received[0].server.as_str(), received[0].method.as_str()),
        ("demo", "notifications/progress")
    );
    assert_eq!(
        received[0].params,
        json!({"progressToken": "tok", "progress": 1.0, "total": 2.0, "message": "half"})
    );
    let silent = client.call_tool("progress", json!({})).unwrap();
    assert_eq!(
        silent.text_content(),
        "progress sent: false",
        "no progress token, nothing sent"
    );
    assert_eq!(notifications.lock().unwrap().len(), 1);

    // Cancelling a call in flight returns Cancelled and tells the server.
    let token = CancellationToken::new();
    let caller = {
        let (client, token) = (Arc::clone(&client), token.clone());
        thread::spawn(move || {
            let options = CallOptions {
                cancellation: Some(token),
                ..CallOptions::default()
            };
            client.call_tool_with_options("wait_for_cancel", json!({}), &options)
        })
    };
    for _ in 0..5_000 {
        if provider.started.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        provider.started.load(Ordering::SeqCst),
        "the server began the call"
    );
    token.cancel();
    assert!(matches!(caller.join().unwrap(), Err(McpError::Cancelled)));
    client.close();
    assert!(
        provider.saw_cancel.load(Ordering::SeqCst),
        "the tool saw notifications/cancelled"
    );
}

/// A hand-written server for the one thing `ServerHost` cannot do, which is to ask the client a
/// question. On `tools/call` it sends `elicitation/create` (tied to the call when the tool argument
/// `related` is true) and returns the client's answer, or its error message, as the tool's text.
fn elicitation_server(reader: PipeReader, mut writer: PipeWriter) {
    let mut input = BufReader::new(reader);
    let mut next = move || -> Option<Value> {
        loop {
            let mut line = String::new();
            match input.read_line(&mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            if let Ok(message) = serde_json::from_str::<Value>(&line) {
                return Some(message);
            }
        }
    };
    while let Some(message) = next() {
        let (Some(id), Some(method)) = (message.get("id").cloned(), message["method"].as_str())
        else {
            continue;
        };
        let reply = match method {
            "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": message["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "asker", "version": "1"}
            }}),
            "tools/call" => {
                let mut params = json!({"message": "Proceed?"});
                if message["params"]["arguments"]["related"] == true {
                    params["_meta"] = json!({"relatedRequestId": id});
                }
                let question = json!({
                    "jsonrpc": "2.0", "id": "q-1", "method": "elicitation/create", "params": params
                });
                if writeln!(writer, "{question}").is_err() {
                    return;
                }
                let answer = loop {
                    match next() {
                        Some(m) if m["id"] == "q-1" && m.get("method").is_none() => break m,
                        Some(_) => {}
                        None => return,
                    }
                };
                let text = match answer.get("result") {
                    Some(result) => result.to_string(),
                    None => answer["error"]["message"].as_str().unwrap_or("").to_owned(),
                };
                json!({"jsonrpc": "2.0", "id": id, "result": {
                    "content": [{"type": "text", "text": text}], "isError": false
                }})
            }
            _ => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
        };
        if writeln!(writer, "{reply}").is_err() {
            return;
        }
    }
}

fn client_elicitation() {
    let harness = launcher_with(elicitation_server);
    let seen: Arc<Mutex<Vec<(ElicitationContext, Value)>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let accept: ElicitationHandler = Arc::new(move |context, params| {
        sink.lock().unwrap().push((context.clone(), params.clone()));
        Ok(json!({"action": "accept", "content": {"answer": "yes"}}))
    });
    let client = McpClient::connect_with_extensions(
        "asker",
        &ServerSource::User,
        &TrustStore::new(),
        stdio_spec(),
        ClientOptions::default(),
        ClientExtensions {
            launcher: Some(Arc::clone(&harness.launcher)),
            elicitation: Some(accept),
            ..ClientExtensions::default()
        },
    )
    .expect("connect");
    let session = client
        .session_id()
        .unwrap()
        .expect("a stdio connection has its own routing session");
    let answer = |client: &McpClient, related: bool| -> Value {
        let result = client
            .call_tool("ask", json!({"related": related}))
            .expect("the call completes");
        serde_json::from_str(&result.text_content()).unwrap_or(Value::String(result.text_content()))
    };

    // The default callback is bound to the connection's own session. A question tied to the call by
    // `_meta.relatedRequestId` and an untied one on the only active call both reach it.
    for related in [true, false] {
        assert_eq!(
            answer(&client, related),
            json!({"action": "accept", "content": {"answer": "yes"}}),
            "related={related}"
        );
    }
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        for (context, params) in seen.iter() {
            assert_eq!(context.server, "asker");
            assert_eq!(context.session_id, session);
            assert_eq!(context.wire_request_id, "q-1");
            assert_eq!(context.request_id(), &json!("q-1"));
            assert!(
                context.cancellation().is_some(),
                "the originating call is identified, so its cancellation is offered"
            );
            assert_eq!(params["message"], "Proceed?");
        }
        assert!(seen[0].1["_meta"]["relatedRequestId"].is_u64());
        assert!(seen[1].1.get("_meta").is_none());
    }

    // A handler registered for the session replaces the default one.
    client
        .set_elicitation_handler(&session, Arc::new(|_, _| Ok(json!({"action": "decline"}))))
        .unwrap();
    assert_eq!(answer(&client, true), json!({"action": "decline"}));
    assert_eq!(seen.lock().unwrap().len(), 2, "the default was not called");

    // A handler error is an error reply to the server.
    client
        .set_elicitation_handler(
            &session,
            Arc::new(|_, _| {
                Err(McpError::Rpc {
                    code: -32000,
                    message: "user is away".to_owned(),
                })
            }),
        )
        .unwrap();
    assert_eq!(
        answer(&client, true),
        Value::String("user is away".to_owned())
    );

    // Clearing the handler leaves nobody to answer: the server gets a refusal, not a hang.
    client.clear_elicitation_handler(&session).unwrap();
    assert_eq!(
        answer(&client, true),
        Value::String("elicitation has no unambiguous active session".to_owned())
    );
    client.close();
}

/// The default build is stdio only: HTTP client extensions need the `advanced-http` feature, and
/// asking for them without it is a configuration error raised before any socket exists.
fn default_build_boundary() {
    let refused = McpClient::connect_with_extensions(
        "remote",
        &ServerSource::User,
        &TrustStore::new(),
        ServerSpec::Http {
            url: "https://mcp.example/mcp".to_owned(),
        },
        ClientOptions::default(),
        ClientExtensions::default(),
    );
    assert!(
        matches!(&refused, Err(McpError::Config(message)) if message.contains("advanced-http")),
        "HTTP extensions are refused without the advanced-http feature: {:?}",
        refused.as_ref().err()
    );
}

fn connection_pool() {
    let events: Arc<Mutex<Vec<McpEvent>>> = Arc::default();
    let sink = Arc::clone(&events);
    let options = ClientOptions::default().on_event(move |e| sink.lock().unwrap().push(e.clone()));
    let pool = ConnectionPool::new(
        PoolConfig {
            max_connections: 2,
            options,
        },
        TrustStore::new(),
    );
    assert!(pool.id().starts_with("mcpp-"));
    assert!(pool.is_empty());
    assert_eq!(PoolConfig::default().max_connections, 16);

    let entry = |name: &str, spec: ServerSpec, source: ServerSource| ServerEntry {
        name: name.to_owned(),
        spec,
        source,
    };
    let unapproved = entry(
        "pending",
        stdio_spec(),
        ServerSource::Workspace("ws".to_owned()),
    );
    let denied = entry(
        "denied",
        stdio_spec(),
        ServerSource::Workspace("ws".to_owned()),
    );
    pool.trust().deny("ws", "denied", &denied.spec);
    assert!(matches!(
        pool.get(&unapproved),
        Err(McpError::NotApproved(_))
    ));
    assert!(matches!(pool.get(&denied), Err(McpError::Denied(_))));
    assert!(pool.is_empty(), "a refused server is never connected");

    let catalog = pool.catalog(&[unapproved.clone(), denied.clone()]).unwrap();
    assert!(catalog.tools.is_empty());
    assert_eq!(catalog.skipped.len(), 2);
    assert_eq!(catalog.skipped[0].0, "pending");
    assert!(
        catalog.skipped[0].1.contains("is not approved"),
        "{}",
        catalog.skipped[0].1
    );
    assert!(catalog.skipped[1].1.starts_with("denied:"));
    assert!(matches!(
        catalog.call(&pool, "mcp__pending__echo", json!({})),
        Err(McpError::ToolNotFound(_))
    ));

    let recorded = events.lock().unwrap().clone();
    assert!(
        recorded.len() >= 2
            && recorded
                .iter()
                .all(|e| e.kind == LifecycleKind::Denied && e.session_id == pool.id())
    );
    assert_eq!(recorded[0].server.as_deref(), Some("pending"));
    pool.evict_idle();
    pool.close_all();
    assert!(pool.is_empty());
    let qualified = QualifiedTool {
        qualified: "mcp__s__t".to_owned(),
        server: "s".to_owned(),
        tool: ToolDescriptor::new("t", "", json!({})),
    };
    assert_eq!(
        parse_qualified_name(&qualified.qualified),
        Some(("s".to_owned(), "t".to_owned()))
    );
}

fn main() {
    // A guard against a hung pipe: the recipe never legitimately runs for this long.
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(50));
        eprintln!("rightkit-mcp consumer exceeded its 50 second budget");
        std::process::exit(2);
    });
    let dir = TempDir::new();
    names_and_types();
    raw_json_and_events();
    mcp_json_config();
    trust(&dir);
    leased_pool();
    pure_client_logic();
    server_dispatch();
    server_policies();
    serving_stdio();
    optional_provider_hooks();
    client_session();
    client_trust_gate();
    client_notifications_and_cancellation();
    client_elicitation();
    default_build_boundary();
    connection_pool();
    println!("OK rightkit-mcp (registry consumer)");
}
