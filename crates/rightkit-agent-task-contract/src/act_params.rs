//! `agent.act` and `agent.ask` task acceptance: the params shape and ceilings, the run mode, who submitted the task (the
//! engine's sealed stamp), the submit-time checks that need no host state, and the registry that lets "stop" latch the running
//! acts.
//!
//! This holds the neutral rules only. What a host supplies stays with the host: the tool catalog and its switches, the seal's
//! MAC (a `sealed` closure), the setting values and the journal. A rule that refuses here is a refusal in the host too, with the
//! same code and message.
//!
//! * [`Mode`]: how a run is started and which tools it may be offered (`Mode::allows`, the read-only guard).
//! * [`ActParams`] and [`parse_params`] / [`parse_ask_params`]: the params of an act and of an ask, with their ceilings.
//! * [`submitter_text`], [`SubmitterKind`], [`verify_submitter`]: the text a submitter's seal covers and what a verified stamp
//!   means.
//! * [`check_act_envelope`] and [`check_ask_envelope`]: the envelope checks of a submit.
//! * [`StopRegistry`]: the running acts and the stop latch.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use serde_json::Value;

use crate::{IntentSource, PrincipalKind, TaskEnvelope};

/// The job kind of an acting run.
pub const ACT_KIND: &str = "agent.act";
/// The job kind of the read-only run a paired device asks for.
pub const ASK_KIND: &str = "agent.ask";
/// The job kind of a run the engine starts from a rule.
pub const ENGINE_KIND: &str = "agent.engine";
/// The only route.
pub const ROUTE: &str = "claude-code";

/// Longest request or question, in bytes.
pub const MAX_REQUEST_BYTES: usize = 4096;
/// Longest model id, in bytes.
pub const MAX_MODEL_BYTES: usize = 128;

pub const DEFAULT_SECONDS: u64 = 600;
pub const MIN_SECONDS: u64 = 30;
pub const MAX_SECONDS: u64 = 3600;
pub const DEFAULT_CALLS: u64 = 40;
pub const MAX_CALLS: u64 = 200;
pub const ASK_DEFAULT_SECONDS: u64 = 120;
pub const ASK_MAX_SECONDS: u64 = 300;
pub const ASK_DEFAULT_CALLS: u64 = 12;
pub const ASK_MAX_CALLS: u64 = 30;

/// A typed refusal: a stable `code` and a message with no request text. A host turns it into its own error type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptError {
    pub code: String,
    pub message: String,
}

impl AcceptError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    /// The `bad_input` refusal.
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self::new("bad_input", message)
    }
}

/// Result of an acceptance check.
pub type AcceptResult<T> = Result<T, AcceptError>;

/// A model id that is plain text and cannot be taken for a command-line flag.
#[must_use]
pub fn plain_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= MAX_MODEL_BYTES
        && !model.starts_with('-')
        && model.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-' | '@' | '[' | ']')
        })
}

/// The quote the request is made of, for the origins that carry one.
#[must_use]
pub fn quote_of(source: &IntentSource) -> Option<&str> {
    match source {
        IntentSource::Direct { quote, .. }
        | IntentSource::Delegated { quote, .. }
        | IntentSource::UnavailablePointer { quote, .. } => Some(quote),
        _ => None,
    }
}

/// The tool-name lists a host offers each mode. Names only; the rows and their `effect` flag are the host's catalog.
#[derive(Clone, Copy, Debug)]
pub struct ModeTools<'a> {
    /// Tools offered to no acting run (they are for asks and engine runs only).
    pub ask_only: &'a [&'a str],
    /// What an ask may be offered.
    pub ask: &'a [&'a str],
    /// What a `brief` run may be offered.
    pub brief: &'a [&'a str],
    /// What a `prepare` run may be offered.
    pub prepared: &'a [&'a str],
}

/// How a run is started. `Act` is the acting run (`agent.act`). `Ask` is the read-only run a paired device owned by the owner
/// may request (`agent.ask`): the SAME broker, shim and limits, with a tool set chosen in code, never by the prompt. Every place
/// that offers, registers, admits or calls a tool takes the mode and asks [`Mode::allows`], and a host never waits for an owner's
/// approval outside `Act`, so an ask cannot reach an effect and cannot park on a question nobody is there to answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Act,
    Ask,
    /// `agent.engine` with the recipe `brief`: the READ tools only and no wait for an approval.
    Brief,
    /// `agent.engine` with the recipe `prepare`: the same reads, and drafts, labels and archive. Never a send, and no wait for
    /// an approval.
    Prepared,
}

