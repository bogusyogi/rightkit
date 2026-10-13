//! Wire contracts for opt-in hosting & a Membrane-shaped adapter fixture.
//! Fixtures exercise host behavior without depending on CodeRight's runtime.

use std::io::{self, BufRead, Cursor, Read, Write};
use std::sync::{Arc, Mutex};

use rightkit_mcp::*;
use serde_json::{json, Value};

struct Legacy;

impl ToolProvider for Legacy {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor::new(
            "echo",
            "Echo",
            json!({"type":"object"}),
        )]
    }

    fn call(&self, _name: &str, args: Value) -> Result<ToolResult> {
        Ok(ToolResult::json(args))
    }

    fn resources(&self) -> Vec<ResourceDescriptor> {
        vec![ResourceDescriptor {
            uri: "demo://readme".into(),
            name: "readme".into(),
            description: "Readme".into(),
            mime_type: "text/plain".into(),
        }]
    }

    fn read_resource(&self, uri: &str) -> Result<ResourceContents> {
        Ok(ResourceContents {
            uri: uri.into(),
            mime_type: "text/plain".into(),
            text: Some("hello".into()),
            blob: None,
        })
    }

    fn prompts(&self) -> Vec<PromptDescriptor> {
        vec![PromptDescriptor {
            name: "greet".into(),
            description: "Greet".into(),
            arguments: vec![],
        }]
    }

    fn get_prompt(&self, _name: &str, _args: Value) -> Result<Vec<PromptMessage>> {
        Ok(vec![PromptMessage {
            role: "user".into(),
            text: "Hello".into(),
        }])
    }
}

fn host() -> ServerHost {
    ServerHost::new("legacy", "1", Arc::new(Legacy))
}

fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params})
}

fn assert_wire(reply: Option<Value>, expected: &str) {
    assert_eq!(
        serde_json::to_vec(&reply.expect("response")).unwrap(),
        expected.as_bytes()
    );
}

fn stdio(host: &ServerHost, input: &str) -> Vec<u8> {
    let mut out = Vec::new();
    host.serve_stdio_on(Cursor::new(input), &mut out).unwrap();
    out
}

#[test]
fn public_dispatch_matches_stdio_frames_without_transport_io() {
    let frame = request("ping", json!({}));
    let expected = r#"{"id":1,"jsonrpc":"2.0","result":{}}"#;
    assert_wire(host().dispatch(&frame), expected);
    assert_eq!(
        stdio(&host(), &format!("{frame}\n")),
        format!("{expected}\n").as_bytes()
    );
    for frame in [
        Value::Null,
        json!(42),
        json!({}),
        json!({"id":1,"method":4}),
        json!({"method":"ping"}),
        json!({"id":null,"method":"ping"}),
    ] {
        assert_eq!(host().dispatch(&frame), None);
    }
}

#[test]
fn id_policy_null_response_matches_membrane_without_changing_default() {
    let opted = host().with_options(ServerOptions {
        id_policy: IdPolicy::NullResponse,
        ..Default::default()
    });
    for frame in [json!({"method":"ping"}), json!({"method":"ping","id":null})] {
        assert_eq!(host().dispatch(&frame), None);
        assert_wire(
            opted.dispatch(&frame),
            r#"{"id":null,"jsonrpc":"2.0","result":{}}"#,
        );
    }
    // Both implementations echo nonnull ids verbatim, even unusual JSON values.
    for id in [
        json!("req"),
        json!(0),
        json!(false),
        json!([]),
        json!({"opaque":1}),
    ] {
        let frame = json!({"method":"ping","id":id});
        let expected = json!({"jsonrpc":"2.0","id":id,"result":{}}).to_string();
        assert_wire(host().dispatch(&frame), &expected);
        assert_wire(opted.dispatch(&frame), &expected);
    }
    assert_eq!(opted.dispatch(&json!({"id":1,"method":null})), None);
}

#[test]
fn initialized_policy_ignores_even_id_bearing_frames_only_when_selected() {
    let opted = host().with_options(ServerOptions {
        id_policy: IdPolicy::NullResponse,
        initialized_policy: InitializedPolicy::Ignore,
        ..Default::default()
    });
    assert_wire(
        host().dispatch(&request("notifications/initialized", Value::Null)),
        r#"{"error":{"code":-32601,"message":"method not found: notifications/initialized"},"id":1,"jsonrpc":"2.0"}"#,
    );
    for frame in [
        request("notifications/initialized", Value::Null),
        json!({"method":"notifications/initialized"}),
        json!({"method":"notifications/initialized","id":null}),
    ] {
        assert_eq!(opted.dispatch(&frame), None);
    }
    // Id handling can be selected independently of initialized handling.
    assert_wire(
        host()
            .with_options(ServerOptions {
                id_policy: IdPolicy::NullResponse,
                ..Default::default()
            })
            .dispatch(&json!({"method":"notifications/initialized"})),
        r#"{"error":{"code":-32601,"message":"method not found: notifications/initialized"},"id":null,"jsonrpc":"2.0"}"#,
    );
}

