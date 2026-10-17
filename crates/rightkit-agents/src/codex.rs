//! Codex `app-server` JSON-RPC client over stdio (newline-delimited JSON).
//!
//! Lifted and adapted from CodeRight (`engine/crates/driver_host/src/codex.rs` and
//! `cli_runtime/transports/codex_app_server.rs`). Kept: the response / server-request /
//! notification split of an incoming line, the set of server requests treated as approvals,
//! `thread/start` and `turn/start` request shapes. Dropped: CodeRight's `NormalizedEvent`
//! and its async process transport. Unknown notifications surface as [`AgentEvent::Unknown`]
//! under [`UnknownPolicy::Lenient`]; under [`UnknownPolicy::FailClosed`] an unknown
//! notification in a required family is refused ([`parse_line_with`]).
//!
//! The wire is `[experimental]` in the Codex CLI. The checked-in schema snapshot at
//! `tests/fixtures/codex/schema-0.161.0.json` pins the method sets this client was written
//! against; a drift test compares it to [`KNOWN_NOTIFICATIONS`] and the other lists.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::event::{AgentEvent, ParseError, RateLimitInfo, UnknownPolicy, UsageCounts};

/// Notification families whose unknown members carry execution truth. Under
/// [`UnknownPolicy::FailClosed`] an unknown method in one of these is refused.
const REQUIRED_NOTIFICATION_PREFIXES: &[&str] = &["turn/", "item/", "serverRequest/"];

/// Whether an unknown notification named `method` must be refused under
/// [`UnknownPolicy::FailClosed`].
pub fn is_required_notification(method: &str) -> bool {
    method == "error"
        || REQUIRED_NOTIFICATION_PREFIXES
            .iter()
            .any(|prefix| method.starts_with(prefix))
}

/// Codex CLI version whose schema this client is written against.
pub const PINNED_CODEX_VERSION: &str = "0.161.0";

/// Server requests that ask the host to approve something, answered by policy.
pub const APPROVAL_REQUESTS: &[&str] = &[
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
];

/// Notifications this client maps to events. Others become [`AgentEvent::Unknown`] unless
/// listed as bookkeeping.
pub const KNOWN_NOTIFICATIONS: &[&str] = &[
    "thread/started",
    "turn/started",
    "turn/completed",
    "item/started",
    "item/completed",
    "item/agentMessage/delta",
    "thread/tokenUsage/updated",
    "thread/compacted",
    "serverRequest/resolved",
];

/// Presentation-only notifications that carry no execution truth.
const BOOKKEEPING_NOTIFICATIONS: &[&str] = &[
    "item/reasoning/summaryTextDelta",
    "item/reasoning/summaryPartAdded",
    "item/reasoning/textDelta",
    "item/commandExecution/outputDelta",
    "item/fileChange/outputDelta",
    "item/plan/delta",
    "turn/plan/updated",
    "turn/diff/updated",
    "account/updated",
    "account/rateLimits/updated",
    "configWarning",
    "warning",
    "thread/name/updated",
    "thread/status/changed",
    "mcpServer/startupStatus/updated",
];

/// One incoming line, classified.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<Value>,
    },
    /// A request from the server. Approval requests also carry the derived event.
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
        event: Option<AgentEvent>,
    },
    Notification {
        method: String,
        events: Vec<AgentEvent>,
    },
}

