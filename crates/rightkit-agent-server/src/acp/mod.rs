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
