//! Caller controls for hosts that need more than the default request shape: extra body
//! fields, per-call retry, truncation as an error, a configurable failover trigger, the NVIDIA
//! preset, key validation through `GET /models`, and the request/response observer. Runs
//! against the loopback mock server, like `e2e.rs`.

use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, RetryPolicy};
use rightkit_llm::anthropic::AnthropicProvider;
use rightkit_llm::credentials::{CredentialSource, StaticCredentials};
use rightkit_llm::failover::{FailoverChain, FailoverOn, FailoverPolicy, Route};
use rightkit_llm::openai::OpenAiCompatProvider;
use rightkit_llm::{CallRetry, ChatRequest, LlmError, Provider, StopReason, StreamEvent};
use serde_json::json;

const SENTINEL: &str = "sk-SENTINEL-do-not-leak-c41e";

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
}

fn client_with_retry(max_attempts: u32) -> Client {
    Client::new(ClientConfig {
        retry: fast_policy(max_attempts),
        ..rightkit_http::testing::loopback_config()
    })
}

/// Retries 503 with no delay, so the tests stay fast.
fn fast_policy(max_attempts: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        initial_backoff: Duration::from_millis(1),
        max_backoff: Duration::from_millis(1),
        jitter: 0.0,
        retry_statuses: vec![503],
        honor_retry_after: false,
        ..RetryPolicy::default()
    }
}

fn creds(id: &str) -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredentials::default().with(id, SENTINEL))
}