/// Parse one JSON-RPC line.
pub fn parse_line(line: &str) -> Result<Incoming, ParseError> {
    let value: Value = serde_json::from_str(line).map_err(|error| ParseError(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| ParseError("root must be an object".into()))?;
    let id = object.get("id").cloned();
    let method = object.get("method").and_then(Value::as_str);
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    match (id, method) {
        (Some(id), None) => Ok(Incoming::Response {
            id,
            result: object.get("result").cloned(),
            error: object.get("error").cloned(),
        }),
        (Some(id), Some(method)) => {
            let event = APPROVAL_REQUESTS
                .contains(&method)
                .then(|| approval_event(&id, method, &params));
            Ok(Incoming::ServerRequest {
                id,
                method: method.to_string(),
                params,
                event,
            })
        }
        (None, Some(method)) => Ok(Incoming::Notification {
            method: method.to_string(),
            events: notification_events(method, &params, &value),
        }),
        (None, None) => Err(ParseError("missing id and method".into())),
    }
}

/// [`parse_line`] under a policy. Under [`UnknownPolicy::FailClosed`] an unknown notification
/// in a required family is refused with [`ParseError::unknown_required`]; the method is
/// readable through [`ParseError::unknown_required_method`].
pub fn parse_line_with(line: &str, policy: UnknownPolicy) -> Result<Incoming, ParseError> {
    let incoming = parse_line(line)?;
    if policy == UnknownPolicy::FailClosed {
        if let Incoming::Notification { method, events } = &incoming {
            let unknown = events
                .iter()
                .any(|event| matches!(event, AgentEvent::Unknown { .. }));
            if unknown && is_required_notification(method) {
                return Err(ParseError::unknown_required(method));
            }
        }
    }
    Ok(incoming)
}

fn id_string(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn approval_event(id: &Value, method: &str, params: &Value) -> AgentEvent {
    AgentEvent::ApprovalRequested {
        id: id_string(id),
        tool: method.to_string(),
        input: params.clone(),
    }
}

fn text_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    current.as_str().map(str::to_string)
}

fn notification_events(method: &str, params: &Value, raw: &Value) -> Vec<AgentEvent> {
    match method {
        "thread/started" => vec![AgentEvent::Init {
            session_id: text_at(params, &["thread", "id"]).unwrap_or_default(),
            model: text_at(params, &["thread", "model"]),
            tools: Vec::new(),
            permission_mode: None,
        }],
        "item/agentMessage/delta" => vec![AgentEvent::TextDelta {
            text: text_at(params, &["delta"]).unwrap_or_default(),
        }],
        "item/started" => item_events(params.get("item"), true),
        "item/completed" => item_events(params.get("item"), false),
        "thread/compacted" => vec![AgentEvent::Compacted],
        "thread/tokenUsage/updated" => {
            let total = params
                .get("tokenUsage")
                .and_then(|usage| usage.get("total"))
                .or_else(|| params.get("tokenUsage"));
            total
                .map(|total| {
                    let get = |key: &str| total.get(key).and_then(Value::as_u64).unwrap_or(0);
                    AgentEvent::Usage {
                        usage: UsageCounts {
                            input: get("inputTokens"),
                            output: get("outputTokens"),
                            cache_read: get("cachedInputTokens"),
                            cache_write: get("cacheWriteInputTokens"),
                        },
                    }
                })
                .into_iter()
                .collect()
        }
        "serverRequest/resolved" => vec![AgentEvent::ApprovalResolved {
            id: params.get("requestId").map(id_string).unwrap_or_default(),
        }],
        "turn/completed" => {
            let turn = params.get("turn");
            let status = turn
                .and_then(|t| t.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let mut events = vec![AgentEvent::Result {
                cost_usd: None,
                duration_ms: turn
                    .and_then(|t| t.get("durationMs"))
                    .and_then(Value::as_u64),
                num_turns: Some(1),
                permission_denials: Vec::new(),
                is_error: status == "failed",
                subtype: status.to_string(),
            }];
            // A usage-limit failure is the ordinary turn failure plus a rejected rate limit.
            events.extend(
                turn.and_then(|t| t.get("error"))
                    .and_then(usage_limit)
                    .map(AgentEvent::RateLimit),
            );
            events
        }
        "turn/failed" => {
            // `turn/failed` ends the turn: the terminal failure is always emitted, and a usage
            // limit adds the rate-limit metadata beside it.
            let carrier = params.get("error").unwrap_or(params);
            let mut events = vec![AgentEvent::Result {
                cost_usd: None,
                duration_ms: params
                    .get("turn")
                    .and_then(|t| t.get("durationMs"))
                    .and_then(Value::as_u64),
                num_turns: Some(1),
                permission_denials: Vec::new(),
                is_error: true,
                subtype: "failed".to_string(),
            }];
            events.extend(usage_limit(carrier).map(AgentEvent::RateLimit));
            events
        }
        "error" => {
            let carrier = params.get("error").unwrap_or(params);
            match usage_limit(carrier) {
                Some(info) => vec![AgentEvent::RateLimit(info)],
                None => vec![AgentEvent::Unknown { raw: raw.clone() }],
            }
        }
        "turn/started" => Vec::new(),
        other if BOOKKEEPING_NOTIFICATIONS.contains(&other) => Vec::new(),
        _ => vec![AgentEvent::Unknown { raw: raw.clone() }],
    }
}

/// A usage-limit rejection carried by a Codex error object: `codexErrorInfo` (or `info`) names
/// `usageLimitExceeded`, or the message says "usage limit". Unverified: those key names and
/// the `resetsAt` / `resets_at` placement follow the CodeRight port, not a live run.
fn usage_limit(carrier: &Value) -> Option<RateLimitInfo> {
    let info = carrier
        .get("codexErrorInfo")
        .or_else(|| carrier.get("info"));
    let keyed = match info {
        Some(Value::String(name)) => name.as_str() == "usageLimitExceeded",
        Some(Value::Object(object)) => object.contains_key("usageLimitExceeded"),
        _ => false,
    };
    let worded = carrier
        .get("message")
        .and_then(Value::as_str)
        .is_some_and(|message| message.to_ascii_lowercase().contains("usage limit"));
    if !keyed && !worded {
        return None;
    }
    Some(RateLimitInfo {
        status: "rejected".to_string(),
        resets_at: carrier
            .get("resetsAt")
            .or_else(|| carrier.get("resets_at"))
            .and_then(Value::as_u64),
        ..RateLimitInfo::default()
    })
}

/// Environment entries for a Codex launch, to add to `SessionConfig::env`. `CODEX_HOME`
/// scopes the CLI's state directory. An account-scoped launch must also name the BYOK
/// variables to remove: call `SessionConfig::scrub_byok_env` (see `session::BYOK_ENV_VARS`).
/// Inherited credentials are not removed by this function.
pub fn launch_env(codex_home: &std::path::Path) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    vec![(
        std::ffi::OsString::from("CODEX_HOME"),
        codex_home.as_os_str().to_os_string(),
    )]
}

fn item_events(item: Option<&Value>, started: bool) -> Vec<AgentEvent> {
    let Some(item) = item else {
        return Vec::new();
    };
    let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
    let id = text_at(item, &["id"]).unwrap_or_default();
    let is_error = item
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|status| status == "failed" || status == "declined");
    match kind {
        "agentMessage" if !started => vec![AgentEvent::Text {
            text: text_at(item, &["text"]).unwrap_or_default(),
            parent_tool_use_id: None,
        }],
        "reasoning" if !started => vec![AgentEvent::Thinking {
            text: item
                .get("summary")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
            parent_tool_use_id: None,
        }],
        "contextCompaction" if !started => vec![AgentEvent::Compacted],
        "commandExecution" | "fileChange" | "mcpToolCall" | "dynamicToolCall" => {
            if started {
                vec![AgentEvent::ToolUse {
                    id,
                    name: kind.to_string(),
                    input: item.clone(),
                    parent_tool_use_id: None,
                }]
            } else {
                vec![AgentEvent::ToolResult {
                    id,
                    is_error,
                    parent_tool_use_id: None,
                }]
            }
        }
        "collabAgentToolCall" => {
            if started {
                vec![
                    AgentEvent::SubagentStarted {
                        id: id.clone(),
                        description: text_at(item, &["prompt"]),
                        parent_tool_use_id: None,
                    },
                    AgentEvent::ToolUse {
                        id,
                        name: kind.to_string(),
                        input: item.clone(),
                        parent_tool_use_id: None,
                    },
                ]
            } else {
                vec![
                    AgentEvent::ToolResult {
                        id: id.clone(),
                        is_error,
                        parent_tool_use_id: None,
                    },
                    AgentEvent::SubagentFinished { id, is_error },
                ]
            }
        }
        _ => Vec::new(),
    }
}

