//! ACP client prompt / stream / cancel surface.
//!
//! PR 7 of the architecture plan. The client owns three operations:
//!
//! - [`AcpClient::prompt`] sends `session/prompt` and returns a
//!   [`VendorTurnHandle`] the executor hands to the streaming loop.
//! - [`AcpClient::stream`] drains `session/update` notifications
//!   until a terminal update is observed. Each notification becomes
//!   an [`AcpStreamEvent`] the higher-layer normalizer turns into a
//!   [`crate::driver_host::NormalizedEvent`].
//! - [`AcpClient::cancel`] sends `session/cancel`.
//!
//! The transport is a *client* of ACP. Every JSON-RPC frame the
//! client sends is built by [`super::codec::AcpCodec`]; every frame
//! the client reads is parsed by the same codec. The client never
//! touches the wire directly.

use crate::driver_host::cli_runtime::adapter::{
    CliPrompt, CliRuntimeError, StartedCliProcess, VendorSessionHandle, VendorTurnHandle,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::codec::{
    decode_message, AcpCodec, AcpCodecError, AcpInbound, AcpResponse, AcpSessionUpdate,
    AcpUpdateBody,
};

/// One event the streaming layer yielded to the executor. Carries
/// the raw inner body so the executor's normalizer can pass it
/// through unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpStreamEvent {
    pub session_id: String,
    pub body: AcpUpdateBody,
}

impl AcpStreamEvent {
    pub fn from_update(update: AcpSessionUpdate) -> Self {
        Self {
            session_id: update.session_id,
            body: update.update,
        }
    }

    /// True for the terminal events the streaming loop uses to
    /// break out of its read loop.
    pub fn is_terminal(&self) -> bool {
        self.body.is_terminal()
    }
}

/// Outcome of an `AcpClient::prompt` call. The executor pulls the
/// turn handle out and continues with [`AcpClient::stream`] until it
/// sees a terminal event.
#[derive(Debug)]
pub struct AcpPromptResult {
    pub turn: VendorTurnHandle,
    /// The JSON-RPC response body from `session/prompt`. Most ACP
    /// servers echo the turn id under `turnId`; the executor uses
    /// this for event tagging.
    pub response: Value,
}

/// Errors the client can raise. Each maps to a stable `code` so the
/// executor's recovery loop can branch on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpClientError {
    Codec(AcpCodecError),
    /// The server returned a JSON-RPC error object on `session/prompt`.
    Rpc {
        code: i64,
        message: String,
    },
    /// The server's response carried no `result` field.
    MissingResult,
    /// The streaming loop saw a non-AcpInbound frame — typically a
    /// partial line.
    StreamEndedUnexpectedly,
}

impl std::fmt::Display for AcpClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcpClientError::Codec(error) => write!(f, "ACP codec error: {error}"),
            AcpClientError::Rpc { code, message } => {
                write!(f, "ACP RPC error {code}: {message}")
            }
            AcpClientError::MissingResult => write!(f, "ACP response missing result"),
            AcpClientError::StreamEndedUnexpectedly => {
                write!(f, "ACP stream ended without a terminal update")
            }
        }
    }
}

impl std::error::Error for AcpClientError {}

impl From<AcpCodecError> for AcpClientError {
    fn from(error: AcpCodecError) -> Self {
        AcpClientError::Codec(error)
    }
}

impl AcpClientError {
    pub fn code(&self) -> &'static str {
        match self {
            AcpClientError::Codec(_) => "acp_client_codec",
            AcpClientError::Rpc { .. } => "acp_client_rpc",
            AcpClientError::MissingResult => "acp_client_missing_result",
            AcpClientError::StreamEndedUnexpectedly => "acp_client_stream_ended",
        }
    }
}

impl From<AcpClientError> for CliRuntimeError {
    fn from(error: AcpClientError) -> Self {
        CliRuntimeError::new(error.code(), error.to_string())
    }
}

/// The ACP client surface. Constructed by [`AcpClient::new`] and
/// stored on the transport.
#[derive(Debug, Default)]
pub struct AcpClient {
    _private: (),
}

