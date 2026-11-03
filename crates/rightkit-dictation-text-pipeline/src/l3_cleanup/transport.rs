//! Provider transport: the single `call_provider` request path (one
//! `rightkit-llm` OpenAI-compatible call), the cached HTTP client pool, the
//! on-device payload ledger with rotation, error classification, and
//! provider-specific payload options (reasoning effort, OpenRouter routing
//! preferences, token headroom).

use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use rightkit_llm::http::{
    AddressPolicy, Client, ClientConfig, HttpError, Phase, ProxyMode, Secret, TrustRoots,
};
use rightkit_llm::openai::OpenAiCompatProvider;
use rightkit_llm::{ChatRequest, CredentialSource, LlmError, Message, Provider as _, RawHandle};
use serde_json::{json, Value};

use rightkit_transcript_library::net::host_of_url;

use super::overrides;
use super::state::{CachedAgent, Provider, ProviderSpec, AGENTS};

/// User agent the former `ureq` calls sent explicitly.
const USER_AGENT: &str = "rightkit-dictation/0.1";
/// `ureq`'s default `read_to_string` body limit.
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;
/// Hosts the provider roster reaches by default. Any other host is a base-URL
/// override (env/debug) and may resolve to a local address, as it always could.
const DEFAULT_PROVIDER_HOSTS: &[&str] = &[
    "api.groq.com",
    "api.cerebras.ai",
    "integrate.api.nvidia.com",
    "openrouter.ai",
];

/// The key for one call, taken from the existing provider-key store. `Secret`
/// redacts itself, so Debug output never shows the value.
#[derive(Debug)]
struct SpecKey(Secret);

impl CredentialSource for SpecKey {
    fn credential(&self, _provider: &str) -> Result<Option<Secret>, LlmError> {
        Ok(Some(self.0.clone()))
    }
}

/// Why one paid provider attempt failed: the static class (never provider text), and whether the
/// lane loop may go on to ANOTHER paid provider. It may not when the request was dispatched and its
/// outcome is uncertain (a timeout or transport failure after the send, a 5xx or 408): that request
/// may already have been processed and billed, and asking a second provider the same question is a
/// replay of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderFailure {
    pub class: &'static str,
    pub may_try_next_provider: bool,
}

impl ProviderFailure {
    /// A failure after which nothing was left uncertain (nothing sent, or a definite answer).
    fn definite(class: &'static str) -> Self {
        Self {
            class,
            may_try_next_provider: true,
        }
    }
}

/// Whether the lane loop may try another paid provider after `error`, from the owner's own
/// classification: `LlmError::billing().may_have_spend()` is false when nothing was sent or the
/// provider rejected the request (HTTP 4xx other than 408), and true once the request was
/// dispatched without usage counts (transport failure, timeout, 5xx). An answer the provider did
/// give but that cannot be used (truncated, malformed, refused, an in-body provider error) is a
/// definite outcome, so asking a different lane is the ladder's quality fallback, not a replay.
pub(crate) fn may_try_next_provider(error: &LlmError) -> bool {
    match error.root() {
        LlmError::Truncated { .. }
        | LlmError::Malformed(_)
        | LlmError::Refused { .. }
        | LlmError::Provider { .. } => true,
        _ => !error.billing().may_have_spend(),
    }
}

pub(crate) fn call_provider<F>(
    spec: &ProviderSpec,
    input: &str,
    timeout: Duration,
    payload: F,
    prompt_version: &'static str,
) -> Result<String, ProviderFailure>
where
    F: Fn(&ProviderSpec, &str) -> Value,
{
    let request_body = payload(spec, input);
    let (result, raw_body) = send_chat(spec, &request_body, timeout, prompt_version);
    // Pre-port parity: once a 2xx body was read it was logged as a success
    // (`error` null) even when parsing then rejected it (truncated, malformed,
    // empty text). `error` is set only when no body was logged.
    let logged = match (&result, &raw_body) {
        (Err(failure), None) => Some(failure.class),
        _ => None,
    };
    log_payload_if_enabled(
        spec,
        prompt_version,
        &request_body,
        raw_body.as_deref(),
        logged,
    );
    result
}