/// Approval decisions the client may send back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

impl Decision {
    fn wire(self) -> &'static str {
        match self {
            Decision::Accept => "accept",
            Decision::AcceptForSession => "acceptForSession",
            Decision::Decline => "decline",
            Decision::Cancel => "cancel",
        }
    }
}

/// Newline-terminated JSON-RPC response answering an approval server request. `id` is the
/// request's wire id exactly as received.
pub fn approval_response_frame(id: &Value, decision: Decision) -> String {
    let mut line =
        json!({"jsonrpc": "2.0", "id": id, "result": {"decision": decision.wire()}}).to_string();
    line.push('\n');
    line
}

/// Newline-terminated JSON-RPC error response, used to refuse a server request this client
/// does not implement so the server is never left waiting.
pub fn error_response_frame(id: &Value, code: i64, message: &str) -> String {
    let mut line = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        .to_string();
    line.push('\n');
    line
}

/// Inputs for a Codex session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CodexOptions {
    pub model: Option<String>,
    /// Defaults to `on-request` so approvals reach the host.
    pub approval_policy: Option<String>,
    pub sandbox: Option<String>,
    /// Resume this thread (`thread/resume`) instead of starting a new one.
    pub resume_thread_id: Option<String>,
    /// Extra `thread/start` / `thread/resume` params merged over the ones built here.
    pub thread_params: Option<Value>,
}