struct Counting(Arc<Mutex<usize>>);
impl ToolProvider for Counting {
    fn list(&self) -> Vec<ToolDescriptor> {
        *self.0.lock().unwrap() += 1;
        vec![]
    }
    fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
        Err(McpError::ToolNotFound(name.into()))
    }
}

#[test]
fn batch_policy_process_default_reject_ignore_and_empty_arrays() {
    let batch = json!([request("tools/list", Value::Null), {"method":"tools/list"},
        null, [], request("ping", Value::Null)]);
    let calls = Arc::new(Mutex::new(0));
    let base = ServerHost::new("count", "1", Arc::new(Counting(calls.clone())));
    assert_wire(
        base.dispatch(&batch),
        r#"[{"id":1,"jsonrpc":"2.0","result":{"tools":[]}},{"id":1,"jsonrpc":"2.0","result":{}}]"#,
    );
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(base.dispatch(&json!([])), None);
    assert_eq!(base.dispatch(&json!([{"method":"ping"}])), None);
    for (policy, expected) in [
        (
            BatchPolicy::Reject,
            Some(
                r#"{"error":{"code":-32600,"message":"Invalid Request"},"id":null,"jsonrpc":"2.0"}"#,
            ),
        ),
        (BatchPolicy::Ignore, None),
    ] {
        let selected = base.clone().with_options(ServerOptions {
            batch_policy: policy,
            ..Default::default()
        });
        for array in [&batch, &json!([]), &json!([{"method":"ping"}])] {
            match expected {
                Some(wire) => assert_wire(selected.dispatch(array), wire),
                None => assert_eq!(selected.dispatch(array), None),
            }
        }
        assert_wire(
            selected.dispatch(&request("ping", Value::Null)),
            r#"{"id":1,"jsonrpc":"2.0","result":{}}"#,
        );
    }
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "rejected/ignored batches cannot invoke handlers"
    );
}

#[test]
fn method_not_found_message_option_preserves_exact_case_and_default() {
    let frame = request("no/such/method", Value::Null);
    assert_wire(
        host().dispatch(&frame),
        r#"{"error":{"code":-32601,"message":"method not found: no/such/method"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        host()
            .with_options(ServerOptions {
                method_not_found_message: Some("Method not found".into()),
                ..Default::default()
            })
            .dispatch(&frame),
        r#"{"error":{"code":-32601,"message":"Method not found"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        host()
            .with_options(ServerOptions {
                method_not_found_message: Some(String::new()),
                ..Default::default()
            })
            .dispatch(&frame),
        r#"{"error":{"code":-32601,"message":""},"id":1,"jsonrpc":"2.0"}"#,
    );
}

struct ErrorProvider;
impl ToolProvider for ErrorProvider {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![]
    }
    fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
        Err(McpError::ToolNotFound(name.into()))
    }
    fn list_resources(&self, request: &Value, _context: &RequestContext) -> Result<Value> {
        match request.pointer("/params/kind").and_then(Value::as_str) {
            Some("data") => Err(McpError::RpcWithData {
                code: -32001,
                message: "Access denied".into(),
                data: json!({"code":"resource_access_denied","message":"Access denied","required":["read"]}),
            }),
            Some("null") => Err(McpError::RpcWithData {
                code: -32002,
                message: "Not found".into(),
                data: Value::Null,
            }),
            Some("internal") => Err(McpError::Protocol("broken".into())),
            _ => Err(McpError::Rpc {
                code: -32001,
                message: "Access denied".into(),
            }),
        }
    }
}

#[test]
fn rpc_error_data_is_verbatim_and_absent_for_legacy_rpc_errors() {
    let host = ServerHost::new("errors", "1", Arc::new(ErrorProvider));
    assert_wire(
        host.dispatch(&request("resources/list", json!({"kind":"data"}))),
        r#"{"error":{"code":-32001,"data":{"code":"resource_access_denied","message":"Access denied","required":["read"]},"message":"Access denied"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        host.dispatch(&request("resources/list", json!({"kind":"null"}))),
        r#"{"error":{"code":-32002,"data":null,"message":"Not found"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        host.dispatch(&request("resources/list", Value::Null)),
        r#"{"error":{"code":-32001,"message":"Access denied"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        host.dispatch(&request("resources/list", json!({"kind":"internal"}))),
        r#"{"error":{"code":-32603,"message":"protocol error: broken"},"id":1,"jsonrpc":"2.0"}"#,
    );
}

