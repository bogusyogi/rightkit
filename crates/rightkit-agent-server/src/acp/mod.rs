//! `coderight acp-server` — RightKit as an Agent Client Protocol (ACP) agent
//! over stdio JSON-RPC 2.0. Implements the stable v1 agent-bound method
//! surface from `agentclientprotocol/agent-client-protocol`
//! (`schema/v1/meta.json`): `initialize`, `authenticate`, `session/new`,
//! `session/load`, `session/prompt`, `session/cancel`, `session/list`,
//! `session/delete`, `session/resume`, `session/close`, `logout`. Streams
//! `session/update` notifications while a prompt runs and sends a real
//! `session/request_permission` round-trip (via [`transport`]) when a tool
//! call needs operator approval — the streaming half of that lives in
//! [`prompt`].
//!
//! CG-A08 canonical home: this module tree consolidates the previously
//! scattered `acp_transport.rs`, `acp_bridge.rs`, `acp_server.rs`, and
//! `acp_server_prompt.rs` top-level files into one `acp/` tree owning the
//! full negotiated-session, prompt, cancel & config lifecycle:
//!
//!   - [`transport`] — the bidirectional JSON-RPC-over-stdio wire (from
//!     `acp_transport.rs`).
//!   - [`session`] — `DaemonClient` HTTP glue, event→update mapping, and the
//!     CRUD-shaped session lifecycle handlers (`session/new`, `.../load`,
//!     `.../resume`, `.../close`, `.../list`, `.../delete`) (from
//!     `acp_bridge.rs` + the CRUD half of `acp_server.rs`).
//!   - [`prompt`] — the streaming `session/prompt` handler and its
//!     tool-approval round-trip (from `acp_server_prompt.rs`).
//!   - [`cancel`] — the `session/cancel` notification handler, split out on
//!     its own per the canon's "session, prompt, cancel & config lifecycle"
//!     framing (previously folded into `acp_server_prompt.rs`).
//!   - [`config`] — `AcpServerOptions`, `initialize`, `authenticate`, and the
//!     JSON-RPC response envelope helpers (from the config half of
//!     `acp_server.rs`).
//!
//! Provenance (G10): same-seam Rust prior art exists in
//! `block/goose/crates/goose/src/acp/server.rs` (full agent-side ACP:
//! sessions, modes, permission options, McpServer negotiation + schema
//! generator + fixture tests). RightKit built its equivalent independently
//! including typed timeout failure + version rejection. No source copied.
//!
//! Live wiring: `main.rs` declares this module & `main_runtime_entry.rs`
//! routes `coderight acp-server` into [`run_stdio`].

pub mod cancel;
pub mod config;
pub mod prompt;
pub mod session;
pub mod transport;

use config::{
    error_response, handle_authenticate, handle_initialize, success_response, AcpServerOptions,
};
use session::DaemonClient;
use transport::AcpTransport;

use rightkit_agents::agent_acp as acp;
use serde_json::{json, Value};
use std::io::BufRead;

pub async fn run_stdio(options: AcpServerOptions) -> i32 {
    // `Stdin` (owned, not `.lock()`) is `Send` and still reads the process's
    // real stdin — `transport::run` needs to move the reader onto a blocking
    // task, which `StdinLock<'static>` cannot do on this target.
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    run_reader(std::io::BufReader::new(stdin), stdout, options).await
}

pub async fn run_reader<R, W>(reader: R, writer: W, options: AcpServerOptions) -> i32
where
    R: BufRead + Send + 'static,
    W: std::io::Write + Send + 'static,
{
    let client = std::sync::Arc::new(DaemonClient::new(options.port, options.token));
    transport::run(reader, writer, move |request, transport| {
        let client = client.clone();
        async move { handle_line(request, &client, &transport).await }
    })
    .await
}