impl Mode {
    /// The job kind this mode serves.
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Mode::Act => ACT_KIND,
            Mode::Ask => ASK_KIND,
            Mode::Brief | Mode::Prepared => ENGINE_KIND,
        }
    }

    /// Whether a run in this mode may be offered, register or call the tool `name` whose catalog row has `effect`. THE read-only
    /// guard.
    #[must_use]
    pub fn allows(self, name: &str, effect: bool, tools: &ModeTools<'_>) -> bool {
        match self {
            Mode::Act => !tools.ask_only.contains(&name),
            Mode::Ask => tools.ask.contains(&name) && !effect,
            Mode::Brief => tools.brief.contains(&name) && !effect,
            Mode::Prepared => tools.prepared.contains(&name),
        }
    }
}

/// The engine's stamp of who submitted an `agent.act` task, written by the resident bridge from the socket's peer: `requester`
/// is `owner`, `chat:<id>` or `local`, and `seal` is the engine process's MAC over it and the task.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submitter {
    pub requester: String,
    pub seal: String,
}

/// The Inbox item an `agent.act` task was submitted for (`inbox.act`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxRef {
    pub item: String,
    pub action: String,
    /// The phone whose signed `inbox.act` started the task, so the result can be alerted to it.
    #[serde(default)]
    pub phone: Option<String>,
}

/// The params of an `agent.act` task.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActParams {
    pub schema_version: u32,
    pub route: Option<String>,
    pub model: Option<String>,
    /// Active wall clock for the run, waiting for the owner not counted.
    pub max_seconds: Option<u64>,
    /// Most tool calls the run may make.
    pub max_calls: Option<u64>,
    /// Who submitted the task, as the engine stamped it. Never the caller's.
    pub submitter: Option<Submitter>,
    /// The Inbox item and action this act carries out. Only `inbox.act` sets it, after the stamp.
    pub inbox: Option<InboxRef>,
}

/// The Inbox item id and action id a stored `agent.act` task carries in its params, when `inbox.act` submitted it.
#[must_use]
pub fn inbox_ref(params: &Value) -> Option<(String, String)> {
    let parsed: ActParams = serde_json::from_value(params.clone()).ok()?;
    parsed.inbox.map(|r| (r.item, r.action))
}

/// The phone whose signed `inbox.act` started a stored `agent.act` task, if one did.
#[must_use]
pub fn inbox_phone(params: &Value) -> Option<String> {
    let parsed: ActParams = serde_json::from_value(params.clone()).ok()?;
    parsed.inbox.and_then(|r| r.phone)
}

/// Parses and bounds the params of an `agent.act` task.
pub fn parse_params(params: &Value) -> AcceptResult<ActParams> {
    let parsed: ActParams = serde_json::from_value(params.clone())
        .map_err(|e| AcceptError::bad_input(format!("invalid agent.act params: {e}")))?;
    if parsed.schema_version != 1 {
        return Err(AcceptError::bad_input(format!(
            "unsupported agent.act params schema version {}",
            parsed.schema_version
        )));
    }
    if parsed.route.as_deref().is_some_and(|r| r != ROUTE) {
        return Err(AcceptError::new(
            "unsupported_route",
            format!("agent.act runs only the {ROUTE} route"),
        ));
    }
    if parsed.model.as_deref().is_some_and(|m| !plain_model(m)) {
        return Err(AcceptError::bad_input("model is not a plain model id"));
    }
    if parsed
        .max_seconds
        .is_some_and(|s| !(MIN_SECONDS..=MAX_SECONDS).contains(&s))
    {
        return Err(AcceptError::bad_input(format!(
            "max_seconds must be {MIN_SECONDS} to {MAX_SECONDS}"
        )));
    }
    if parsed
        .max_calls
        .is_some_and(|c| !(1..=MAX_CALLS).contains(&c))
    {
        return Err(AcceptError::bad_input(format!(
            "max_calls must be 1 to {MAX_CALLS}"
        )));
    }
    Ok(parsed)
}

/// The params of an `agent.ask` task: the same shape as an act's, with no submitter stamp and no Inbox reference, no model and
/// no route, and the lower ceilings. A caller may lower the limits, never raise them.
pub fn parse_ask_params(params: &Value) -> AcceptResult<ActParams> {
    let parsed = parse_params(params)?;
    if parsed.submitter.is_some() || parsed.inbox.is_some() {
        return Err(AcceptError::bad_input(
            "agent.ask takes no submitter or inbox param",
        ));
    }
    if parsed.model.is_some() || parsed.route.is_some() {
        return Err(AcceptError::bad_input(
            "agent.ask takes no model or route param",
        ));
    }
    if parsed.max_seconds.is_some_and(|s| s > ASK_MAX_SECONDS)
        || parsed.max_calls.is_some_and(|c| c > ASK_MAX_CALLS)
    {
        return Err(AcceptError::bad_input(format!(
            "agent.ask allows max_seconds up to {ASK_MAX_SECONDS} and max_calls up to {ASK_MAX_CALLS}"
        )));
    }
    Ok(parsed)
}

