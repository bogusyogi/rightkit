//! `{app} rpc`: a JSON-RPC 2.0 session over stdio (headless R42/D35, the shape of
//! `pi --mode rpc` and `codex app-server`). Available on a host that turns `host_options::rpc_mode`
//! on.
//!
//! It is a thin adapter over the multi-turn stdio mode: it runs this executable's own
//! `run --input-format stream-json --stream-json [RUN FLAGS]` as a child, so the session
//! semantics, policy and telemetry are exactly those of `run`. One request or notification
//! per line on stdin; responses and `event` notifications, one per line, on stdout.
//!
//! Methods: `initialize`, `prompt` (`{"text"}`; answered when its turn ends with
//! `{"status","text"}`), `abort`, `ping`, `shutdown`, `approval/respond`. Every stream event of
//! the session is sent as `{"jsonrpc":"2.0","method":"event","params":<stream event>}`.
//!
//! Approvals (CLI-03): a client that declares `{"capabilities":{"approvals":true}}` in
//! `initialize` receives each tool approval as an `approval/request` notification (right after
//! the `event` notification of the same `toolApprovalRequested`) and answers it with
//! `approval/respond` `{"toolCallId","approved","remember"?,"reason"?}`. An approval nobody
//! answers inside the approval timeout is announced with `approval/expired` and DENIED. Without
//! the capability an approval is denied at once as `unattended`, as in `run`; closing the input
//! or `shutdown` denies every open approval as `detached`. An approval is never approved by
//! default.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The protocol id the `initialize` answer reports: `{app}.rpc.v1`.
fn protocol() -> String {
    crate::host_identity::schema("rpc.v1")
}

/// Longest request line accepted on stdin. It matches the `run` child's stream-json line
/// bound, so an oversized prompt is refused here with an error instead of being buffered,
/// forwarded and then killing the child session (audit 2026-11-05 CLI L1).
const MAX_REQUEST_LINE_BYTES: usize = 1024 * 1024;

/// One read from the request stream.
#[derive(Debug, PartialEq, Eq)]
enum RequestLine {
    Line(String),
    /// The line was not valid UTF-8; it was consumed and the stream continues.
    NotUtf8,
    /// The line exceeded [`MAX_REQUEST_LINE_BYTES`]; it was consumed and the stream continues.
    TooLong,
    /// End of input, or the stream itself failed.
    End,
}

