//! Async client tests against the local mock server (feature `async`).
#![cfg(feature = "async")]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rightkit_http::testing::{MockResponse, MockServer};
use rightkit_http::*;

#[path = "common/refused.rs"]
mod refused;

fn fast_retry(max: u32) -> RetryPolicy {
    RetryPolicy {
        max_attempts: max,
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(200),
        jitter: 0.0,
        ..RetryPolicy::default()
    }
}

fn client(config: ClientConfig) -> AsyncClient {
    AsyncClient::new(config).unwrap()
}

fn base(retry: RetryPolicy) -> ClientConfig {
    ClientConfig {
        retry,
        request_timeout: Duration::from_secs(5),
        response_timeout: Some(Duration::from_secs(5)),
        ..rightkit_http::testing::loopback_config()
    }
}

#[tokio::test]
async fn sends_exact_request_and_parses_json() {
    let server = MockServer::start(|_, _| MockResponse::json(200, &serde_json::json!({"ok": 1})));
    let c = client(ClientConfig {
        user_agent: "rk-test/1".into(),
        ..base(RetryPolicy::none())
    });
    let req = Request::post(server.url("/v1/x?a=b"))
        .bearer("tok")
        .json(&serde_json::json!({"q": "hi"}))
        .unwrap();
    let resp = c.send(&req).await.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.json::<serde_json::Value>().unwrap()["ok"], 1);
    let seen = &server.requests()[0];
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.target, "/v1/x?a=b");
    assert_eq!(seen.header("authorization"), Some("Bearer tok"));
    assert_eq!(seen.header("user-agent"), Some("rk-test/1"));
    assert_eq!(seen.body_json()["q"], "hi");
}

#[tokio::test]
async fn retries_503_honoring_backoff_and_notifies_observer() {
    let server = MockServer::start(|_, i| {
        if i < 2 {
            MockResponse::text(503, "busy")
        } else {
            MockResponse::text(200, "fine")
        }
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let c = client(ClientConfig {
        on_retry: Some(Arc::new(move |e: &RetryEvent| {
            sink.lock().unwrap().push((e.retry, e.reason.clone()))
        })),
        ..base(fast_retry(3))
    });
    let resp = c.send(&Request::get(server.url("/"))).await.unwrap();
    assert_eq!((resp.status, resp.attempts), (200, 3));
    assert_eq!(
        *events.lock().unwrap(),
        vec![(1, "HTTP 503".to_string()), (2, "HTTP 503".to_string())]
    );
}

#[tokio::test]
async fn retry_after_is_honored_and_capped() {
    let server = MockServer::start(|_, i| {
        if i == 0 {
            MockResponse::text(429, "slow down").header("Retry-After", "1")
        } else {
            MockResponse::text(200, "ok")
        }
    });
    let start = Instant::now();
    c_get(&client(base(fast_retry(2))), &server.url("/")).await;
    assert!(start.elapsed() >= Duration::from_millis(900));

    let server = MockServer::start(|_, i| {
        if i == 0 {
            MockResponse::text(429, "slow down").header("Retry-After", "600")
        } else {
            MockResponse::text(200, "ok")
        }
    });
    let mut policy = fast_retry(2);
    policy.max_retry_after = Duration::from_millis(100);
    let start = Instant::now();
    c_get(&client(base(policy)), &server.url("/")).await;
    assert!(start.elapsed() < Duration::from_secs(2));
}

async fn c_get(c: &AsyncClient, url: &str) {
    assert_eq!(c.send(&Request::get(url)).await.unwrap().status, 200);
}

#[tokio::test]
async fn exhausted_retries_surface_status_and_non_idempotent_rules() {
    let server = MockServer::start(|_, _| MockResponse::text(500, "boom"));
    let c = client(base(fast_retry(3)));
    match c.send(&Request::get(server.url("/"))).await.unwrap_err() {
        HttpError::Status {
            status: 500,
            body,
            attempts: 3,
            ..
        } => assert_eq!(body, "boom"),
        other => panic!("{other:?}"),
    }
    let before = server.request_count();
    let mut post = Request::post(server.url("/"));
    post.retry_unsafe_statuses = false;
    assert!(c.send(&post).await.is_err());
    assert_eq!(server.request_count() - before, 1);
    let server = MockServer::start(|_, _| MockResponse::text(404, "nope"));
    assert!(c.send(&Request::get(server.url("/"))).await.is_err());
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn connection_refused_is_connect_phase_and_retried() {
    let refused = refused::RefusedPort::hold();
    let port = refused.port();
    let retries = Arc::new(Mutex::new(0u32));
    let sink = retries.clone();
    let c = client(ClientConfig {
        on_retry: Some(Arc::new(move |_| *sink.lock().unwrap() += 1)),
        ..base(fast_retry(3))
    });
    let err = c
        .send(&Request::post(format!("http://127.0.0.1:{port}/")))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            HttpError::Transport {
                connect_phase: true,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(
        *retries.lock().unwrap(),
        2,
        "connect failures retry any method"
    );
}

#[tokio::test]
async fn response_timeout_fires_and_retries_idempotent_only() {
    let server =
        MockServer::start(|_, _| MockResponse::text(200, "late").delay(Duration::from_millis(600)));
    let c = client(ClientConfig {
        response_timeout: Some(Duration::from_millis(150)),
        ..base(fast_retry(2))
    });
    let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
    assert!(matches!(err, HttpError::Timeout { .. }), "{err:?}");
    assert_eq!(server.request_count(), 2);
    let before = server.request_count();
    let err = c.send(&Request::post(server.url("/"))).await.unwrap_err();
    assert!(matches!(err, HttpError::Timeout { .. }), "{err:?}");
    assert_eq!(server.request_count() - before, 1);
}

#[tokio::test]
async fn request_timeout_bounds_buffered_body() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, b"fast".to_vec()),
            (Duration::from_millis(800), b"late".to_vec()),
        ])
    });
    let c = client(ClientConfig {
        recv_body_timeout: Some(Duration::from_millis(150)),
        ..base(RetryPolicy::none())
    });
    let err = c.send(&Request::get(server.url("/"))).await.unwrap_err();
    assert!(matches!(err, HttpError::Timeout { .. }), "{err:?}");
    let c = client(base(RetryPolicy::none()));
    let err = c
        .send(&Request::get(server.url("/")).timeout(Duration::from_millis(200)))
        .await
        .unwrap_err();
    assert!(matches!(err, HttpError::Timeout { .. }), "{err:?}");
}