/// Exactly one HTTP attempt. The caller's lane loop owns fail-over, the
/// deadline split, and the circuit breaker. Also returns the provider's raw
/// response body (capped) for every call that got an HTTP response, for the
/// on-device payload ledger.
fn send_chat(
    spec: &ProviderSpec,
    request_body: &Value,
    timeout: Duration,
    prompt_version: &str,
) -> (Result<String, ProviderFailure>, Option<String>) {
    let Some(request) = chat_request_from_payload(request_body) else {
        // Nothing was sent.
        return (Err(ProviderFailure::definite("bad_response")), None);
    };
    let request = request.without_retries().reject_truncated_output();
    let client = agent_for_endpoint(timeout, &spec.base_url);
    // The observer is the only access to the raw exchange on success and on an
    // HTTP error status. It keeps just this call's capped-body handle and
    // never logs.
    let captured: Arc<Mutex<Option<RawHandle>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let provider = OpenAiCompatProvider::new(
        spec.provider.as_str(),
        &spec.base_url,
        (*client).clone(),
        Arc::new(SpecKey(Secret::new(spec.api_key.clone()))),
    )
    .with_header("X-RightKit-AI-Prompt-Version", prompt_version)
    // Keep the whole body the transport read: the ledger parses it as JSON and caps it in
    // characters afterwards, as before the port. A smaller byte cap here would cut the JSON.
    .with_raw_capture(MAX_RESPONSE_BYTES)
    .with_observer(move |record| {
        if let Some(raw) = record.raw {
            *sink.lock() = Some(Arc::clone(raw));
        }
    });
    let outcome = provider.complete(&request);
    let raw = captured
        .lock()
        .take()
        .or_else(|| outcome.as_ref().err().and_then(|e| e.raw().cloned()));
    // Pre-port parity: only a 2xx body reached the ledger; an HTTP error
    // status logged `response: null` (provider error text is not uploaded).
    let raw = raw.filter(|raw| (200..300).contains(&raw.status));
    // A body that is not UTF-8 never reached the ledger and was a bad response.
    if raw
        .as_ref()
        .is_some_and(|raw| std::str::from_utf8(&raw.body).is_err())
    {
        // A 2xx answer was received: definite.
        return (Err(ProviderFailure::definite("bad_response")), None);
    }
    let raw_body = raw.map(|raw| raw.body_text().into_owned());
    let result = match outcome {
        Ok(response) => Some(response.text().trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or(ProviderFailure::definite("bad_response")),
        Err(error) => Err(ProviderFailure {
            class: error_class_from_llm(&error),
            may_try_next_provider: may_try_next_provider(&error),
        }),
    };
    (result, raw_body)
}

/// Rebuild the lane payload as a `rightkit-llm` request. Every top-level key
/// beyond the envelope (`provider`, `reasoning_effort`, `reasoning_format`,
/// `include_reasoning`) travels as an extra body field with its value intact.
fn chat_request_from_payload(payload: &Value) -> Option<ChatRequest> {
    let object = payload.as_object()?;
    let mut system = None;
    let mut turns = Vec::new();
    for (index, message) in object.get("messages")?.as_array()?.iter().enumerate() {
        let content = message.get("content")?.as_str()?;
        match message.get("role")?.as_str()? {
            "system" if index == 0 => system = Some(content.to_string()),
            "user" => turns.push(Message::user(content)),
            "assistant" => turns.push(Message::assistant(content)),
            _ => return None,
        }
    }
    let mut request =
        ChatRequest::new(object.get("model")?.as_str()?, turns).keep_empty_messages(true);
    request.system = system;
    request = match object.get("max_tokens").and_then(Value::as_u64) {
        Some(max) => request.with_max_tokens(max.min(u64::from(u32::MAX)) as u32),
        None => request.without_max_tokens(),
    };
    if let Some(temperature) = object.get("temperature").and_then(Value::as_f64) {
        request = request.with_temperature(temperature as f32);
    }
    for (key, value) in object {
        if !matches!(
            key.as_str(),
            "model" | "messages" | "max_tokens" | "temperature" | "stream"
        ) {
            request = request.with_extra(key.clone(), value.clone());
        }
    }
    Some(request)
}

/// Support payload ledger. Request/response text always stays on-device first;
/// shell telemetry later decides whether this retained evidence is uploaded.
/// Headers are never included. Secret-shaped JSON fields are removed before
/// disk write so a diagnostic export cannot contain provider credentials.
fn log_payload_if_enabled(
    spec: &ProviderSpec,
    prompt_version: &str,
    request_body: &Value,
    raw_response: Option<&str>,
    error: Option<&'static str>,
) {
    // The engine's own app data root (the same resolution the shell uses): no environment
    // variable selects it in a release build.
    let dir = crate::settings::app_data_root();
    if std::fs::create_dir_all(&dir).is_err() {
        emit_payload_logging_diagnostic("ai_polish_payload_log_create_failed");
        return;
    }
    let entry = serde_json::json!({
        "payload_id": next_payload_id(),
        "at": chrono::Local::now().to_rfc3339(),
        "provider": spec.provider.as_str(),
        "model": spec.model,
        "prompt_version": prompt_version,
        "request": rightkit_dictation_core::redact_payload_value(
            request_body.clone(),
            MAX_PAYLOAD_TEXT_CHARS,
        ),
        "response": raw_response.map(scrub_payload_text),
        "error": error,
    });
    let payload_path = dir.join("polish-payloads.jsonl");
    // The owner's store rotates at the size threshold and keeps one `.1` predecessor (the
    // diagnostics bundle reads `polish-payloads.jsonl.1`); a failed rotation is absorbed and counted
    // in its status, and the append still happens.
    if rightkit_logs::JsonlStore::new(payload_path, MAX_PAYLOAD_LOG_BYTES)
        .append_value(&entry)
        .is_err()
    {
        emit_payload_logging_diagnostic("ai_polish_payload_log_write_failed");
    }
}

const MAX_PAYLOAD_TEXT_CHARS: usize = 16 * 1024;
const MAX_PAYLOAD_LOG_BYTES: u64 = 2 * 1024 * 1024;
const RUNTIME_DIAGNOSTICS_PREFIX: &str = "RIGHTKIT_RUNTIME_DIAGNOSTIC_JSON=";

fn next_payload_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        "p{nanos:x}-{:x}-{:x}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// This marker is deliberately content-free. The desktop supervisor turns it
/// into its normal structured diagnostic event; this logger never recurses
/// through tracing or its own payload ledger when local storage fails.
fn emit_payload_logging_diagnostic(code: &'static str) {
    emit_runtime_diagnostic("error", "polish", code, true);
}

pub(crate) fn emit_runtime_diagnostic(
    severity: &'static str,
    component: &'static str,
    code: &'static str,
    recoverable: bool,
) {
    let marker = serde_json::json!({
        "schema_version": 1,
        "event": "runtime_diagnostic",
        "severity": severity,
        "component": component,
        "code": code,
        "recoverable": recoverable,
    });
    let Ok(serialized) = serde_json::to_string(&marker) else {
        return;
    };
    use std::io::Write as _;
    let _ = std::io::stderr()
        .write_all(format!("{RUNTIME_DIAGNOSTICS_PREFIX}{serialized}\n").as_bytes());
}

fn scrub_payload_text(value: &str) -> String {
    rightkit_dictation_core::redact_payload_text(value, MAX_PAYLOAD_TEXT_CHARS)
}

/// Cached client for a timeout bucket. A URL whose host is not a default
/// provider host (a base-URL override) may resolve to local addresses, which
/// `ureq` always permitted; every other client keeps the public-web policy.
pub(crate) fn agent_for_endpoint(timeout: Duration, url: &str) -> Arc<Client> {
    let timeout_ms = timeout_bucket_ms(timeout);
    let private_host =
        host_of_url(url).filter(|host| !DEFAULT_PROVIDER_HOSTS.contains(&host.as_str()));
    let agents = AGENTS.get_or_init(|| Mutex::new(Vec::new()));
    let mut guard = agents.lock();
    if let Some(cached) = guard
        .iter()
        .find(|cached| cached.timeout_ms == timeout_ms && cached.private_host == private_host)
    {
        return Arc::clone(&cached.agent);
    }
    let agent = Arc::new(parity_client(
        Duration::from_millis(timeout_ms),
        private_host.as_deref(),
    ));
    guard.push(CachedAgent {
        timeout_ms,
        private_host,
        agent: Arc::clone(&agent),
    });
    agent
}

/// Reproduces the former `ureq` agent: environment proxy, bundled roots, up to
/// 10 redirects (an error past that), no retries, one global per-call timeout,
/// and a 10 MiB buffered-body cap.
fn parity_client(timeout: Duration, private_host: Option<&str>) -> Client {
    let mut policy = AddressPolicy::default();
    if let Some(host) = private_host {
        policy = policy.allow_if(vec![host.to_string()], |_| true);
    }
    let config = ClientConfig {
        connect_timeout: None,
        response_timeout: None,
        recv_body_timeout: None,
        request_timeout: timeout,
        stream_timeout: Some(timeout),
        max_response_bytes: MAX_RESPONSE_BYTES,
        ..ClientConfig::legacy_ureq_parity(USER_AGENT)
    }
    .address_policy(policy)
    .proxy(ProxyMode::Environment)
    .trust(TrustRoots::Bundled);
    match Client::try_new(config.clone()) {
        Ok(client) => client,
        // ureq ignored an unusable proxy variable and connected directly.
        Err(_) => Client::new(config.proxy(ProxyMode::None)),
    }
}

pub(crate) fn timeout_bucket_ms(timeout: Duration) -> u64 {
    let ms = timeout.as_millis().clamp(1, u128::from(u64::MAX)) as u64;
    if ms < 100 {
        ms
    } else {
        (ms / 100).saturating_mul(100)
    }
}

pub(crate) fn openrouter_provider_preferences(model: &str) -> Option<Value> {
    let only = overrides::openrouter_provider_only();
    let order = overrides::openrouter_provider_order();
    let sort = overrides::openrouter_provider_sort();
    let only = if only.is_empty() && model.contains("llama-3.3-70b") {
        vec!["groq".to_string()]
    } else {
        only
    };
    let mut provider = serde_json::Map::new();
    provider.insert("data_collection".to_string(), json!("deny"));
    if !only.is_empty() {
        provider.insert("only".to_string(), json!(only));
        provider.insert("allow_fallbacks".to_string(), json!(false));
    } else if !order.is_empty() {
        provider.insert("order".to_string(), json!(order));
    }
    if let Some(sort) = sort {
        provider.insert("sort".to_string(), json!(sort));
    }
    Some(Value::Object(provider))
}

fn error_class_from_status(status: u16) -> &'static str {
    match status {
        401 | 403 => "auth",
        408 => "timeout",
        429 => "rate_limit",
        500..=599 => "server",
        _ => "http",
    }
}

