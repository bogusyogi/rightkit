//! Per-call options and error surface: omitted `max_tokens`, `temperature`, per-call timeout,
//! typed HTTP status with bounded provider text, and credential redaction in every
//! formatted output. Runs against the loopback mock server, like `e2e.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, HttpError, RetryPolicy};
use rightkit_llm::anthropic::AnthropicProvider;
use rightkit_llm::credentials::{CredentialSource, StaticCredentials};
use rightkit_llm::openai::OpenAiCompatProvider;
use rightkit_llm::{Attempt, ChatRequest, LlmError, Provider, StreamEvent};

const SENTINEL: &str = "sk-SENTINEL-do-not-leak-7f3a9c";

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
}

fn client_with_response_timeout(d: Duration) -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        response_timeout: Some(d),
        ..rightkit_http::testing::loopback_config()
    })
}

fn creds(id: &str, key: &str) -> Arc<StaticCredentials> {
    Arc::new(StaticCredentials::default().with(id, key))
}

fn ok_openai() -> MockResponse {
    MockResponse::json(
        200,
        &serde_json::json!({
            "id": "c1", "model": "m",
            "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }),
    )
}

fn ok_anthropic() -> MockResponse {
    MockResponse::json(
        200,
        &serde_json::json!({
            "id": "msg_1", "model": "m", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
    )
}

// ---- 1. max_tokens -------------------------------------------------------------------

#[test]
fn default_request_still_sends_max_tokens_1024() {
    let server = MockServer::start(|_, _| ok_openai());
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", "k"));
    provider
        .complete(&ChatRequest::simple("m", "s", "u"))
        .unwrap();
    let body = server.requests()[0].body_json();
    assert_eq!(body["max_tokens"], 1024);
}

#[test]
fn openai_omits_max_tokens_field_when_requested() {
    let server = MockServer::start(|_, _| ok_openai());
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", "k"));
    let req = ChatRequest::simple("m", "s", "u").without_max_tokens();
    provider.complete(&req).unwrap();
    let body = server.requests()[0].body_json();
    let obj = body.as_object().unwrap();
    assert!(!obj.contains_key("max_tokens"), "body: {body}");
    assert!(!obj.contains_key("max_completion_tokens"), "body: {body}");
}

#[test]
fn openai_omits_max_completion_tokens_field_when_requested() {
    use rightkit_llm::openai::MaxTokensField;
    let server = MockServer::start(|_, _| ok_openai());
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", "k"))
        .with_max_tokens_field(MaxTokensField::MaxCompletionTokens);
    provider
        .complete(&ChatRequest::simple("m", "s", "u").without_max_tokens())
        .unwrap();
    let body = server.requests()[0].body_json();
    assert!(!body.as_object().unwrap().contains_key("max_completion_tokens"), "body: {body}");
}

#[test]
fn with_max_tokens_restores_an_explicit_value_after_omission() {
    let server = MockServer::start(|_, _| ok_openai());
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", "k"));
    let req = ChatRequest::simple("m", "s", "u")
        .without_max_tokens()
        .with_max_tokens(77);
    provider.complete(&req).unwrap();
    assert_eq!(server.requests()[0].body_json()["max_tokens"], 77);
}

#[test]
fn anthropic_keeps_sending_max_tokens_when_omitted() {
    // The Messages API requires max_tokens, so omission is ignored there: the value the
    // request carries (1024 unless the caller set it) is still sent.
    let server = MockServer::start(|_, _| ok_anthropic());
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic", "k"));
    provider
        .complete(&ChatRequest::simple("m", "s", "u").without_max_tokens())
        .unwrap();
    assert_eq!(server.requests()[0].body_json()["max_tokens"], 1024);
}

// ---- 2. temperature ------------------------------------------------------------------

#[test]
fn temperature_is_settable_on_a_simple_request() {
    let server = MockServer::start(|_, _| ok_openai());
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", "k"));
    provider
        .complete(&ChatRequest::simple("m", "s", "u").with_temperature(0.3))
        .unwrap();
    let t = server.requests()[0].body_json()["temperature"].as_f64().unwrap();
    assert!((t - 0.3).abs() < 1e-6, "temperature was {t}");
}

#[test]
fn temperature_is_omitted_by_default() {
    let server = MockServer::start(|_, _| ok_anthropic());
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic", "k"));
    provider.complete(&ChatRequest::simple("m", "s", "u")).unwrap();
    assert!(!server.requests()[0].body_json().as_object().unwrap().contains_key("temperature"));
}

// ---- 3. per-call timeout -------------------------------------------------------------

#[test]
fn per_call_timeout_fails_a_slow_server_with_the_timeout_error() {
    let server = MockServer::start(|_, _| ok_openai().delay(Duration::from_millis(1500)));
    let provider = OpenAiCompatProvider::new("local", &server.base_url(), client(), creds("local", "k"));
    let started = Instant::now();
    let err = provider
        .complete(&ChatRequest::simple("m", "s", "u").with_timeout(Duration::from_millis(200)))
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Http(HttpError::Timeout { .. })),
        "got {err:?}"
    );
    assert!(started.elapsed() < Duration::from_millis(1200), "took {:?}", started.elapsed());
}

#[test]
fn per_call_timeout_lets_a_slow_server_succeed_within_budget() {
    let server = MockServer::start(|_, _| ok_openai().delay(Duration::from_millis(300)));
    let provider = OpenAiCompatProvider::new("local", &server.base_url(), client(), creds("local", "k"));
    let answer = provider
        .complete(&ChatRequest::simple("m", "s", "u").with_timeout(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(answer.text(), "ok");
}

#[test]
fn per_call_timeout_is_capped_by_client_response_timeout() {
    // Documented limit: the header wait is also bounded by ClientConfig::response_timeout
    // (60 s by default), which a per-call timeout does not raise. Apps with slower models
    // raise response_timeout on the client they build.
    let server = MockServer::start(|_, _| ok_anthropic().delay(Duration::from_millis(1000)));
    let provider = AnthropicProvider::new(
        &server.base_url(),
        client_with_response_timeout(Duration::from_millis(200)),
        creds("anthropic", "k"),
    );
    let err = provider
        .complete(&ChatRequest::simple("m", "s", "u").with_timeout(Duration::from_secs(10)))
        .unwrap_err();
    assert!(matches!(err, LlmError::Http(HttpError::Timeout { .. })), "got {err:?}");
}

#[test]
fn per_call_timeout_on_stream_bounds_the_stream_open() {
    let server = MockServer::start(|_, _| {
        MockResponse::sse(&["data: [DONE]\n\n"]).delay(Duration::from_millis(1500))
    });
    let provider = OpenAiCompatProvider::new("local", &server.base_url(), client(), creds("local", "k"));
    let err = provider
        .stream(
            &ChatRequest::simple("m", "s", "u").with_timeout(Duration::from_millis(200)),
            &mut |_: StreamEvent| std::ops::ControlFlow::Continue(()),
        )
        .unwrap_err();
    assert!(matches!(err, LlmError::Http(HttpError::Timeout { .. })), "got {err:?}");
}

// ---- 4. typed HTTP status with bounded provider text --------------------------------

#[test]
fn a_429_is_a_typed_status_error_carrying_the_provider_message() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            429,
            &serde_json::json!({"error": {"type": "rate_limit_error", "message": "Rate limit reached for m"}}),
        )
    });
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", "k"));
    let err = provider.complete(&ChatRequest::simple("m", "s", "u")).unwrap_err();
    match &err {
        LlmError::Status { status, message } => {
            assert_eq!(*status, 429);
            assert_eq!(message, "Rate limit reached for m");
        }
        other => panic!("expected LlmError::Status, got {other:?}"),
    }
    assert_eq!(err.status(), Some(429));
    assert!(err.to_string().contains("429"), "display: {err}");
    assert!(err.is_failover_trigger());
}

