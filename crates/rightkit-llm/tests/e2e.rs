use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, RetryPolicy};
use rightkit_llm::anthropic::AnthropicProvider;
use rightkit_llm::credentials::StaticCredentials;
use rightkit_llm::failover::{FailoverChain, FailoverPolicy, Route};
use rightkit_llm::openai::OpenAiCompatProvider;
use rightkit_llm::{ChatRequest, LlmError, Message, Provider, StreamEvent};

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..ClientConfig::default()
    })
}

fn creds(id: &str) -> Arc<StaticCredentials> {
    Arc::new(StaticCredentials::default().with(id, "test-secret"))
}

#[test]
fn anthropic_complete_sends_exact_headers_body_and_parses_tool() {
    let server = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &serde_json::json!({
                "id": "msg_1", "model": "claude-test", "stop_reason": "tool_use",
                "content": [{"type": "tool_use", "id": "call_1", "name": "lookup", "input": {"q": "rust"}}],
                "usage": {"input_tokens": 10, "output_tokens": 4}
            }),
        )
    });
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let mut req = ChatRequest::simple("claude-test", "You are concise", "Find Rust");
    req.max_tokens = 77;
    req.tools.push(rightkit_llm::ToolSpec {
        name: "lookup".into(),
        description: "Find item".into(),
        input_schema: serde_json::json!({"type":"object"}),
    });
    let answer = provider.complete(&req).unwrap();
    assert_eq!(answer.tool_input("lookup").unwrap()["q"], "rust");
    let seen = &server.requests()[0];
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.path(), "/v1/messages");
    assert_eq!(seen.header("x-api-key"), Some("test-secret"));
    assert_eq!(seen.header("anthropic-version"), Some("2023-06-01"));
    let body = seen.body_json();
    assert_eq!(body["model"], "claude-test");
    assert_eq!(body["max_tokens"], 77);
    assert_eq!(body["system"], "You are concise");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
}

#[test]
fn anthropic_stream_assembles_text_and_tool_input() {
    let frames = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"model\":\"claude\",\"usage\":{\"input_tokens\":3}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Hi\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" there\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"lookup\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"q\\\":\\\"x\\\"}\"}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];
    let server = MockServer::start(move |_, _| MockResponse::sse(&frames));
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let mut events = Vec::new();
    let answer = provider
        .stream(
            &ChatRequest::new("claude", vec![Message::user("go")]),
            &mut |e| {
                events.push(e);
                ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert_eq!(answer.text(), "Hi there");
    assert_eq!(answer.tool_input("lookup").unwrap()["q"], "x");
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::Text(t) if t == " there")));
    assert!(events
        .iter()
        .any(|e| matches!(e, StreamEvent::ToolInputDelta { .. })));
}

