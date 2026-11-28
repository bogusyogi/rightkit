use clap::ValueEnum;
use rightkit_agent_engine_protocol::TrustEnvelope;
use serde::{Deserialize, Serialize};

/// CLI flag selector for the prompt input channel used by `{app} run`.
/// `text` (default) keeps the existing positional-prompt behavior untouched;
/// `stream-json` reads JSONL events from stdin and drives the run from them
/// (FEATURE-MATRIX par §#32 + #35 — inbound complement to `--stream-json`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub enum InputFormat {
    #[default]
    Text,
    StreamJson,
}

/// Verb for [`RunInputEvent::Control`]. Today only `abort` is honored at the
/// run layer; `ping` is reserved for future liveness/heartbeat use and
/// round-trips cleanly so scripts written today keep working tomorrow.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlAction {
    Abort,
    Ping,
}

/// Inbound JSONL event schema for `{app} run --input-format stream-json`.
///
/// The discriminator is `"type"` (per the FEATURE-MATRIX spec) NOT the
/// outbound wire's `"$kind"`. Field names mirror the OUTBOUND stream-json
/// vocabulary — `user_message` reuses `text`/`nonce`/`trust` from
/// `ProtocolEvent::UserMessageSubmitted`; `tool_result` mirrors the body of
/// `TranscriptMessage::Tool`. We deliberately reuse the EXISTING concepts
/// instead of inventing a parallel schema. The variants are `user_message`, `tool_result`,
/// `control` and `approval_response`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RunInputEvent {
    /// A user/operator turn submitted to the session. The first `user_message`
    /// event in input order seeds the engine's initial prompt; later events
    /// are accounted for in the post-run summary.
    #[serde(rename = "user_message")]
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trust: Option<TrustEnvelope>,
    },
    /// A synthetic tool result. Today the schema validates and the event is
    /// counted in the post-run summary; injecting it into the engine requires
    /// a future `submit_tool_result` API on the engine crate (out of scope
    /// here — primary rule is ADD, never expand engine surface area
    /// incidentally).
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_call_id: String,
        summary: String,
        #[serde(default)]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process_id: Option<String>,
    },
    /// Operator-side control event. `action=abort` surfaces as a typed
    /// hint in the post-run summary today; `action=ping` is a no-op liveness
    /// marker kept for forward compatibility.
    #[serde(rename = "control")]
    Control {
        action: ControlAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Answers a `toolApprovalRequested` event that the run raised under `--approval-callback`.
    /// Counted and refused otherwise: a response for no open approval is ignored.
    #[serde(rename = "approval_response")]
    ApprovalResponse {
        tool_call_id: String,
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// One malformed-line JSONL parse error. `line` is the 1-based physical line
/// number where the failure occurred (blank lines included so operators can
/// jump to the bad row in their script verbatim). `raw` is the trimmed line
/// that failed to deserialize, kept so downstream error envelopes include the
/// offending payload verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputParseError {
    pub line: usize,
    pub message: String,
    pub raw: String,
}

/// Maximum accepted physical JSONL line. Keeping this bound at reader edge
/// prevents an unterminated line from growing input memory without limit.
pub const MAX_JSONL_LINE_BYTES: usize = 1024 * 1024;

/// Events become available as each newline arrives; EOF closes channel after
/// already parsed events have been delivered.
pub type JsonlInputReceiver =
    tokio::sync::mpsc::Receiver<Result<(usize, RunInputEvent), InputParseError>>;

/// Start bounded live JSONL input. Producer owns reader on blocking thread;
/// consumer can admit first turn before stdin reaches EOF.
pub fn spawn_jsonl_reader<R>(reader: R, capacity: usize) -> JsonlInputReceiver
where
    R: BufRead + Send + 'static,
{
    let (sender, receiver) = tokio::sync::mpsc::channel(capacity.max(1));
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut line_no = 0usize;
        loop {
            line_no += 1;
            let mut buf = String::new();
            let read = match read_bounded_line(&mut reader, &mut buf) {
                Ok(read) => read,
                Err(error) => {
                    let _ = sender.blocking_send(Err(InputParseError {
                        line: line_no,
                        message: error,
                        raw: buf.trim().to_string(),
                    }));
                    return;
                }
            };
            if read == 0 {
                return;
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let event = match serde_json::from_str::<RunInputEvent>(trimmed) {
                Ok(event) => event,
                Err(error) => {
                    let _ = sender.blocking_send(Err(InputParseError {
                        line: line_no,
                        message: error.to_string(),
                        raw: trimmed.to_string(),
                    }));
                    return;
                }
            };
            if sender.blocking_send(Ok((line_no, event))).is_err() {
                return;
            }
        }
    });
    receiver
}

/// Read one physical line without allowing its allocation to exceed protocol
/// bound, while preserving normal line semantics for existing parser.
fn read_bounded_line<R: BufRead>(reader: &mut R, output: &mut String) -> Result<usize, String> {
    let mut total = 0usize;
    let mut bytes = Vec::new();
    loop {
        let chunk = reader
            .fill_buf()
            .map_err(|error| format!("I/O read failed: {error}"))?;
        if chunk.is_empty() {
            *output = String::from_utf8(bytes)
                .map_err(|_| "JSONL input is not valid UTF-8".to_string())?;
            return Ok(total);
        }
        let newline = chunk.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(chunk.len(), |index| index + 1);
        if total.saturating_add(take) > MAX_JSONL_LINE_BYTES {
            return Err(format!("JSONL line exceeds {} bytes", MAX_JSONL_LINE_BYTES));
        }
        bytes.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        total += take;
        if newline.is_some() {
            *output = String::from_utf8(bytes)
                .map_err(|_| "JSONL input is not valid UTF-8".to_string())?;
            return Ok(total);
        }
    }
}

impl std::fmt::Display for InputParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "malformed JSONL on stdin line {}: {} (raw: {})",
            self.line, self.message, self.raw
        )
    }
}

impl std::error::Error for InputParseError {}

// NOTE: `BufRead` is intentionally NOT `use`'d here — it is found via the
// parent `main_runtime.rs`'s `use std::io::{BufRead, Read, Write};` per the
// include!-module rule that forbids re-importing parent symbols. Future
// contributors: do NOT add a `use std::io::BufRead;` line to this file.

#[cfg(test)]
mod approval_response_tests {
    use super::*;

    #[test]
    fn approval_response_round_trips() {
        let line = r#"{"type":"approval_response","tool_call_id":"call-1","approved":true,"remember":"session"}"#;
        let event = serde_json::from_str::<RunInputEvent>(line).expect("parses");
        assert_eq!(
            event,
            RunInputEvent::ApprovalResponse {
                tool_call_id: "call-1".to_string(),
                approved: true,
                remember: Some("session".to_string()),
                reason: None,
            }
        );
        let text = serde_json::to_string(&event).expect("serializes");
        assert!(
            !text.contains("reason"),
            "an absent reason is omitted: {text}"
        );
        let back = serde_json::from_str::<RunInputEvent>(&text).expect("round-trips");
        assert_eq!(back, event);
    }
}