#[test]
fn a_status_error_message_is_bounded_and_falls_back_to_raw_body() {
    let long = "x".repeat(5000);
    let server = MockServer::start(move |_, _| MockResponse::text(502, &long));
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic", "k"));
    let err = provider.complete(&ChatRequest::simple("m", "s", "u")).unwrap_err();
    match err {
        LlmError::Status { status, message } => {
            assert_eq!(status, 502);
            assert!(!message.is_empty());
            assert!(message.len() <= 512, "len {}", message.len());
        }
        other => panic!("expected LlmError::Status, got {other:?}"),
    }
}

#[test]
fn a_streamed_non_2xx_is_a_typed_status_error() {
    let server = MockServer::start(|_, _| MockResponse::text(429, "slow down"));
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", "k"));
    let err = provider
        .stream(
            &ChatRequest::simple("m", "s", "u"),
            &mut |_: StreamEvent| std::ops::ControlFlow::Continue(()),
        )
        .unwrap_err();
    assert!(matches!(err, LlmError::Status { status: 429, .. }), "got {err:?}");
}

// ---- 5. credentials never appear in formatted output ---------------------------------

/// A caller-side credential source, the shape an app implements over its own store.
struct AppStore;
impl CredentialSource for AppStore {
    fn credential(&self, _provider: &str) -> Result<Option<rightkit_http::Secret>, LlmError> {
        Ok(Some(rightkit_http::Secret::new(SENTINEL)))
    }
}

