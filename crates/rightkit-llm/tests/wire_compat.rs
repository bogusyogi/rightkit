//! Wire-level behaviour the MailRight port relies on: empty user messages, `choices`
//! shape errors, null completions, refusals, and the exact `temperature` number on the wire.
//! Runs against the loopback mock server, like `call_options.rs`.

use std::sync::Arc;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, RetryPolicy};
use rightkit_llm::anthropic::AnthropicProvider;
use rightkit_llm::credentials::{CredentialSource, StaticCredentials};
use rightkit_llm::openai::OpenAiCompatProvider;
use rightkit_llm::{ChatRequest, ChatResponse, LlmError, Message, Provider};
use serde_json::{json, Value};

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
}

fn creds(id: &str) -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredentials::default().with(id, "k"))
}

fn openai_body(choices: Value) -> Value {
    json!({"id": "c1", "model": "m", "choices": choices,
           "usage": {"prompt_tokens": 1, "completion_tokens": 1}})
}

fn openai_choice(message: Value, finish: &str) -> Value {
    openai_body(json!([{"message": message, "finish_reason": finish}]))
}

/// A 200 answer whose single choice carries `message`.
fn openai_ok(message: Value) -> MockResponse {
    MockResponse::json(200, &openai_choice(message, "stop"))
}

fn anthropic_ok() -> MockResponse {
    MockResponse::json(
        200,
        &json!({"id": "msg_1", "model": "m", "stop_reason": "end_turn",
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 1, "output_tokens": 1}}),
    )
}

/// Sends `body` as the 200 answer and returns the parsed completion.
fn openai_complete(body: Value) -> Result<ChatResponse, LlmError> {
    let server = MockServer::start(move |_, _| MockResponse::json(200, &body));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    provider.complete(&ChatRequest::simple("m", "", "hi"))
}

fn user_messages(body: &Value) -> Vec<Value> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .cloned()
        .collect()
}

// ---- 1. empty user messages ----------------------------------------------------------

#[test]
fn openai_drops_an_empty_user_message_by_default() {
    let server = MockServer::start(|_, _| openai_ok(json!({"role": "assistant", "content": "ok"})));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::new("m", vec![Message::user(""), Message::user("hi")]);
    provider.complete(&req).unwrap();
    let body = server.requests()[0].body_json();
    assert_eq!(
        user_messages(&body),
        vec![json!({"role": "user", "content": "hi"})]
    );
}

#[test]
fn openai_sends_an_empty_user_message_as_empty_string_when_kept() {
    let server = MockServer::start(|_, _| openai_ok(json!({"role": "assistant", "content": "ok"})));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::new("m", vec![Message::user(""), Message::user("hi")])
        .keep_empty_messages(true);
    provider.complete(&req).unwrap();
    let body = server.requests()[0].body_json();
    assert_eq!(
        user_messages(&body),
        vec![
            json!({"role": "user", "content": ""}),
            json!({"role": "user", "content": "hi"})
        ]
    );
}

#[test]
fn openai_keep_option_does_not_add_a_message_for_a_tool_result_turn() {
    let server = MockServer::start(|_, _| openai_ok(json!({"role": "assistant", "content": "ok"})));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    let req = ChatRequest::new("m", vec![Message::tool_result("call_1", "result", false)])
        .keep_empty_messages(true);
    provider.complete(&req).unwrap();
    let body = server.requests()[0].body_json();
    assert!(user_messages(&body).is_empty(), "body: {body}");
    assert_eq!(body["messages"][0]["role"], "tool");
}

#[test]
fn anthropic_sends_an_empty_user_message_unchanged_by_default() {
    let server = MockServer::start(|_, _| anthropic_ok());
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let req = ChatRequest::new("m", vec![Message::user(""), Message::user("hi")]);
    provider.complete(&req).unwrap();
    let body = server.requests()[0].body_json();
    assert_eq!(body["messages"][0]["content"][0]["text"], "");
}

#[test]
fn anthropic_rejects_an_empty_user_message_before_sending_when_kept() {
    let server = MockServer::start(|_, _| anthropic_ok());
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let req = ChatRequest::new("m", vec![Message::user("hi"), Message::user("")])
        .keep_empty_messages(true);
    let err = provider.complete(&req).unwrap_err();
    assert!(matches!(err, LlmError::InvalidRequest(_)), "got {err:?}");
    assert!(server.requests().is_empty(), "nothing may be sent");
}

