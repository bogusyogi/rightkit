//! OpenAI-compatible HTTP streaming client (SSE).
//!
//! This module defines the wire types for interacting with OpenAI-compatible
//! endpoints and the `ProviderError` type used throughout the crate.

use rightkit_agent_engine_protocol::ToolCall;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Outcome of one physical attempt to submit an already-admitted provider
/// request. A transport error is deliberately ambiguous: bytes may have
/// reached provider before connection failed, so callers must reconcile it
/// instead of replaying automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionState {
    Submitted,
    SubmissionUnknown,
    Rejected,
}

/// Provider-local receipt for one physical attempt. `prepared` is cloned from
/// immutable request bytes, preserving exact digest/route/content-type
/// identity across identical-byte retries.
#[derive(Debug, Clone)]
pub struct ProviderSubmissionObservation {
    pub request_identity: Option<crate::agent_providers::client::ProviderRequestIdentityV1>,
    pub authorization_id: Option<String>,
    pub authority_state: crate::agent_providers::client::RequestAuthorityStateV1,
    pub prepared: crate::agent_providers::client::PreparedProviderRequestV1,
    pub measured_tokens: u64,
    pub attempt: u32,
    pub state: SubmissionState,
}

/// Sink invoked once for every physical request attempt at provider boundary.
pub type SubmissionObserver = Arc<dyn Fn(ProviderSubmissionObservation) + Send + Sync>;

/// Classify the result at the actual HTTP submit boundary. HTTP responses
/// prove provider received a request: success is `Submitted`, an explicit
/// client-side refusal (4xx, 429 included) is `Rejected`; a transport error or
/// a 5xx (the provider may have run and billed the turn) is `SubmissionUnknown`.
pub fn observe_submission_attempt(
    prepared: &crate::agent_providers::client::PreparedProviderRequestV1,
    measured_tokens: u64,
    attempt: u32,
    result: Result<&crate::agent_providers::http::ProviderHttpResponse, &rightkit_http::HttpError>,
) -> ProviderSubmissionObservation {
    record_context_inspection(prepared);
    let state = match result {
        Ok(response) if response.is_success() => SubmissionState::Submitted,
        Ok(response) if (500..=599).contains(&response.status()) => {
            SubmissionState::SubmissionUnknown
        }
        Ok(_) => SubmissionState::Rejected,
        Err(_) => SubmissionState::SubmissionUnknown,
    };
    ProviderSubmissionObservation {
        request_identity: prepared.request_identity.clone(),
        authorization_id: prepared.authorization_id.clone(),
        authority_state: prepared.authority_state,
        prepared: prepared.clone(),
        measured_tokens,
        attempt,
        state,
    }
}

/// HCTX-001 qualification commitment over the exact immutable wire bytes.
/// The fixture supplies a private key; neither that key nor context is emitted.
pub fn provider_context_fingerprint(bytes: &[u8], key: &[u8]) -> String {
    let mut message = Vec::with_capacity(PROVIDER_CONTEXT_DOMAIN.len() + bytes.len());
    message.extend_from_slice(PROVIDER_CONTEXT_DOMAIN);
    message.extend_from_slice(bytes);
    format!(
        "hmac-sha256:{}",
        rightkit_crypto::hex_encode(&rightkit_crypto::hmac_sha256(key, &message))
    )
}

/// Domain-separation prefix of the HCTX-001 commitment (the MAC covers prefix then wire bytes).
const PROVIDER_CONTEXT_DOMAIN: &[u8] = b"coderight.provider-context.v1\0";

/// Fixture-only inspection hook: present in `host-env` and test builds, absent
/// from the mobile runtime.
#[cfg(not(feature = "agent_providers-host-env"))]
fn record_context_inspection(
    _prepared: &crate::agent_providers::client::PreparedProviderRequestV1,
) {
}