#[tokio::test]
async fn oversized_body_is_rejected_at_limit() {
    let server = MockServer::start(|_, _| MockResponse::new(200).body(vec![b'x'; 2000]));
    let c = client(ClientConfig {
        max_response_bytes: 1000,
        ..base(RetryPolicy::none())
    });
    assert!(matches!(
        c.send(&Request::get(server.url("/"))).await.unwrap_err(),
        HttpError::BodyTooLarge { limit: 1000 }
    ));
    let c = client(ClientConfig {
        max_response_bytes: 2000,
        ..base(RetryPolicy::none())
    });
    assert_eq!(
        c.send(&Request::get(server.url("/")))
            .await
            .unwrap()
            .body
            .len(),
        2000
    );
}

#[tokio::test]
async fn redirect_policy_matches_blocking_client() {
    // Default: never followed.
    let server = MockServer::start(|_, _| MockResponse::new(302).header("Location", "/next"));
    let c = client(base(RetryPolicy::none()));
    let resp = c.send_any(&Request::get(server.url("/"))).await.unwrap();
    assert_eq!(resp.status, 302);
    assert_eq!(server.request_count(), 1);

    // Followed within the limit.
    let server = MockServer::start(|req, _| {
        if req.path() == "/final" {
            MockResponse::text(200, "done")
        } else {
            MockResponse::new(302).header("Location", "/final")
        }
    });
    let c = client(ClientConfig {
        max_redirects: 2,
        ..base(RetryPolicy::none())
    });
    assert_eq!(
        c.send(&Request::get(server.url("/a")))
            .await
            .unwrap()
            .text(),
        "done"
    );

    // Limit exceeded errors as transport; only idempotent methods retry.
    let server = MockServer::start(|_, _| MockResponse::new(302).header("Location", "/loop"));
    let c = client(ClientConfig {
        max_redirects: 2,
        ..base(fast_retry(2))
    });
    for (method, expected) in [(Method::Get, 6), (Method::Post, 3)] {
        let before = server.request_count();
        match c
            .send_any(&Request::new(method, server.url("/loop")))
            .await
            .unwrap_err()
        {
            HttpError::Transport {
                message,
                connect_phase: false,
                ..
            } => assert_eq!(message, "too many redirects"),
            other => panic!("{other:?}"),
        }
        assert_eq!(server.request_count() - before, expected);
    }

    // will_error = false returns the final 3xx.
    let c = client(ClientConfig {
        max_redirects: 2,
        max_redirects_will_error: false,
        ..base(RetryPolicy::none())
    });
    let resp = c
        .send_any(&Request::get(server.url("/loop")))
        .await
        .unwrap();
    assert_eq!(resp.status, 302);
}

