//! Raw response capture: the observer and the `Truncated` / `Malformed` errors receive the
//! provider's response bytes, status and request id when `with_raw_capture` is on, with a size
//! cap; nothing changes when it is off; streams are not captured; the credential never reaches
//! the retained bytes. Runs against the loopback mock server, like `call_controls.rs`.

use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::{Client, ClientConfig, RetryPolicy};
use rightkit_llm::anthropic::AnthropicProvider;
use rightkit_llm::credentials::{CredentialSource, StaticCredentials};
use rightkit_llm::failover::FailoverOn;
use rightkit_llm::openai::OpenAiCompatProvider;
use rightkit_llm::{ChatRequest, LlmError, Provider, RawHandle, StreamEvent};
use serde_json::{json, Value};

const SENTINEL: &str = "sk-SENTINEL-do-not-leak-9b2d";

fn client() -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy::none(),
        ..rightkit_http::testing::loopback_config()
    })
}

fn client_with_retry(max_attempts: u32) -> Client {
    Client::new(ClientConfig {
        retry: RetryPolicy {
            max_attempts,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(1),
            jitter: 0.0,
            retry_statuses: vec![503],
            honor_retry_after: false,
            ..RetryPolicy::default()
        },
        ..rightkit_http::testing::loopback_config()
    })
}

fn creds(id: &str) -> Arc<dyn CredentialSource> {
    Arc::new(StaticCredentials::default().with(id, SENTINEL))
}

fn bytes_of(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn openai_ok_json() -> Value {
    json!({
        "id": "c1", "model": "m",
        "choices": [{"message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    })
}

fn openai_length_json() -> Value {
    json!({
        "id": "c2", "model": "m",
        "choices": [{"message": {"role": "assistant", "content": "par"}, "finish_reason": "length"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 7}
    })
}

fn anthropic_ok_json() -> Value {
    json!({
        "id": "msg_1", "model": "m", "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "ok"}],
        "usage": {"input_tokens": 1, "output_tokens": 1}
    })
}

/// A 2xx or error answer with the exact `body` bytes and a request id header.
fn answer(status: u16, body: Vec<u8>, request_id_header: &str, id: &str) -> MockResponse {
    MockResponse::new(status)
        .header("Content-Type", "application/json")
        .header(request_id_header, id)
        .body(body)
}

#[derive(Default)]
struct Seen {
    raws: Vec<Option<RawHandle>>,
    ok: Vec<bool>,
    streams: Vec<bool>,
}

type Sink = Arc<Mutex<Seen>>;

fn openai(server: &MockServer, client: Client, cap: Option<usize>) -> (OpenAiCompatProvider, Sink) {
    let sink: Sink = Arc::default();
    let s = sink.clone();
    let mut provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client, creds("openai"))
            .with_observer(move |rec| {
                let mut g = s.lock().unwrap();
                g.raws.push(rec.raw.cloned());
                g.ok.push(rec.result.is_ok());
                g.streams.push(rec.stream);
            });
    if let Some(cap) = cap {
        provider = provider.with_raw_capture(cap);
    }
    (provider, sink)
}

fn anthropic(server: &MockServer, client: Client, cap: Option<usize>) -> (AnthropicProvider, Sink) {
    let sink: Sink = Arc::default();
    let s = sink.clone();
    let mut provider = AnthropicProvider::new(&server.base_url(), client, creds("anthropic"))
        .with_observer(move |rec| {
            let mut g = s.lock().unwrap();
            g.raws.push(rec.raw.cloned());
            g.ok.push(rec.result.is_ok());
            g.streams.push(rec.stream);
        });
    if let Some(cap) = cap {
        provider = provider.with_raw_capture(cap);
    }
    (provider, sink)
}

fn request() -> ChatRequest {
    ChatRequest::simple("m", "s", "u")
}

fn only_raw(sink: &Sink) -> RawHandle {
    let g = sink.lock().unwrap();
    assert_eq!(g.raws.len(), 1, "observer calls");
    g.raws[0].clone().expect("a captured exchange")
}

// ---- off by default -------------------------------------------------------------------

#[test]
fn capture_is_off_by_default_and_errors_are_bare() {
    let body = bytes_of(&openai_length_json());
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "req_1"));
    let (provider, sink) = openai(&server, client(), None);
    let err = provider
        .complete(&request().reject_truncated_output())
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
    assert!(err.raw().is_none());
    assert!(sink.lock().unwrap().raws[0].is_none());
}

#[test]
fn a_successful_call_without_capture_reports_no_raw_exchange() {
    let body = bytes_of(&openai_ok_json());
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "req_1"));
    let (provider, sink) = openai(&server, client(), None);
    assert_eq!(provider.complete(&request()).unwrap().text(), "ok");
    let g = sink.lock().unwrap();
    assert_eq!(g.raws.len(), 1);
    assert!(g.raws[0].is_none());
}

// ---- success --------------------------------------------------------------------------

#[test]
fn openai_success_hands_the_observer_the_exact_bytes_status_and_request_id() {
    let body = bytes_of(&openai_ok_json());
    let sent = body.clone();
    let server =
        MockServer::start(move |_, _| answer(200, sent.clone(), "x-request-id", "req_oa_7"));
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    let reply = provider.complete(&request()).unwrap();
    assert_eq!(reply.text(), "ok");

    let raw = only_raw(&sink);
    assert_eq!(raw.status, 200);
    assert_eq!(raw.request_id.as_deref(), Some("req_oa_7"));
    assert_eq!(raw.body, body);
    assert_eq!(raw.received_bytes, body.len());
    assert_eq!(raw.attempts, 1);
    assert!(!raw.truncated);
    assert!(!raw.credential_removed);
    assert_eq!(
        raw.body_text().into_owned(),
        String::from_utf8(body).unwrap()
    );
}

#[test]
fn anthropic_success_reads_the_request_id_header_of_that_wire() {
    let body = bytes_of(&anthropic_ok_json());
    let sent = body.clone();
    let server = MockServer::start(move |_, _| answer(200, sent.clone(), "request-id", "req_an_3"));
    let (provider, sink) = anthropic(&server, client(), Some(1 << 20));
    assert_eq!(provider.complete(&request()).unwrap().text(), "ok");

    let raw = only_raw(&sink);
    assert_eq!(raw.status, 200);
    assert_eq!(raw.request_id.as_deref(), Some("req_an_3"));
    assert_eq!(raw.body, body);
}

#[test]
fn a_missing_request_id_is_none() {
    let body = bytes_of(&openai_ok_json());
    let server = MockServer::start(move |_, _| {
        MockResponse::new(200)
            .header("Content-Type", "application/json")
            .body(body.clone())
    });
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    provider.complete(&request()).unwrap();
    assert_eq!(only_raw(&sink).request_id, None);
}

// ---- the cap --------------------------------------------------------------------------

#[test]
fn the_cap_bounds_the_retained_body_and_sets_the_truncated_flag() {
    let body = bytes_of(&openai_ok_json());
    let sent = body.clone();
    let server = MockServer::start(move |_, _| answer(200, sent.clone(), "x-request-id", "r"));
    let (provider, sink) = openai(&server, client(), Some(10));
    // The call itself still parses the whole body.
    assert_eq!(provider.complete(&request()).unwrap().text(), "ok");

    let raw = only_raw(&sink);
    assert_eq!(raw.body, body[..10].to_vec());
    assert_eq!(raw.received_bytes, body.len());
    assert!(raw.truncated);
}

#[test]
fn a_zero_cap_keeps_status_and_request_id_but_no_body() {
    let body = bytes_of(&openai_ok_json());
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "req_z"));
    let (provider, sink) = openai(&server, client(), Some(0));
    provider.complete(&request()).unwrap();
    let raw = only_raw(&sink);
    assert!(raw.body.is_empty());
    assert!(raw.truncated);
    assert_eq!(raw.status, 200);
    assert_eq!(raw.request_id.as_deref(), Some("req_z"));
}