fn ok_openai() -> MockResponse {
    MockResponse::json(
        200,
        &json!({
            "id": "c1", "model": "m",
            "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }),
    )
}

fn ok_anthropic() -> MockResponse {
    MockResponse::json(
        200,
        &json!({
            "id": "msg_1", "model": "m", "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
    )
}

// ---- 1. extra body fields -------------------------------------------------------------

#[test]
fn extra_fields_are_merged_into_the_openai_body() {
    let server = MockServer::start(|_, _| ok_openai());
    let provider = OpenAiCompatProvider::new("groq", &server.base_url(), client(), creds("groq"));
    let req = ChatRequest::simple("m", "s", "u")
        .with_extra("reasoning_effort", json!("low"))
        .with_extra("include_reasoning", json!(false))
        .with_extra("provider", json!({"order": ["Azure"]}));
    provider.complete(&req).unwrap();
    let body = server.requests()[0].body_json();
    assert_eq!(body["reasoning_effort"], "low");
    assert_eq!(body["include_reasoning"], false);
    assert_eq!(body["provider"]["order"][0], "Azure");
    assert_eq!(body["model"], "m");
}

#[test]
fn extra_fields_are_merged_into_the_anthropic_body() {
    let server = MockServer::start(|_, _| ok_anthropic());
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let req = ChatRequest::simple("m", "s", "u").with_extra("metadata", json!({"user_id": "u1"}));
    provider.complete(&req).unwrap();
    assert_eq!(
        server.requests()[0].body_json()["metadata"]["user_id"],
        "u1"
    );
}

#[test]
fn extra_field_that_collides_with_a_crate_field_is_rejected_before_sending() {
    let server = MockServer::start(|_, _| ok_openai());
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::simple("m", "s", "u").with_extra("max_tokens", json!(5));
    let err = provider.complete(&req).unwrap_err();
    assert!(
        matches!(err, LlmError::InvalidRequest(ref m) if m.contains("max_tokens")),
        "got {err:?}"
    );
    assert_eq!(server.request_count(), 0);
}

#[test]
fn extra_field_that_collides_on_anthropic_is_rejected_before_sending() {
    let server = MockServer::start(|_, _| ok_anthropic());
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let req = ChatRequest::simple("m", "s", "u").with_extra("messages", json!([]));
    let err = provider.complete(&req).unwrap_err();
    assert!(matches!(err, LlmError::InvalidRequest(_)), "got {err:?}");
    assert_eq!(server.request_count(), 0);
}

#[test]
fn extra_override_replaces_a_crate_field_only_when_asked() {
    let server = MockServer::start(|_, _| ok_openai());
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::simple("m", "s", "u").with_extra_override("max_tokens", json!(5));
    provider.complete(&req).unwrap();
    assert_eq!(server.requests()[0].body_json()["max_tokens"], 5);
}

#[test]
fn with_extra_after_an_override_restores_the_collision_check() {
    let server = MockServer::start(|_, _| ok_openai());
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::simple("m", "s", "u")
        .with_extra_override("max_tokens", json!(5))
        .with_extra("max_tokens", json!(6));
    assert!(matches!(
        provider.complete(&req).unwrap_err(),
        LlmError::InvalidRequest(_)
    ));
    assert_eq!(server.request_count(), 0);
}

// ---- 2. per-call retry policy ---------------------------------------------------------

#[test]
fn default_call_keeps_the_client_retry_policy() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let provider = OpenAiCompatProvider::new(
        "openai",
        &server.base_url(),
        client_with_retry(3),
        creds("openai"),
    );
    let err = provider
        .complete(&ChatRequest::simple("m", "s", "u"))
        .unwrap_err();
    assert_eq!(err.status(), Some(503), "got {err:?}");
    assert_eq!(server.request_count(), 3);
}

#[test]
fn without_retries_makes_exactly_one_attempt_on_openai() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let provider = OpenAiCompatProvider::new(
        "openai",
        &server.base_url(),
        client_with_retry(3),
        creds("openai"),
    );
    let req = ChatRequest::simple("m", "s", "u").without_retries();
    assert!(provider.complete(&req).is_err());
    assert_eq!(server.request_count(), 1);
}

#[test]
fn without_retries_makes_exactly_one_attempt_on_anthropic() {
    let server = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let provider =
        AnthropicProvider::new(&server.base_url(), client_with_retry(3), creds("anthropic"));
    let req = ChatRequest::simple("m", "s", "u").without_retries();
    assert!(provider.complete(&req).is_err());
    assert_eq!(server.request_count(), 1);
}

#[test]
fn per_call_policy_replaces_a_client_with_no_retries_on_openai() {
    let server = MockServer::start(|_, n| {
        if n == 0 {
            MockResponse::text(503, "busy")
        } else {
            ok_openai()
        }
    });
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::simple("m", "s", "u").with_retry(CallRetry::Policy(fast_policy(3)));
    assert_eq!(provider.complete(&req).unwrap().text(), "ok");
    assert_eq!(server.request_count(), 2);
}

#[test]
fn per_call_policy_replaces_a_client_with_no_retries_on_anthropic() {
    let server = MockServer::start(|_, n| {
        if n == 0 {
            MockResponse::text(503, "busy")
        } else {
            ok_anthropic()
        }
    });
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let req = ChatRequest::simple("m", "s", "u").with_retry(CallRetry::Policy(fast_policy(3)));
    assert_eq!(provider.complete(&req).unwrap().text(), "ok");
    assert_eq!(server.request_count(), 2);
}

// ---- 3. truncation as a typed error ---------------------------------------------------

fn truncated_openai() -> MockResponse {
    MockResponse::json(
        200,
        &json!({
            "id": "c2", "model": "m",
            "choices": [{"message": {"role": "assistant", "content": "par"}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 7}
        }),
    )
}

#[test]
fn truncated_openai_output_is_a_max_tokens_answer_by_default() {
    let server = MockServer::start(|_, _| truncated_openai());
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let answer = provider
        .complete(&ChatRequest::simple("m", "s", "u"))
        .unwrap();
    assert_eq!(answer.stop_reason, StopReason::MaxTokens);
    assert_eq!(answer.text(), "par");
}

#[test]
fn reject_truncated_output_turns_openai_length_into_a_typed_error() {
    let server = MockServer::start(|_, _| truncated_openai());
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::simple("m", "s", "u").reject_truncated_output();
    let err = provider.complete(&req).unwrap_err();
    assert!(
        matches!(
            err,
            LlmError::Truncated {
                output_tokens: Some(7)
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn reject_truncated_output_turns_anthropic_max_tokens_into_a_typed_error() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({
                "id": "msg_2", "model": "m", "stop_reason": "max_tokens",
                "content": [{"type": "text", "text": "par"}],
                "usage": {"input_tokens": 1, "output_tokens": 4}
            }),
        )
    });
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let req = ChatRequest::simple("m", "s", "u").reject_truncated_output();
    let err = provider.complete(&req).unwrap_err();
    assert!(
        matches!(
            err,
            LlmError::Truncated {
                output_tokens: Some(4)
            }
        ),
        "got {err:?}"
    );
}

#[test]
fn reject_truncated_output_applies_to_a_stream() {
    let frames = [
        "data: {\"choices\":[{\"delta\":{\"content\":\"par\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":7}}\n\n",
        "data: [DONE]\n\n",
    ];
    let server = MockServer::start(move |_, _| MockResponse::sse(&frames));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::simple("m", "s", "u").reject_truncated_output();
    let err = provider
        .stream(&req, &mut |_: StreamEvent| ControlFlow::Continue(()))
        .unwrap_err();
    assert!(
        matches!(
            err,
            LlmError::Truncated {
                output_tokens: Some(7)
            }
        ),
        "got {err:?}"
    );
}

// ---- 4. failover trigger --------------------------------------------------------------

fn two_routes(primary: &MockServer, backup: &MockServer) -> Vec<Route> {
    vec![
        Route::new(
            "primary",
            Arc::new(OpenAiCompatProvider::new(
                "primary",
                &primary.base_url(),
                client(),
                creds("primary"),
            )),
            "m",
        ),
        Route::new(
            "backup",
            Arc::new(OpenAiCompatProvider::new(
                "backup",
                &backup.base_url(),
                client(),
                creds("backup"),
            )),
            "m",
        ),
    ]
}

#[test]
fn default_failover_does_not_move_an_auth_error_to_the_next_route() {
    let primary = MockServer::start(|_, _| {
        MockResponse::json(401, &json!({"error": {"message": "invalid key"}}))
    });
    let backup = MockServer::start(|_, _| ok_openai());
    let chain = FailoverChain::new(two_routes(&primary, &backup), FailoverPolicy::default());
    let err = chain
        .complete(&ChatRequest::simple("m", "s", "u"))
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Status { status: 401, .. }),
        "got {err:?}"
    );
    assert_eq!(backup.request_count(), 0);
}