struct Raw;
impl ToolProvider for Raw {
    fn list(&self) -> Vec<ToolDescriptor> {
        vec![
            ToolDescriptor::new("pull", "Pull", json!({})).read_only(true),
            ToolDescriptor::new("push", "Push", json!({})),
        ]
    }
    fn call(&self, _name: &str, _args: Value) -> Result<ToolResult> {
        panic!("raw hook must handle calls")
    }
    fn list_tools(&self, request: &Value, context: &RequestContext) -> Result<Value> {
        Ok(
            json!({"tools":[{"name":"pull","inputSchema":{},"x-authority":"read"},
            {"name":"push","inputSchema":{},"x-authority":"write"}],
            "nextCursor":request.pointer("/params/cursor").unwrap_or(&Value::Null),
            "_meta":context.metadata}),
        )
    }
    fn call_tool(&self, request: &Value, context: &RequestContext) -> Option<Result<Value>> {
        let name = request
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        let result = match name {
            "pull" | "push" => json!({"content":[],"isError":true,"structuredContent":{
                "operation":name,"result":{"code":if name == "pull" {"context_envelope_invalid"} else {"memory_envelope_invalid"}}}}),
            "raw" => {
                json!({"request":request,"session":context.session,"metadata":context.metadata})
            }
            _ => json!({"content":[{"type":"text","text":"unknown_tool"}],"isError":true}),
        };
        Some(Ok(result))
    }
    fn list_resources(&self, _request: &Value, _context: &RequestContext) -> Result<Value> {
        Ok(
            json!({"resources":[{"uri":"membrane://status","accessGrants":["read"]}],"_meta":{"bounded":true}}),
        )
    }
    fn list_resource_templates(
        &self,
        _request: &Value,
        _context: &RequestContext,
    ) -> Option<Result<Value>> {
        Some(Ok(json!({"resourceTemplates":[]})))
    }
    fn list_prompts(&self, _request: &Value, _context: &RequestContext) -> Result<Value> {
        Ok(
            json!({"prompts":[{"name":"recap","operations":["pull"],"authorityScope":"repository",
            "authorityEscalation":false,"authorityNotes":"bounded"}]}),
        )
    }
    fn get_prompt_payload(&self, request: &Value, _context: &RequestContext) -> Result<Value> {
        let name = request
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if name != "recap" {
            return Ok(json!({"error":"unknown_prompt","name":name}));
        }
        Ok(
            json!({"name":"recap","description":"Recap","arguments":[],"operations":["pull"],
            "authorityScope":"repository","authorityEscalation":false,"authorityNotes":"bounded",
            "messages":[{"role":"user","content":{"type":"text","text":"Recap"}}]}),
        )
    }
    fn read_resource_with_context(&self, uri: &str, context: &ReadContext<'_>) -> Result<Value> {
        if uri != "membrane://status" {
            return Err(McpError::RpcWithData {
                code: -32002,
                message: "Resource not found".into(),
                data: json!({"code":"resource_not_found","message":"Resource not found"}),
            });
        }
        if !context.access_grants.contains(&"read") {
            return Err(McpError::RpcWithData {
                code: -32001,
                message: "Access denied".into(),
                data: json!({"code":"resource_access_denied","message":"Access denied"}),
            });
        }
        let body = json!({"grants":context.access_grants,"metadata":context.context.metadata,
            "session":context.context.session,"caller":context.request.pointer("/params/caller").unwrap_or(&Value::Null)});
        Ok(json!({"contents":[{"uri":uri,"mimeType":"application/json","text":body.to_string()}]}))
    }
}

fn raw_host() -> ServerHost {
    ServerHost::new("raw", "1", Arc::new(Raw))
}