/// Read one request line without buffering more than [`MAX_REQUEST_LINE_BYTES`], and
/// without ending the session on a bad line: a non-UTF-8 line used to make `lines()`
/// return an error that the loop treated as EOF, closing the session with no reply
/// (audit 2026-11-05 CLI M2).
async fn next_request_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> RequestLine {
    let mut bytes = Vec::new();
    let mut too_long = false;
    loop {
        let chunk = match reader.fill_buf().await {
            Ok(chunk) => chunk,
            Err(_) => return RequestLine::End,
        };
        if chunk.is_empty() {
            if bytes.is_empty() && !too_long {
                return RequestLine::End;
            }
            break;
        }
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(chunk.len(), |index| index + 1);
        if !too_long {
            if bytes.len().saturating_add(take) > MAX_REQUEST_LINE_BYTES + 1 {
                too_long = true;
                bytes = Vec::new();
            } else {
                bytes.extend_from_slice(&chunk[..take]);
            }
        }
        reader.consume(take);
        if newline.is_some() {
            break;
        }
    }
    if too_long {
        return RequestLine::TooLong;
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    match String::from_utf8(bytes) {
        Ok(line) => RequestLine::Line(line),
        Err(_) => RequestLine::NotUtf8,
    }
}

fn emit(out: &Mutex<std::io::Stdout>, value: &Value) {
    let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(error) = writeln!(out, "{value}").and_then(|()| out.flush()) {
        tracing::debug!(%error, "rpc: the client closed stdout; the message was not delivered");
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result(id: Value, value: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": value})
}

/// The child session's stdin, shared by the request loop and the child-stream reader task.
type ChildInput = Arc<tokio::sync::Mutex<Option<tokio::process::ChildStdin>>>;
/// Tool call ids whose approval was forwarded to the client and is still unanswered. A call id
/// leaves the set exactly once (answer, expiry, engine resolution or detach), so a double answer
/// and an answer racing the timer have one deterministic winner.
type OpenApprovals = Arc<Mutex<HashMap<String, ()>>>;

/// Write one record to the child's stdin. False when the input is closed or the write failed.
async fn write_child(child_in: &ChildInput, record: &str) -> bool {
    let mut guard = child_in.lock().await;
    match guard.as_mut() {
        Some(stdin) => {
            stdin.write_all(record.as_bytes()).await.is_ok() && stdin.flush().await.is_ok()
        }
        None => false,
    }
}

/// Write a record whose delivery nobody can act on if it fails (a denial, an abort): a closed
/// session input means the session is over, so a failed write is only noted.
async fn write_child_noting(child_in: &ChildInput, record: &str, what: &str) {
    if !write_child(child_in, record).await {
        tracing::debug!(
            what,
            "rpc: the session input is closed; nothing was delivered"
        );
    }
}

/// The child input line that answers one approval.
fn approval_response_record(
    tool_call_id: &str,
    approved: bool,
    remember: Option<&str>,
    reason: Option<&str>,
) -> String {
    format!(
        "{}\n",
        json!({
            "type": "approval_response",
            "tool_call_id": tool_call_id,
            "approved": approved,
            "remember": remember,
            "reason": reason,
        })
    )
}

/// The end of one turn in the `run` stream, as `(status, final text)`.
fn turn_end(event: &Value) -> Option<(&'static str, String)> {
    let data = event.get("data").unwrap_or(&Value::Null);
    match event.get("event").and_then(Value::as_str)? {
        "assistantMessageCompleted" => Some((
            "completed",
            data["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )),
        "runFailed" => Some((
            "failed",
            data["error"].as_str().unwrap_or_default().to_string(),
        )),
        "runInterrupted" => Some(("interrupted", String::new())),
        _ => None,
    }
}

pub(crate) async fn run_rpc(run_args: Vec<String>, approval_timeout: std::time::Duration) -> i32 {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            emit(
                &out,
                &error(
                    Value::Null,
                    -32603,
                    &format!("cannot locate {}: {err}", crate::host_identity::app()),
                ),
            );
            return 1;
        }
    };
    let mut command = rightkit_child_process::tokio_support::command(&exe);
    command
        .arg("run")
        .args(["--input-format", "stream-json", "--stream-json"])
        .arg("--approval-callback")
        .args(&run_args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit());
    // This process removed the credential names from its own environment after reading
    // them (providers R46); the session child is this executable itself and needs the same keys,
    // so it gets them on its launch spec. `.env` keys it reads itself.
    command.envs(rightkit_llm::agent_providers::host_credentials::host().environment_keys());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            emit(
                &out,
                &error(
                    Value::Null,
                    -32603,
                    &format!("cannot start the session: {err}"),
                ),
            );
            return 1;
        }
    };
    let child_in: ChildInput = Arc::new(tokio::sync::Mutex::new(child.stdin.take()));
    let child_out = child.stdout.take();
    let pending: Arc<Mutex<VecDeque<Value>>> = Arc::new(Mutex::new(VecDeque::new()));
    let open: OpenApprovals = Arc::new(Mutex::new(HashMap::new()));
    let capable = Arc::new(AtomicBool::new(false));

    // Child stream -> `event` notifications, and the answer to the oldest open prompt.
    let reader = {
        let out = out.clone();
        let pending = pending.clone();
        let child_in = child_in.clone();
        let open = open.clone();
        let capable = capable.clone();
        tokio::spawn(async move {
            let Some(child_out) = child_out else { return };
            let mut lines = BufReader::new(child_out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(event) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                emit(
                    &out,
                    &json!({"jsonrpc": "2.0", "method": "event", "params": event}),
                );
                match event.get("event").and_then(Value::as_str) {
                    Some("toolApprovalRequested") => {
                        let data = event.get("data").unwrap_or(&Value::Null);
                        let Some(call_id) = data.pointer("/call/id").and_then(Value::as_str) else {
                            continue;
                        };
                        let call_id = call_id.to_string();
                        if !capable.load(Ordering::SeqCst) {
                            // No client declared `capabilities.approvals`: denied at once, as in `run`.
                            let record =
                                approval_response_record(&call_id, false, None, Some("unattended"));
                            write_child_noting(&child_in, &record, "unattended denial").await;
                        } else {
                            // Open before the request goes out, so no answer can arrive first.
                            open.lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .insert(call_id.clone(), ());
                            emit(
                                &out,
                                &json!({
                                    "jsonrpc": "2.0",
                                    "method": "approval/request",
                                    "params": {
                                        "toolCallId": call_id,
                                        "tool": data.pointer("/call/name").cloned().unwrap_or(Value::Null),
                                        "argumentsJson": data.pointer("/call/argumentsJson").cloned().unwrap_or(Value::Null),
                                        "riskLabel": data.get("riskLabel").cloned().unwrap_or(Value::Null),
                                        "proposedChange": data.get("proposedChange").cloned().unwrap_or(Value::Null),
                                        "policySource": data.get("policySource").cloned().unwrap_or(Value::Null),
                                        "effects": data.get("effects").cloned().unwrap_or(Value::Null),
                                        "sessionId": event.get("session_id").cloned().unwrap_or(Value::Null),
                                        "timeoutSecs": approval_timeout.as_secs(),
                                    },
                                }),
                            );
                            // The timer denies the call when nobody answered: never approved,
                            // never left waiting.
                            let out = out.clone();
                            let open = open.clone();
                            let child_in = child_in.clone();
                            tokio::spawn(async move {
                                tokio::time::sleep(approval_timeout).await;
                                let still_open = open
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .remove(&call_id)
                                    .is_some();
                                if still_open {
                                    emit(
                                        &out,
                                        &json!({
                                            "jsonrpc": "2.0",
                                            "method": "approval/expired",
                                            "params": {"toolCallId": call_id, "reason": "timeout"},
                                        }),
                                    );
                                    let record = approval_response_record(
                                        &call_id,
                                        false,
                                        None,
                                        Some("timeout"),
                                    );
                                    write_child_noting(&child_in, &record, "timeout denial").await;
                                }
                            });
                        }
                    }
                    Some("toolApprovalResolved") => {
                        // The engine answered (or ended) the call itself: it is no longer open.
                        let data = event.get("data").unwrap_or(&Value::Null);
                        let resolved = data
                            .get("toolCallId")
                            .or_else(|| data.pointer("/call/id"))
                            .and_then(Value::as_str);
                        if let Some(call_id) = resolved {
                            open.lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .remove(call_id);
                        }
                    }
                    _ => {}
                }
                if let Some((status, text)) = turn_end(&event) {
                    let id = pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pop_front();
                    if let Some(id) = id {
                        emit(&out, &result(id, json!({"status": status, "text": text})));
                    }
                }
            }
            // The session ended: no open prompt will be answered.
            let open: Vec<Value> = pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .drain(..)
                .collect();
            for id in open {
                emit(
                    &out,
                    &error(id, -32000, "the session ended before this prompt finished"),
                );
            }
        })
    };

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut shutdown_id: Option<Value> = None;
    loop {
        let line = match next_request_line(&mut stdin).await {
            RequestLine::Line(line) => line,
            RequestLine::NotUtf8 => {
                emit(
                    &out,
                    &error(
                        Value::Null,
                        -32700,
                        "parse error: request line is not valid UTF-8",
                    ),
                );
                continue;
            }
            RequestLine::TooLong => {
                emit(
                    &out,
                    &error(
                        Value::Null,
                        -32700,
                        &format!(
                            "parse error: request line exceeds {MAX_REQUEST_LINE_BYTES} bytes"
                        ),
                    ),
                );
                continue;
            }
            RequestLine::End => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                emit(
                    &out,
                    &error(Value::Null, -32700, &format!("parse error: {err}")),
                );
                continue;
            }
        };
        let id = request.get("id").cloned();
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_empty() {
            if let Some(id) = id {
                emit(
                    &out,
                    &error(
                        id,
                        -32600,
                        "invalid request: need jsonrpc \"2.0\" and a method",
                    ),
                );
            }
            continue;
        }
        let send = |record: Value| {
            let line = format!("{record}\n");
            async move { line }
        };
        match method {
            "initialize" => {
                if request["params"]["capabilities"]["approvals"].as_bool() == Some(true) {
                    capable.store(true, Ordering::SeqCst);
                }
                if let Some(id) = id {
                    emit(
                        &out,
                        &result(
                            id,
                            json!({
                                "protocol": protocol(),
                                "serverInfo": {"name": crate::host_identity::app(), "version": env!("CARGO_PKG_VERSION")},
                                "methods": ["initialize", "prompt", "abort", "ping", "shutdown", "approval/respond"],
                                "notifications": ["event", "approval/request", "approval/expired"],
                                "approvalTimeoutSecs": approval_timeout.as_secs(),
                            }),
                        ),
                    );
                }
            }
            "ping" => {
                if let Some(id) = id {
                    emit(&out, &result(id, json!({})));
                }
            }
            "prompt" => {
                let text = request["params"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if text.trim().is_empty() {
                    if let Some(id) = id {
                        emit(&out, &error(id, -32602, "prompt needs params.text"));
                    }
                    continue;
                }
                let record = send(json!({"type": "user_message", "text": text})).await;
                let written = write_child(&child_in, &record).await;
                match (written, id) {
                    (true, Some(id)) => pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push_back(id),
                    (false, Some(id)) => emit(
                        &out,
                        &error(id, -32000, "the session is not accepting prompts"),
                    ),
                    _ => {}
                }
            }
            "abort" => {
                let record = send(json!({"type": "control", "action": "abort"})).await;
                // The engine cancels its own approval waits on abort: nothing stays open here.
                open.lock().unwrap_or_else(|e| e.into_inner()).clear();
                write_child_noting(&child_in, &record, "abort").await;
                if let Some(id) = id {
                    emit(&out, &result(id, json!({"aborting": true})));
                }
            }
            "approval/respond" => {
                let params = &request["params"];
                let call_id = params.get("toolCallId").and_then(Value::as_str);
                let approved = params.get("approved").and_then(Value::as_bool);
                let (Some(call_id), Some(approved)) = (call_id, approved) else {
                    if let Some(id) = id {
                        emit(
                            &out,
                            &error(
                                id,
                                -32602,
                                "approval/respond needs params.toolCallId and params.approved",
                            ),
                        );
                    }
                    continue;
                };
                let remember = match params.get("remember") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(value)) if value == "session" || value == "project" => {
                        Some(value.as_str())
                    }
                    Some(_) => {
                        if let Some(id) = id {
                            emit(
                                &out,
                                &error(id, -32602, "remember must be session or project"),
                            );
                        }
                        continue;
                    }
                };
                let reason = params.get("reason").and_then(Value::as_str);
                // Leave the open set first: exactly one of an answer, the timer, a second answer
                // or the engine wins; the loser finds the id gone.
                let was_open = open
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(call_id)
                    .is_some();
                if !was_open {
                    if let Some(id) = id {
                        emit(
                            &out,
                            &error(
                                id,
                                -32602,
                                &format!("no open approval for toolCallId {call_id}"),
                            ),
                        );
                    }
                    continue;
                }
                let record = approval_response_record(
                    call_id,
                    approved,
                    if approved { remember } else { None },
                    reason,
                );
                let written = write_child(&child_in, &record).await;
                if let Some(id) = id {
                    if written {
                        emit(&out, &result(id, json!({"resolved": true})));
                    } else {
                        emit(
                            &out,
                            &error(
                                id,
                                -32000,
                                "the session is not accepting approval responses",
                            ),
                        );
                    }
                }
            }
            "shutdown" => {
                shutdown_id = id;
                break;
            }
            other => {
                if let Some(id) = id {
                    emit(
                        &out,
                        &error(id, -32601, &format!("method not found: {other}")),
                    );
                }
            }
        }
    }
    // End of input or `shutdown`: every open approval is denied as `detached` BEFORE the
    // session's input closes, so the turn never waits on a client that is gone.
    let detached: Vec<String> = open
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain()
        .map(|(call_id, ())| call_id)
        .collect();
    for call_id in detached {
        let record = approval_response_record(&call_id, false, None, Some("detached"));
        write_child_noting(&child_in, &record, "detached denial").await;
    }
    // Close the session's input and let it finish.
    drop(child_in.lock().await.take());
    let status = child
        .wait()
        .await
        .ok()
        .and_then(|status| status.code())
        .unwrap_or(1);
    if let Err(error) = reader.await {
        tracing::debug!(%error, "rpc: the session reader task ended abnormally");
    }
    if let Some(id) = shutdown_id {
        emit(&out, &result(id, json!({"exitCode": status})));
    }
    status
}