#[test]
fn a_caller_credential_source_is_usable_and_its_key_is_redacted_from_errors() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            401,
            &serde_json::json!({"error": {"message": format!("Incorrect API key provided: {SENTINEL}")}}),
        )
    });
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), Arc::new(AppStore));
    let err = provider.complete(&ChatRequest::simple("m", "s", "u")).unwrap_err();
    assert_eq!(server.requests()[0].header("authorization"), Some(format!("Bearer {SENTINEL}").as_str()));
    assert!(!format!("{err}").contains(SENTINEL), "display: {err}");
    assert!(!format!("{err:?}").contains(SENTINEL), "debug: {err:?}");
}

#[test]
fn anthropic_key_is_redacted_when_the_provider_echoes_it() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            401,
            &serde_json::json!({"type": "error", "error": {"type": "authentication_error", "message": format!("invalid x-api-key {SENTINEL}")}}),
        )
    });
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic", SENTINEL));
    let err = provider.complete(&ChatRequest::simple("m", "s", "u")).unwrap_err();
    assert!(!format!("{err}").contains(SENTINEL), "display: {err}");
    assert!(!format!("{err:?}").contains(SENTINEL), "debug: {err:?}");
}

#[test]
fn stream_error_event_echoing_the_key_is_redacted() {
    let frame = format!(
        "data: {{\"error\":{{\"type\":\"invalid_request_error\",\"message\":\"bad key {SENTINEL}\"}}}}\n\n"
    );
    let server = MockServer::start(move |_, _| MockResponse::sse(&[frame.as_str()]));
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", SENTINEL));
    let err = provider
        .stream(
            &ChatRequest::simple("m", "s", "u"),
            &mut |_: StreamEvent| std::ops::ControlFlow::Continue(()),
        )
        .unwrap_err();
    assert!(matches!(err, LlmError::Provider { .. }), "got {err:?}");
    assert!(!format!("{err}").contains(SENTINEL), "display: {err}");
    assert!(!format!("{err:?}").contains(SENTINEL), "debug: {err:?}");
}

#[test]
fn every_error_variant_and_provider_and_credential_debug_omit_the_sentinel() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(401, &serde_json::json!({"error": {"message": format!("bad {SENTINEL}")}}))
    });
    let provider = OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai", SENTINEL));
    let real = provider.complete(&ChatRequest::simple("m", "s", "u")).unwrap_err();
    let anthropic = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic", SENTINEL));

    let variants: Vec<LlmError> = vec![
        real,
        LlmError::Status { status: 429, message: "slow".into() },
        LlmError::Http(HttpError::Status { status: 500, body: "x".into(), retry_after: None, attempts: 1 }),
        LlmError::Http(HttpError::Timeout { message: "t".into(), phase: rightkit_http::Phase::Headers }),
        LlmError::Http(HttpError::Transport { message: "t".into(), connect_phase: true, phase: rightkit_http::Phase::Connect }),
        LlmError::Http(HttpError::Stream("s".into())),
        LlmError::MissingCredential("openai".into()),
        LlmError::Malformed("m".into()),
        LlmError::MidStream("m".into()),
        LlmError::Provider { kind: "k".into(), message: "m".into() },
        LlmError::Cancelled,
        LlmError::AllRoutesFailed(vec![Attempt { route: "openai".into(), reason: "HTTP 429".into() }]),
    ];
    for e in &variants {
        assert!(!format!("{e}").contains(SENTINEL), "display: {e}");
        assert!(!format!("{e:?}").contains(SENTINEL), "debug: {e:?}");
    }
    let debug = format!(
        "{:?} {:?} {:?} {:?} {:?}",
        provider,
        anthropic,
        StaticCredentials::default().with("openai", SENTINEL),
        ChatRequest::simple("m", "s", "u").with_timeout(Duration::from_secs(1)),
        AppStore.credential("openai").unwrap().unwrap(),
    );
    assert!(!debug.contains(SENTINEL), "debug: {debug}");
}

// ---- 6. documented defaults ----------------------------------------------------------

#[test]
fn client_defaults_are_unchanged_no_redirects_and_no_proxy() {
    let config = ClientConfig::default();
    assert_eq!(config.max_redirects, 0);
    assert_eq!(config.request_timeout, Duration::from_secs(60));
    assert!(matches!(config.proxy, rightkit_http::ProxyMode::None));
}
