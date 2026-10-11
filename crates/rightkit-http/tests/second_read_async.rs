//! Async regression test for the second independent read of 0.3.0, finding 4
//! (docs/audits/2026-10-11-astra-rightkit-http-second-read.md).
#![cfg(feature = "async")]

mod common;

use std::io::Write;
use std::time::{Duration, Instant};

use common::RawServer;
use futures_util::StreamExt;
use rightkit_http::testing::loopback_config;
use rightkit_http::*;

fn cfg() -> ClientConfig {
    // No `stream_timeout` and no `recv_body_timeout` (the defaults): nothing but the cancel
    // token can end a stream whose server has gone quiet.
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(10),
        response_timeout: Some(Duration::from_secs(10)),
        ..loopback_config()
    }
}

/// Sends the response head and then nothing at all: the body read stays pending.
fn silent_after_headers() -> RawServer {
    RawServer::start(|s, _, _| {
        let _ = s.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n",
        );
        let _ = s.flush();
        std::thread::sleep(Duration::from_secs(10));
    })
}

/// The existing cancellation test sends periodic chunks, which wake the task and so mask this:
/// `CancelToken::cancel` only sets a flag, and a read that is pending on a silent server is
/// never polled again unless something wakes it.
#[tokio::test]
async fn f4_cancel_wakes_a_body_stream_pending_on_a_silent_server() {
    let server = silent_after_headers();
    let token = CancelToken::new();
    let response = AsyncClient::new(cfg())
        .unwrap()
        .stream(&Request::get(server.url("/")).cancel(token.clone()))
        .await
        .unwrap();
    let mut body = response.into_stream();

    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        canceller.cancel();
    });

    let started = Instant::now();
    let next = tokio::time::timeout(Duration::from_secs(3), body.next())
        .await
        .expect("cancel must wake a stream that is pending on a silent server");
    assert!(matches!(next, Some(Err(HttpError::Cancelled))), "{next:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    // The stream ends after the error.
    assert!(body.next().await.is_none());
}

/// Same wake-up through the SSE decoder, which is how a quiet event stream is consumed.
#[tokio::test]
async fn f4_cancel_wakes_a_quiet_sse_stream() {
    let server = silent_after_headers();
    let token = CancelToken::new();
    let response = AsyncClient::new(cfg())
        .unwrap()
        .stream(&Request::get(server.url("/")).cancel(token.clone()))
        .await
        .unwrap();
    let mut events = response.sse();

    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        canceller.cancel();
    });

    let next = tokio::time::timeout(Duration::from_secs(3), events.next())
        .await
        .expect("cancel must wake a quiet SSE stream");
    assert!(matches!(next, Some(Err(HttpError::Cancelled))), "{next:?}");
}

/// A stream without a token keeps its old behaviour: it is not woken by anything but the
/// socket, and it still delivers what the server sends.
#[tokio::test]
async fn f4_a_stream_without_a_token_still_delivers_chunks() {
    let server = RawServer::start(|s, _, _| {
        let _ =
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
    });
    let response = AsyncClient::new(cfg())
        .unwrap()
        .stream(&Request::get(server.url("/")))
        .await
        .unwrap();
    let mut body = response.into_stream();
    let mut got = Vec::new();
    while let Some(chunk) = body.next().await {
        got.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(got, b"hello");
}