/// The text a submitter's seal covers: the stamp, then what names the task (its idempotency key, its utterance, its target
/// device and the SHA-256 of its quote), so a seal fits one task only.
#[must_use]
pub fn submitter_text(requester: &str, envelope: &TaskEnvelope) -> String {
    let quote = quote_of(&envelope.intent.source).unwrap_or_default();
    format!(
        "pulse.agent-act.submitter.v1\n{requester}\n{}\n{}\n{}\n{}",
        envelope.idempotency_key,
        envelope.utterance.utterance_id,
        envelope.target_device.device_id,
        rightkit_crypto::hex_encode(&rightkit_crypto::sha256(quote.as_bytes())),
    )
}

/// Who a verified submitter stamp names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitterKind {
    /// The owner's installed app or hub: an agent that is already unlocked.
    Owner,
    /// A verified chat, by its id: an agent that acts under that chat's unlock.
    Chat(String),
    /// Anything else: refused while chat unlock is on.
    Local,
}

impl SubmitterKind {
    /// The word the stamp carries: `owner`, `chat:<id>` or `local`.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            SubmitterKind::Owner => "owner".to_string(),
            SubmitterKind::Chat(id) => format!("chat:{id}"),
            SubmitterKind::Local => "local".to_string(),
        }
    }
}

fn submitter_refused() -> AcceptError {
    AcceptError::new(
        "unsupported_origin",
        "agent.act needs this engine's own record of who submitted it (none, or one this engine did not make, for example before a restart); submit it again as a new task (a new idempotency key)",
    )
}

/// The submitter a task's stamp names, once the stamp verifies. `sealed(text, mac)` is the host's check that `mac` is this engine
/// process's seal of `text`. A stamp that is missing, or does not verify, or names no known requester is `unsupported_origin`.
pub fn verify_submitter(
    params: &ActParams,
    envelope: &TaskEnvelope,
    sealed: impl Fn(&str, &str) -> bool,
) -> AcceptResult<SubmitterKind> {
    let Some(stamp) = &params.submitter else {
        return Err(submitter_refused());
    };
    if !sealed(&submitter_text(&stamp.requester, envelope), &stamp.seal) {
        return Err(submitter_refused());
    }
    match stamp.requester.as_str() {
        "owner" => Ok(SubmitterKind::Owner),
        "local" => Ok(SubmitterKind::Local),
        other => other
            .strip_prefix("chat:")
            .filter(|id| !id.is_empty())
            .map(|id| SubmitterKind::Chat(id.to_string()))
            .ok_or_else(submitter_refused),
    }
}

/// Removes what a caller must never set from an act's params before the host stamps them: any `submitter`, any `inbox`, and the
/// `fromChat` claim, which is returned for the host to verify. `None` if `params` is not an object (a body with no params has an
/// empty object made for it by the caller).
pub fn strip_caller_fields(params: &mut serde_json::Map<String, Value>) -> Value {
    params.remove("submitter");
    // Only `inbox.act` names an Inbox item, and it does so after the stamp.
    params.remove("inbox");
    params.remove("fromChat").unwrap_or(Value::Null)
}

/// The envelope checks of an `agent.act` submit: no context references, the owner's own quoted request, an owner or local-client
/// origin, a quote of 1 to [`MAX_REQUEST_BYTES`] bytes, and this computer as the target. The task's JSON here can only refuse,
/// never grant. The host's switch, tool and submitter checks come around this one, in the same order as the host's.
pub fn check_act_envelope(envelope: &TaskEnvelope, local_device: Option<&str>) -> AcceptResult<()> {
    if !envelope.context_refs.is_empty() {
        return Err(AcceptError::new(
            "unsupported_context",
            "agent.act takes no context references yet",
        ));
    }
    // An act is the owner's own words, said or typed on this computer. A relayed or delegated request, a grant and a paired
    // device are refused: nothing relayed may start an agent that acts.
    let IntentSource::Direct { quote, .. } = &envelope.intent.source else {
        return Err(AcceptError::new(
            "unsupported_origin",
            "agent.act needs the owner's own request quoted in intent.source (direct)",
        ));
    };
    if !matches!(
        envelope.origin.kind,
        PrincipalKind::Owner | PrincipalKind::LocalClient
    ) {
        return Err(AcceptError::new(
            "unsupported_origin",
            "agent.act is accepted only from this computer's owner or its own clients",
        ));
    }
    if quote.trim().is_empty() || quote.len() > MAX_REQUEST_BYTES {
        return Err(AcceptError::bad_input(format!(
            "the quoted request must be 1 to {MAX_REQUEST_BYTES} bytes"
        )));
    }
    check_target(envelope, local_device)
}

