//! Hook runtime (C6) and the context pull it shares with `membrane context pull`.
//!
//! A hook always succeeds from the host's point of view: SessionStart and UserPromptSubmit
//! answer with `additionalContext` (at most 10,000 characters, final marker line included) in
//! the harness's schema, every other event answers with nothing, and an internal error is a
//! receipt state, never a failure. Every run leaves a per-session heartbeat; a run that returns
//! context is `emitted`, and records a durable receipt that `membrane receipts confirm` later
//! flips to `delivered` when the marker shows in the host's transcript. `run` is the one implementation: `membrane hook`
//! calls it in-process, and the resident calls it for op `hook` with its warm stores.

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Instant,
};

use rightkit_context_protocol::{
    normalize_hook_payload, HookInputEnvelopeV1, RemainingContextCeilingV1, HOOK_MODULE_DEADLINE_MS,
};
use serde_json::{json, Value};

use crate::receipt::{
    new_receipt_id, receipt_marker, write_context_receipt, write_hook_heartbeat, ContextReceipt,
    HookReceipt, HookReceiptState,
};

/// Longest `additionalContext` a hook delivers, in characters.
pub const ADDITIONAL_CONTEXT_CAP_CHARS: usize = 10_000;

/// Configured attention cap for ambient recall (`RIGHTKIT_CONTEXT_HOOK_RECALL_MAX_TOKENS` overrides it).
///
/// Packet reduction is fail-closed below its floor: representations shrink block text but never
/// drop evidence, so a multi-source orientation packet (Cortex memory, Blueprint candidates,
/// provenance) has a minimum viable size in the low thousands of tokens. A smaller cap could only
/// serve a Cortex-only packet and refused with `NoRepresentationFits` once Blueprint delivered.
/// `STARTUP_PACKET_MAX_BYTES` still bounds the injected startup text.
const DEFAULT_RECALL_MAX_TOKENS: u64 = 8_192;
const STARTUP_PACKET_MAX_BYTES: usize = 8 * 1024;
/// Time left for the pull after the hook's own start-up and reply.
const BUDGET_MARGIN_MS: u64 = 200;
/// Wall budget for an explicit `membrane context pull`, which no host timeout bounds.
const PULL_DEADLINE_MS: u64 = 10_000;

/// Schema of the one JSON document `membrane context pull --json` prints.
pub const PULL_DOCUMENT_SCHEMA: &str = "membrane.context.pull.v1";
/// The session a `context pull --wire coderight` receipt is filed under.
pub const PULL_RECEIPT_SESSION: &str = "coderight";

/// How an explicit pull hands its answer to its consumer. Anything not named `text` is the
/// strict JSON document: a missing or unknown mode never selects the smaller representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PullDelivery {
    /// `membrane context pull`: the context and a newline, or nothing when there is none.
    Text,
    /// `membrane context pull --json`: the whole `membrane.context.pull.v1` document.
    #[default]
    Json,
}

impl PullDelivery {
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("text") => Self::Text,
            _ => Self::Json,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

/// What an explicit pull's consumer receives, fixed before selection: the delivery mode and, for
/// `--wire` with `--request-id`, the validated receipt id the JSON document carries. Selection,
/// the resident answer and the CLI all project the same answer through this one value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullProjectionSpec {
    pub delivery: PullDelivery,
    pub receipt_id: Option<String>,
}

impl PullProjectionSpec {
    /// Read `{delivery?, receiptId?}` from a resident `pull` request. A missing or unknown
    /// delivery is the strict JSON projection; a malformed receipt id is a refusal.
    pub fn from_args(args: &Value) -> Result<Self, String> {
        let receipt_id = match args.get("receiptId") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if crate::receipt::valid_receipt_id(id) => Some(id.clone()),
            Some(_) => return Err("pull request carries an invalid receiptId".to_owned()),
        };
        Ok(Self {
            delivery: PullDelivery::parse(args.get("delivery").and_then(Value::as_str)),
            receipt_id,
        })
    }
}

/// The caller's declared response budget in tokens: the requested value when positive, else the
/// configured default. The one place both the resident answer and the CLI's final check read it.
pub fn effective_pull_budget(requested: Option<u64>) -> u64 {
    requested
        .filter(|budget| *budget > 0)
        .unwrap_or(DEFAULT_RECALL_MAX_TOKENS)
}