#[cfg(feature = "agent_providers-host-env")]
fn record_context_inspection(prepared: &crate::agent_providers::client::PreparedProviderRequestV1) {
    // Explicit fixture opt-in only: no ordinary prompt log or shadow history.
    let Some(path) = std::env::var_os(rightkit_agent_config::env_name("CONTEXT_INSPECTION_PATH"))
    else {
        return;
    };
    let Ok(key) = std::env::var(rightkit_agent_config::env_name("CONTEXT_INSPECTION_KEY")) else {
        return;
    };
    if key.len() < 32 {
        return;
    }
    let report = serde_json::json!({
        "schema": "coderight.provider-context-inspection.v1",
        "fingerprint": provider_context_fingerprint(&prepared.bytes, key.as_bytes()),
        "bodyBytes": prepared.bytes.len(),
        "serializerGeneration": prepared.serializer_generation,
        "toolCatalogGeneration": prepared.tool_catalog_generation,
    });
    // Inspection is a protected, optional qualification hook. A missing report
    // fails its caller's qualification without changing provider delivery.
    let _ = serde_json::to_vec(&report)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(path, bytes));
}

/// Whether a status is retried at stream establishment (see
/// `ProviderHostOptions::status_retries`).
fn is_retryable_stream_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504 | 529)
}

/// A retryable 5xx at stream establishment is `Rejected` rather than `SubmissionUnknown`: the
/// status line precedes any generation, so nothing was produced or billed.
fn stream_establishment_state(status: u16, base: SubmissionState) -> SubmissionState {
    if super::host::host_options().status_retries
        && (500..=599).contains(&status)
        && is_retryable_stream_status(status)
    {
        SubmissionState::Rejected
    } else {
        base
    }
}

/// [`observe_submission_attempt`] for a stream-establishment attempt. Identical unless the host
/// set `status_retries`, which makes a retryable 5xx `Rejected` (it is re-sent, never billed).
pub(crate) fn observe_stream_establishment_attempt(
    prepared: &crate::agent_providers::client::PreparedProviderRequestV1,
    measured_tokens: u64,
    attempt: u32,
    result: Result<&crate::agent_providers::http::ProviderHttpResponse, &rightkit_http::HttpError>,
) -> ProviderSubmissionObservation {
    let mut observation = observe_submission_attempt(prepared, measured_tokens, attempt, result);
    if let Ok(response) = result {
        observation.state = stream_establishment_state(response.status(), observation.state);
    }
    observation
}

/// One counter spans every transport ladder for this prepared request.
#[derive(Default)]
pub(crate) struct PhysicalAttemptCounter(u32);

impl PhysicalAttemptCounter {
    pub(crate) fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .checked_add(1)
            .expect("bounded physical request attempts");
        self.0
    }
}