/// The envelope checks of an `agent.ask` submit that come after the switch and the params: no context references, the owner's own
/// question quoted in `intent.source` (direct), 1 to [`MAX_REQUEST_BYTES`] bytes, and this computer as the target. Who asked is
/// the host's registry check.
pub fn check_ask_envelope(envelope: &TaskEnvelope, local_device: Option<&str>) -> AcceptResult<()> {
    if !envelope.context_refs.is_empty() {
        return Err(AcceptError::new(
            "unsupported_context",
            "agent.ask takes no context references",
        ));
    }
    // The question is the owner's own words, typed on the phone. A relayed or delegated request is refused: nothing relayed may
    // start a run that reads the owner's mail.
    let IntentSource::Direct { quote, .. } = &envelope.intent.source else {
        return Err(AcceptError::new(
            "unsupported_origin",
            "agent.ask needs the owner's own question quoted in intent.source (direct)",
        ));
    };
    if quote.trim().is_empty() || quote.len() > MAX_REQUEST_BYTES {
        return Err(AcceptError::bad_input(format!(
            "the question must be 1 to {MAX_REQUEST_BYTES} bytes"
        )));
    }
    check_target(envelope, local_device)
}

/// A task that names another device never runs here.
pub fn check_target(envelope: &TaskEnvelope, local_device: Option<&str>) -> AcceptResult<()> {
    if let Some(local) = local_device
        && local != envelope.target_device.device_id
    {
        return Err(AcceptError::new(
            "not_this_device",
            "the task names another device; it never runs here",
        ));
    }
    Ok(())
}

/// A local `task.submit` carries the bridge's stamp (the device-link host and the phone door add none), so a stamped ask came
/// from a local process and never from a paired device.
pub fn check_ask_unstamped(params: &Value) -> AcceptResult<()> {
    if params.get("submitter").is_some() {
        return Err(AcceptError::new(
            "unsupported_origin",
            "agent.ask is accepted only from a paired device the owner owns, not from a local client",
        ));
    }
    Ok(())
}

/// An act that starts within this long after "stop" was queued behind the stopped run and is refused, so Stop also drains the
/// queue.
pub const STOP_WINDOW_MS: u64 = 10_000;

/// The running acts and the stop latch. A host holds one in a `static`. Every run notes [`StopRegistry::generation`] when it
/// starts; a later value means "stop" was called.
#[derive(Debug)]
pub struct StopRegistry {
    gen_: AtomicU64,
    stopped_at_ms: AtomicU64,
    active: Mutex<Vec<(String, PathBuf)>>,
}

impl StopRegistry {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            gen_: AtomicU64::new(0),
            stopped_at_ms: AtomicU64::new(0),
            active: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(String, PathBuf)>> {
        self.active.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The stop generation now.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.gen_.load(Ordering::SeqCst)
    }

    /// When "stop" last ran, in milliseconds (0 if never).
    #[must_use]
    pub fn stopped_at_ms(&self) -> u64 {
        self.stopped_at_ms.load(Ordering::SeqCst)
    }

    /// Latches the stop (no broker call of any running act executes after this returns) and returns the ids of the running acts so
    /// the caller can cancel them in the journal. `now_ms` is the host's clock.
    pub fn stop_all(&self, now_ms: u64) -> Vec<String> {
        self.gen_.fetch_add(1, Ordering::SeqCst);
        self.stopped_at_ms.store(now_ms, Ordering::SeqCst);
        self.lock().iter().map(|(id, _)| id.clone()).collect()
    }

    /// Notes a running act (its task id and scratch folder). The returned guard removes it when dropped, however the run ends.
    pub fn register(&self, task_id: &str, scratch: PathBuf) -> Registered<'_> {
        self.lock().push((task_id.to_string(), scratch));
        Registered {
            registry: self,
            task_id: task_id.to_string(),
        }
    }

    /// The running acts: (task id, scratch folder).
    #[must_use]
    pub fn running(&self) -> Vec<(String, PathBuf)> {
        self.lock().clone()
    }
}

impl Default for StopRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Removes a run from its [`StopRegistry`] when it ends, however it ends.
#[derive(Debug)]
pub struct Registered<'a> {
    registry: &'a StopRegistry,
    task_id: String,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.registry.lock().retain(|(id, _)| *id != self.task_id);
    }
}