#[cfg(test)]
mod request_line_tests {
    use super::{next_request_line, RequestLine, MAX_REQUEST_LINE_BYTES};

    async fn read_all(input: Vec<u8>) -> Vec<RequestLine> {
        let mut reader = tokio::io::BufReader::with_capacity(64, std::io::Cursor::new(input));
        let mut out = Vec::new();
        loop {
            let line = next_request_line(&mut reader).await;
            let end = line == RequestLine::End;
            out.push(line);
            if end {
                return out;
            }
        }
    }

    #[tokio::test]
    async fn non_utf8_line_is_reported_and_the_stream_continues() {
        let mut input = b"{\"a\":1}\r\n".to_vec();
        input.extend_from_slice(b"{\"t\":\"\xff\"}\n");
        input.extend_from_slice(b"last");
        assert_eq!(
            read_all(input).await,
            vec![
                RequestLine::Line("{\"a\":1}".to_string()),
                RequestLine::NotUtf8,
                RequestLine::Line("last".to_string()),
                RequestLine::End,
            ]
        );
    }

    #[tokio::test]
    async fn oversized_line_is_refused_without_ending_the_stream() {
        let mut input = vec![b'x'; MAX_REQUEST_LINE_BYTES + 10];
        input.push(b'\n');
        input.extend_from_slice(b"ok\n");
        assert_eq!(
            read_all(input).await,
            vec![
                RequestLine::TooLong,
                RequestLine::Line("ok".to_string()),
                RequestLine::End
            ]
        );
    }