/// Observed equivalent of the existing stream establishment ladder. Kept at
/// this shared boundary so both protocols retain identical retry semantics.
pub(crate) async fn send_observed_stream_request_with_retry(
    http: &rightkit_http::AsyncClient,
    mut build: impl FnMut() -> rightkit_http::Request,
    prepared: &crate::agent_providers::client::PreparedProviderRequestV1,
    observer: &SubmissionObserver,
    notifier: &rightkit_agent_engine_protocol::RetryNotifier,
    base_url: &str,
) -> Result<crate::agent_providers::http::ProviderHttpResponse, ProviderError> {
    dump_request_body(&prepared.bytes);
    let mut physical = PhysicalAttemptCounter::default();
    let mut connection_retries = 0u32;
    let mut status_retries = 0u32;
    // Replay policy of the published attempt-delivery semantics (`rightkit-http`
    // `AttemptDelivery`): this is a paid, non-idempotent POST, so an HTTP answer (429, 5xx, any
    // status) is surfaced as typed and never re-sent here, and only a failure before the request
    // left (`NotSent`: connect phase) is retried. A 429 carries its real `Retry-After` in the
    // typed error for the caller to act on.
    //
    // With the host's `status_retries`, a 429 or retryable 5xx at stream establishment is also
    // re-sent, up to MAX_STATUS_RETRIES: the status line precedes any SSE event and a streamed
    // generation answers 200 then streams, so a retryable error status means nothing was produced
    // or billed. With `quota_errors`, a quota or billing refusal (402, a 429 or 400 whose body says
    // credit is exhausted) is the exception: it is surfaced as `QuotaExceeded` at once, since the
    // same account cannot succeed.
    loop {
        let attempt = physical.next();
        let result = crate::agent_providers::http::send(http, build()).await;
        observer(observe_stream_establishment_attempt(
            prepared,
            prepared
                .measured_tokens
                .expect("prepared request admitted before send"),
            attempt,
            result.as_ref(),
        ));
        match result {
            Ok(response) if response.is_success() => return Ok(response),
            Ok(response) => {
                let status = response.status();
                let options = super::host::host_options();
                // A 429 or retryable 5xx at stream establishment arrives before any generation, so
                // nothing was billed: a transient overload or rate limit must not end a long run.
                // 429 honours the provider's `Retry-After` (capped, not jittered); the rest back
                // off about 1s, 2s, 4s (±10%). Anything longer than the cap is surfaced.
                if options.status_retries
                    && status != 429
                    && is_retryable_stream_status(status)
                    && status_retries < MAX_STATUS_RETRIES
                {
                    let delay = jittered(std::time::Duration::from_secs(1u64 << status_retries));
                    if delay <= MAX_STATUS_RETRY_DELAY {
                        // A 5xx that is re-sent never needs its body (often a large proxy page):
                        // drop it unread.
                        drop(response);
                        status_retries += 1;
                        crate::agent_providers::client::wait_status_retry(
                            notifier,
                            base_url,
                            format!("HTTP {status}"),
                            status_retries,
                            MAX_STATUS_RETRIES,
                            delay,
                        )
                        .await;
                        continue;
                    }
                }
                if !options.status_retries && !options.quota_errors {
                    if status == 429 {
                        return Err(ProviderError::RateLimited {
                            retry_after_secs: crate::agent_providers::client::retry_after_secs(
                                &response,
                            )
                            .unwrap_or(2),
                        });
                    }
                    return Err(ProviderError::Http {
                        status,
                        body: response.text().await.unwrap_or_default(),
                    });
                }
                // The `Retry-After` header must be read before `text()` consumes the response. A
                // 429 body is read before any retry because it decides between a rate limit and an
                // exhausted quota.
                let hint = if status == 429 {
                    crate::agent_providers::client::retry_after_secs(&response)
                } else {
                    None
                };
                let body = response.text().await.unwrap_or_default();
                if status == 429 && !(options.quota_errors && is_quota_exhausted_body(&body)) {
                    if options.status_retries {
                        let delay = match hint {
                            Some(secs) => std::time::Duration::from_secs(secs),
                            None => {
                                jittered(std::time::Duration::from_secs(1u64 << status_retries))
                            }
                        };
                        if status_retries < MAX_STATUS_RETRIES && delay <= MAX_STATUS_RETRY_DELAY {
                            status_retries += 1;
                            crate::agent_providers::client::wait_status_retry(
                                notifier,
                                base_url,
                                "rate limited".to_string(),
                                status_retries,
                                MAX_STATUS_RETRIES,
                                delay,
                            )
                            .await;
                            continue;
                        }
                    }
                    return Err(ProviderError::RateLimited {
                        retry_after_secs: hint.unwrap_or(2),
                    });
                }
                return Err(status_error(status, body));
            }
            Err(error) => {
                if crate::agent_providers::client::is_connect_phase_failure(&error)
                    && connection_retries < MAX_CONNECTION_RETRIES
                {
                    connection_retries += 1;
                    crate::agent_providers::client::wait_connection_retry(
                        notifier,
                        base_url,
                        connection_retries,
                        MAX_CONNECTION_RETRIES,
                    )
                    .await;
                    continue;
                }
                return Err(ProviderError::from_send_error(&error));
            }
        }
    }
}

/// `delay` with a random spread of ±10%, so concurrent clients do not retry in lockstep.
fn jittered(delay: std::time::Duration) -> std::time::Duration {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    let draw = hasher.finish() % 2001;
    delay.mul_f64(0.9 + draw as f64 / 10_000.0)
}

