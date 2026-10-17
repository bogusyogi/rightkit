//! Usage and billing on failed attempts (Orthic-Labs/rightkit#304).

use std::ops::ControlFlow;
use std::sync::Arc;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, RetryPolicy};
use rightkit_llm::credentials::{CredentialSource, StaticCredentials};
use rightkit_llm::failover::{FailoverChain, FailoverPolicy, Route};
use rightkit_llm::openai::OpenAiCompatProvider;
use rightkit_llm::{
    ChatRequest, LlmError, Metering, Provider, Spend, StreamEvent, Usage,
};
use serde_json::json;

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
}

fn creds(id: &str) -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredentials::default().with(id, "sk-test-failed-usage"))
}

fn truncated() -> MockResponse {
    MockResponse::json(
        200,
        &json!({
            "id": "c", "model": "m",
            "choices": [{"message": {"role": "assistant", "content": "par"}, "finish_reason": "length"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 7}
        }),
    )
}

fn expected() -> Usage {
    Usage {
        input_tokens: 3,
        output_tokens: 7,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    }
}

#[test]
fn truncated_error_carries_usage_when_asked() {
    let server = MockServer::start(|_, _| truncated());
    let provider = OpenAiCompatProvider::new("p", &server.base_url(), client(), creds("p"));
    let req = ChatRequest::simple("m", "s", "u")
        .reject_truncated_output()
        .report_failed_usage();
    let err = provider.complete(&req).unwrap_err();
    assert!(err.is_truncated(), "got {err:?}");
    let billing = err.billing();
    assert_eq!(billing.spend, Spend::Reported(expected()));
    assert_eq!(billing.metering, Metering::PerToken);
    assert!(matches!(err, LlmError::Billed { .. }));
}

#[test]
fn truncated_error_is_bare_by_default_but_still_classified() {
    let server = MockServer::start(|_, _| truncated());
    let provider = OpenAiCompatProvider::new("p", &server.base_url(), client(), creds("p"));
    let req = ChatRequest::simple("m", "s", "u").reject_truncated_output();
    let err = provider.complete(&req).unwrap_err();
    assert!(matches!(err, LlmError::Truncated { .. }), "got {err:?}");
    assert_eq!(err.billing().spend, Spend::Unknown);
}

#[test]
fn refusal_with_usage_is_billed_when_asked() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &json!({
                "id": "c", "model": "m",
                "choices": [{"message": {"role": "assistant", "content": null, "refusal": "no"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 7}
            }),
        )
    });
    let provider = OpenAiCompatProvider::new("p", &server.base_url(), client(), creds("p"));
    let err = provider
        .complete(&ChatRequest::simple("m", "s", "u").report_failed_usage())
        .unwrap_err();
    assert!(matches!(err.root(), LlmError::Refused { .. }), "got {err:?}");
    assert_eq!(err.billing().spend, Spend::Reported(expected()));
}

#[test]
fn rejected_request_is_not_spent_and_server_error_is_unknown() {
    let rejected = MockServer::start(|_, _| MockResponse::text(400, "bad"));
    let provider = OpenAiCompatProvider::new("p", &rejected.base_url(), client(), creds("p"));
    let err = provider
        .complete(&ChatRequest::simple("m", "s", "u"))
        .unwrap_err();
    assert_eq!(err.billing().spend, Spend::NotSpent);

    let broken = MockServer::start(|_, _| MockResponse::text(500, "boom"));
    let provider = OpenAiCompatProvider::new("p", &broken.base_url(), client(), creds("p"));
    let err = provider
        .complete(&ChatRequest::simple("m", "s", "u"))
        .unwrap_err();
    assert_eq!(err.billing().spend, Spend::Unknown);
    assert!(err.billing().may_have_spend());
}

#[test]
fn a_cancelled_stream_reports_the_usage_it_had() {
    let frames = [
        "data: {\"choices\":[{\"delta\":{\"content\":\"a\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":7}}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"b\"},\"finish_reason\":null}]}\n\n",
        "data: [DONE]\n\n",
    ];
    let server = MockServer::start(move |_, _| MockResponse::sse(&frames));
    let provider = OpenAiCompatProvider::new("p", &server.base_url(), client(), creds("p"));
    let req = ChatRequest::simple("m", "s", "u").report_failed_usage();
    let err = provider
        .stream(&req, &mut |_: StreamEvent| ControlFlow::Break(()))
        .unwrap_err();
    assert!(matches!(err.root(), LlmError::Cancelled), "got {err:?}");
    assert_eq!(err.billing().spend, Spend::Reported(expected()));
}

#[test]
fn failover_attempts_carry_billing() {
    let down = MockServer::start(|_, _| MockResponse::text(500, "boom"));
    let chain = FailoverChain::new(
        vec![Route::new(
            "only",
            Arc::new(OpenAiCompatProvider::new(
                "only",
                &down.base_url(),
                client(),
                creds("only"),
            )),
            "m",
        )],
        FailoverPolicy::default(),
    );
    let err = chain
        .complete(&ChatRequest::simple("m", "s", "u"))
        .unwrap_err();
    match err {
        LlmError::AllRoutesFailed(attempts) => {
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].billing.spend, Spend::Unknown);
        }
        other => panic!("got {other:?}"),
    }
}