/// Argv (without the program) for the stdio app-server.
pub fn build_args() -> Vec<String> {
    vec!["app-server".to_string()]
}

/// `thread/start` or `thread/resume` params for `options` and an optional working directory.
pub fn thread_params(options: &CodexOptions, cwd: Option<&str>) -> Value {
    let mut params = serde_json::Map::new();
    if let Some(model) = &options.model {
        params.insert("model".into(), json!(model));
    }
    if let Some(cwd) = cwd {
        params.insert("cwd".into(), json!(cwd));
    }
    params.insert(
        "approvalPolicy".into(),
        json!(options.approval_policy.as_deref().unwrap_or("on-request")),
    );
    if let Some(sandbox) = &options.sandbox {
        params.insert("sandbox".into(), json!(sandbox));
    }
    if let Some(thread) = &options.resume_thread_id {
        params.insert("threadId".into(), json!(thread));
    }
    if let Some(Value::Object(extra)) = &options.thread_params {
        for (key, value) in extra {
            params.insert(key.clone(), value.clone());
        }
    }
    Value::Object(params)
}

/// JSON-RPC client over a reader and a writer (the child's stdout and stdin).
///
/// Blocking and single-threaded: `call` writes one request and reads lines until its
/// response arrives, buffering everything else for [`CodexClient::drain_pending`].
pub struct CodexClient<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    next_id: u64,
    pending: Vec<Incoming>,
    policy: UnknownPolicy,
}

/// Failure of a blocking client call.
#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    Parse(ParseError),
    /// The server answered with a JSON-RPC error object.
    Rpc(Value),
    /// stdout closed before the response arrived.
    Closed,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(error) => write!(f, "io: {error}"),
            ClientError::Parse(error) => write!(f, "parse: {error}"),
            ClientError::Rpc(value) => write!(f, "rpc error: {value}"),
            ClientError::Closed => f.write_str("app-server closed stdout before the response"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(error: std::io::Error) -> Self {
        ClientError::Io(error)
    }
}

impl<R: BufRead, W: Write> CodexClient<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            pending: Vec::new(),
            policy: UnknownPolicy::Lenient,
        }
    }

    /// The client with an unknown-message policy. A refused line makes `call` or
    /// `next_incoming` return [`ClientError::Parse`] carrying
    /// [`ParseError::unknown_required_method`].
    pub fn with_policy(mut self, policy: UnknownPolicy) -> Self {
        self.policy = policy;
        self
    }

    fn send(&mut self, value: &Value) -> Result<(), ClientError> {
        let mut line = value.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send a request and block until its response; other lines are buffered.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(ClientError::Closed);
            }
            if line.trim().is_empty() {
                continue;
            }
            match parse_line_with(line.trim_end(), self.policy).map_err(ClientError::Parse)? {
                Incoming::Response {
                    id: response_id,
                    result,
                    error,
                } if response_id == json!(id) => {
                    if let Some(error) = error {
                        return Err(ClientError::Rpc(error));
                    }
                    return Ok(result.unwrap_or(Value::Null));
                }
                other => self.pending.push(other),
            }
        }
    }

    /// Read the next incoming line (buffered ones first). `None` at end of stream.
    pub fn next_incoming(&mut self) -> Result<Option<Incoming>, ClientError> {
        if !self.pending.is_empty() {
            return Ok(Some(self.pending.remove(0)));
        }
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            return parse_line_with(line.trim_end(), self.policy)
                .map(Some)
                .map_err(ClientError::Parse);
        }
    }

    /// Take everything buffered while waiting on responses.
    pub fn drain_pending(&mut self) -> Vec<Incoming> {
        std::mem::take(&mut self.pending)
    }

    /// `initialize` then the `initialized` notification.
    pub fn initialize(
        &mut self,
        client_name: &str,
        client_version: &str,
    ) -> Result<Value, ClientError> {
        let result = self.call(
            "initialize",
            json!({"clientInfo": {"name": client_name, "version": client_version}}),
        )?;
        self.send(&json!({"jsonrpc": "2.0", "method": "initialized"}))?;
        Ok(result)
    }

    /// `thread/start`; returns the thread id.
    pub fn thread_start(&mut self, params: Value) -> Result<String, ClientError> {
        let result = self.call("thread/start", params)?;
        Ok(thread_id(&result))
    }

    /// `thread/resume`; returns the thread id.
    pub fn thread_resume(
        &mut self,
        thread_id_value: &str,
        params: Value,
    ) -> Result<String, ClientError> {
        let mut params = params;
        if let Some(object) = params.as_object_mut() {
            object.insert("threadId".into(), json!(thread_id_value));
        } else {
            params = json!({"threadId": thread_id_value});
        }
        let result = self.call("thread/resume", params)?;
        Ok(thread_id(&result))
    }

    /// `turn/start` with one text input; returns the turn id when the response carries one.
    pub fn turn_start(&mut self, thread: &str, text: &str) -> Result<Option<String>, ClientError> {
        let result = self.call(
            "turn/start",
            json!({"threadId": thread, "input": [{"type": "text", "text": text}]}),
        )?;
        Ok(text_at(&result, &["turn", "id"]).or_else(|| text_at(&result, &["turnId"])))
    }

    /// `turn/interrupt`.
    pub fn turn_interrupt(&mut self, thread: &str, turn: &str) -> Result<(), ClientError> {
        self.call(
            "turn/interrupt",
            json!({"threadId": thread, "turnId": turn}),
        )
        .map(|_| ())
    }

    /// Answer a server approval request.
    pub fn respond_approval(&mut self, id: &Value, decision: Decision) -> Result<(), ClientError> {
        let frame = approval_response_frame(id, decision);
        self.writer.write_all(frame.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }
}