/// Writes the exact request body to `<PREFIX>_DUMP_REQUEST_DIR` when that variable names a
/// directory (a debugging switch, owner-only files). Present only with the `host-env` feature.
#[cfg(feature = "agent_providers-host-env")]
fn dump_request_body(bytes: &[u8]) {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Some(dir) = std::env::var_os(rightkit_agent_config::env_name("DUMP_REQUEST_DIR"))
        .filter(|dir| !dir.is_empty())
    else {
        return;
    };
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let _ = write_request_dump(
        std::path::Path::new(&dir),
        &format!("{millis}-{n}.json"),
        bytes,
    );
}

#[cfg(not(feature = "agent_providers-host-env"))]
fn dump_request_body(_bytes: &[u8]) {}

#[cfg(feature = "agent_providers-host-env")]
fn write_request_dump(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(dir.join(name))?.write_all(bytes)
}

/// Most connect-phase re-sends of one stream request (nothing had reached the server).
const MAX_CONNECTION_RETRIES: u32 = 3;
/// Most status re-sends of one stream request with `status_retries`.
const MAX_STATUS_RETRIES: u32 = 3;
/// Longest wait before a status re-send; a longer delay is surfaced to the caller.
const MAX_STATUS_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(30);

/// Wire protocol family a provider route speaks. `S:M2`/`S:M3` amendment
/// (`docs/canon/daemon-master-canon-atoms.md` ~line 156): the normalized
/// turn/transport contract must never assume OpenAI-compatibility as the
/// only shape — ordinary OpenAI-compatible routes use
/// [`ProtocolFamily::OpenAiCompatible`], ChatGPT/Codex OAuth uses
/// [`ProtocolFamily::OpenAiCodexResponses`],
/// `anthropic_client.rs` speaks [`ProtocolFamily::AnthropicNative`], and
/// this stays independent of which auth source (API key, OAuth, ambient)
/// authenticated the request. Non-exhaustive so a third family (e.g. a
/// future Gemini-native adapter) is additive, not a breaking match change
/// for downstream callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProtocolFamily {
    /// `client.rs` — OpenAI chat-completions-shaped SSE.
    OpenAiCompatible,
    /// ChatGPT/Codex OAuth — `/codex/responses` Responses SSE, not
    /// `/chat/completions`.
    OpenAiCodexResponses,
    /// `anthropic_client.rs` — Anthropic Messages-shaped SSE (content
    /// blocks, `message_start`/`message_delta`, native `thinking` blocks).
    AnthropicNative,
}

/// Rate-limit metadata surfaced from provider response headers, independent
/// of auth source. `S:M2`/`S:M3` amendment: callers get remaining budget
/// and reset timing on the normalized turn contract rather than only
/// discovering a limit once a 429 arrives. All fields optional because not
/// every route/protocol family reports every axis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitMetadata {
    /// Requests remaining in the current window (e.g. OpenAI-compatible
    /// `x-ratelimit-remaining-requests`, Anthropic
    /// `anthropic-ratelimit-requests-remaining`).
    pub remaining_requests: Option<u64>,
    /// Tokens remaining in the current window (`x-ratelimit-remaining-tokens`
    /// / `anthropic-ratelimit-tokens-remaining`).
    pub remaining_tokens: Option<u64>,
    /// Seconds until the request-count window resets.
    pub reset_requests_secs: Option<u64>,
    /// Seconds until the token-count window resets.
    pub reset_tokens_secs: Option<u64>,
}

impl RateLimitMetadata {
    pub fn is_empty(&self) -> bool {
        self.remaining_requests.is_none()
            && self.remaining_tokens.is_none()
            && self.reset_requests_secs.is_none()
            && self.reset_tokens_secs.is_none()
    }
}

/// A single SSE delta from the OpenAI chat completions stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionDelta {
    pub index: usize,
    pub id: Option<String>,
    pub role: Option<String>,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_call_name: Option<String>,
    pub tool_call_arguments_fragment: Option<String>,
    pub finish_reason: Option<String>,
}