#[test]
fn a_body_within_the_cap_is_not_flagged() {
    let body = bytes_of(&openai_ok_json());
    let len = body.len();
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "r"));
    let (provider, sink) = openai(&server, client(), Some(len));
    provider.complete(&request()).unwrap();
    let raw = only_raw(&sink);
    assert_eq!(raw.body.len(), len);
    assert!(!raw.truncated);
}

// ---- Truncated and Malformed carry the same handle -------------------------------------

#[test]
fn truncated_carries_the_same_handle_the_observer_saw() {
    let body = bytes_of(&openai_length_json());
    let sent = body.clone();
    let server = MockServer::start(move |_, _| answer(200, sent.clone(), "x-request-id", "req_t"));
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    let err = provider
        .complete(&request().reject_truncated_output())
        .unwrap_err();

    assert!(err.is_truncated(), "got {err:?}");
    assert!(
        matches!(
            err.root(),
            LlmError::Truncated {
                output_tokens: Some(7)
            }
        ),
        "got {err:?}"
    );
    let from_error = err.raw().expect("handle on the error");
    let from_observer = only_raw(&sink);
    assert!(Arc::ptr_eq(from_error, &from_observer));
    assert_eq!(from_error.body, body);
    assert_eq!(from_error.request_id.as_deref(), Some("req_t"));
    // The wrapper reads like the error it wraps.
    assert_eq!(
        err.to_string(),
        "output truncated at the output limit after 7 tokens"
    );
    assert!(!err.is_failover_trigger());
}