/// Which harness a hook answers. Both accept the same two keys for the events that carry
/// context, and both reject unknown keys, so nothing else is ever emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    Claude,
    Codex,
}

impl Wire {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// What one hook run produced: its receipt, and the JSON for the host's stdout when there is any.
#[derive(Clone, Debug, PartialEq)]
pub struct HookOutcome {
    pub receipt: HookReceipt,
    pub output: Option<Value>,
}

/// The events whose schema takes `additionalContext`.
pub fn emits_context(event: &str) -> bool {
    matches!(event, "SessionStart" | "UserPromptSubmit")
}

/// The host-schema reply carrying `context`, or `None` when the event takes none or there is none.
pub fn host_output(wire: Wire, event: &str, context: &str) -> Option<Value> {
    if !emits_context(event) || context.is_empty() {
        return None;
    }
    match wire {
        Wire::Claude | Wire::Codex => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": context,
            }
        })),
    }
}

fn string_field(payload: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        payload
            .get(*name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn receipt(
    wire: Wire,
    event: &str,
    state: HookReceiptState,
    reason: &str,
    chars: usize,
) -> HookReceipt {
    HookReceipt {
        state,
        reason: reason.to_owned(),
        event: event.to_owned(),
        wire: wire.as_str().to_owned(),
        chars,
        id: String::new(),
    }
}

/// Run one hook event. Never fails: every error is a receipt state.
pub fn run(wire: Wire, event: &str, mut payload: Value) -> HookOutcome {
    let started = Instant::now();
    let session = string_field(&payload, &["thread_id", "session_id", "sessionId"]);
    if let Some(session) = &session {
        let _ = write_hook_heartbeat(&crate::cli::state_root(), session, event);
    }
    let done = |state, reason: &str, output| HookOutcome {
        receipt: receipt(wire, event, state, reason, 0),
        output,
    };
    if !emits_context(event) {
        return done(HookReceiptState::Empty, "event_not_applicable", None);
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "hook_event_name".to_owned(),
            Value::String(event.to_owned()),
        );
    }
    let input = match normalize_hook_payload(payload) {
        Ok(input) => input,
        Err(_) => return done(HookReceiptState::Refused, "invalid_hook_payload", None),
    };
    let request = match recall_request(wire, &input, event) {
        Ok(request) => request,
        Err(reason) => return done(HookReceiptState::Refused, &reason, None),
    };
    let budget_ms = HOOK_MODULE_DEADLINE_MS
        .saturating_sub(started.elapsed().as_millis() as u64)
        .saturating_sub(BUDGET_MARGIN_MS)
        .max(10);
    match recall(request, budget_ms, None) {
        Ok(outcome) => {
            let context = outcome.context.filter(|context| !context.trim().is_empty());
            match context {
                Some(context) => {
                    let id = new_receipt_id();
                    let marker = receipt_marker(&id);
                    // The marker is the context's last line and counts against the cap.
                    let room = ADDITIONAL_CONTEXT_CAP_CHARS - marker.chars().count() - 1;
                    let context = format!("{}\n{marker}", cap_context(context, room));
                    let _ = write_context_receipt(
                        &crate::cli::state_root(),
                        &ContextReceipt::emitted(
                            &id,
                            session.as_deref().unwrap_or("host-native"),
                            wire.as_str(),
                            event,
                        ),
                    );
                    let mut receipt = receipt(
                        wire,
                        event,
                        HookReceiptState::Emitted,
                        outcome.reason,
                        context.chars().count(),
                    );
                    receipt.id = id;
                    HookOutcome {
                        receipt,
                        output: host_output(wire, event, &context),
                    }
                }
                None => done(HookReceiptState::Empty, outcome.reason, None),
            }
        }
        Err(RecallFailure::Refused(reason)) => done(HookReceiptState::Refused, &reason, None),
        Err(RecallFailure::Off(reason)) => done(HookReceiptState::Off, &reason, None),
        Err(RecallFailure::Failed(reason)) => done(HookReceiptState::Unknown, &reason, None),
    }
}

/// Resident op `hook`: `{wire, event, payload}` answers `{state, reason, chars, output}`, where
/// `output` is the host's reply or null. A malformed request is itself a refused receipt: a hook
/// never fails.
pub fn run_hook_payload(args: Value) -> Value {
    let wire = args
        .get("wire")
        .and_then(Value::as_str)
        .and_then(Wire::parse);
    let event = args
        .get("event")
        .and_then(Value::as_str)
        .filter(|event| !event.is_empty());
    let (Some(wire), Some(event)) = (wire, event) else {
        return json!({
            "state": HookReceiptState::Refused.as_str(),
            "reason": "invalid_hook_request",
            "chars": 0,
            "output": null,
        });
    };
    let payload = args.get("payload").cloned().unwrap_or_else(|| json!({}));
    let outcome = run(wire, event, payload);
    json!({
        "state": outcome.receipt.state.as_str(),
        "reason": outcome.receipt.reason,
        "chars": outcome.receipt.chars,
        "id": outcome.receipt.id,
        "output": outcome.output,
    })
}

/// The context text in a pull answer: `Some(Some(text))` when it carries context,
/// `Some(None)` when it says there is none, `None` when the answer is not a pull answer (the
/// caller then runs the pull itself). Reads both this crate's own `{context}` shape and a
/// planner packet `{packet: {blocks}}`.
pub fn context_text(data: &Value) -> Option<Option<String>> {
    let text = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
    if let Some(context) = data.get("context") {
        return Some(text(context.as_str().map(str::to_owned)));
    }
    data.get("packet").map(|packet| text(packet_text(packet)))
}

/// The `{context, reason, detail}` answer of one recall: what the resident returns and what
/// every delivery mode of `membrane context pull` projects from.
fn pull_answer(outcome: &RecallOutcome) -> Value {
    json!({
        "context": outcome.context,
        "reason": outcome.reason,
        "detail": outcome.detail,
    })
}

/// The exact bytes the consumer of an explicit pull receives for `answer` under `spec`.
///
/// This is the one projection: selection counts it, the resident answer is verified against it
/// and `membrane context pull` prints exactly it (nothing else serializes a pull answer).
/// - Text: the context and one newline, or nothing when the answer carries no context.
/// - Json: one compact `membrane.context.pull.v1` document and a newline: the answer's fields,
///   `context` (null when there is none) and, with a receipt id, `receipt`
///   `{id, session, state}` (`emitted` when there is context, else `empty`); a `schema` the
///   answer already carried moves to `payload_schema` when it differs.
pub fn project_pull_answer(answer: &Value, spec: &PullProjectionSpec) -> Result<String, String> {
    let context = context_text(answer).flatten();
    match spec.delivery {
        PullDelivery::Text => Ok(context
            .map(|context| format!("{context}\n"))
            .unwrap_or_default()),
        PullDelivery::Json => {
            let receipt = spec
                .receipt_id
                .as_deref()
                .map(|id| pull_receipt_projection(id, context.is_some()));
            let mut fields = match answer {
                Value::Object(fields) => fields.clone(),
                _ => serde_json::Map::new(),
            };
            fields.insert("context".into(), context.map_or(Value::Null, Value::String));
            if let Some(receipt) = receipt {
                fields.insert("receipt".into(), receipt);
            }
            if let Some(inner) = fields.remove("schema") {
                if inner != Value::String(PULL_DOCUMENT_SCHEMA.to_owned()) {
                    fields.insert("payload_schema".into(), inner);
                }
            }
            let mut document = serde_json::Map::new();
            document.insert(
                "schema".into(),
                Value::String(PULL_DOCUMENT_SCHEMA.to_owned()),
            );
            document.extend(fields);
            serde_json::to_string(&Value::Object(document))
                .map(|encoded| format!("{encoded}\n"))
                .map_err(|error| format!("encode answer: {error}"))
        }
    }
}

/// The receipt a `--wire --request-id` pull reports: `emitted` when there was context to
/// deliver, `empty` when there was none.
pub fn pull_receipt_projection(id: &str, emitted: bool) -> Value {
    let state = if emitted {
        HookReceiptState::Emitted
    } else {
        HookReceiptState::Empty
    };
    json!({"id": id, "session": PULL_RECEIPT_SESSION, "state": state.as_str()})
}

/// The projection of one candidate response during selection: the answer a planner response
/// would produce with `packet` as its representation and `delivered` as its accounting, every
/// other value frozen from `base` (the response so far). Reads exactly what
/// `outcome_from_response` reads.
pub(crate) fn project_pull_view(
    base: &serde_json::Map<String, Value>,
    packet: &Value,
    delivered: &Value,
    spec: &PullProjectionSpec,
) -> Result<String, String> {
    let mut view = serde_json::Map::new();
    for key in [
        "correctiveRetrieval",
        "insufficientConfidence",
        "freshness",
        "budgetPolicy",
        "cortexRecall",
    ] {
        if let Some(value) = base.get(key) {
            view.insert(key.to_owned(), value.clone());
        }
    }
    view.insert("packet".to_owned(), packet.clone());
    view.insert("deliveredResponse".to_owned(), delivered.clone());
    let outcome = outcome_from_response(&Value::Object(view));
    project_pull_answer(&pull_answer(&outcome), spec)
}

/// The refusal for an exact count that cannot be made: never a heuristic count.
pub const TOKENIZER_UNAVAILABLE_REASON: &str =
    "request_time_selection_refused:budget_insufficient:tokenizer unavailable";

/// Exact o200k_base/1 token count, or the typed refusal.
pub fn count_o200k(text: &str) -> Result<u64, String> {
    rightkit_memory_core::ContextTokenAccounting::count_exact(text)
        .map(|tokens| tokens as u64)
        .map_err(|_| TOKENIZER_UNAVAILABLE_REASON.to_owned())
}

/// Re-project `answer` under `spec` and prove it is the response that was approved: the answer's
/// accounting names this delivery mode, its whole-response tokens and bytes are exactly those of
/// the projected bytes (recounted here under o200k_base/1) and the tokens fit `budget`. Returns
/// the projected bytes, the only text a consumer may be handed. An answer without context
/// (no matches, no memory store yet) carries no selection accounting, so its projection is
/// measured here and bounded by the same declared budget: the exact bytes of the requested mode
/// (the `--json` document with its reason, detail and optional empty receipt; nothing at all in
/// text mode, which is zero tokens and always fits) are counted under o200k_base/1 and refused as
/// `budget_insufficient` when they exceed it. A context without whole-response accounting (a
/// stale resident, a selection that did not run) is refused, never delivered unmeasured.
pub fn verify_pull_projection(
    answer: &Value,
    spec: &PullProjectionSpec,
    budget: u64,
) -> Result<String, String> {
    let projected = project_pull_answer(answer, spec)?;
    let mode = spec.delivery.as_str();
    let Some(context) = context_text(answer).flatten() else {
        // The exact projected cost is known before this early return: an empty projection costs
        // nothing (and needs no tokenizer); any other is counted and bounded like a response that
        // carries context.
        let tokens = if projected.is_empty() {
            0
        } else {
            count_o200k(&projected)?
        };
        if tokens > budget {
            return Err(budget_insufficient_refusal(mode, tokens, budget));
        }
        return Ok(projected);
    };
    let delivered = answer.pointer("/detail/deliveredResponse");
    let accounted = |key: &str| delivered.and_then(|d| d.get(key)).and_then(Value::as_u64);
    let (Some(approved_tokens), Some(approved_bytes)) = (
        accounted("wholeResponseTokens"),
        accounted("wholeResponseBytes"),
    ) else {
        return Err(format!(
            "request_time_selection_refused:projection_stale:the answer carries no whole-response accounting for the {mode} response"
        ));
    };
    let approved_mode = delivered
        .and_then(|d| d.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or("<absent>");
    if approved_mode != mode {
        return Err(format!(
            "request_time_selection_refused:projection_stale:the answer was approved for the {approved_mode} response, not the {mode} response"
        ));
    }
    let tokens = count_o200k(&projected)?;
    let context_tokens = count_o200k(&context)?;
    if tokens != approved_tokens
        || projected.len() as u64 != approved_bytes
        || accounted("contextTokens") != Some(context_tokens)
    {
        return Err(format!(
            "request_time_selection_refused:projection_mismatch:approved {approved_tokens} tokens and {approved_bytes} bytes, the {mode} response is {tokens} tokens and {} bytes",
            projected.len()
        ));
    }
    if tokens > budget {
        return Err(budget_insufficient_refusal(mode, tokens, budget));
    }
    Ok(projected)
}

/// The typed refusal for a complete response that exceeds the declared budget: one wording for
/// every answer, with or without context.
fn budget_insufficient_refusal(mode: &str, tokens: u64, budget: u64) -> String {
    format!(
        "request_time_selection_refused:budget_insufficient:complete {mode} response {tokens} tokens exceeds declared {budget}"
    )
}

/// Resident op `pull`: `{cwd?, prompt?, budget?, client?, session?, delivery?, receiptId?}`
/// answers `{context, reason, detail}`; `context` is null when nothing matched. `delivery`
/// (`text` or `json`, anything else is `json`) and `receiptId` name the response the consumer
/// receives, which the declared `budget` bounds whole.
pub fn pull_value(args: &Value) -> Result<Value, String> {
    let root = args
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::current_dir().ok())
        .map(|root| canonical_path(&root))
        .ok_or("cwd is unavailable")?;
    let task = args
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .unwrap_or("orient current task")
        .to_owned();
    let budget = effective_pull_budget(args.get("budget").and_then(Value::as_u64));
    let spec = PullProjectionSpec::from_args(args)?;
    let request = RecallRequest {
        task,
        root,
        max_tokens: budget as usize,
        client: args
            .get("client")
            .and_then(Value::as_str)
            .unwrap_or("cli")
            .to_owned(),
        session: args
            .get("session")
            .and_then(Value::as_str)
            .unwrap_or("cli")
            .to_owned(),
        ceiling: None,
        startup: false,
        projection: Some(spec.clone()),
    };
    // One authorization decision per explicit pull (one-shot and resident alike), made from a
    // single registry read before the store is looked at and before any candidate is selected: a
    // malformed, unreadable, mismatched, cross-root or revoked state refuses here, even with no
    // memory store; a bound root reads its binding's full descriptor; a root the healthy
    // registry does not hold (`None`) keeps the scope derived from its working directory.
    let authorized_scope = crate::pull::federation_sources::authorized_read_scope(&request.root)?;
    // A fresh install has no memory store until the first write: that is an empty answer, not an
    // error (the in-process path opens read-only and must not create it). Its projection in the
    // requested mode is held to the declared budget through the same check as every other answer.
    if !crate::cli::cortex_db_path().exists() {
        let answer = json!({
            "context": "",
            "reason": "empty",
            "detail": "no memory store yet",
        });
        verify_pull_projection(&answer, &spec, budget)?;
        return Ok(answer);
    }
    match recall(request, PULL_DEADLINE_MS, authorized_scope) {
        Ok(outcome) => {
            // The selection approved a response; prove the answer is that response before it
            // leaves this process.
            let answer = pull_answer(&outcome);
            verify_pull_projection(&answer, &spec, budget)?;
            Ok(answer)
        }
        Err(RecallFailure::Refused(reason))
        | Err(RecallFailure::Off(reason))
        | Err(RecallFailure::Failed(reason)) => Err(reason),
    }
}

/// `text` cut to at most `limit` characters, ending in a truncation note when it was cut.
fn cap_context(text: String, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text;
    }
    let marker =
        format!("\n[Membrane context truncated at {ADDITIONAL_CONTEXT_CAP_CHARS} characters]");
    let keep = limit.saturating_sub(marker.chars().count());
    let mut capped: String = text.chars().take(keep).collect();
    capped.push_str(&marker);
    capped
}