#[tokio::test]
async fn credentials_are_not_replayed_to_a_new_host() {
    let target = MockServer::start(|_, _| MockResponse::text(200, "other host"));
    let url = target.url("/landing");
    let origin = MockServer::start(move |_, _| {
        // 127.0.0.1 -> localhost is a different host name for the same listener.
        MockResponse::new(302).header("Location", &url.replace("127.0.0.1", "localhost"))
    });
    let c = client(ClientConfig {
        max_redirects: 3,
        ..base(RetryPolicy::none())
    });
    let resp = c
        .send(&Request::get(origin.url("/")).bearer("secret"))
        .await
        .unwrap();
    assert_eq!(resp.text(), "other host");
    assert_eq!(target.requests()[0].header("authorization"), None);
}

#[tokio::test]
async fn stream_error_status_carries_body_and_retries_before_first_byte() {
    let server = MockServer::start(|_, i| {
        if i == 0 {
            MockResponse::text(503, "warming")
        } else {
            MockResponse::text(400, "bad input")
        }
    });
    let c = client(base(fast_retry(3)));
    match c.stream(&Request::get(server.url("/"))).await {
        Err(HttpError::Status {
            status: 400,
            body,
            attempts: 2,
            ..
        }) => assert_eq!(body, "bad input"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("expected status error"),
    }
}

#[tokio::test]
async fn stream_any_returns_error_status_headers_and_streaming_body() {
    let server = MockServer::start(|_, _| {
        MockResponse::text(401, "denied").header("WWW-Authenticate", "Bearer realm=\"x\"")
    });
    let c = client(base(RetryPolicy::none()));
    let resp = c.stream_any(&Request::get(server.url("/"))).await.unwrap();
    assert_eq!(resp.status, 401);
    assert_eq!(resp.header("www-authenticate"), Some("Bearer realm=\"x\""));
    assert_eq!(resp.attempts, 1);
    let mut body = Vec::new();
    let mut s = resp.into_stream();
    while let Some(chunk) = s.next().await {
        body.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(body, b"denied");
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn body_stream_yields_chunks_and_enforces_deadlines() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![
            (Duration::ZERO, b"one".to_vec()),
            (Duration::from_millis(600), b"two".to_vec()),
        ])
    });
    let c = client(ClientConfig {
        recv_body_timeout: Some(Duration::from_millis(150)),
        ..base(RetryPolicy::none())
    });
    let mut s = c
        .stream(&Request::get(server.url("/")))
        .await
        .unwrap()
        .into_stream();
    assert_eq!(&s.next().await.unwrap().unwrap()[..], b"one");
    assert!(matches!(
        s.next().await,
        Some(Err(HttpError::Timeout { .. }))
    ));
    assert!(s.next().await.is_none(), "stream ends after the error");

    // Without a deadline the whole body arrives.
    let c = client(base(RetryPolicy::none()));
    let mut s = c
        .stream(&Request::get(server.url("/")))
        .await
        .unwrap()
        .into_stream();
    let mut all = Vec::new();
    while let Some(chunk) = s.next().await {
        all.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(all, b"onetwo");
}