#[test]
fn anthropic_keep_option_accepts_a_message_with_text() {
    let server = MockServer::start(|_, _| anthropic_ok());
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    let req = ChatRequest::new("m", vec![Message::user("hi")]).keep_empty_messages(true);
    assert_eq!(provider.complete(&req).unwrap().text(), "ok");
    assert_eq!(server.requests().len(), 1);
}

// ---- 2. absent versus empty choices --------------------------------------------------

#[test]
fn absent_choices_is_malformed_with_a_distinct_message() {
    let err = openai_complete(json!({"id": "c1", "model": "m"})).unwrap_err();
    match err {
        LlmError::Malformed(m) => assert!(m.contains("missing choices"), "message: {m}"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn empty_choices_array_is_malformed_with_a_distinct_message() {
    let err = openai_complete(openai_body(json!([]))).unwrap_err();
    match err {
        LlmError::Malformed(m) => assert!(m.contains("empty choices"), "message: {m}"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn absent_and_empty_choices_have_different_messages() {
    let absent = openai_complete(json!({})).unwrap_err();
    let empty = openai_complete(openai_body(json!([]))).unwrap_err();
    assert_ne!(absent.to_string(), empty.to_string());
}

// ---- 3. null content -----------------------------------------------------------------

#[test]
fn null_content_without_tool_call_or_refusal_is_an_empty_completion_error() {
    let err = openai_complete(openai_choice(
        json!({"role": "assistant", "content": null}),
        "stop",
    ))
    .unwrap_err();
    match err {
        LlmError::Malformed(m) => assert!(m.contains("empty completion"), "message: {m}"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn absent_content_field_is_also_an_empty_completion_error() {
    let err = openai_complete(openai_choice(json!({"role": "assistant"}), "stop")).unwrap_err();
    assert!(
        matches!(err, LlmError::Malformed(ref m) if m.contains("empty completion")),
        "got {err:?}"
    );
}

#[test]
fn explicit_empty_string_content_is_accepted_as_empty_text() {
    let answer = openai_complete(openai_choice(
        json!({"role": "assistant", "content": ""}),
        "stop",
    ))
    .unwrap();
    assert_eq!(answer.text(), "");
}

#[test]
fn null_content_with_a_tool_call_is_not_an_error() {
    let answer = openai_complete(openai_choice(
        json!({"role": "assistant", "content": null, "tool_calls": [{
            "id": "call_1", "type": "function",
            "function": {"name": "pick", "arguments": "{\"a\":1}"}}]}),
        "tool_calls",
    ))
    .unwrap();
    assert_eq!(answer.tool_input("pick"), Some(&json!({"a": 1})));
}

#[test]
fn refusal_field_with_null_content_is_a_refused_error() {
    let err = openai_complete(openai_choice(
        json!({"role": "assistant", "content": null, "refusal": "I can't help with that."}),
        "stop",
    ))
    .unwrap_err();
    match err {
        LlmError::Refused { message } => assert_eq!(message, "I can't help with that."),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn content_filter_finish_with_null_content_is_a_refused_error() {
    let err = openai_complete(openai_choice(
        json!({"role": "assistant", "content": null}),
        "content_filter",
    ))
    .unwrap_err();
    assert!(matches!(err, LlmError::Refused { .. }), "got {err:?}");
}

#[test]
fn refused_error_is_not_a_failover_trigger() {
    let err = LlmError::Refused {
        message: "no".into(),
    };
    assert!(!err.is_failover_trigger());
}

// ---- 4. temperature on the wire ------------------------------------------------------

#[test]
fn openai_wire_body_carries_temperature_0_3_exactly() {
    let server = MockServer::start(|_, _| openai_ok(json!({"role": "assistant", "content": "ok"})));
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"));
    provider
        .complete(&ChatRequest::simple("m", "", "hi").with_temperature(0.3))
        .unwrap();
    let raw = server.requests()[0].body_text();
    assert!(
        raw.contains("\"temperature\":0.3,") || raw.contains("\"temperature\":0.3}"),
        "body: {raw}"
    );
    assert!(!raw.contains("0.30000001"), "body: {raw}");
}

#[test]
fn anthropic_wire_body_carries_temperature_0_3_exactly() {
    let server = MockServer::start(|_, _| anthropic_ok());
    let provider = AnthropicProvider::new(&server.base_url(), client(), creds("anthropic"));
    provider
        .complete(&ChatRequest::simple("m", "", "hi").with_temperature(0.3))
        .unwrap();
    let raw = server.requests()[0].body_text();
    assert!(
        raw.contains("\"temperature\":0.3,") || raw.contains("\"temperature\":0.3}"),
        "body: {raw}"
    );
}