#[test]
fn request_aware_listing_and_raw_payload_preserve_metadata() {
    let context = RequestContext {
        transport: ServerTransport::Http {
            peer: "127.0.0.1:1234".parse().unwrap(),
        },
        session: Some("session".into()),
        metadata: json!({"caller":"test"}),
    };
    assert_wire(
        raw_host().dispatch_with_context(
            &request(
                "tools/list",
                json!({"cursor":"next","_meta":{"membrane.toolsets.v1":["operator"]}}),
            ),
            &context,
        ),
        r#"{"id":1,"jsonrpc":"2.0","result":{"_meta":{"caller":"test"},"nextCursor":"next","tools":[{"inputSchema":{},"name":"pull","x-authority":"read"},{"inputSchema":{},"name":"push","x-authority":"write"}]}}"#,
    );
    assert_wire(
        raw_host().dispatch(&request("resources/list", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":{"_meta":{"bounded":true},"resources":[{"accessGrants":["read"],"uri":"membrane://status"}]}}"#,
    );
    // Missing/nonstring names reach raw handlers without a transport error.
    for params in [Value::Null, json!({"name":null}), json!({"name":17})] {
        assert_wire(
            raw_host().dispatch(&request("tools/call", params)),
            r#"{"id":1,"jsonrpc":"2.0","result":{"content":[{"text":"unknown_tool","type":"text"}],"isError":true}}"#,
        );
    }
    assert_wire(
        raw_host().dispatch(&request("tools/call", json!({"name":"retired"}))),
        r#"{"id":1,"jsonrpc":"2.0","result":{"content":[{"text":"unknown_tool","type":"text"}],"isError":true}}"#,
    );
    // A raw payload need not fit ToolResult or a typed descriptor. Preserve
    // complete request/context & distinguish missing arguments from explicit null.
    assert_wire(
        raw_host().dispatch_with_context(
            &request(
                "tools/call",
                json!({"name":"raw","arguments":null,"custom":true}),
            ),
            &context,
        ),
        r#"{"id":1,"jsonrpc":"2.0","result":{"metadata":{"caller":"test"},"request":{"id":1,"jsonrpc":"2.0","method":"tools/call","params":{"arguments":null,"custom":true,"name":"raw"}},"session":"session"}}"#,
    );
    assert_wire(
        raw_host().dispatch(&request("tools/call", json!({"name":"raw"}))),
        r#"{"id":1,"jsonrpc":"2.0","result":{"metadata":null,"request":{"id":1,"jsonrpc":"2.0","method":"tools/call","params":{"name":"raw"}},"session":null}}"#,
    );
}

#[test]
fn resource_templates_are_opt_in_and_prompt_metadata_is_verbatim() {
    assert_wire(
        host().dispatch(&request("resources/templates/list", Value::Null)),
        r#"{"error":{"code":-32601,"message":"method not found: resources/templates/list"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        raw_host().dispatch(&request("resources/templates/list", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":{"resourceTemplates":[]}}"#,
    );
    assert_wire(
        raw_host().dispatch(&request("prompts/list", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":{"prompts":[{"authorityEscalation":false,"authorityNotes":"bounded","authorityScope":"repository","name":"recap","operations":["pull"]}]}}"#,
    );
    assert_wire(
        raw_host().dispatch(&request("prompts/get", json!({"name":"recap"}))),
        r#"{"id":1,"jsonrpc":"2.0","result":{"arguments":[],"authorityEscalation":false,"authorityNotes":"bounded","authorityScope":"repository","description":"Recap","messages":[{"content":{"text":"Recap","type":"text"},"role":"user"}],"name":"recap","operations":["pull"]}}"#,
    );
    assert_wire(
        raw_host().dispatch(&request("prompts/get", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":{"error":"unknown_prompt","name":""}}"#,
    );
}

#[test]
fn resource_reads_receive_grants_original_frame_and_host_context() {
    let context = RequestContext {
        session: Some("s1".into()),
        metadata: json!({"trusted":"host"}),
        ..Default::default()
    };
    let frame = request(
        "resources/read",
        json!({"uri":"membrane://status","accessGrants":["read",42,"read",null,"extra"],"caller":"agent"}),
    );
    assert_wire(
        raw_host().dispatch_with_context(&frame, &context),
        r#"{"id":1,"jsonrpc":"2.0","result":{"contents":[{"mimeType":"application/json","text":"{\"caller\":\"agent\",\"grants\":[\"read\",\"read\",\"extra\"],\"metadata\":{\"trusted\":\"host\"},\"session\":\"s1\"}","uri":"membrane://status"}]}}"#,
    );
    for grants in [Value::Null, json!("read"), json!([]), json!([false, 4])] {
        assert_wire(
            raw_host().dispatch(&request(
                "resources/read",
                json!({"uri":"membrane://status","accessGrants":grants}),
            )),
            r#"{"error":{"code":-32001,"data":{"code":"resource_access_denied","message":"Access denied"},"message":"Access denied"},"id":1,"jsonrpc":"2.0"}"#,
        );
    }
    assert_wire(
        raw_host().dispatch(&request("resources/read", Value::Null)),
        r#"{"error":{"code":-32002,"data":{"code":"resource_not_found","message":"Resource not found"},"message":"Resource not found"},"id":1,"jsonrpc":"2.0"}"#,
    );
}

#[test]
fn fixed_initialize_payload_bypasses_negotiation_and_generated_fields() {
    let fixed = json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{},"resources":{},"prompts":{}},
        "serverInfo":{"name":"membrane","version":"1.0.0"},"instructions":"Use pull & push","_meta":{"fixed":true}});
    let selected = host()
        .with_options(ServerOptions {
            initialize_result: Some(fixed),
            ..Default::default()
        })
        .with_instructions("must not overwrite fixed instructions");
    for params in [
        json!({"protocolVersion":"2024-11-05"}),
        json!({"protocolVersion":"future"}),
        Value::Null,
    ] {
        assert_wire(
            selected.dispatch(&request("initialize", params)),
            r#"{"id":1,"jsonrpc":"2.0","result":{"_meta":{"fixed":true},"capabilities":{"prompts":{},"resources":{},"tools":{}},"instructions":"Use pull & push","protocolVersion":"2025-03-26","serverInfo":{"name":"membrane","version":"1.0.0"}}}"#,
        );
    }
    // Nonobject results are still returned verbatim, never indexed/mutated.
    assert_wire(
        host()
            .with_options(ServerOptions {
                initialize_result: Some(Value::Null),
                ..Default::default()
            })
            .dispatch(&request("initialize", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":null}"#,
    );
}

#[test]
fn legacy_typed_surface_and_initialize_negotiation_are_unchanged() {
    for (requested, negotiated) in [
        ("2024-11-05", "2024-11-05"),
        ("2025-03-26", "2025-03-26"),
        ("future", "2025-03-26"),
    ] {
        assert_wire(
            host()
                .with_instructions("Legacy instructions")
                .dispatch(&request("initialize", json!({"protocolVersion":requested}))),
            &format!(
                r#"{{"id":1,"jsonrpc":"2.0","result":{{"capabilities":{{"prompts":{{}},"resources":{{}},"tools":{{}}}},"instructions":"Legacy instructions","protocolVersion":"{negotiated}","serverInfo":{{"name":"legacy","version":"1"}}}}}}"#
            ),
        );
    }
    assert_wire(
        host().dispatch(&request("tools/list", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":{"tools":[{"annotations":{},"description":"Echo","inputSchema":{"type":"object"},"name":"echo"}]}}"#,
    );
    assert_wire(
        host().dispatch(&request("tools/call", json!({"name":"echo"}))),
        r#"{"id":1,"jsonrpc":"2.0","result":{"content":[{"text":"{}","type":"text"}],"isError":false,"structuredContent":{}}}"#,
    );
    assert_wire(
        host().dispatch(&request("tools/call", Value::Null)),
        r#"{"error":{"code":-32602,"message":"missing tool name"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        host().dispatch(&request("tools/call", json!({"name":"retired"}))),
        r#"{"error":{"code":-32602,"message":"unknown: retired"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        host().dispatch(&request("resources/list", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":{"resources":[{"description":"Readme","mimeType":"text/plain","name":"readme","uri":"demo://readme"}]}}"#,
    );
    assert_wire(
        host().dispatch(&request("resources/read", json!({"uri":"demo://readme"}))),
        r#"{"id":1,"jsonrpc":"2.0","result":{"contents":[{"mimeType":"text/plain","text":"hello","uri":"demo://readme"}]}}"#,
    );
    assert_wire(
        host().dispatch(&request("prompts/list", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":{"prompts":[{"arguments":[],"description":"Greet","name":"greet"}]}}"#,
    );
    assert_wire(
        host().dispatch(&request("prompts/get", json!({"name":"greet"}))),
        r#"{"id":1,"jsonrpc":"2.0","result":{"messages":[{"content":{"text":"Hello","type":"text"},"role":"user"}]}}"#,
    );
}

#[test]
fn stdio_fail_fast_preserves_parse_source_and_emits_no_error_reply() {
    let input = "\n{\n{\"id\":1,\"method\":\"ping\"}\n";
    let selected = host().with_options(ServerOptions {
        stdio_input_mode: StdioInputMode::FailFast,
        ..Default::default()
    });
    let mut out = Vec::new();
    let err = selected
        .serve_stdio_on_io(Cursor::new(input), &mut out)
        .unwrap_err();
    assert_eq!(out, b"");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.to_string(),
        "EOF while parsing an object at line 1 column 1"
    );
    assert!(err
        .get_ref()
        .unwrap()
        .downcast_ref::<serde_json::Error>()
        .is_some());
    assert_eq!(stdio(&host(), input),
        b"{\"error\":{\"code\":-32700,\"message\":\"parse error: EOF while parsing an object at line 1 column 1\"},\"id\":null,\"jsonrpc\":\"2.0\"}\n{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{}}\n");
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let selected = selected.on_event(move |e| captured.lock().unwrap().push(e.kind));
    assert!(matches!(
        selected.serve_stdio_on(Cursor::new(input), Vec::new()),
        Err(McpError::IoError(_))
    ));
    assert_eq!(
        *events.lock().unwrap(),
        vec![LifecycleKind::Start, LifecycleKind::Crash]
    );
    let valid = "  \n{\"id\":1,\"method\":\"ping\"}\n\n";
    assert_eq!(
        stdio(&selected, valid),
        b"{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{}}\n"
    );
}

#[derive(Debug)]
struct Original(&'static str);
impl std::fmt::Display for Original {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Original {}

struct BrokenReader;
impl Read for BrokenReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            Original("read failed"),
        ))
    }
}
impl BufRead for BrokenReader {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            Original("read failed"),
        ))
    }
    fn consume(&mut self, _amount: usize) {}
}
struct BrokenWriter(bool);
impl Write for BrokenWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0 {
            Ok(bytes.len())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                Original("write failed"),
            ))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            Original("flush failed"),
        ))
    }
}

