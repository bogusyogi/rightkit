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
/// instead of inventing a parallel schema.
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

/// Pure line-by-line JSONL event producer. For every non-blank line the parser
/// invokes `on_event(line_no, &event)` in source order. Returns the count of
/// non-blank events consumed on clean EOF, or `Err(InputParseError)` on the
/// FIRST malformed line — the loop short-circuits so a downstream operator
/// gets a single, correct line number. Blank lines are skipped silently.
pub fn drive_jsonl_script<R, F>(mut reader: R, mut on_event: F) -> Result<usize, InputParseError>
where
    R: BufRead,
    F: FnMut(usize, &RunInputEvent),
{
    let mut line_no: usize = 0;
    let mut non_blank: usize = 0;
    let mut buf = String::new();
    loop {
        line_no += 1;
        buf.clear();
        let n = match reader.read_line(&mut buf) {
            Ok(n) => n,
            Err(e) => {
                return Err(InputParseError {
                    line: line_no,
                    message: format!("I/O read failed: {e}"),
                    raw: buf.trim().to_string(),
                });
            }
        };
        if n == 0 {
            return Ok(non_blank);
        }
        let trimmed = buf.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<RunInputEvent>(trimmed) {
            Ok(ev) => {
                on_event(line_no, &ev);
                non_blank += 1;
            }
            Err(e) => {
                return Err(InputParseError {
                    line: line_no,
                    message: e.to_string(),
                    raw: trimmed.to_string(),
                });
            }
        }
    }
}

/// Drain every JSONL line from `reader` into an in-memory ordering-preserving
/// vector (1-based physical line per entry). This is the testable, pure
/// version of the bind point between stream-json input and the engine.
pub fn drain_jsonl_script<R: BufRead>(
    reader: R,
) -> Result<Vec<(usize, RunInputEvent)>, InputParseError> {
    let mut events = Vec::new();
    drive_jsonl_script(reader, |line, ev| events.push((line, ev.clone())))?;
    Ok(events)
}

/// Pick the FIRST [`RunInputEvent::UserMessage`] in emission order. Returns
/// its 1-based physical line number and the user text that will seed the
/// engine's first turn.
pub fn pick_initial_prompt(events: &[(usize, RunInputEvent)]) -> Option<(usize, String)> {
    for (line, ev) in events {
        if let RunInputEvent::UserMessage { text, .. } = ev {
            return Some((*line, text.clone()));
        }
    }
    None
}

/// First [`RunInputEvent::Control`] with `action=abort` in emission order —
/// the run layer maps this onto a typed `UserAbort` result after the engine
/// has finished its current iteration budget.
pub fn detect_abort(events: &[(usize, RunInputEvent)]) -> Option<(usize, Option<String>)> {
    for (line, ev) in events {
        if let RunInputEvent::Control {
            action: ControlAction::Abort,
            reason,
        } = ev
        {
            return Some((*line, reason.clone()));
        }
    }
    None
}

/// Per-script summary the run layer logs to stderr after the engine has
/// returned, so operators can audit that downstream events were observed
/// even though only the first `user_message` actually drives the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptSummary {
    pub event_count: usize,
    pub initial_prompt_line: usize,
    pub abort_line: Option<usize>,
    pub trailing_event_count: usize,
}

/// Decompose a drained script into the parts the engine bin consumes:
/// (initial_prompt_text, summary_record). Returns `None` when the script
/// contains no `user_message` event (treated as a Usage error upstream).
pub fn summarize_script(events: &[(usize, RunInputEvent)]) -> Option<(String, ScriptSummary)> {
    let (init_line, prompt) = pick_initial_prompt(events)?;
    let abort_line = detect_abort(events).map(|(line, _)| line);
    let trailing = events
        .iter()
        .filter(|(other_line, _)| *other_line != init_line)
        .count();
    Some((
        prompt,
        ScriptSummary {
            event_count: events.len(),
            initial_prompt_line: init_line,
            abort_line,
            trailing_event_count: trailing,
        },
    ))
}