impl AcpClient {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Issue `session/prompt`. The transport returns a turn handle
    /// plus the server's response so the executor can call
    /// [`stream`] right away.
    ///
    /// [`stream`]: AcpClient::stream
    pub async fn prompt(
        &self,
        session: &VendorSessionHandle,
        prompt: &CliPrompt,
        codec: &AcpCodec,
    ) -> Result<AcpPromptResult, CliRuntimeError> {
        let line = codec
            .encode_session_prompt(
                &session.vendor_session_id,
                &prompt.text,
                prompt.model_id.as_deref(),
            )
            .map_err(AcpClientError::from)?;
        write_line(&session.process, &line)?;
        let response = read_response(&session.process)?;
        if let Some(error) = &response.error {
            return Err(CliRuntimeError::new(
                AcpClientError::Rpc {
                    code: error.code,
                    message: error.message.clone(),
                }
                .code(),
                error.message.clone(),
            ));
        }
        let result = response.result.clone().unwrap_or(Value::Null);
        let vendor_turn_id = result
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(AcpPromptResult {
            turn: VendorTurnHandle {
                vendor_turn_id,
                session: clone_session_handle(session),
            },
            response: result,
        })
    }

    /// Drain `session/update` notifications until a terminal event
    /// arrives. The transport returns the collected events so the
    /// executor's normalizer can fold them into a
    /// [`crate::driver_host::NormalizedEvent`] stream.
    pub async fn stream(
        &self,
        session: &VendorSessionHandle,
    ) -> Result<Vec<AcpStreamEvent>, CliRuntimeError> {
        let mut events = Vec::new();
        loop {
            let frame = read_frame(&session.process)?;
            match frame {
                AcpInbound::Notification(notification) => {
                    if notification.method != "session/update" {
                        // The transport should never see non-update
                        // notifications on this stream; if it does,
                        // surface as a hard failure so the executor
                        // can decide whether to recover.
                        return Err(CliRuntimeError::new(
                            "acp_client_unexpected_notification",
                            format!(
                                "unexpected notification method {:?} on ACP stream",
                                notification.method
                            ),
                        ));
                    }
                    let update = super::codec::parse_session_update(&notification.params)
                        .map_err(AcpClientError::from)?;
                    let terminal = update.update.is_terminal();
                    events.push(AcpStreamEvent::from_update(update));
                    if terminal {
                        return Ok(events);
                    }
                }
                AcpInbound::Response(_) => {
                    // A response appearing on the streaming loop is
                    // almost certainly a stale reply from a previous
                    // request. Drop it and keep reading.
                    continue;
                }
                AcpInbound::ProtocolError(error) => {
                    return Err(CliRuntimeError::new(
                        "acp_client_protocol_error",
                        format!("{}: {}", error.code, error.message),
                    ));
                }
            }
        }
    }

    /// Send `session/cancel` for an in-flight turn. The transport
    /// does not wait for a terminal update here; the streaming loop
    /// is expected to observe a `TurnCancelled` body next.
    pub async fn cancel(
        &self,
        session: &VendorSessionHandle,
        codec: &AcpCodec,
    ) -> Result<(), CliRuntimeError> {
        let line = codec
            .encode_session_cancel(&session.vendor_session_id)
            .map_err(AcpClientError::from)?;
        write_line(&session.process, &line)?;
        // Read and discard the JSON-RPC reply. The transport does
        // not need the response body — a successful ack is the
        // success signal.
        let _ = read_response(&session.process)?;
        Ok(())
    }
}

/// Write a single newline-terminated line to the child stdin. The
/// transport never writes anything else to the wire.
fn write_line(process: &StartedCliProcess, line: &str) -> Result<(), CliRuntimeError> {
    process.inner.write_line(line).map_err(|error| {
        CliRuntimeError::new(
            "acp_client_write",
            format!("failed to write ACP frame: {}", error.message),
        )
    })
}

/// Read one JSON-RPC *response* frame from the child. The streaming
/// loop uses [`read_frame`] instead so it can also surface
/// notifications.
fn read_response(process: &StartedCliProcess) -> Result<AcpResponse, CliRuntimeError> {
    loop {
        match read_frame(process)? {
            AcpInbound::Response(response) => return Ok(response),
            // Drain notifications past while we wait for the response.
            AcpInbound::Notification(_) => continue,
            AcpInbound::ProtocolError(error) => {
                return Err(CliRuntimeError::new(
                    "acp_client_protocol_error",
                    format!("{}: {}", error.code, error.message),
                ));
            }
        }
    }
}

/// Read one JSON-RPC envelope off the child and classify it.
fn read_frame(process: &StartedCliProcess) -> Result<AcpInbound, CliRuntimeError> {
    let Some(value) = process.inner.read_event().map_err(|error| {
        CliRuntimeError::new(
            "acp_client_read",
            format!("failed to read ACP frame: {}", error.message),
        )
    })?
    else {
        return Err(CliRuntimeError::new(
            "acp_client_stream_ended",
            "ACP child closed stdout before responding",
        ));
    };
    let line = value.to_string();
    decode_message(&line)
        .map_err(|error| CliRuntimeError::new("acp_client_codec", error.to_string()))
}