#[test]
fn stdio_returns_original_read_write_flush_errors_and_legacy_conversion() {
    let selected = host().with_options(ServerOptions {
        stdio_input_mode: StdioInputMode::FailFast,
        ..Default::default()
    });
    let input = "{\"id\":1,\"method\":\"ping\"}\n";
    for (err, kind, text) in [
        (
            selected
                .serve_stdio_on_io(BrokenReader, Vec::new())
                .unwrap_err(),
            io::ErrorKind::PermissionDenied,
            "read failed",
        ),
        (
            selected
                .serve_stdio_on_io(Cursor::new(input), BrokenWriter(false))
                .unwrap_err(),
            io::ErrorKind::BrokenPipe,
            "write failed",
        ),
        (
            selected
                .serve_stdio_on_io(Cursor::new(input), BrokenWriter(true))
                .unwrap_err(),
            io::ErrorKind::TimedOut,
            "flush failed",
        ),
    ] {
        assert_eq!(err.kind(), kind);
        assert_eq!(
            err.get_ref().unwrap().downcast_ref::<Original>().unwrap().0,
            text
        );
    }
    let wrapped = selected
        .serve_stdio_on(BrokenReader, Vec::new())
        .unwrap_err();
    let source = std::error::Error::source(&wrapped)
        .unwrap()
        .downcast_ref::<io::Error>()
        .unwrap();
    assert_eq!(source.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        source
            .get_ref()
            .unwrap()
            .downcast_ref::<Original>()
            .unwrap()
            .0,
        "read failed"
    );
    assert!(
        matches!(host().serve_stdio_on(BrokenReader, Vec::new()), Err(McpError::Io(text)) if text == "read failed")
    );
}