#[test]
fn anthropic_truncated_carries_the_handle() {
    let body = bytes_of(&json!({
        "id": "msg_2", "model": "m", "stop_reason": "max_tokens",
        "content": [{"type": "text", "text": "par"}],
        "usage": {"input_tokens": 1, "output_tokens": 4}
    }));
    let sent = body.clone();
    let server = MockServer::start(move |_, _| answer(200, sent.clone(), "request-id", "req_at"));
    let (provider, _sink) = anthropic(&server, client(), Some(1 << 20));
    let err = provider
        .complete(&request().reject_truncated_output())
        .unwrap_err();
    assert!(err.is_truncated(), "got {err:?}");
    assert_eq!(err.raw().unwrap().body, body);
}

#[test]
fn malformed_json_and_malformed_shape_carry_the_exact_bytes() {
    // Not JSON at all.
    let not_json = b"<html>gateway says hello</html>".to_vec();
    let sent = not_json.clone();
    let server = MockServer::start(move |_, _| answer(200, sent.clone(), "x-request-id", "req_m1"));
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    let err = provider.complete(&request()).unwrap_err();
    assert!(err.is_malformed(), "got {err:?}");
    assert_eq!(err.raw().unwrap().body, not_json);
    assert!(Arc::ptr_eq(err.raw().unwrap(), &only_raw(&sink)));

    // JSON that is not a completion: no `choices`.
    let no_choices = bytes_of(&json!({"id": "x", "model": "m"}));
    let sent = no_choices.clone();
    let server = MockServer::start(move |_, _| answer(200, sent.clone(), "x-request-id", "req_m2"));
    let (provider, _sink) = openai(&server, client(), Some(1 << 20));
    let err = provider.complete(&request()).unwrap_err();
    assert!(err.is_malformed(), "got {err:?}");
    assert!(
        matches!(err.root(), LlmError::Malformed(m) if m.contains("missing choices")),
        "got {err:?}"
    );
    assert_eq!(err.raw().unwrap().body, no_choices);
    assert_eq!(err.raw().unwrap().request_id.as_deref(), Some("req_m2"));
}

#[test]
fn anthropic_malformed_carries_the_handle() {
    let body = bytes_of(&json!({"id": "msg", "type": "message"}));
    let sent = body.clone();
    let server = MockServer::start(move |_, _| answer(200, sent.clone(), "request-id", "req_am"));
    let (provider, _sink) = anthropic(&server, client(), Some(1 << 20));
    let err = provider.complete(&request()).unwrap_err();
    assert!(err.is_malformed(), "got {err:?}");
    assert_eq!(err.raw().unwrap().body, body);
}

#[test]
fn a_capped_error_handle_is_flagged_truncated() {
    let body = bytes_of(&openai_length_json());
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "r"));
    let (provider, _sink) = openai(&server, client(), Some(16));
    let err = provider
        .complete(&request().reject_truncated_output())
        .unwrap_err();
    let raw = err.raw().unwrap();
    assert_eq!(raw.body.len(), 16);
    assert!(raw.truncated);
}

#[test]
fn failover_policies_see_the_inner_error() {
    let body = bytes_of(&openai_length_json());
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "r"));
    let (provider, _sink) = openai(&server, client(), Some(1 << 20));
    let err = provider
        .complete(&request().reject_truncated_output())
        .unwrap_err();
    assert!(matches!(err, LlmError::Captured { .. }));
    assert!(!FailoverOn::Retryable.matches(&err));
    assert!(FailoverOn::AnyError.matches(&err));
    assert!(FailoverOn::custom(|e| matches!(e, LlmError::Truncated { .. })).matches(&err));
}

// ---- failures -------------------------------------------------------------------------

#[test]
fn an_error_status_reaches_the_observer_with_its_body_and_the_error_stays_a_status() {
    let body = bytes_of(&json!({"error": {"message": "slow down", "type": "rate_limit"}}));
    let sent = body.clone();
    let server =
        MockServer::start(move |_, _| answer(429, sent.clone(), "x-request-id", "req_429"));
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    let err = provider.complete(&request()).unwrap_err();

    // The error is exactly what it was without capture.
    assert!(
        matches!(&err, LlmError::Status { status: 429, message } if message == "slow down"),
        "got {err:?}"
    );
    assert!(err.raw().is_none());

    let raw = only_raw(&sink);
    assert_eq!(raw.status, 429);
    assert_eq!(raw.request_id.as_deref(), Some("req_429"));
    assert_eq!(raw.body, body);
    assert!(!sink.lock().unwrap().ok[0]);
}