/// Errors that can occur during provider communication.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP error {status}: {body}")]
    Http { status: u16, body: String },

    #[error("SSE parse error: {0}")]
    Sse(String),

    #[error("rate limited — retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("auth failed")]
    Auth,

    /// The account is out of quota or credit (HTTP 402, or a 400/429 whose body says so). Only
    /// produced when the host set `quota_errors`; not retried and not counted by the breaker.
    #[error("provider quota or billing limit reached: {0}")]
    QuotaExceeded(String),

    /// A transport failure at or after the request was (possibly) sent: the provider may have received, run and
    /// billed it. Never a reason to send the same paid request to another route.
    #[error("network error: {0}")]
    Network(String),

    /// The request provably never left this machine (connect-phase failure after the connect retries). Nothing was
    /// delivered or billed, so another route may be tried.
    #[error("network error (request not sent): {0}")]
    NotDelivered(String),

    #[error("provider error: {0}")]
    Provider(String),

    /// A paid request's stream had started (the provider received it) and ended, or went idle,
    /// without a completed answer. Whether the provider ran or billed the turn is unknown, so the
    /// request is never resent automatically; `delivered_output` says whether any output reached us.
    #[error("provider stream outcome uncertain ({reason}); delivered_output={delivered_output}; the request was not resent")]
    DeliveryUncertain {
        reason: String,
        delivered_output: bool,
    },

    #[error(transparent)]
    Capacity(#[from] crate::agent_providers::client::CapacityError),

    #[error(transparent)]
    Authority(#[from] crate::agent_providers::client::RequestAuthorityErrorV1),

    /// The provider stopped because its output ceiling was reached.  Partial
    /// bytes remain available to the runtime for durable evidence and one
    /// bounded continuation; callers must never report this as completion.
    #[error("provider output incomplete ({finish_reason}); continuation_attempted={continuation_attempted}")]
    IncompleteOutput {
        partial_text: String,
        partial_reasoning: Option<String>,
        partial_tool_calls: Vec<ToolCall>,
        prompt_tokens: u32,
        completion_tokens: u32,
        cache_read_tokens: u32,
        cache_creation_tokens: u32,
        finish_reason: String,
        continuation_attempted: bool,
        continuation_error: Option<String>,
    },
}

/// Whether an error body says the account is out of quota or credit, as opposed to a transient rate limit.
/// "exceeded your current quota" alone is deliberately not a signal: Gemini per-minute 429s use that
/// phrase and are retryable, so a body that mentions a retry delay or a per-minute limit is never quota
/// exhaustion.
pub fn is_quota_exhausted_body(body: &str) -> bool {
    let body = body.to_lowercase();
    if body.contains("retry in") || body.contains("perminute") || body.contains("per minute") {
        return false;
    }
    [
        "insufficient_quota",
        "billing_error",
        "credit balance is too low",
        "billing hard limit",
        "out of credits",
        "billing_hard_limit_reached",
    ]
    .iter()
    .any(|marker| body.contains(*marker))
}

/// A non-success HTTP answer as a typed error, shared by every client: with the host's
/// `quota_errors`, a 402, or a 400/429 whose body says credit or quota is exhausted, is
/// [`ProviderError::QuotaExceeded`]; anything else is [`ProviderError::Http`]. Callers that retry a
/// transient 429 or map it to [`ProviderError::RateLimited`] do so before calling this.
pub(crate) fn status_error(status: u16, body: String) -> ProviderError {
    if super::host::host_options().quota_errors
        && (status == 402 || (matches!(status, 400 | 429) && is_quota_exhausted_body(&body)))
    {
        ProviderError::QuotaExceeded(body)
    } else {
        ProviderError::Http { status, body }
    }
}

/// The provider client's error text for an answer that carried only reasoning (no visible text, no tool
/// calls). The engine loop matches the substring "no visible content or tool calls" to retry such an
/// answer, so this text must stay stable.
pub const REASONING_ONLY_MESSAGE: &str =
    "provider returned no visible content or tool calls after reasoning was separated";

impl ProviderError {
    /// A failed send as a typed error: a connect-phase failure (`NotSent`, nothing reached the provider) is
    /// [`ProviderError::NotDelivered`]; any later transport failure or timeout may have been delivered, so it is
    /// [`ProviderError::Network`] and is never replayed to another route.
    pub(crate) fn from_send_error(error: &rightkit_http::HttpError) -> Self {
        if crate::agent_providers::client::is_connect_phase_failure(error) {
            ProviderError::NotDelivered(error.to_string())
        } else {
            ProviderError::Network(error.to_string())
        }
    }
}

impl From<rightkit_http::HttpError> for ProviderError {
    fn from(e: rightkit_http::HttpError) -> Self {
        ProviderError::from_send_error(&e)
    }
}

/// Coarse failure classification driving retry/backoff and cross-route
/// failover decisions (ENG-31). `Auth` never triggers failover — a bad key
/// is bad on every route sharing it — while `RateLimit` and `Network` do,
/// since a sibling route may not be affected by the same outage or quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    Auth,
    RateLimit,
    Network,
    /// The request may have been delivered (post-send transport failure, 408/425/5xx, a stream that ended): never
    /// failed over to another route and never given a retry hint.
    Uncertain,
    Other,
}