#[test]
fn raw_calls_cannot_bypass_admission_and_raw_listing_is_filtered() {
    let selected = raw_host().with_admission(Arc::new(ReadOnlyPolicy {
        allow_mutate: false,
        blocked: vec!["pull".into()],
    }));
    assert_wire(
        selected.dispatch(&request("tools/list", Value::Null)),
        r#"{"id":1,"jsonrpc":"2.0","result":{"_meta":null,"nextCursor":null,"tools":[{"inputSchema":{},"name":"push","x-authority":"write"}]}}"#,
    );
    assert_wire(
        selected.dispatch(&request("tools/call", json!({"name":"push"}))),
        r#"{"id":1,"jsonrpc":"2.0","result":{"content":[{"text":"denied: tool 'push' is mutating and mutation is not enabled","type":"text"}],"isError":true,"structuredContent":{"denied":true,"reason":"tool 'push' is mutating and mutation is not enabled"}}}"#,
    );
    assert_wire(
        selected.dispatch(&request("tools/call", json!({"name":"pull"}))),
        r#"{"id":1,"jsonrpc":"2.0","result":{"content":[{"text":"denied: tool 'pull' is not exposed","type":"text"}],"isError":true,"structuredContent":{"denied":true,"reason":"tool 'pull' is not exposed"}}}"#,
    );
    assert_wire(
        selected.dispatch(&request("tools/call", Value::Null)),
        r#"{"error":{"code":-32602,"message":"missing tool name"},"id":1,"jsonrpc":"2.0"}"#,
    );
}