/// Static class for a transport failure; the error text is never used. A
/// failure while reading the body was `bad_response` under `ureq`.
pub(crate) fn error_class_from_http(error: &HttpError) -> &'static str {
    match error {
        HttpError::Status { status, .. } => error_class_from_status(*status),
        HttpError::Timeout { phase, .. } if *phase != Phase::Body => "timeout",
        HttpError::Transport { phase, .. } if *phase != Phase::Body => "network",
        HttpError::Timeout { .. }
        | HttpError::Transport { .. }
        | HttpError::BodyTooLarge { .. }
        | HttpError::Json(_)
        | HttpError::Stream(_) => "bad_response",
        _ => "network",
    }
}

/// Static class for a `rightkit-llm` failure. The crate's message text can
/// carry provider response text, so it is never logged or returned.
pub(crate) fn error_class_from_llm(error: &LlmError) -> &'static str {
    match error {
        LlmError::Http(error) => error_class_from_http(error),
        LlmError::Status { status, .. } | LlmError::Auth { status, .. } => {
            error_class_from_status(*status)
        }
        // Raw capture wraps only Truncated and Malformed; classify the inner
        // error so a captured one can never fall through to `network`.
        LlmError::Captured { error, .. } => match error.as_ref() {
            LlmError::Truncated { .. } => "truncated_response",
            LlmError::Malformed(_) => "bad_response",
            other => error_class_from_llm(other),
        },
        LlmError::Truncated { .. } => "truncated_response",
        LlmError::Malformed(_)
        | LlmError::Refused { .. }
        | LlmError::MidStream(_)
        | LlmError::Provider { .. } => "bad_response",
        LlmError::MissingCredential(_) => "auth",
        _ => "network",
    }
}