fn thread_id(result: &Value) -> String {
    text_at(result, &["thread", "id"])
        .or_else(|| text_at(result, &["threadId"]))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_failed(error: Value) -> String {
        json!({
            "jsonrpc": "2.0",
            "method": "turn/failed",
            "params": {"turn": {"id": "t1", "durationMs": 900}, "error": error}
        })
        .to_string()
    }

    fn notification_events(line: &str) -> Vec<AgentEvent> {
        match parse_line(line) {
            Ok(Incoming::Notification { events, .. }) => events,
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    #[test]
    fn usage_limit_turn_failure_is_terminal_and_rate_limited() {
        let events = notification_events(&turn_failed(json!({
            "message": "You hit your usage limit",
            "codexErrorInfo": "usageLimitExceeded",
            "resetsAt": 1700000000u64
        })));
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            AgentEvent::Result { is_error: true, subtype, duration_ms: Some(900), .. }
                if subtype == "failed"
        ));
        assert!(matches!(
            &events[1],
            AgentEvent::RateLimit(info) if info.status == "rejected" && info.resets_at == Some(1700000000)
        ));
    }

    #[test]
    fn plain_turn_failure_is_a_terminal_failure_without_rate_limit() {
        let events = notification_events(&turn_failed(json!({"message": "model overloaded"})));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AgentEvent::Result { is_error: true, subtype, .. } if subtype == "failed"
        ));
    }

    #[test]
    fn fail_closed_refuses_unknown_required_notifications_only() {
        let unknown_turn = json!({"jsonrpc": "2.0", "method": "turn/teleported", "params": {}});
        let refused = parse_line_with(&unknown_turn.to_string(), UnknownPolicy::FailClosed);
        assert_eq!(
            refused.unwrap_err().unknown_required_method(),
            Some("turn/teleported")
        );
        assert!(parse_line_with(&unknown_turn.to_string(), UnknownPolicy::Lenient).is_ok());

        let unknown_other = json!({"jsonrpc": "2.0", "method": "x/unsupported", "params": {}});
        assert!(parse_line_with(&unknown_other.to_string(), UnknownPolicy::FailClosed).is_ok());

        let bookkeeping =
            json!({"jsonrpc": "2.0", "method": "item/reasoning/textDelta", "params": {}});
        assert!(parse_line_with(&bookkeeping.to_string(), UnknownPolicy::FailClosed).is_ok());
    }

    #[test]
    fn fail_closed_client_surfaces_the_typed_refusal() {
        let wire = format!(
            "{}\n",
            json!({"jsonrpc": "2.0", "method": "turn/teleported", "params": {}})
        );
        let mut client = CodexClient::new(std::io::Cursor::new(wire.into_bytes()), Vec::new())
            .with_policy(UnknownPolicy::FailClosed);
        match client.next_incoming() {
            Err(ClientError::Parse(error)) => {
                assert_eq!(error.unknown_required_method(), Some("turn/teleported"))
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