#[test]
fn membrane_e2e_discovery_and_typed_failures_survive_shared_dispatch() {
    // Portable expectations from membrane-runtime/tests/e2e_mcp_surface.rs:
    // pull/push-only discovery, no negotiation widening, typed malformed-call
    // envelopes, retired names refused, & serving continues after unknown methods.
    let selected = raw_host().with_options(ServerOptions {
        id_policy: IdPolicy::NullResponse, initialized_policy: InitializedPolicy::Ignore,
        batch_policy: BatchPolicy::Ignore, method_not_found_message: Some("Method not found".into()),
        initialize_result: Some(json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{},"resources":{},"prompts":{}},
            "serverInfo":{"name":"membrane","version":"1.0.0"},"instructions":"Use pull & push"})),
        stdio_input_mode: StdioInputMode::FailFast,
    });
    assert_wire(
        selected.dispatch(&request(
            "initialize",
            json!({"protocolVersion":"2024-11-05"}),
        )),
        r#"{"id":1,"jsonrpc":"2.0","result":{"capabilities":{"prompts":{},"resources":{},"tools":{}},"instructions":"Use pull & push","protocolVersion":"2025-03-26","serverInfo":{"name":"membrane","version":"1.0.0"}}}"#,
    );
    let list_wire = r#"{"id":1,"jsonrpc":"2.0","result":{"_meta":null,"nextCursor":null,"tools":[{"inputSchema":{},"name":"pull","x-authority":"read"},{"inputSchema":{},"name":"push","x-authority":"write"}]}}"#;
    for params in [
        json!({}),
        json!({"_meta":{"membrane.toolsets.v1":["memory","blueprint","adapt","operator"]}}),
    ] {
        assert_wire(selected.dispatch(&request("tools/list", params)), list_wire);
    }
    assert_wire(
        selected.dispatch(&request(
            "tools/call",
            json!({"name":"pull","arguments":{}}),
        )),
        r#"{"id":1,"jsonrpc":"2.0","result":{"content":[],"isError":true,"structuredContent":{"operation":"pull","result":{"code":"context_envelope_invalid"}}}}"#,
    );
    assert_wire(
        selected.dispatch(&request(
            "tools/call",
            json!({"name":"push","arguments":{}}),
        )),
        r#"{"id":1,"jsonrpc":"2.0","result":{"content":[],"isError":true,"structuredContent":{"operation":"push","result":{"code":"memory_envelope_invalid"}}}}"#,
    );
    assert_wire(
        selected.dispatch(&request(
            "tools/call",
            json!({"name":"membrane_push_prepare","arguments":{}}),
        )),
        r#"{"id":1,"jsonrpc":"2.0","result":{"content":[{"text":"unknown_tool","type":"text"}],"isError":true}}"#,
    );
    assert_wire(
        selected.dispatch(&request("no/such/method", json!({}))),
        r#"{"error":{"code":-32601,"message":"Method not found"},"id":1,"jsonrpc":"2.0"}"#,
    );
    assert_wire(
        selected.dispatch(&request("tools/list", json!({}))),
        list_wire,
    );
    assert_eq!(
        selected.dispatch(&json!([request("ping", Value::Null)])),
        None
    );
    assert_eq!(
        selected.dispatch(&request("notifications/initialized", Value::Null)),
        None
    );
    assert_wire(
        selected.dispatch(&json!({"method":"ping"})),
        r#"{"id":null,"jsonrpc":"2.0","result":{}}"#,
    );
}

const DEFAULT_LIMIT: usize = rightkit_mcp::server::DEFAULT_MAX_STDIO_FRAME_BYTES;

fn stdio_with_limit(limit: usize, input: &[u8], mode: StdioInputMode) -> Vec<Value> {
    let mut out = Vec::new();
    host()
        .with_options(ServerOptions {
            stdio_input_mode: mode,
            ..Default::default()
        })
        .with_max_stdio_frame_bytes(limit)
        .serve_stdio_on(Cursor::new(input.to_vec()), &mut out)
        .unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn default_stdio_frame_limit_is_eight_mib() {
    assert_eq!(DEFAULT_LIMIT, 8 * 1024 * 1024);
    let mut out = Vec::new();
    let big = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\",\"pad\":\"{}\"}}\n",
        "q".repeat(DEFAULT_LIMIT)
    );
    host().serve_stdio_on(Cursor::new(big), &mut out).unwrap();
    let reply: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(reply["error"]["code"], json!(-32600));
}