pub(crate) fn apply_provider_payload_options(spec: &ProviderSpec, payload: &mut Value) {
    match spec.provider {
        Provider::Cerebras if spec.model.contains("gpt-oss") => {
            payload["reasoning_effort"] = json!("low");
            payload["reasoning_format"] = json!("hidden");
        }
        // Groq gpt-oss: reasoning cannot be disabled (only low|medium|high;
        // default medium) and reasoning_format is NOT supported for these
        // models — include_reasoning:false is the supported way to keep the
        // chain-of-thought out of the response. Without these, medium-effort
        // reasoning burned most of the completion budget and polishes came
        // back truncated or empty.
        Provider::Groq if spec.model.contains("gpt-oss") => {
            payload["reasoning_effort"] = json!("low");
            payload["include_reasoning"] = json!(false);
        }
        Provider::Groq | Provider::Nvidia if spec.model.contains("qwen") => {
            payload["reasoning_effort"] = json!("none");
        }
        _ => {}
    }
    // Reasoning models spend completion tokens on hidden reasoning BEFORE the
    // visible answer, and those tokens bill against max_tokens. The base
    // budget (max_tokens_for: input-scaled, 160 headroom, 1200 cap) is sized
    // for the ANSWER alone, so give reasoning routes explicit extra room.
    // Replay of 30 logged payloads measured 7-187 reasoning
    // tokens on completed calls and up to ~500 on the calls that truncated;
    // 768 covers that observed worst case with slack.
    if spec.model.contains("gpt-oss") {
        if let Some(base) = payload["max_tokens"].as_u64() {
            payload["max_tokens"] = json!((base + 768).min(2_048));
        }
    }
}