#[test]
fn a_transport_failure_has_no_response_to_capture() {
    // Nothing listens on the port the server held: the connection is refused.
    let server = MockServer::start(|_, _| MockResponse::new(200));
    let base = server.base_url();
    drop(server);
    let sink: Sink = Arc::default();
    let s = sink.clone();
    let provider = OpenAiCompatProvider::new("openai", &base, client(), creds("openai"))
        .with_observer(move |rec| s.lock().unwrap().raws.push(rec.raw.cloned()))
        .with_raw_capture(1024);
    assert!(provider.complete(&request()).is_err());
    let g = sink.lock().unwrap();
    assert_eq!(g.raws.len(), 1);
    assert!(g.raws[0].is_none());
}

#[test]
fn the_exchange_describes_the_final_attempt_of_a_retried_call() {
    let ok = bytes_of(&openai_ok_json());
    let server = MockServer::start(move |_, n| {
        if n == 0 {
            answer(
                503,
                b"{\"error\":\"busy\"}".to_vec(),
                "x-request-id",
                "req_first",
            )
        } else {
            answer(200, ok.clone(), "x-request-id", "req_second")
        }
    });
    let (provider, sink) = openai(&server, client_with_retry(3), Some(1 << 20));
    assert_eq!(provider.complete(&request()).unwrap().text(), "ok");
    let raw = only_raw(&sink);
    assert_eq!(raw.status, 200);
    assert_eq!(raw.request_id.as_deref(), Some("req_second"));
    assert_eq!(raw.attempts, 2);
}

// ---- secrets --------------------------------------------------------------------------

#[test]
fn the_credential_is_removed_from_the_retained_bytes_and_flagged() {
    let body = bytes_of(&json!({"error": {"message": format!("bad key {SENTINEL} given")}}));
    let server = MockServer::start(move |_, _| answer(401, body.clone(), "x-request-id", "req_k"));
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    let err = provider.complete(&request()).unwrap_err();
    assert!(!format!("{err}").contains(SENTINEL));
    assert!(!format!("{err:?}").contains(SENTINEL));

    let raw = only_raw(&sink);
    assert!(raw.credential_removed);
    let text = raw.body_text();
    assert!(!text.contains(SENTINEL), "credential reached the capture");
    assert!(text.contains("[redacted]"));
    // The key was sent on the wire, so the checks above mean something.
    assert!(server.requests()[0]
        .header("authorization")
        .unwrap_or_default()
        .contains(SENTINEL));
}

#[test]
fn a_credential_in_a_successful_body_is_removed_too() {
    let body = bytes_of(&json!({
        "id": "c1", "model": "m",
        "choices": [{"message": {"role": "assistant", "content": format!("echo {SENTINEL}")}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    }));
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "r"));
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    provider.complete(&request()).unwrap();
    let raw = only_raw(&sink);
    assert!(raw.credential_removed);
    assert!(!raw.body_text().contains(SENTINEL));
}

#[test]
fn debug_output_of_an_exchange_prints_no_body() {
    let body = bytes_of(&json!({
        "id": "c1", "model": "m",
        "choices": [{"message": {"role": "assistant", "content": "private dictated sentence"}, "finish_reason": "stop"}]
    }));
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "r"));
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    provider.complete(&request()).unwrap();
    let raw = only_raw(&sink);
    let shown = format!("{raw:?}");
    assert!(!shown.contains("private dictated sentence"), "{shown}");
    assert!(shown.contains("retained_bytes"));
}

#[test]
fn the_observer_never_sees_the_request_credential_with_capture_on() {
    let body = bytes_of(&openai_ok_json());
    let server = MockServer::start(move |_, _| answer(200, body.clone(), "x-request-id", "r"));
    let seen = Arc::new(Mutex::new(String::new()));
    let s = seen.clone();
    let provider =
        OpenAiCompatProvider::new("openai", &server.base_url(), client(), creds("openai"))
            .with_raw_capture(1 << 20)
            .with_observer(move |rec| {
                s.lock().unwrap().push_str(&format!(
                    "{} {} {:?} {:?}",
                    rec.provider, rec.request_body, rec.result, rec.raw
                ));
            });
    provider.complete(&request()).unwrap();
    assert!(!seen.lock().unwrap().contains(SENTINEL));
}

// ---- streams are not captured ---------------------------------------------------------

#[test]
fn a_stream_is_not_captured_even_when_capture_is_on() {
    let frames = [
        "data: {\"id\":\"c3\",\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    ];
    let server = MockServer::start(move |_, _| MockResponse::sse(&frames));
    let (provider, sink) = openai(&server, client(), Some(1 << 20));
    let reply = provider
        .stream(&request(), &mut |_: StreamEvent| ControlFlow::Continue(()))
        .unwrap();
    assert_eq!(reply.text(), "hi");
    let g = sink.lock().unwrap();
    assert_eq!(g.streams, vec![true]);
    assert!(g.raws[0].is_none());
}