/// Classify a [`ProviderError`] for retry/failover purposes. Callers that
/// already have a live backoff loop (e.g. the internal ladders in
/// `client.rs`/`anthropic_client.rs`) should keep using that loop; this is
/// for callers deciding whether to fail over to a *different* route once
/// their own retries are exhausted.
pub fn classify_provider_error(error: &ProviderError) -> FailureClass {
    match error {
        ProviderError::Auth => FailureClass::Auth,
        ProviderError::RateLimited { .. } => FailureClass::RateLimit,
        ProviderError::NotDelivered(_) => FailureClass::Network,
        ProviderError::Network(_)
        | ProviderError::Sse(_)
        | ProviderError::DeliveryUncertain { .. }
        | ProviderError::IncompleteOutput { .. } => FailureClass::Uncertain,
        ProviderError::Http { status, .. } if *status == 401 || *status == 403 => {
            FailureClass::Auth
        }
        ProviderError::Http { status, .. } if *status == 429 => FailureClass::RateLimit,
        ProviderError::Http { status, .. }
            if *status == 408 || *status == 425 || (500..=599).contains(status) =>
        {
            FailureClass::Uncertain
        }
        _ => FailureClass::Other,
    }
}

/// Retry-After hint in seconds, when the error carries one. Honoring this
/// (rather than a blind exponential curve) is the "retry hints" half of
/// ENG-31. `S:M2`/`S:M3` amendment: also derives a sane default hint for
/// 5xx/network failures that never sent an explicit `Retry-After`, so
/// callers do not have to special-case "no hint" as "retry immediately".
pub fn retry_after_hint(error: &ProviderError) -> Option<u64> {
    const DEFAULT_TRANSIENT_RETRY_SECS: u64 = 2;
    match error {
        ProviderError::RateLimited { retry_after_secs } => Some(*retry_after_secs),
        // Only a request that provably never left is worth a timed retry hint; an uncertain outcome
        // (post-send transport error, 5xx, 408/425) carries none, so no caller resends the paid request.
        ProviderError::NotDelivered(_) => Some(DEFAULT_TRANSIENT_RETRY_SECS),
        _ => None,
    }
}