#[test]
fn oversize_stdio_frame_gets_rpc_error_and_server_keeps_serving() {
    let ping = r#"{"jsonrpc":"2.0","id":7,"method":"ping","params":{}}"#;
    let oversize = format!(
        r#"{{"jsonrpc":"2.0","id":8,"method":"ping","pad":"{}"}}"#,
        "x".repeat(200)
    );
    let input = format!("{oversize}\n{ping}\n{ping}\n");
    let replies = stdio_with_limit(128, input.as_bytes(), StdioInputMode::ParseErrorReply);
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_eq!(replies[0]["id"], Value::Null);
    assert_eq!(replies[0]["error"]["code"], json!(-32600));
    assert!(replies[0]["error"]["message"]
        .as_str()
        .unwrap()
        .contains("128"));
    assert_eq!(replies[1], json!({"id":7,"jsonrpc":"2.0","result":{}}));
    assert_eq!(replies[2], json!({"id":7,"jsonrpc":"2.0","result":{}}));
}

#[test]
fn stdio_frame_limit_is_inclusive_and_ignores_crlf_terminator() {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let len = body.len();
    let exact = format!("{body}\r\n");
    let replies = stdio_with_limit(len, exact.as_bytes(), StdioInputMode::ParseErrorReply);
    assert_eq!(replies, vec![json!({"id":1,"jsonrpc":"2.0","result":{}})]);
    let replies = stdio_with_limit(len - 1, exact.as_bytes(), StdioInputMode::ParseErrorReply);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["error"]["code"], json!(-32600));
}

#[test]
fn oversize_final_frame_without_newline_is_answered_then_eof_ends_cleanly() {
    let input = format!(r#"{{"pad":"{}"}}"#, "y".repeat(500));
    let replies = stdio_with_limit(64, input.as_bytes(), StdioInputMode::ParseErrorReply);
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["error"]["code"], json!(-32600));
}

#[test]
fn oversize_frame_is_rejected_even_in_fail_fast_mode_without_ending_serving() {
    let ping = r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#;
    let input = format!("{}\n{ping}\n", "z".repeat(100));
    let replies = stdio_with_limit(50, input.as_bytes(), StdioInputMode::FailFast);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["error"]["code"], json!(-32600));
    assert_eq!(replies[1], json!({"id":3,"jsonrpc":"2.0","result":{}}));
}

#[test]
fn stdio_frame_at_default_limit_is_served_and_one_byte_over_is_rejected() {
    let limit = DEFAULT_LIMIT;
    let prefix = r#"{"jsonrpc":"2.0","id":5,"method":"ping","pad":""#;
    let suffix = r#""}"#;
    let pad_len = limit - prefix.len() - suffix.len();
    let at_limit = format!("{prefix}{}{suffix}\n", "p".repeat(pad_len));
    let over_limit = format!("{prefix}{}{suffix}\n", "p".repeat(pad_len + 1));
    let input = format!("{at_limit}{over_limit}");
    let replies = stdio_with_limit(limit, input.as_bytes(), StdioInputMode::ParseErrorReply);
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0], json!({"id":5,"jsonrpc":"2.0","result":{}}));
    assert_eq!(replies[1]["error"]["code"], json!(-32600));
}

#[test]
fn default_negotiation_accepts_2025_06_18_and_keeps_2025_03_26_fallback() {
    for (requested, negotiated) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-03-26"),
        ("future", "2025-03-26"),
    ] {
        let reply = host()
            .dispatch(&request("initialize", json!({"protocolVersion":requested})))
            .expect("response");
        assert_eq!(reply["result"]["protocolVersion"], negotiated);
    }
    let reply = host()
        .dispatch(&request(
            "initialize",
            json!({"protocolVersion":"2024-11-05","supportedProtocolVersions":["2024-11-05","2025-06-18"]}),
        ))
        .expect("response");
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(PROTOCOL_VERSION, "2025-03-26");
}

#[test]
fn unannotated_tools_are_reported_as_mutating() {
    let schema = json!({"type":"object"});
    let tools = vec![
        ToolDescriptor::new("read", "r", schema.clone()).read_only(true),
        ToolDescriptor::new("write", "w", schema.clone()).read_only(false),
        ToolDescriptor::new("unannotated", "u", schema),
    ];
    assert_eq!(mutating_tools(&tools), vec!["write", "unannotated"]);
    assert!(!tools[0].is_mutating());
    assert!(tools[2].is_mutating());
}
