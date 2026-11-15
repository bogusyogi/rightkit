//! Names and behaviour rules the host supplies to the provider layer, each fixed once at start-up.
//!
//! Identity (the display name, the OS keychain service, the OpenRouter attribution headers) has
//! one setter each. Behaviour that a host may want different from the defaults is one struct,
//! [`ProviderHostOptions`], installed with [`set_host_options`]. Every setter returns `false` when
//! a value was already installed (or the value was read first), and every default reproduces what
//! the layer did before the setter existed, so a host that calls nothing sees no change.
//!
//! Environment-variable names that this layer reads come from
//! `rightkit_agent_config::set_env_prefix`, and the state folder from
//! `rightkit_agent_config::set_state_dir_name` / `set_app_name`; this module holds what is left.

use std::sync::OnceLock;

/// OS keychain service that holds provider keys, account keys and the subscription tokens
/// (`<service>.subscription_auth`) until the host calls [`set_keychain_service`].
pub const DEFAULT_KEYCHAIN_SERVICE: &str = "rightkit-agent";

static APP_LABEL: OnceLock<String> = OnceLock::new();
static KEYCHAIN_SERVICE: OnceLock<String> = OnceLock::new();
static OPENROUTER_ATTRIBUTION: OnceLock<(String, String)> = OnceLock::new();
static OPTIONS: OnceLock<ProviderHostOptions> = OnceLock::new();
static CONTEXT_WINDOW: OnceLock<fn(&str) -> Option<u64>> = OnceLock::new();

/// Sets the product name this layer puts in its `User-Agent` (`<label>/0.1`), in the OpenRouter
/// `X-Title` header and in the messages it shows (for example `CodeRight`). Without a call the
/// `User-Agent` product is `Agent`, the `X-Title` is `Agent` and the messages say `Host`. Call
/// once at start-up; returns `false` when one was already set.
pub fn set_app_label(label: &str) -> bool {
    APP_LABEL.set(label.to_string()).is_ok()
}

/// The label set by [`set_app_label`], when there is one.
pub fn app_label() -> Option<&'static str> {
    APP_LABEL.get().map(String::as_str)
}

/// The label for text that read `Host` before the setter existed.
pub(crate) fn message_label() -> &'static str {
    app_label().unwrap_or("Host")
}

/// The product token of the `User-Agent` and the `X-Title`.
pub(crate) fn user_agent_product() -> &'static str {
    app_label().unwrap_or("Agent")
}

/// Sets the OS keychain service (for example `myapp`) that stores provider keys, account keys,
/// `<service>.subscription_auth` tokens and the command-code sign-in entry
/// (`<service>-commandcode-api-key`); the GitHub client identifies itself as
/// `<service>-providers/0.1`. Keys are stored under it, so a host that changes it later loses
/// the keys stored under the old name. Call once at start-up; returns `false` when one was
/// already set.
pub fn set_keychain_service(service: &str) -> bool {
    KEYCHAIN_SERVICE.set(service.to_string()).is_ok()
}

/// The keychain service in force.
pub fn keychain_service() -> &'static str {
    KEYCHAIN_SERVICE
        .get()
        .map(String::as_str)
        .unwrap_or(DEFAULT_KEYCHAIN_SERVICE)
}

/// Sets the `HTTP-Referer` and `X-Title` attribution headers sent to OpenRouter (and, unless
/// [`ProviderHostOptions::attribution_for_openrouter_only`] is set, to every other
/// OpenAI-compatible endpoint). Without a call the referer is
/// `https://github.com/adrdsouza/rightkit-agent` and the title is the `User-Agent` product
/// (`Agent`, or the label from [`set_app_label`]). Call once at start-up; returns `false` when
/// one was already set.
pub fn set_openrouter_attribution(referer: &str, title: &str) -> bool {
    OPENROUTER_ATTRIBUTION
        .set((referer.to_string(), title.to_string()))
        .is_ok()
}

pub(crate) fn openrouter_attribution() -> (&'static str, &'static str) {
    match OPENROUTER_ATTRIBUTION.get() {
        Some((referer, title)) => (referer.as_str(), title.as_str()),
        None => (
            "https://github.com/adrdsouza/rightkit-agent",
            user_agent_product(),
        ),
    }
}