#[test]
fn any_error_policy_moves_an_auth_error_to_the_next_route() {
    let primary = MockServer::start(|_, _| {
        MockResponse::json(401, &json!({"error": {"message": "invalid key"}}))
    });
    let backup = MockServer::start(|_, _| ok_openai());
    let chain = FailoverChain::new(two_routes(&primary, &backup), FailoverPolicy::default())
        .fail_over_on(FailoverOn::AnyError);
    let answer = chain.complete(&ChatRequest::simple("m", "s", "u")).unwrap();
    assert_eq!(answer.text(), "ok");
    assert_eq!(primary.request_count(), 1);
    assert_eq!(backup.request_count(), 1);
}

#[test]
fn custom_failover_predicate_is_consulted() {
    let primary = MockServer::start(|_, _| {
        MockResponse::json(401, &json!({"error": {"message": "invalid key"}}))
    });
    let backup = MockServer::start(|_, _| ok_openai());
    let chain = FailoverChain::new(two_routes(&primary, &backup), FailoverPolicy::default())
        .fail_over_on(FailoverOn::custom(|e| {
            matches!(e, LlmError::Status { status: 401, .. })
        }));
    assert_eq!(
        chain
            .complete(&ChatRequest::simple("m", "s", "u"))
            .unwrap()
            .text(),
        "ok"
    );
}

#[test]
fn any_error_policy_never_fails_over_a_rejected_request() {
    // A collision is a caller-side request problem: every route would reject it the same way.
    let primary = MockServer::start(|_, _| ok_openai());
    let backup = MockServer::start(|_, _| ok_openai());
    let chain = FailoverChain::new(two_routes(&primary, &backup), FailoverPolicy::default())
        .fail_over_on(FailoverOn::AnyError);
    let req = ChatRequest::simple("m", "s", "u").with_extra("max_tokens", json!(5));
    assert!(matches!(
        chain.complete(&req).unwrap_err(),
        LlmError::InvalidRequest(_)
    ));
    assert_eq!(backup.request_count(), 0);
}

// ---- 5. NVIDIA preset and key validation ----------------------------------------------

#[test]
fn nvidia_preset_points_at_the_documented_endpoint() {
    let preset = rightkit_llm::presets::preset("nvidia").expect("nvidia preset");
    assert_eq!(preset.base_url, "https://integrate.api.nvidia.com/v1");
    assert!(preset.requires_credential);
    let provider = rightkit_llm::presets::build("nvidia", None, client(), creds("nvidia"))
        .expect("nvidia provider");
    assert_eq!(provider.id(), "nvidia");
}

#[test]
fn list_models_returns_ids_and_sends_the_bearer_key() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({"object": "list", "data": [{"id": "alpha"}, {"object": "model"}, {"id": "beta"}]}),
        )
    });
    let provider =
        OpenAiCompatProvider::new("nvidia", &server.base_url(), client(), creds("nvidia"));
    let ids = provider.list_models().unwrap();
    assert_eq!(ids, vec!["alpha".to_string(), "beta".to_string()]);
    let requests = server.requests();
    let seen = &requests[0];
    assert_eq!(seen.method, "GET");
    assert_eq!(seen.path(), "/models");
    assert_eq!(
        seen.header("authorization"),
        Some(format!("Bearer {SENTINEL}").as_str())
    );
}