/// Clone a `VendorSessionHandle` so the executor can hand it to the
/// prompt / stream / cancel calls without re-borrowing the
/// transport.
fn clone_session_handle(session: &VendorSessionHandle) -> VendorSessionHandle {
    VendorSessionHandle {
        provider_id: session.provider_id.clone(),
        transport: session.transport,
        vendor_session_id: session.vendor_session_id.clone(),
        model_id: session.model_id.clone(),
        process: StartedCliProcess {
            pid: session.process.pid,
            generation: session.process.generation,
            inner: session.process.inner.clone_box(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::adapter::{CliProcessIo, StartedCliProcess};
    use crate::driver_host::cli_runtime::ids::{CliProviderId, CliTransportKind};
    use std::sync::{Arc, Mutex};

    /// Minimal in-process fake I/O. Captures writes and feeds a
    /// pre-canned script of responses back to the client.
    #[derive(Clone)]
    struct FakeProcess {
        pid: u32,
        generation: u64,
        written: Arc<Mutex<Vec<String>>>,
        scripted: Arc<Mutex<Vec<String>>>,
    }

    impl FakeProcess {
        fn new(pid: u32, scripted: Vec<String>) -> Self {
            Self {
                pid,
                generation: 1,
                written: Arc::new(Mutex::new(Vec::new())),
                scripted: Arc::new(Mutex::new(scripted)),
            }
        }

        fn written_lines(&self) -> Vec<String> {
            self.written.lock().unwrap().clone()
        }
    }

    impl CliProcessIo for FakeProcess {
        fn write_line(&self, line: &str) -> Result<(), CliRuntimeError> {
            self.written.lock().unwrap().push(line.to_string());
            Ok(())
        }
        fn read_event(&self) -> Result<Option<Value>, CliRuntimeError> {
            let mut queue = self.scripted.lock().unwrap();
            if queue.is_empty() {
                Ok(None)
            } else {
                serde_json::from_str(&queue.remove(0))
                    .map(Some)
                    .map_err(|error| {
                        crate::driver_host::cli_runtime::adapter::CliRuntimeError::new(
                            "fake_acp_json",
                            error.to_string(),
                        )
                    })
            }
        }
        fn shutdown(&self) -> Result<(), CliRuntimeError> {
            Ok(())
        }
        fn clone_box(&self) -> Box<dyn CliProcessIo> {
            Box::new(self.clone())
        }
    }

    fn fake_process(scripted: Vec<&str>) -> (StartedCliProcess, FakeProcess) {
        let fake = FakeProcess::new(11, scripted.into_iter().map(str::to_string).collect());
        let process = StartedCliProcess {
            pid: fake.pid,
            generation: fake.generation,
            inner: Box::new(fake.clone()),
        };
        (process, fake)
    }

    fn sample_session(process: StartedCliProcess) -> VendorSessionHandle {
        VendorSessionHandle {
            provider_id: CliProviderId::new("grok"),
            transport: CliTransportKind::Acp,
            vendor_session_id: "v-1".into(),
            model_id: Some("gpt-x".into()),
            process,
        }
    }

    fn sample_prompt() -> CliPrompt {
        CliPrompt {
            text: "summarize the diff".into(),
            tool_ceiling: Vec::new(),
            model_id: Some("gpt-x".into()),
        }
    }

    /// `prompt` must write a `session/prompt` envelope carrying the
    /// user's text and the model switch.
    #[test]
    fn prompt_writes_session_prompt_envelope() {
        let (process, fake) = fake_process(vec![
            r#"{"jsonrpc":"2.0","id":1,"result":{"turnId":"t-1"}}"#,
        ]);
        let session = sample_session(process);
        let codec = AcpCodec::new();
        let result = block_on(AcpClient::new().prompt(&session, &sample_prompt(), &codec)).unwrap();
        assert_eq!(result.turn.vendor_turn_id.as_deref(), Some("t-1"));
        let written = fake.written_lines();
        let parsed: Value = serde_json::from_str(&written[0]).unwrap();
        assert_eq!(parsed["method"], "session/prompt");
        assert_eq!(parsed["params"]["sessionId"], "v-1");
        assert_eq!(parsed["params"]["modelId"], "gpt-x");
        assert_eq!(parsed["params"]["prompt"][0]["text"], "summarize the diff");
    }

    /// `prompt` must surface server-side errors with a stable code.
    #[test]
    fn prompt_surfaces_server_error() {
        let (process, _fake) = fake_process(vec![
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#,
        ]);
        let session = sample_session(process);
        let codec = AcpCodec::new();
        let err =
            block_on(AcpClient::new().prompt(&session, &sample_prompt(), &codec)).unwrap_err();
        assert_eq!(err.code, "acp_client_rpc");
        assert!(err.message.contains("nope"));
    }

    /// `stream` must drain `session/update` notifications and stop
    /// at a terminal update.
    #[test]
    fn stream_collects_events_until_terminal_update() {
        let (process, _fake) = fake_process(vec![
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"agent_message_chunk","text":"hello"}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"tool_call","call_id":"c-1","tool_name":"mcp.read_file","server_name":"host","arguments":{"path":"/etc/passwd"}}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"turn_completed"}}}"#,
        ]);
        let session = sample_session(process);
        let events = block_on(AcpClient::new().stream(&session)).unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0].body,
            AcpUpdateBody::AgentMessageChunk { .. }
        ));
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
        assert!(events[2].is_terminal());
    }

    /// `stream` must surface a `TurnFailed` body as the terminal
    /// marker so the executor can fold it into a
    /// `NormalizedEvent::TurnFailed`.
    #[test]
    fn stream_terminates_on_turn_failed() {
        let (process, _fake) = fake_process(vec![
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"turn_failed","message":"denied"}}}"#,
        ]);
        let session = sample_session(process);
        let events = block_on(AcpClient::new().stream(&session)).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0].body {
            AcpUpdateBody::TurnFailed { message } => assert_eq!(message, "denied"),
            other => panic!("expected TurnFailed, got {other:?}"),
        }
    }

    /// `stream` must surface a `TurnCancelled` body as the terminal
    /// marker so the executor can fold it into a
    /// `NormalizedEvent::TurnInterrupted`.
    #[test]
    fn stream_terminates_on_turn_cancelled() {
        let (process, _fake) = fake_process(vec![
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"turn_cancelled"}}}"#,
        ]);
        let session = sample_session(process);
        let events = block_on(AcpClient::new().stream(&session)).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].body, AcpUpdateBody::TurnCancelled));
    }

    /// `stream` must fail closed when the underlying stream ends
    /// without a terminal marker.
    #[test]
    fn stream_fails_closed_when_stream_ends_unexpectedly() {
        let (process, _fake) = fake_process(vec![
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"agent_message_chunk","text":"hi"}}}"#,
        ]);
        let session = sample_session(process);
        let err = block_on(AcpClient::new().stream(&session)).unwrap_err();
        assert_eq!(err.code, "acp_client_stream_ended");
    }

    /// `cancel` must write a `session/cancel` envelope and read the
    /// server's reply.
    #[test]
    fn cancel_writes_session_cancel_envelope() {
        let (process, fake) =
            fake_process(vec![r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#]);
        let session = sample_session(process);
        let codec = AcpCodec::new();
        block_on(AcpClient::new().cancel(&session, &codec)).unwrap();
        let written = fake.written_lines();
        let parsed: Value = serde_json::from_str(&written[0]).unwrap();
        assert_eq!(parsed["method"], "session/cancel");
        assert_eq!(parsed["params"]["sessionId"], "v-1");
    }

    /// The streaming loop must reject non-update notifications
    /// because the transport should never see them here.
    #[test]
    fn stream_rejects_unexpected_notification() {
        let (process, _fake) = fake_process(vec![
            r#"{"jsonrpc":"2.0","method":"session/request_permission","params":{"foo":"bar"}}"#,
        ]);
        let session = sample_session(process);
        let err = block_on(AcpClient::new().stream(&session)).unwrap_err();
        assert_eq!(err.code, "acp_client_unexpected_notification");
    }

    /// An unknown execution event must surface as
    /// `AcpUpdateBody::Unknown` so the executor's normalizer can
    /// refuse to forward it to any executor.
    #[test]
    fn unknown_execution_event_parses_as_unknown() {
        let (process, _fake) = fake_process(vec![
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"exec_shell","command":"rm -rf /"}}}"#,
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"v-1","update":{"type":"turn_completed"}}}"#,
        ]);
        let session = sample_session(process);
        let events = block_on(AcpClient::new().stream(&session)).unwrap();
        assert!(matches!(events[0].body, AcpUpdateBody::Unknown));
    }
}

#[cfg(test)]
mod block_on {
    pub fn run<F: std::future::Future>(future: F) -> F::Output {
        use std::sync::{Arc, Condvar, Mutex};
        use std::task::{Context, Poll, Wake, Waker};

        struct Park {
            flag: Mutex<bool>,
            cv: Condvar,
        }
        impl Wake for Park {
            fn wake(self: Arc<Self>) {
                *self.flag.lock().unwrap() = true;
                self.cv.notify_all();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.clone().wake();
            }
        }

        let park = Arc::new(Park {
            flag: Mutex::new(false),
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
}

#[cfg(test)]
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    block_on::run(future)
}