#[test]
fn openai_stream_sends_exact_request_and_normalizes_usage() {
    let frames = vec![
        "data: {\"id\":\"chat_1\",\"model\":\"gpt-test\",\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chat_1\",\"model\":\"gpt-test\",\"choices\":[{\"delta\":{\"content\":\"!\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4,\"prompt_tokens_details\":{\"cached_tokens\":2}}}\n\n",
        "data: [DONE]\n\n",
    ];
    let server = MockServer::start(move |_, _| MockResponse::sse(&frames));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::simple("gpt-test", "system", "user");
    let mut text = String::new();
    let answer = provider
        .stream(&req, &mut |e| {
            if let StreamEvent::Text(t) = e {
                text.push_str(&t);
            }
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(text, "Hello!");
    assert_eq!(answer.usage.input_tokens, 10);
    assert_eq!(answer.usage.cache_read_tokens, 2);
    let body = server.requests()[0].body_json();
    assert_eq!(
        body["messages"][0],
        serde_json::json!({"role":"system","content":"system"})
    );
    assert_eq!(
        body["messages"][1],
        serde_json::json!({"role":"user","content":"user"})
    );
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[test]
fn failover_retries_retryable_route_but_surfaces_auth_and_midstream_errors() {
    let bad = MockServer::start(|_, _| MockResponse::text(503, "busy"));
    let good = MockServer::start(|_, _| {
        MockResponse::json(
            200,
            &serde_json::json!({
                "id":"ok", "model":"gpt", "choices":[{"message":{"role":"assistant","content":"yes"},"finish_reason":"stop"}], "usage":{}
            }),
        )
    });
    let first = Arc::new(OpenAiCompatProvider::new(
        "one",
        &bad.base_url(),
        client(),
        creds("one"),
    ));
    let second = Arc::new(OpenAiCompatProvider::new(
        "two",
        &good.base_url(),
        client(),
        creds("two"),
    ));
    let chain = FailoverChain::new(
        vec![
            Route::new("one", first, "gpt"),
            Route::new("two", second, "gpt"),
        ],
        FailoverPolicy {
            breaker_threshold: 10,
            ..FailoverPolicy::default()
        },
    );
    assert_eq!(
        chain
            .complete(&ChatRequest::new("ignored", vec![Message::user("x")]))
            .unwrap()
            .text(),
        "yes"
    );
    assert_eq!(bad.request_count(), 1);
    assert_eq!(good.request_count(), 1);

    let auth = MockServer::start(|_, _| MockResponse::text(401, "bad key"));
    let auth_provider =
        OpenAiCompatProvider::new("auth", &auth.base_url(), client(), creds("auth"));
    assert!(
        matches!(auth_provider.complete(&ChatRequest::new("g", vec![Message::user("x")])), Err(LlmError::Http(e)) if e.status() == Some(401))
    );

    let mid = MockServer::start(|_, _| {
        MockResponse::sse(&[
        "data: {\"id\":\"m\",\"model\":\"g\",\"choices\":[{\"delta\":{\"content\":\"first\"},\"finish_reason\":null}]}\n\n",
        "data: {bad-json}\n\n",
    ])
    });
    let mid_provider = OpenAiCompatProvider::new("mid", &mid.base_url(), client(), creds("mid"));
    let mut seen = false;
    let result = mid_provider.stream(
        &ChatRequest::new("g", vec![Message::user("x")]),
        &mut |_| {
            seen = true;
            ControlFlow::Continue(())
        },
    );
    assert!(seen);
    assert!(matches!(result, Err(LlmError::MidStream(_))));
}

#[test]
fn failover_first_token_deadline_and_breaker_skip_unhealthy_route() {
    let slow = MockServer::start(|_, _| {
        MockResponse::sse(&[
            "data: {\"id\":\"slow\",\"model\":\"g\",\"choices\":[{\"delta\":{\"content\":\"late\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ]).delay(Duration::from_millis(150))
    });
    let good = MockServer::start(|_, _| {
        MockResponse::sse(&[
        "data: {\"id\":\"good\",\"model\":\"g\",\"choices\":[{\"delta\":{\"content\":\"fast\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    ])
    });
    let slow_provider = Arc::new(OpenAiCompatProvider::new(
        "slow",
        &slow.base_url(),
        client(),
        creds("slow"),
    ));
    let good_provider = Arc::new(OpenAiCompatProvider::new(
        "good",
        &good.base_url(),
        client(),
        creds("good"),
    ));
    let chain = FailoverChain::new(
        vec![
            Route::new("slow", slow_provider, "g"),
            Route::new("good", good_provider, "g"),
        ],
        FailoverPolicy {
            first_token_timeout: Duration::from_millis(20),
            breaker_threshold: 1,
            breaker_cool_down: Duration::from_secs(600),
            ..FailoverPolicy::default()
        },
    );
    let req = ChatRequest::new("g", vec![Message::user("x")]);
    let mut events = Vec::new();
    chain
        .stream(&req, &mut |e| {
            events.push(e);
            ControlFlow::Continue(())
        })
        .unwrap();
    assert!(!events.is_empty());
    assert_eq!(slow.request_count(), 1);
    chain
        .stream(&req, &mut |_| ControlFlow::Continue(()))
        .unwrap();
    assert_eq!(slow.request_count(), 1, "breaker skipped cooling route");
    assert_eq!(good.request_count(), 2);
}