/// Behaviour rules for the provider layer. `Default` is what the layer did before each field
/// existed; set a field to opt in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderHostOptions {
    /// Stream establishment retries a retryable status (429 that is not an exhausted quota, 500,
    /// 502, 503, 504, 529) up to three times with a jittered one-second-doubling delay of at most
    /// 30 s (or the `Retry-After` hint for 429), telling the retry notifier before each wait. By
    /// default the status is returned at once.
    pub status_retries: bool,
    /// A 402, or a 400/429 whose body names an exhausted quota or billing limit, becomes
    /// `ProviderError::QuotaExceeded` (not retried, counted by the breaker) instead of
    /// `ProviderError::Http` / `RateLimited`. Anthropic `billing_error` stream events map to it.
    pub quota_errors: bool,
    /// Context-overflow detection also recognises the wording of more vendors and refuses to
    /// treat throttling or "service unavailable" bodies as overflow.
    pub extended_overflow_markers: bool,
    /// The circuit breaker is keyed by base URL and model (`<base>#<model>`), so a rate limit on
    /// one model of a gateway does not refuse another. By default the key is the base URL.
    pub breaker_per_model: bool,
    /// Fraction of random spread (`0.0..1.0`) applied to the connection-error backoff delay.
    pub connection_backoff_jitter: f64,
    /// Provider streams have no whole-stream lifetime cap (the per-chunk semantic-progress idle
    /// timeout still applies). By default a stream is cut at the 180 s request ceiling.
    pub uncapped_stream_lifetime: bool,
    /// The semantic-progress idle timeout also covers the Codex Responses stream and the
    /// Anthropic body collector.
    pub idle_timeout_everywhere: bool,
    /// Anthropic SSE bodies may be up to 64 MiB (default 4 MiB).
    pub large_sse_limit: bool,
    /// The OpenAI-compatible and Anthropic streams emit `ProviderStreamEvent::UsageCost` (a
    /// provider-reported cost) and `ProviderStreamEvent::RateLimit` (rate-limit headers).
    pub usage_and_rate_limit_events: bool,
    /// Streams emit `ProviderStreamEvent::ToolArgumentsPartial` previews of a call's arguments.
    pub partial_tool_argument_events: bool,
    /// A tool call cut by a length finish reason is dropped and the turn is reported as
    /// incomplete (so the engine's larger-cap retry is reached) instead of failing on the
    /// half-written call; an Anthropic tool call that streamed empty arguments is sent as `{}`.
    pub truncated_tool_call_recovery: bool,
    /// A Codex Responses turn returns its tool calls in the order the stream announced them
    /// (default: sorted by call id).
    pub ordered_codex_tool_calls: bool,
    /// Signed Anthropic thinking and encrypted Codex reasoning are kept in a file under the state
    /// root so a restart or a resumed session still replays them (default: Anthropic in memory
    /// only, Codex not kept).
    pub persist_reasoning: bool,
    /// Encrypted Codex reasoning items are replayed with the next request.
    pub replay_codex_reasoning: bool,
    /// Wire history is repaired for the target route before sending (foreign reasoning dropped,
    /// tool-call ids normalised, unanswered calls closed, images replaced when the model takes
    /// none); Anthropic requests normalise tool-call ids.
    pub history_repair: bool,
    /// Anthropic requests drop blank text blocks and trailing thinking blocks of an assistant
    /// turn, merge `oneOf`/`anyOf`/`allOf` tool schemas into one object schema, drop duplicate
    /// tool names, mark cache breakpoints on stable messages only (two of them when the budget
    /// allows) and cap `max_tokens` at the model's known maximum.
    pub anthropic_request_hygiene: bool,
    /// A reasoning effort of `max` is sent as `xhigh` on chat routes, and reasoning fields are
    /// sent only for models whose metadata row allows reasoning (default: sent for every model).
    pub chat_request_rules: bool,
    /// OpenRouter attribution headers are sent only to OpenRouter hosts (default: every
    /// OpenAI-compatible endpoint).
    pub attribution_for_openrouter_only: bool,
    /// Model lookup also resolves `claude-<family>-<major>-<minor>` to a dotted row and a bare
    /// model name to the one provider-prefixed row that ends with it, and a model with no row
    /// takes its context window from the host's [`set_context_window_resolver`].
    pub lenient_model_lookup: bool,
    /// The embedded model table also lists the Claude 5.x rows (fable-5-1, fable-5-2, opus-5-5,
    /// sonnet-5-5, haiku-5-5) with their long-context tiers.
    pub extra_model_rows: bool,
    /// Provider keys and API-key account secrets are read, saved and removed through the host's
    /// credential layer (`host_credentials`): the OS store on a desktop, the process environment
    /// and `<state root>/.env` on a host with none, with no environment fallback after a store
    /// miss. The first use probes the OS store and removes the credential variables from this
    /// process's environment. By default the OS keychain is read, then the environment.
    pub host_credential_store: bool,
    /// The OS keychain disable switch (`<PREFIX>_DISABLE_OS_KEYCHAIN`) is ignored.
    pub ignore_keychain_disable_env: bool,
    /// The "capacity unavailable" error reads `model capacity unavailable for route ...` instead
    /// of `CRA-10 capacity unavailable for route ...`.
    pub neutral_capacity_wording: bool,
    /// A routing-policy preset name (`agent_router`) matches ignoring case and surrounding
    /// whitespace (default: the exact lower-case name only; anything else is `conservative`).
    pub lenient_policy_names: bool,
}

