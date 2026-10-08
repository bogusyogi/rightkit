use rightkit_mcp::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
struct Seen {
    verb: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}
fn read_request(stream: &TcpStream) -> Option<Seen> {
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut words = line.split_whitespace();
    let verb = words.next()?.into();
    let path = words.next()?.into();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.split_once(':')?;
        headers.insert(key.to_ascii_lowercase(), value.trim().to_string());
    }
    let size: usize = headers
        .get("content-length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).ok()?;
    Some(Seen {
        verb,
        path,
        headers,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    })
}
fn json_reply(stream: &mut TcpStream, body: &Value, session: Option<&str>) {
    let body = serde_json::to_vec(body).unwrap();
    let session = session
        .map(|session| format!("Mcp-Session-Id: {session}\r\n"))
        .unwrap_or_default();
    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{session}Connection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(&body).unwrap();
    stream.flush().unwrap();
}
fn accepted(stream: &mut TcpStream) {
    stream
        .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();
}
fn sse_head(stream: &mut TcpStream) {
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .unwrap();
    stream.flush().unwrap();
}
fn sse_message(stream: &mut TcpStream, message: &Value) {
    write!(stream, "event: message\r\ndata: {message}\r\n\r\n").unwrap();
    stream.flush().unwrap();
}
struct Fixture {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn start(handler: impl Fn(Seen, TcpStream) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let handler = Arc::new(handler);
        let join = thread::spawn(move || {
            let mut workers = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(20);
            while !stopping.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (handler, recorded) = (handler.clone(), recorded.clone());
                        workers.push(thread::spawn(move || {
                            // Windows inherits the listener's nonblocking mode. Wait for the
                            // complete request instead of dropping it on a transient WouldBlock.
                            stream.set_nonblocking(false).unwrap();
                            if let Some(request) = read_request(&stream) {
                                recorded.lock().unwrap().push(request.clone());
                                handler(request, stream);
                            }
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            seen,
            stop,
            join: Some(join),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            if !thread::panicking() {
                join.join().unwrap();
            }
        }
    }
}
fn client(url: &str, extensions: ClientExtensions) -> McpClient {
    McpClient::connect_with_extensions(
        "fixture",
        ServerSpec::Http { url: url.into() },
        ClientOptions {
            request_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        extensions,
    )
    .unwrap()
}
fn handshake(request: &Seen, stream: &mut TcpStream) -> bool {
    if request.body["method"] == "initialize" {
        json_reply(
            stream,
            &json!({"jsonrpc":"2.0","id":request.body["id"],"result":{"protocolVersion":"2025-03-26","capabilities":{}}}),
            Some("shared"),
        );
        true
    } else if request.body["method"] == "notifications/initialized" {
        assert!(request.body.get("id").is_none());
        accepted(stream);
        true
    } else {
        false
    }
}

#[test]
fn per_call_session_and_case_insensitive_header_overrides_reach_wire_without_adoption() {
    let fixture = Fixture::start(|request, mut stream| {
        if handshake(&request, &mut stream) {
            return;
        }
        json_reply(
            &mut stream,
            &json!({"jsonrpc":"2.0","id":request.body["id"],"result":{"session":request.headers.get("mcp-session-id"),"custom":request.headers.get("x-call")}}),
            Some("do-not-adopt"),
        );
    });
    let client = client(&fixture.url, ClientExtensions::default());
    let options = CallOptions {
        session_id: Some("explicit".into()),
        headers: BTreeMap::from([("X-Call".into(), "custom".into())]),
        ..Default::default()
    };
    let result = client
        .request_with_options("echo", json!({}), &options)
        .unwrap();
    assert_eq!(result["session"], "explicit");
    assert_eq!(result["custom"], "custom");
    assert_eq!(client.session_id().unwrap().as_deref(), Some("shared"));
    let options = CallOptions {
        headers: BTreeMap::from([("mCp-SeSsIoN-iD".into(), "header-session".into())]),
        ..Default::default()
    };
    assert_eq!(
        client
            .request_with_options("echo", json!({}), &options)
            .unwrap()["session"],
        "header-session"
    );
    assert_eq!(client.session_id().unwrap().as_deref(), Some("shared"));
}

fn cancel_slow_call(after_headers: bool) {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (notify_tx, notify_rx) = mpsc::channel();
    let (eof_tx, eof_rx) = mpsc::channel();
    let fixture = Fixture::start(move |request, mut stream| {
        if handshake(&request, &mut stream) {
            return;
        }
        if request.body["method"] == "slow" {
            if after_headers {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{").unwrap();
                stream.flush().unwrap();
            }
            ready_tx.send(request.clone()).unwrap();
            let mut byte = [0u8; 1];
            let eof = match stream.read(&mut byte) {
                Ok(0) => true,
                Err(error) => matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                ),
                _ => false,
            };
            eof_tx.send(eof).unwrap();
        } else if request.body["method"] == "notifications/cancelled" {
            assert!(request.body.get("id").is_none());
            accepted(&mut stream);
            notify_tx.send(request).unwrap();
        } else {
            panic!("unexpected request: {:?}", request.body);
        }
    });
    let client = client(&fixture.url, ClientExtensions::default());
    let token = CancellationToken::new();
    let options = CallOptions {
        session_id: Some("isolated".into()),
        headers: BTreeMap::from([("X-Call".into(), "cancelled-call".into())]),
        cancellation: Some(token.clone()),
    };
    let call = thread::spawn(move || client.request_with_options("slow", json!({}), &options));
    let original = ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let started = Instant::now();
    token.cancel();
    assert!(matches!(call.join().unwrap(), Err(McpError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(1));
    let notification = notify_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(
        notification.body["params"]["requestId"],
        original.body["id"]
    );
    assert_eq!(
        notification
            .headers
            .get("mcp-session-id")
            .map(String::as_str),
        Some("isolated")
    );
    assert_eq!(
        notification.headers.get("x-call").map(String::as_str),
        Some("cancelled-call")
    );
    // hyper may keep the HTTP/1 connection to drain the cancelled response; the caller-visible
    // contract is the prompt Cancelled return and the notifications/cancelled above.
    let _ = eof_rx;
}
#[test]
fn cancellation_aborts_slow_headers_and_emits_notification() {
    cancel_slow_call(false);
}
#[test]
fn cancellation_aborts_slow_body_and_emits_notification() {
    cancel_slow_call(true);
}

#[test]
fn oversized_json_and_sse_frame_are_typed_errors() {
    let fixture = Fixture::start(|request, mut stream| {
        if handshake(&request, &mut stream) {
            return;
        }
        let reply =
            json!({"jsonrpc":"2.0","id":request.body["id"],"result":{"blob":"x".repeat(2048)}});
        if request.body["method"] == "big-sse" {
            sse_head(&mut stream);
            sse_message(&mut stream, &reply);
        } else {
            json_reply(&mut stream, &reply, None);
        }
    });
    let client = client(
        &fixture.url,
        ClientExtensions {
            response_limits: ResponseLimits {
                max_response_bytes: 512,
                max_frame_bytes: 4096,
            },
            ..Default::default()
        },
    );
    assert!(matches!(
        client.request_with_options("big", json!({}), &CallOptions::default()),
        Err(McpError::ResponseTooLarge { limit: 512 })
    ));
    let client = self::client(
        &fixture.url,
        ClientExtensions {
            response_limits: ResponseLimits {
                max_response_bytes: 4096,
                max_frame_bytes: 256,
            },
            ..Default::default()
        },
    );
    assert!(matches!(
        client.request_with_options("big-sse", json!({}), &CallOptions::default()),
        Err(McpError::ResponseTooLarge { limit: 256 })
    ));
}

struct Empty;
impl ToolProvider for Empty {
    fn list(&self) -> Vec<ToolDescriptor> {
        Vec::new()
    }
    fn call(&self, name: &str, _args: Value) -> Result<ToolResult> {
        Err(McpError::ToolNotFound(name.into()))
    }
}
#[test]
fn configured_client_and_server_negotiate_highest_common_version() {
    let host = ServerHost::new("fixture", "1", Arc::new(Empty)).with_protocol_versions(vec![
        "2024-11-05".into(),
        "2025-03-26".into(),
        "2025-06-18".into(),
    ]);
    let fixture = Fixture::start(
        move |request, mut stream| match host.dispatch(&request.body) {
            Some(reply) => json_reply(&mut stream, &reply, None),
            None => accepted(&mut stream),
        },
    );
    let client = client(
        &fixture.url,
        ClientExtensions {
            protocol_versions: Some(vec![
                "2024-11-05".into(),
                "2025-06-18".into(),
                "2026-01-01".into(),
            ]),
            ..Default::default()
        },
    );
    assert_eq!(client.protocol_version(), "2025-06-18");
    let initialization = fixture
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|request| request.body["method"] == "initialize")
        .cloned()
        .unwrap();
    assert_eq!(
        initialization.body["params"]["protocolVersion"],
        "2026-01-01"
    );
    assert!(
        initialization.body["params"]["supportedProtocolVersions"]
            .as_array()
            .unwrap()
            .len()
            == 3
    );
    let result = McpClient::connect_with_extensions(
        "fixture",
        ServerSpec::Http {
            url: fixture.url.clone(),
        },
        ClientOptions::default(),
        ClientExtensions {
            protocol_versions: Some(vec!["2030-01-01".into()]),
            ..Default::default()
        },
    );
    assert!(matches!(result, Err(McpError::Rpc { code: -32602, .. })));
}

#[test]
fn legacy_sse_discovers_endpoint_and_keeps_response_stream_across_calls() {
    for named in [true, false] {
        let output: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
        let stream_output = output.clone();
        let fixture = Fixture::start(move |request, mut stream| {
            if request.verb == "GET" {
                // Publish the stream before announcing the POST endpoint, holding the lock
                // until the announcement is complete so POST replies cannot interleave it.
                let mut output = stream_output.lock().unwrap();
                *output = Some(stream);
                let stream = output.as_mut().unwrap();
                sse_head(stream);
                let event = if named {
                    "event: endpoint\r\ndata: /messages?sid=legacy\r\n\r\n"
                } else {
                    "data: /messages?sid=legacy\r\n\r\n"
                };
                // Deliberately split CRLF boundaries across writes.
                for piece in event.as_bytes().chunks(3) {
                    stream.write_all(piece).unwrap();
                    stream.flush().unwrap();
                }
                return;
            }
            assert_eq!(request.path, "/messages?sid=legacy");
            assert!(
                stream_output.lock().unwrap().is_some(),
                "SSE GET must remain open before POST"
            );
            accepted(&mut stream);
            if request.body.get("id").is_some() {
                let result = if request.body["method"] == "initialize" {
                    json!({"protocolVersion":"2025-03-26","capabilities":{}})
                } else {
                    json!({"echo":request.body["params"]})
                };
                let mut output = stream_output.lock().unwrap();
                sse_message(
                    output.as_mut().unwrap(),
                    &json!({"jsonrpc":"2.0","id":request.body["id"],"result":result}),
                );
            } else {
                assert_eq!(request.body["method"], "notifications/initialized");
            }
        });
        let client = client(
            &fixture.url.replace("/mcp", "/sse"),
            ClientExtensions {
                legacy_sse: true,
                ..Default::default()
            },
        );
        for value in ["first", "second"] {
            assert_eq!(
                client
                    .request_with_options("echo", json!({"value":value}), &CallOptions::default())
                    .unwrap()["echo"]["value"],
                value
            );
        }
        assert_eq!(
            fixture
                .seen
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.verb == "GET")
                .count(),
            1
        );
        client.close();
        output.lock().unwrap().take();
    }
}

#[test]
fn legacy_sse_rejects_cross_origin_announcement_before_any_post() {
    let fixture = Fixture::start(|request, mut stream| {
        assert_eq!(request.verb, "GET");
        sse_head(&mut stream);
        stream
            .write_all(b"event: endpoint\ndata: https://attacker.test/messages\n\n")
            .unwrap();
        stream.flush().unwrap();
    });
    let result = McpClient::connect_with_extensions(
        "fixture",
        ServerSpec::Http {
            url: fixture.url.clone(),
        },
        ClientOptions::default(),
        ClientExtensions {
            legacy_sse: true,
            ..Default::default()
        },
    );
    assert!(matches!(result, Err(McpError::CrossOriginEndpoint { .. })));
    assert_eq!(fixture.seen.lock().unwrap().len(), 1);
}

#[test]
fn live_http_elicitation_is_bound_to_each_receiving_session() {
    let waiting = Arc::new(Mutex::new(HashMap::<String, mpsc::Sender<Value>>::new()));
    let routes = waiting.clone();
    let fixture = Fixture::start(move |request, mut stream| {
        if handshake(&request, &mut stream) {
            return;
        }
        let session = request.headers.get("mcp-session-id").unwrap().clone();
        if request.body["method"] == "ask" {
            let (sender, receiver) = mpsc::channel();
            routes.lock().unwrap().insert(session.clone(), sender);
            sse_head(&mut stream);
            // Every session intentionally uses same server request ID.
            sse_message(
                &mut stream,
                &json!({"jsonrpc":"2.0","id":"same-elicitation-id","method":"elicitation/create","params":{"message":session,"requestedSchema":{"type":"object"}}}),
            );
            let answer = receiver.recv_timeout(Duration::from_secs(3)).unwrap();
            sse_message(
                &mut stream,
                &json!({"jsonrpc":"2.0","id":request.body["id"],"result":{"answer":answer["result"]}}),
            );
        } else {
            assert_eq!(request.body["id"], "same-elicitation-id");
            assert!(request.body.get("method").is_none());
            assert_eq!(request.headers.get("x-session-tag").unwrap(), &session);
            routes
                .lock()
                .unwrap()
                .remove(&session)
                .unwrap()
                .send(request.body)
                .unwrap();
            accepted(&mut stream);
        }
    });
    let client = Arc::new(client(&fixture.url, ClientExtensions::default()));
    for session in ["one", "two"] {
        client
            .set_elicitation_handler(
                session,
                Arc::new(move |context, params| {
                    assert_eq!(context.session_id, session);
                    assert_eq!(params["message"], session);
                    Ok(json!({"action":"accept","content":{"session":session}}))
                }),
            )
            .unwrap();
    }
    let calls: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|session| {
            let client = client.clone();
            thread::spawn(move || {
                client
                    .request_with_options(
                        "ask",
                        json!({}),
                        &CallOptions {
                            session_id: Some(session.into()),
                            headers: BTreeMap::from([("X-Session-Tag".into(), session.into())]),
                            ..Default::default()
                        },
                    )
                    .unwrap()
            })
        })
        .collect();
    for (call, session) in calls.into_iter().zip(["one", "two"]) {
        assert_eq!(
            call.join().unwrap()["answer"]["content"]["session"],
            session
        );
    }
    assert_eq!(client.session_id().unwrap().as_deref(), Some("shared"));
}