#[test]
fn rejected_key_on_models_is_a_typed_auth_error_with_the_key_removed() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            401,
            &json!({"error": {"message": format!("Incorrect API key provided: {SENTINEL}")}}),
        )
    });
    let provider =
        OpenAiCompatProvider::new("nvidia", &server.base_url(), client(), creds("nvidia"));
    let err = provider.validate_credential().unwrap_err();
    assert!(
        matches!(err, LlmError::Auth { status: 401, .. }),
        "got {err:?}"
    );
    assert_eq!(err.status(), Some(401));
    assert!(!err.to_string().contains(SENTINEL), "leaked: {err}");
}

#[test]
fn models_forbidden_is_also_an_auth_error() {
    let server =
        MockServer::start(|_, _| MockResponse::json(403, &json!({"error": {"message": "no"}})));
    let provider =
        OpenAiCompatProvider::new("nvidia", &server.base_url(), client(), creds("nvidia"));
    assert!(matches!(
        provider.list_models().unwrap_err(),
        LlmError::Auth { status: 403, .. }
    ));
}

#[test]
fn accepted_key_validates() {
    let server = MockServer::start(|_, _| MockResponse::json(200, &json!({"data": []})));
    let provider =
        OpenAiCompatProvider::new("nvidia", &server.base_url(), client(), creds("nvidia"));
    provider.validate_credential().unwrap();
    assert!(provider.list_models().unwrap().is_empty());
}

// ---- 6. request/response observer -----------------------------------------------------

type Seen = Arc<Mutex<Vec<(String, String, bool, Option<u64>)>>>;

#[test]
fn observer_sees_the_body_and_usage_but_never_the_credential() {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let server = MockServer::start(|_, _| ok_openai());
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"))
            .with_observer(move |rec| {
                sink.lock().unwrap().push((
                    rec.provider.to_string(),
                    rec.request_body.to_string(),
                    rec.result.is_ok(),
                    rec.result.ok().map(|r| r.usage.output_tokens),
                ));
            });
    let req = ChatRequest::simple("m", "s", "u").with_extra("reasoning_effort", json!("low"));
    provider.complete(&req).unwrap();

    let records = seen.lock().unwrap();
    assert_eq!(records.len(), 1);
    let (id, body, ok, output) = &records[0];
    assert_eq!(id, "openai");
    assert!(*ok);
    assert_eq!(*output, Some(1));
    assert!(
        body.contains("\"reasoning_effort\":\"low\""),
        "body: {body}"
    );
    assert!(!body.contains(SENTINEL), "credential reached the observer");
    // The key was sent on the wire, so the check above is meaningful.
    let requests = server.requests();
    assert!(requests[0]
        .header("authorization")
        .unwrap_or_default()
        .contains(SENTINEL));
}

#[test]
fn observer_sees_a_failed_call_with_the_error_text_redacted() {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            400,
            &json!({"error": {"message": format!("bad {SENTINEL}")}}),
        )
    });
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"))
            .with_observer(move |rec| {
                sink.lock().unwrap().push((
                    rec.provider.to_string(),
                    rec.request_body.to_string(),
                    rec.result.is_ok(),
                    None,
                ));
            });
    assert!(provider
        .complete(&ChatRequest::simple("m", "s", "u"))
        .is_err());
    let records = seen.lock().unwrap();
    assert_eq!(records.len(), 1);
    assert!(!records[0].2);
    assert!(!records[0].1.contains(SENTINEL));
}

#[test]
fn observer_is_called_once_for_a_stream_with_stream_flag_set() {
    let calls = Arc::new(Mutex::new(Vec::<bool>::new()));
    let sink = calls.clone();
    let frames = [
        "data: {\"id\":\"c3\",\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    ];
    let server = MockServer::start(move |_, _| MockResponse::sse(&frames));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"))
            .with_observer(move |rec| sink.lock().unwrap().push(rec.stream));
    let answer = provider
        .stream(
            &ChatRequest::simple("m", "s", "u"),
            &mut |_: StreamEvent| ControlFlow::Continue(()),
        )
        .unwrap();
    assert_eq!(answer.text(), "hi");
    assert_eq!(*calls.lock().unwrap(), vec![true]);
}

#[test]
fn observer_is_not_called_when_nothing_was_sent() {
    let calls = Arc::new(Mutex::new(0usize));
    let sink = calls.clone();
    let server = MockServer::start(|_, _| ok_openai());
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"))
            .with_observer(move |_| *sink.lock().unwrap() += 1);
    let req = ChatRequest::simple("m", "s", "u").with_extra("model", json!("x"));
    assert!(provider.complete(&req).is_err());
    assert_eq!(*calls.lock().unwrap(), 0);
    assert_eq!(server.request_count(), 0);
}