    #[tokio::test]
    async fn line_at_the_bound_is_accepted() {
        let mut input = vec![b'y'; MAX_REQUEST_LINE_BYTES];
        input.push(b'\n');
        let lines = read_all(input).await;
        assert!(
            matches!(&lines[0], RequestLine::Line(line) if line.len() == MAX_REQUEST_LINE_BYTES)
        );
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::{approval_response_record, turn_end};
    use serde_json::json;

    #[test]
    fn an_approval_answer_is_never_approved_by_default() {
        let record = approval_response_record("call-1", false, None, Some("timeout"));
        let parsed: serde_json::Value = serde_json::from_str(record.trim_end()).expect("json");
        assert_eq!(parsed["type"], "approval_response");
        assert_eq!(parsed["tool_call_id"], "call-1");
        assert_eq!(parsed["approved"], false);
        assert_eq!(parsed["reason"], "timeout");
        assert!(record.ends_with('\n'));
    }

    #[test]
    fn a_turn_ends_on_the_three_terminal_events_only() {
        let done =
            json!({"event": "assistantMessageCompleted", "data": {"message": {"content": "hi"}}});
        assert_eq!(turn_end(&done), Some(("completed", "hi".to_string())));
        let failed = json!({"event": "runFailed", "data": {"error": "boom"}});
        assert_eq!(turn_end(&failed), Some(("failed", "boom".to_string())));
        let stopped = json!({"event": "runInterrupted"});
        assert_eq!(turn_end(&stopped), Some(("interrupted", String::new())));
        assert_eq!(turn_end(&json!({"event": "toolApprovalRequested"})), None);
        assert_eq!(turn_end(&json!({"data": {}})), None);
    }
}