/// Shared, documented context-overflow classification. `S:M2`/`S:M3`
/// amendment: both protocol families (`client.rs`, `anthropic_client.rs`)
/// must classify context-overflow the same way rather than each guessing
/// independently from ad hoc substring checks scattered per call site.
/// Mirrors (and is the providers-crate half of) the reactive detector at
/// `engine::overflow_recovery::is_reactive_context_overflow` (`VG-002`),
/// which classifies the already-stringified error the engine turn loop
/// sees; this is the single place providers-crate code should call before
/// constructing a generic [`ProviderError::Provider`]/[`ProviderError::Http`]
/// for an overflow condition, so the message text both layers key off of
/// stays consistent.
pub fn is_context_overflow_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let extended = super::host::host_options().extended_overflow_markers;
    if extended
        && [
            "throttling",
            "rate limit",
            "too many requests",
            "service unavailable",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return false;
    }
    let explicit_context = lower.contains("context_length_exceeded")
        || lower.contains("context window")
        || lower.contains("context length")
        || lower.contains("maximum context")
        || lower.contains("maximum input tokens")
        || lower.contains("too many tokens")
        || lower.contains("prompt_too_long")
        || lower.contains("prompt is too long")
        || lower.contains("input is too long");
    let extended_context = extended
        && (lower.contains("input is too long")
            || lower.contains("prompt too long")
            || lower.contains("prompt exceeds max length")
            || lower.contains("request_too_large")
            || lower.contains("exceeds the context window")
            || lower.contains("context window exceeded")
            || lower.contains("maximum prompt length is")
            || lower.contains("reduce the length of the messages")
            || lower.contains("maximum allowed input length")
            || lower.contains("exceeds the limit of")
            || lower.contains("is longer than the model's context length")
            || lower.contains("exceeds the available context size")
            || lower.contains("greater than the context length")
            || lower.contains("context window exceeds limit")
            || lower.contains("exceeded model token limit")
            || lower.contains("maximum context length")
            || lower.contains("model_context_window_exceeded")
            || lower.contains("range of input length should be")
            || lower.contains("token limit exceeded")
            || lower.contains("but the configured context size is")
            || (lower.contains("input token count") && lower.contains("exceeds the maximum")));
    explicit_context
        || extended_context
        || (lower.contains("413")
            && (lower.contains("prompt") || lower.contains("context") || lower.contains("token")))
}

/// Classify whether a [`ProviderError`] represents a context-overflow
/// condition, independent of protocol family. `S:M2`/`S:M3` amendment item
/// E — hand off to [`is_context_overflow_message`] against whichever
/// variant carries provider-supplied text, rather than each client
/// constructing its own overflow guess.
pub fn classify_context_overflow(error: &ProviderError) -> bool {
    match error {
        ProviderError::Http { status, body } => *status == 413 || is_context_overflow_message(body),
        ProviderError::Provider(message) => is_context_overflow_message(message),
        _ => false,
    }
}

/// Transport-layer request-shaping hooks used by constrained sampling
/// ([`crate::SchemaConstraint`] via `ChatRequest::with_response_schema`) and explicit prompt-cache breakpoint
/// placement (`prompt_cache`). Optional and additive: a caller that
/// never populates these fields gets byte-identical wire requests to before
/// this type existed.
#[derive(Debug, Clone, Default)]
pub struct RequestShaping {
    /// A JSON-schema constraint to force onto the response shape, beyond
    /// whatever tool-call schemas are already offered.
    pub json_schema_constraint: Option<serde_json::Value>,
    /// Indices into the wire message/content array that should carry an
    /// explicit cache breakpoint marker.
    pub cache_breakpoints: Vec<usize>,
}

impl RequestShaping {
    pub fn is_empty(&self) -> bool {
        self.json_schema_constraint.is_none() && self.cache_breakpoints.is_empty()
    }
}

/// Maximum time a stream may go without semantic progress (text, reasoning,
/// tool-argument bytes, or a terminal event). Keepalive comments, empty
/// frames and unknown events reset the per-chunk idle timer but NOT this one,
/// so a provider that only pings cannot hold a turn open forever.
pub(crate) const SEMANTIC_PROGRESS_SECS: u64 = 120;

pub(crate) fn semantic_progress_error(limit: std::time::Duration) -> ProviderError {
    ProviderError::Network(format!(
        "stream made no semantic progress for {}s (keepalives do not count)",
        limit.as_secs_f32()
    ))
}
