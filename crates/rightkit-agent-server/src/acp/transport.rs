//! CG-A08 — bidirectional JSON-RPC-over-stdio transport for the ACP stdio
//! agent surface.
//!
//! NOTICE: this module is a relocation of the pre-existing
//! `engine/bins/coderight/src/acp_transport.rs` (RightKit-original, RETAIN
//! action per canon CG-A08) into the ACP module's canonical home. Logic,
//! shape, and test lineage are carried forward unchanged; only the module
//! path moves.
//!
//! ACP is bidirectional on a single stdio pipe: the client sends agent-bound
//! requests (`session/prompt`, ...) and the agent, while handling one of
//! those, may need to send its OWN request back to the client
//! (`session/request_permission`) and block until that specific response
//! arrives — all while still reading whatever the client sends next. A plain
//! `for line in reader.lines()` loop cannot do this: reading the next line
//! and awaiting a correlated response are the same operation. This module
//! gives the request handler a `send_request` it can `.await`, backed by a
//! pending-response map keyed by request id, fed by a dedicated reader task.

use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

/// Shared handle for writing JSON-RPC lines to the client and issuing
/// outbound requests that block on a correlated response.
#[derive(Clone)]
pub struct AcpTransport {
    writer: Arc<Mutex<dyn Write + Send>>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    next_id: Arc<Mutex<i64>>,
}

impl AcpTransport {
    pub(crate) fn new(writer: Arc<Mutex<dyn Write + Send>>) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(1)),
        }
    }

    /// Writes one JSON-RPC line (a response, notification, or outbound
    /// request) to the client. Returns `Err` only on a broken pipe.
    pub fn send_line(&self, value: &Value) -> std::io::Result<()> {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        rightkit_sidecar::write_json_line_flush(
            &mut &mut *writer,
            value,
            rightkit_sidecar::UNCAPPED,
        )
        .map_err(|error| match error {
            rightkit_sidecar::FrameError::Io(error) => error,
            error => std::io::Error::other(error.to_string()),
        })
    }

    /// Sends an outbound JSON-RPC request (agent → client) and awaits the
    /// matching response by id. Used for `session/request_permission`.
    /// Returns `None` if the pipe closes before a response arrives.
    pub async fn send_request(&self, method: &str, params: Value) -> Option<Value> {
        let id = {
            let mut next_id = self
                .next_id
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = *next_id;
            *next_id += 1;
            id
        };
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, tx);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if self.send_line(&request).is_err() {
            self.pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
            return None;
        }
        rx.await.ok()
    }

    /// Feeds an incoming line to the transport. Lines carrying a `result` or
    /// `error` whose `id` matches a pending outbound request resolve that
    /// request instead of being handed to the inbound dispatcher; everything
    /// else is returned as `Some(value)` for normal dispatch.
    fn intercept_response(&self, value: Value) -> Option<Value> {
        let has_result_or_error = value.get("result").is_some() || value.get("error").is_some();
        let Some(id) = value.get("id").and_then(Value::as_i64) else {
            return Some(value);
        };
        if !has_result_or_error {
            return Some(value);
        }
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        match sender {
            Some(sender) => {
                let _ = sender.send(value);
                None
            }
            None => Some(value),
        }
    }

    /// Wake outbound request waiters when the peer closes stdin. Without
    /// this, an ACP prompt blocked on permission can keep the transport alive
    /// forever while EOF waits for its task to finish.
    fn fail_pending(&self) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

/// Runs the stdio loop: reads lines on a blocking thread, feeds each parsed
/// line through the transport's response interceptor, and dispatches
/// whatever isn't a correlated response to `on_request`. Returns the process
/// exit code once the reader hits EOF or an I/O error.
pub async fn run<R, W, F, Fut>(reader: R, writer: W, on_request: F) -> i32
where
    R: BufRead + Send + 'static,
    W: Write + Send + 'static,
    F: Fn(Value, AcpTransport) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Option<Value>> + Send + 'static,
{
    let transport = AcpTransport::new(Arc::new(Mutex::new(writer)));
    let on_request = Arc::new(on_request);
    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<Result<Value, String>>();
    let mut request_tasks = tokio::task::JoinSet::new();

    let read_handle = tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut buffer = Vec::new();
        loop {
            match rightkit_sidecar::read_json_line::<_, Value>(
                &mut reader,
                &mut buffer,
                rightkit_sidecar::UNCAPPED,
            ) {
                Ok(None) | Err(rightkit_sidecar::FrameError::Io(_)) => break,
                Ok(Some(value)) => {
                    if line_tx.send(Ok(value)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let message = error.json_message().unwrap_or_else(|| error.to_string());
                    if line_tx.send(Err(message)).is_err() {
                        break;
                    }
                }
            }
        }
    });

    while let Some(line) = line_rx.recv().await {
        let parsed = match line {
            Ok(value) => value,
            Err(message) => {
                let error = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": format!("parse error: {message}")},
                });
                let _ = transport.send_line(&error);
                continue;
            }
        };
        let Some(request) = transport.intercept_response(parsed) else {
            continue;
        };
        // Prompt handling can issue an outbound permission request and then
        // await its correlated response. Keep this reader loop free to
        // intercept that response (or an inbound cancel notification);
        // awaiting the handler here would deadlock both lifecycle paths.
        let transport = transport.clone();
        let on_request = on_request.clone();
        request_tasks.spawn(async move {
            if let Some(response) = on_request(request, transport.clone()).await {
                let _ = transport.send_line(&response);
            }
        });
    }

    // Wake permission/prompt waiters before draining tasks. A client that
    // closes stdin cannot provide any remaining correlated response.
    transport.fail_pending();

    // Preserve request completion semantics at EOF: a client that closes its
    // input after sending a finite request batch still receives every queued
    // response before this bridge exits.
    while request_tasks.join_next().await.is_some() {
        // Each task writes its own response before completing.
    }
    let _ = read_handle.await;
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatches_plain_requests_to_handler() {
        let input: &'static [u8] =
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"logout\",\"params\":{}}\n";
        let output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let output_writer = SharedVecWriter(output.clone());
        let code = run(input, output_writer, |request, _transport| async move {
            let id = request.get("id").cloned();
            Some(serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}}))
        })
        .await;
        assert_eq!(code, 0);
        let bytes = output.lock().unwrap().clone();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"result\""));
    }

    #[tokio::test]
    async fn intercepts_responses_matching_a_pending_outbound_request() {
        let input =
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"outcome\":\"cancelled\"}}\n".to_vec();
        let output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let output_writer = SharedVecWriter(output.clone());
        let transport = AcpTransport::new(Arc::new(Mutex::new(output_writer)));
        let (tx, rx) = oneshot::channel();
        transport.pending.lock().unwrap().insert(1, tx);
        let parsed: Value = serde_json::from_slice(&input[..input.len() - 1]).unwrap();
        let intercepted = transport.intercept_response(parsed);
        assert!(intercepted.is_none());
        let response = rx.await.unwrap();
        assert_eq!(response["result"]["outcome"], "cancelled");
    }

    struct SharedVecWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for SharedVecWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