impl ProviderHostOptions {
    /// The options in force before any host call.
    pub const DEFAULT: Self = Self {
        status_retries: false,
        quota_errors: false,
        extended_overflow_markers: false,
        breaker_per_model: false,
        connection_backoff_jitter: 0.0,
        uncapped_stream_lifetime: false,
        idle_timeout_everywhere: false,
        large_sse_limit: false,
        usage_and_rate_limit_events: false,
        partial_tool_argument_events: false,
        truncated_tool_call_recovery: false,
        ordered_codex_tool_calls: false,
        persist_reasoning: false,
        replay_codex_reasoning: false,
        history_repair: false,
        anthropic_request_hygiene: false,
        chat_request_rules: false,
        attribution_for_openrouter_only: false,
        lenient_model_lookup: false,
        extra_model_rows: false,
        host_credential_store: false,
        ignore_keychain_disable_env: false,
        neutral_capacity_wording: false,
        lenient_policy_names: false,
    };

    /// Every option on, with the connection jitter CodeRight ships (`0.1`).
    pub const ALL: Self = Self {
        status_retries: true,
        quota_errors: true,
        extended_overflow_markers: true,
        breaker_per_model: true,
        connection_backoff_jitter: 0.1,
        uncapped_stream_lifetime: true,
        idle_timeout_everywhere: true,
        large_sse_limit: true,
        usage_and_rate_limit_events: true,
        partial_tool_argument_events: true,
        truncated_tool_call_recovery: true,
        ordered_codex_tool_calls: true,
        persist_reasoning: true,
        replay_codex_reasoning: true,
        history_repair: true,
        anthropic_request_hygiene: true,
        chat_request_rules: true,
        attribution_for_openrouter_only: true,
        lenient_model_lookup: true,
        extra_model_rows: true,
        host_credential_store: true,
        ignore_keychain_disable_env: true,
        neutral_capacity_wording: true,
        lenient_policy_names: true,
    };
}

impl Default for ProviderHostOptions {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Installs the behaviour rules. Call once at start-up, before any request is built or model
/// looked up; returns `false` when options were already installed (or already read, which fixes
/// the defaults for the life of the process).
pub fn set_host_options(options: ProviderHostOptions) -> bool {
    OPTIONS.set(options).is_ok()
}

/// The behaviour rules in force. The first call without an earlier [`set_host_options`] fixes
/// the defaults for the life of the process.
pub fn host_options() -> &'static ProviderHostOptions {
    OPTIONS.get_or_init(|| ProviderHostOptions::DEFAULT)
}

/// Sets the lookup for a model's context window, in tokens, when the model has no row in the
/// metadata table (for example the host's `[model-limits.<route or model>]` configuration). It is
/// consulted only when [`ProviderHostOptions::lenient_model_lookup`] is set; without a call, or when
/// it returns `None`, a model with no row has no capacity profile and a send fails with a typed
/// capacity error. Call once at start-up; returns `false` when one was already set.
pub fn set_context_window_resolver(resolver: fn(&str) -> Option<u64>) -> bool {
    CONTEXT_WINDOW.set(resolver).is_ok()
}

/// The configured context window of `model`, when the host supplied a resolver and it knows one.
pub(crate) fn context_window_for(model: &str) -> Option<u64> {
    CONTEXT_WINDOW.get().and_then(|resolver| resolver(model))
}