#[tokio::test]
async fn streamed_sse_is_incremental_and_exact() {
    // Chunks split mid-field, between CR and LF, and across events.
    let chunks = vec![
        (
            Duration::ZERO,
            b": hi\r\ndata: one\r\n\r\nevent: delta\nda".to_vec(),
        ),
        (Duration::from_millis(100), b"ta: a\r".to_vec()),
        (Duration::from_millis(100), b"\ndata: b\nid: 9\n\n".to_vec()),
        (
            Duration::from_millis(100),
            b"retry: 1500\ndata: [DONE]\n\n".to_vec(),
        ),
    ];
    let server = MockServer::start(move |_, _| {
        MockResponse::new(200)
            .header("Content-Type", "text/event-stream")
            .chunks(chunks.clone())
    });
    let c = client(base(RetryPolicy::none()));
    let start = Instant::now();
    let mut events = c
        .stream(&Request::post(server.url("/s")).body(b"{}".to_vec()))
        .await
        .unwrap()
        .sse();
    let first = events.next().await.unwrap().unwrap();
    assert_eq!(first.data, "one");
    assert!(
        start.elapsed() < Duration::from_millis(200),
        "not incremental"
    );
    let second = events.next().await.unwrap().unwrap();
    assert_eq!(
        second,
        SseEvent {
            event: "delta".into(),
            data: "a\nb".into(),
            id: Some("9".into()),
            retry_ms: None
        }
    );
    let third = events.next().await.unwrap().unwrap();
    assert_eq!(
        (third.data.as_str(), third.retry_ms),
        ("[DONE]", Some(1500))
    );
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn truncated_sse_stream_errors_and_oversized_event_is_bounded() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![(
            Duration::ZERO,
            [b"data: ".as_slice(), &[b'x'; 300], b"\n\n"].concat(),
        )])
    });
    let c = client(base(RetryPolicy::none()));
    let mut events = c
        .stream(&Request::get(server.url("/")))
        .await
        .unwrap()
        .sse_with_limits(100, 10_000);
    assert!(matches!(
        events.next().await,
        Some(Err(HttpError::Stream(_)))
    ));
    assert!(events.next().await.is_none());

    let mut events = c
        .stream(&Request::get(server.url("/")))
        .await
        .unwrap()
        .sse_with_limits(10_000, 100);
    assert!(matches!(
        events.next().await,
        Some(Err(HttpError::Stream(_)))
    ));
}

#[test]
fn sse_decoder_is_chunk_boundary_independent() {
    let wire =
        b"\xEF\xBB\xBF: comment\r\nevent: a\r\ndata: 1\r\ndata: 2\r\nid: 7\r\nretry: 250\r\n\r\n\
                 data\n\ndata: x\rdata: y\r\r:tail\nevent: z\ndata:no-space\n\n";
    let whole = SseDecoder::default().push(wire).unwrap();
    // A bare `data` line is an empty-data event (dispatched, per the spec).
    assert_eq!(whole.len(), 4, "{whole:?}");
    assert_eq!(
        whole[0],
        SseEvent {
            event: "a".into(),
            data: "1\n2".into(),
            id: Some("7".into()),
            retry_ms: Some(250)
        }
    );
    assert_eq!(
        (whole[1].data.as_str(), whole[1].id.as_deref()),
        ("", Some("7"))
    );
    assert_eq!(whole[2].data, "x\ny");
    assert_eq!(
        (whole[3].event.as_str(), whole[3].data.as_str()),
        ("z", "no-space")
    );
    // Every possible split point, and byte-at-a-time, decode identically.
    for split in 0..=wire.len() {
        let mut d = SseDecoder::default();
        let mut got = d.push(&wire[..split]).unwrap();
        got.extend(d.push(&wire[split..]).unwrap());
        assert_eq!(got, whole, "split at {split}");
    }
    let mut d = SseDecoder::default();
    let mut got = Vec::new();
    for b in wire {
        got.extend(d.push(&[*b]).unwrap());
    }
    assert_eq!(got, whole);
}

#[test]
fn sse_event_limit_counts_all_lines_and_resets_per_event() {
    let mut d = SseDecoder::with_limits(1024, 10);
    assert_eq!(d.push(b"data: 1234\n\ndata: 5678\n\n").unwrap().len(), 2);
    assert!(d.push(b"data: 12345\ndata: 12345\n\n").is_err());
}