/// Dispatches one JSON-RPC line. Returns `None` for notifications (no `id`,
/// e.g. `session/cancel`) per JSON-RPC 2.0 — notifications never get a
/// response, success or error — and for responses to CR's own outbound
/// requests (already consumed by `transport`'s interceptor before this is
/// called).
async fn handle_line(
    request: Value,
    client: &DaemonClient,
    transport: &AcpTransport,
) -> Option<Value> {
    let id = request.get("id").cloned();
    let is_notification = id.is_none();

    // session/cancel is the one notification-only method in this surface;
    // dispatch it before the request/response machinery below so a failed
    // cancel never produces a stray response line.
    if is_notification
        && request.get("method").and_then(Value::as_str) == Some(acp::METHOD_SESSION_CANCEL)
    {
        let params = request.get("params").cloned().unwrap_or(json!({}));
        let _ = cancel::handle_session_cancel(params, client).await;
        return None;
    }

    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return if is_notification {
            None
        } else {
            Some(error_response(id, acp::AcpError::invalid_request()))
        };
    };
    let params = request.get("params").cloned().unwrap_or(json!({}));

    let result = match method {
        acp::METHOD_INITIALIZE => handle_initialize(params),
        acp::METHOD_AUTHENTICATE => handle_authenticate(params),
        acp::METHOD_SESSION_NEW => session::handle_session_new(params, client).await,
        acp::METHOD_SESSION_LOAD => session::handle_session_load(params, client).await,
        acp::METHOD_SESSION_RESUME => session::handle_session_resume(params, client).await,
        acp::METHOD_SESSION_CLOSE => session::handle_session_close(params, client).await,
        acp::METHOD_SESSION_PROMPT => {
            prompt::handle_session_prompt(params, client, transport).await
        }
        acp::METHOD_SESSION_CANCEL => {
            // Reached only when a caller sends session/cancel WITH an id
            // (not spec-conformant, but still handled instead of panicking).
            let _ = cancel::handle_session_cancel(params, client).await;
            Ok(json!({}))
        }
        acp::METHOD_SESSION_LIST => session::handle_session_list(params, client).await,
        acp::METHOD_SESSION_DELETE => session::handle_session_delete(params, client).await,
        acp::METHOD_LOGOUT => Ok(json!({})),
        "" => Err(acp::AcpError::invalid_request()),
        other => Err(acp::AcpError::method_not_found(other)),
    };

    if is_notification {
        return None;
    }
    Some(match result {
        Ok(value) => success_response(id, value),
        Err(error) => error_response(id, error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn stdio_negotiates_session_prompt_update_cancel_and_config_lifecycle() {
        use axum::{
            http::StatusCode,
            routing::{get, post},
            Json, Router,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};

        let interrupt_calls = std::sync::Arc::new(AtomicUsize::new(0));
        let interrupt_counter = interrupt_calls.clone();
        let app = Router::new()
            .route(
                "/sessions",
                get(|| async { Json(json!([{"id": "created"}])) }),
            )
            .route(
                "/sessions",
                post(|| async { Json(json!({"id": "created"})) }),
            )
            .route(
                "/sessions/{id}/messages",
                post(|| async { Json(json!({"seq": 1})) }),
            )
            .route(
                "/sessions/{id}/events/history",
                get(|| async {
                    Json(json!([
                        {"seq": 2, "$kind": "assistantDelta", "text": "done"},
                        {"seq": 3, "$kind": "sessionTurnFinished"}
                    ]))
                }),
            )
            .route(
                "/sessions/{id}/interrupt",
                post(move || {
                    let calls = interrupt_counter.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let workspace = tempfile::tempdir().unwrap();
        let workspace_root = workspace.path().to_string_lossy().into_owned();

        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":acp::ACP_PROTOCOL_VERSION}}),
            json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":workspace_root}}),
            json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"created","prompt":[{"type":"text","text":"hello"}]}}),
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"created"}}),
        ];
        let mut input = requests
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        input.push(b'\n');
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let code = run_reader(
            std::io::Cursor::new(input),
            SharedWriter(output.clone()),
            AcpServerOptions { port, token: None },
        )
        .await;
        server.abort();

        assert_eq!(code, 0);
        assert_eq!(interrupt_calls.load(Ordering::SeqCst), 1);
        let lines = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        let messages = lines
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(messages.iter().any(|message| {
            message["id"] == 1 && message["result"]["protocolVersion"] == acp::ACP_PROTOCOL_VERSION
        }));
        assert!(messages
            .iter()
            .any(|message| message["id"] == 2 && message["result"]["sessionId"].is_string()));
        assert!(messages.iter().any(|message| {
            message["method"] == acp::METHOD_SESSION_UPDATE
                && message["params"]["sessionId"] == "created"
        }));
        assert!(messages.iter().any(|message| {
            message["id"] == 3 && message["result"]["stopReason"] == "end_turn"
        }));
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.get("id").is_none())
                .count(),
            1,
            "cancel notification must not emit a response"
        );
    }

    #[tokio::test]
    async fn logout_notification_produces_no_response_line() {
        // Build a minimal transport backed by an in-memory sink so
        // `handle_line` has somewhere to write outbound requests (unused on
        // this path).
        let sink: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct SinkWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for SinkWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let input: &[u8] = b"";
        let writer = SinkWriter(sink);
        let code = run_reader(
            input,
            writer,
            AcpServerOptions {
                port: 0,
                token: None,
            },
        )
        .await;
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn unknown_method_with_id_yields_method_not_found_error() {
        let client = DaemonClient::new(0, None);
        let sink: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        struct SinkWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for SinkWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writer = SinkWriter(sink.clone());
        let transport = AcpTransport::new(std::sync::Arc::new(std::sync::Mutex::new(writer)));
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "acp/bogus", "params": {} });
        let response = handle_line(request, &client, &transport).await.unwrap();
        assert_eq!(
            response["error"]["code"],
            acp::AcpErrorCode::MethodNotFound.code()
        );
    }

    /// In-memory pipe: a `BufRead` source fed by a channel so a test can
    /// answer the agent's outbound request only after it has seen it (no
    /// read-ahead race, unlike a pre-filled cursor).
    struct ChannelRead {
        rx: std::sync::mpsc::Receiver<Vec<u8>>,
        buf: Vec<u8>,
    }

    impl std::io::Read for ChannelRead {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.buf.is_empty() {
                match self.rx.recv() {
                    Ok(chunk) => self.buf = chunk,
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            Ok(n)
        }
    }

    fn parse_output(output: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>) -> Vec<Value> {
        String::from_utf8(output.lock().unwrap().clone())
            .unwrap()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect()
    }

    /// Drives initialize + session/prompt over an in-memory pipe against a
    /// mock daemon that raises one tool approval, answers the agent's
    /// `session/request_permission` with `option_id`, and returns whether the
    /// daemon saw an approval plus every line the agent wrote.
    async fn permission_roundtrip(option_id: &str) -> (bool, Vec<Value>) {
        use axum::{
            routing::{get, post},
            Json, Router,
        };
        use std::sync::atomic::{AtomicBool, Ordering};

        let approved = std::sync::Arc::new(AtomicBool::new(false));
        let resolved = std::sync::Arc::new(AtomicBool::new(false));
        let (approved_w, resolved_w) = (approved.clone(), resolved.clone());
        let resolved_r = resolved.clone();
        let app = Router::new()
            .route("/sessions", get(|| async { Json(json!([{"id": "s1"}])) }))
            .route(
                "/sessions/{id}/messages",
                post(|| async { Json(json!({"seq": 1})) }),
            )
            .route(
                "/sessions/{id}/events/history",
                get(move || {
                    let resolved = resolved_r.clone();
                    async move {
                        let mut events = vec![json!({
                            "seq": 2, "$kind": "toolApprovalRequested",
                            "call": {"id": "call-1", "name": "shell", "input": {"cmd": "ls"}}
                        })];
                        if resolved.load(Ordering::SeqCst) {
                            events.push(json!({"seq": 3, "$kind": "sessionTurnFinished"}));
                        }
                        Json(Value::Array(events))
                    }
                }),
            )
            .route(
                "/sessions/{id}/tool-approval",
                post(move |Json(body): Json<Value>| {
                    let (approved, resolved) = (approved_w.clone(), resolved_w.clone());
                    async move {
                        approved.store(body["approved"] == true, Ordering::SeqCst);
                        resolved.store(true, Ordering::SeqCst);
                        Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let reader = std::io::BufReader::new(ChannelRead {
            rx,
            buf: Vec::new(),
        });
        let run = tokio::spawn(run_reader(
            reader,
            SharedWriter(output.clone()),
            AcpServerOptions { port, token: None },
        ));
        let send = |v: Value| tx.send(format!("{v}\n").into_bytes()).unwrap();
        send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":acp::ACP_PROTOCOL_VERSION}}),
        );
        send(
            json!({"jsonrpc":"2.0","id":2,"method":"session/prompt","params":{"sessionId":"s1","prompt":[{"type":"text","text":"go"}]}}),
        );

        let mut permission_id = None;
        for _ in 0..100 {
            permission_id = parse_output(&output)
                .into_iter()
                .find(|m| m["method"] == acp::METHOD_SESSION_REQUEST_PERMISSION)
                .map(|m| m["id"].clone());
            if permission_id.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let permission_id = permission_id.expect("agent must send session/request_permission");
        send(
            json!({"jsonrpc":"2.0","id":permission_id,"result":{"outcome":{"outcome":"selected","optionId":option_id}}}),
        );

        for _ in 0..100 {
            if parse_output(&output).iter().any(|m| m["id"] == 2) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        drop(tx);
        assert_eq!(run.await.unwrap(), 0);
        server.abort();
        (approved.load(Ordering::SeqCst), parse_output(&output))
    }

    #[tokio::test]
    async fn permission_request_allow_once_approves_tool_over_pipe() {
        let (approved, messages) = permission_roundtrip("allow_once").await;
        assert!(approved, "allow_once must approve the daemon tool call");
        let request = messages
            .iter()
            .find(|m| m["method"] == acp::METHOD_SESSION_REQUEST_PERMISSION)
            .unwrap();
        assert_eq!(request["params"]["sessionId"], "s1");
        assert_eq!(request["params"]["toolCall"]["toolCallId"], "call-1");
        assert!(request["params"]["options"].as_array().unwrap().len() >= 2);
        assert!(messages
            .iter()
            .any(|m| m["id"] == 2 && m["result"]["stopReason"] == "end_turn"));
    }

    #[tokio::test]
    async fn permission_request_reject_once_denies_tool_over_pipe() {
        let (approved, messages) = permission_roundtrip("reject_once").await;
        assert!(!approved, "reject_once must deny the daemon tool call");
        assert!(messages
            .iter()
            .any(|m| m["id"] == 2 && m["result"].is_object()));
    }

    #[tokio::test]
    async fn unsupported_protocol_version_is_rejected_with_supported_list_over_pipe() {
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let input = format!(
            "{}\n",
            json!({"jsonrpc":"2.0","id":7,"method":"initialize","params":{"protocolVersion":2}})
        )
        .into_bytes();
        let code = run_reader(
            std::io::Cursor::new(input),
            SharedWriter(output.clone()),
            AcpServerOptions {
                port: 0,
                token: None,
            },
        )
        .await;
        assert_eq!(code, 0);
        let reply = parse_output(&output).remove(0);
        assert_eq!(reply["id"], 7);
        assert_eq!(
            reply["error"]["data"]["supportedProtocolVersions"],
            json!([acp::ACP_PROTOCOL_VERSION])
        );
    }

    #[tokio::test]
    async fn malformed_json_line_yields_parse_error_and_pipe_stays_alive() {
        let output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let input = format!(
            "not json\n{}\n",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":acp::ACP_PROTOCOL_VERSION}})
        )
        .into_bytes();
        let code = run_reader(
            std::io::Cursor::new(input),
            SharedWriter(output.clone()),
            AcpServerOptions {
                port: 0,
                token: None,
            },
        )
        .await;
        assert_eq!(code, 0);
        let messages = parse_output(&output);
        assert!(messages.iter().any(|m| m["error"]["code"] == -32700));
        assert!(messages
            .iter()
            .any(|m| m["id"] == 1 && m["result"].is_object()));
    }
}