struct RecallRequest {
    task: String,
    root: PathBuf,
    max_tokens: usize,
    client: String,
    session: String,
    ceiling: Option<RemainingContextCeilingV1>,
    startup: bool,
    /// An explicit `membrane context pull`, not a hook: it alone selects its representation by
    /// the complete response its consumer receives under this delivery, and may answer recall by
    /// the registry scan. `None` for every hook.
    projection: Option<PullProjectionSpec>,
}

struct RecallOutcome {
    context: Option<String>,
    reason: &'static str,
    detail: Value,
}

enum RecallFailure {
    /// The request itself was rejected (invalid host observation, identity mismatch).
    Refused(String),
    /// Membrane is not serving this run.
    Off(String),
    /// An internal error.
    Failed(String),
}

fn recall_request(
    wire: Wire,
    input: &HookInputEnvelopeV1,
    event: &str,
) -> Result<RecallRequest, String> {
    let client = input
        .payload
        .get("client")
        .and_then(Value::as_str)
        .or_else(|| input.payload.get("client_id").and_then(Value::as_str))
        .unwrap_or(wire.as_str())
        .to_owned();
    // Host integrations may provide either canonical camelCase or native snake_case envelope
    // fields. Absence stays explicit instead of inventing a context window; a supplied but
    // invalid observation is a refusal, never silently discarded into the configured cap.
    let ceiling_field = input
        .payload
        .get("remainingContextCeiling")
        .or_else(|| input.payload.get("remaining_context_ceiling"));
    let ceiling = match ceiling_field {
        Some(value) if !value.is_null() => {
            let ceiling = serde_json::from_value::<RemainingContextCeilingV1>(value.clone())
                .map_err(|error| format!("host_observation_invalid: {error}"))?;
            ceiling
                .validate()
                .map_err(|error| format!("host_observation_invalid: {error}"))?;
            Some(ceiling)
        }
        _ => None,
    };
    let max_tokens = env::var("RIGHTKIT_CONTEXT_HOOK_RECALL_MAX_TOKENS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .or_else(|| input.payload.get("max_tokens").and_then(Value::as_u64))
        .unwrap_or(DEFAULT_RECALL_MAX_TOKENS) as usize;
    let startup = event == "SessionStart";
    let prompt = input
        .payload
        .get("prompt")
        .or_else(|| input.payload.get("user_prompt"))
        .or_else(|| input.payload.get("task"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let task = match prompt {
        Some(prompt) => prompt,
        None if startup => "startup orientation for current repository".to_owned(),
        None => "orient current task".to_owned(),
    };
    Ok(RecallRequest {
        task,
        root: project_root(input),
        max_tokens,
        client,
        session: input
            .session_id
            .clone()
            .unwrap_or_else(|| "host-native".to_owned()),
        ceiling,
        startup,
        projection: None,
    })
}

/// Federate once in this process under the configured cap, within `budget_ms`.
/// `authorized_scope` is the binding descriptor an explicit pull was authorized for; a hook
/// passes `None` and reads the scope derived from its working directory.
fn recall(
    request: RecallRequest,
    budget_ms: u64,
    authorized_scope: Option<Value>,
) -> Result<RecallOutcome, RecallFailure> {
    let response = crate::pull::federation::hook_mode_federate_authorized(
        &request.task,
        &request.root,
        request.max_tokens,
        &request.client,
        &request.session,
        budget_ms,
        request.ceiling,
        authorized_scope,
        request.projection.clone(),
    )
    .map_err(|error| {
        // An authorization denial keeps its gate code and detail whole: it is the caller's answer.
        if error.starts_with(crate::authorization::PULL_REFUSAL_PREFIX) {
            return RecallFailure::Refused(error.chars().take(1_000).collect());
        }
        let error = error.chars().take(200).collect::<String>();
        if error.starts_with("request_time_selection_refused") {
            RecallFailure::Refused(error)
        } else if error.contains("schema") && error.contains("mismatch") {
            RecallFailure::Off("schema".to_owned())
        } else {
            RecallFailure::Failed(error)
        }
    })?;
    let outcome = outcome_from_response(&response);
    Ok(if request.startup {
        startup_outcome(&request.root, outcome, &response)
    } else {
        outcome
    })
}

fn packet_text(packet: &Value) -> Option<String> {
    let blocks = packet
        .get("blocks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if !blocks.is_empty() {
        return Some(blocks.join("\n\n"));
    }
    packet
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
}

fn outcome_from_response(response: &Value) -> RecallOutcome {
    let packet = response.get("packet").unwrap_or(&Value::Null);
    let context = packet_text(packet).filter(|text| !text.trim().is_empty());
    let sufficiency = response
        .pointer("/correctiveRetrieval/sufficiency")
        .cloned()
        .unwrap_or(Value::Null);
    let sufficient = context.is_some()
        && sufficiency.get("state").and_then(Value::as_str) == Some("sufficient")
        && response.get("insufficientConfidence").is_none();
    let reason = if context.is_none() {
        "membrane_no_matches"
    } else if sufficient {
        "memory_recalled"
    } else {
        "membrane_retrieval_insufficient"
    };
    RecallOutcome {
        context,
        reason,
        detail: json!({
            "packetId": packet.get("id"),
            "freshness": packet.get("freshness").or_else(|| response.get("freshness")),
            "sufficiency": sufficiency,
            "omissions": packet.get("omissions"),
            "insufficientConfidence": response.get("insufficientConfidence"),
            "budgetPolicy": response.get("budgetPolicy"),
            "deliveredResponse": response.get("deliveredResponse"),
            "cortexRecall": response.get("cortexRecall"),
            "sources": packet.get("blocks").and_then(Value::as_array).into_iter().flatten()
                .map(|block| json!({"id":block.get("id"),"sourceRef":block.get("sourceRef"),"sourceHash":block.get("sourceHash")})).collect::<Vec<_>>(),
        }),
    }
}

/// Convert one ordinary planner packet into the bounded startup orientation surface. The
/// planner remains the sole source of candidates, freshness, provider accounting, omissions, and
/// Adapt output; this only selects a compact host representation and never reads or builds a
/// graph.
fn startup_outcome(root: &Path, mut outcome: RecallOutcome, response: &Value) -> RecallOutcome {
    let generation = response
        .pointer("/freshness/revision")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let stale = response
        .pointer("/freshness/stale")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let indexed_at = response
        .pointer("/freshness/indexedAt")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let providers = response
        .get("providerDiagnostics")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item
                        .get("provider")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("name").and_then(Value::as_str))?;
                    let status = item
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("observed");
                    Some(format!("{name}={status}"))
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".to_owned());
    let cortex = if providers.contains("cortex=") {
        "available"
    } else if providers.contains("cortex") {
        "observed"
    } else {
        "unavailable"
    };
    let ledger = if providers.contains("ledger=")
        || providers.contains("rules=")
        || providers.contains("documents=")
    {
        "available"
    } else {
        "unavailable"
    };
    let adapt = response
        .get("adaptDelivery")
        .map_or("unavailable", |_| "observed");
    let omissions = response
        .get("omissions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("id")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("detailId").and_then(Value::as_str))
                })
                .take(12)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "none".to_owned());
    let blocks = outcome.context.take().unwrap_or_default();
    let evidence_count = response
        .pointer("/packet/blocks")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let freshness = if stale {
        "stale (admitted with age recorded)"
    } else {
        "current"
    };
    let text = format!(
        "Membrane startup orientation\nrepository: {}\nBlueprint: generation={} freshness={} indexed={}\nCortex: {}\nLedger: {}\nAdapt/taste: {}\nproviders: {}\nevidence blocks: {}\nomissions: {}\n{}",
        root.display(),
        generation,
        freshness,
        indexed_at,
        cortex,
        ledger,
        adapt,
        providers,
        evidence_count,
        omissions,
        blocks
    );
    let bounded = if text.len() > STARTUP_PACKET_MAX_BYTES {
        let marker = format!(
            "\n[Membrane startup packet truncated at {} bytes]",
            STARTUP_PACKET_MAX_BYTES
        );
        let mut end = STARTUP_PACKET_MAX_BYTES.saturating_sub(marker.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}{}", &text[..end], marker)
    } else {
        text
    };
    let has_content = !bounded.trim().is_empty();
    outcome.context = has_content.then_some(bounded.clone());
    outcome.reason = if has_content {
        "startup_orientation_emitted"
    } else {
        "membrane_no_matches"
    };
    outcome.detail["startup"] = json!({
        "repository": root,
        "blueprint": {"generation": generation, "stale": stale, "indexedAt": indexed_at},
        "providers": providers,
        "cortex": cortex,
        "ledger": ledger,
        "adapt": adapt,
        "omissions": omissions,
        "byteBudget": STARTUP_PACKET_MAX_BYTES,
        "bytes": bounded.len(),
    });
    outcome
}

fn project_root(input: &HookInputEnvelopeV1) -> PathBuf {
    let requested = input
        .payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| {
            input
                .payload
                .get("working_directory")
                .and_then(Value::as_str)
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    canonical_path(&requested)
}

fn canonical_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
