//! Wire framing of bodiless requests, checked on both clients against the raw server: a
//! GET, HEAD or DELETE without a body carries no `Content-Length` and no `Transfer-Encoding`;
//! an explicitly empty POST keeps `Content-Length: 0`; a JSON body is unchanged; retries and
//! redirects rebuild the same framing.

mod common;

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::RawServer;
use rightkit_http::testing::loopback_config;
use rightkit_http::*;

/// Each request head (lowercased) and its body, in arrival order.
type Seen = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

fn cfg(retry: RetryPolicy, max_redirects: u32) -> ClientConfig {
    ClientConfig {
        retry,
        max_redirects,
        request_timeout: Duration::from_secs(10),
        response_timeout: Some(Duration::from_secs(10)),
        ..loopback_config()
    }
}

fn no_backoff() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        jitter: 0.0,
        ..RetryPolicy::default()
    }
}

const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

/// Records every request, reads the declared body, then answers with `respond(head, index)`.
fn recording(
    seen: Seen,
    respond: impl Fn(&str, usize) -> Vec<u8> + Send + Sync + 'static,
) -> RawServer {
    RawServer::start(move |s, head, index| {
        let mut body = vec![0u8; content_length_of(head)];
        let _ = s.read_exact(&mut body);
        seen.lock().unwrap().push((head.to_ascii_lowercase(), body));
        let _ = s.write_all(&respond(head, index));
        let _ = s.flush();
    })
}

fn content_length_of(head: &str) -> usize {
    header_value(head, "content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Value of the first header named `name` in a request head; the request line is skipped.
fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

fn assert_no_framing(head: &str) {
    assert!(
        header_value(head, "content-length").is_none(),
        "unexpected Content-Length in {head:?}"
    );
    assert!(
        header_value(head, "transfer-encoding").is_none(),
        "unexpected Transfer-Encoding in {head:?}"
    );
}

fn head_only(head: &str) -> bool {
    head.starts_with("HEAD ")
}

fn ok_for(head: &str) -> Vec<u8> {
    if head_only(head) {
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n".to_vec()
    } else {
        OK.to_vec()
    }
}

// ------------------------------------------------------------------ blocking client

#[test]
fn blocking_bodiless_get_has_no_length_or_chunked_framing() {
    let seen = Seen::default();
    let server = recording(seen.clone(), |head, _| ok_for(head));
    let client = Client::new(cfg(RetryPolicy::none(), 0));
    let r = client.send(&Request::get(server.url("/"))).unwrap();
    assert_eq!(r.status, 200);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_no_framing(&seen[0].0);
}

#[test]
fn blocking_bodiless_head_and_delete_have_no_length_or_chunked_framing() {
    let seen = Seen::default();
    let server = recording(seen.clone(), |head, _| ok_for(head));
    let client = Client::new(cfg(RetryPolicy::none(), 0));
    client
        .send_headers(&Request::new(Method::Head, server.url("/")))
        .unwrap();
    client.send(&Request::delete(server.url("/"))).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for (head, _) in seen.iter() {
        assert_no_framing(head);
    }
}

#[test]
fn blocking_empty_post_keeps_content_length_zero() {
    let seen = Seen::default();
    let server = recording(seen.clone(), |head, _| ok_for(head));
    let client = Client::new(cfg(RetryPolicy::none(), 0));
    client
        .send(&Request::post(server.url("/")).body(Vec::new()))
        .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        header_value(&seen[0].0, "content-length").as_deref(),
        Some("0")
    );
    assert!(seen[0].1.is_empty());
}

#[test]
fn blocking_post_json_body_is_unchanged() {
    let seen = Seen::default();
    let server = recording(seen.clone(), |head, _| ok_for(head));
    let client = Client::new(cfg(RetryPolicy::none(), 0));
    client
        .send(
            &Request::post(server.url("/"))
                .header("Content-Type", "application/json")
                .body(br#"{"a":1}"#.to_vec()),
        )
        .unwrap();
    let seen = seen.lock().unwrap();
    let (head, body) = &seen[0];
    assert_eq!(header_value(head, "content-length").as_deref(), Some("7"));
    assert_eq!(
        header_value(head, "content-type").as_deref(),
        Some("application/json")
    );
    assert_eq!(body.as_slice(), br#"{"a":1}"#);
}

#[test]
fn blocking_retried_get_has_no_length_or_chunked_framing() {
    let seen = Seen::default();
    let server = recording(seen.clone(), |head, index| {
        if index == 0 {
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec()
        } else {
            ok_for(head)
        }
    });
    let client = Client::new(cfg(no_backoff(), 0));
    let r = client.send(&Request::get(server.url("/"))).unwrap();
    assert_eq!(r.status, 200);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "the 503 must have been retried");
    for (head, _) in seen.iter() {
        assert_no_framing(head);
    }
}

#[test]
fn blocking_redirected_get_has_no_length_or_chunked_framing() {
    let seen = Seen::default();
    let server = recording(seen.clone(), |head, _| {
        if head.starts_with("GET /start ") {
            b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec()
        } else {
            ok_for(head)
        }
    });
    let client = Client::new(cfg(RetryPolicy::none(), 5));
    let r = client.send(&Request::get(server.url("/start"))).unwrap();
    assert_eq!(r.status, 200);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for (head, _) in seen.iter() {
        assert_no_framing(head);
    }
}

// ------------------------------------------------------------------ async client

#[cfg(feature = "async")]
mod async_client {
    use super::*;

    fn async_client(retry: RetryPolicy, max_redirects: u32) -> AsyncClient {
        AsyncClient::new(cfg(retry, max_redirects)).unwrap()
    }

    #[tokio::test]
    async fn async_bodiless_get_has_no_length_or_chunked_framing() {
        let seen = Seen::default();
        let server = recording(seen.clone(), |head, _| ok_for(head));
        let r = async_client(RetryPolicy::none(), 0)
            .send(&Request::get(server.url("/")))
            .await
            .unwrap();
        assert_eq!(r.status, 200);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_no_framing(&seen[0].0);
    }

    #[tokio::test]
    async fn async_bodiless_head_and_delete_have_no_length_or_chunked_framing() {
        let seen = Seen::default();
        let server = recording(seen.clone(), |head, _| ok_for(head));
        let client = async_client(RetryPolicy::none(), 0);
        client
            .send_headers(&Request::new(Method::Head, server.url("/")))
            .await
            .unwrap();
        client
            .send(&Request::delete(server.url("/")))
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        for (head, _) in seen.iter() {
            assert_no_framing(head);
        }
    }

    #[tokio::test]
    async fn async_empty_post_keeps_content_length_zero() {
        let seen = Seen::default();
        let server = recording(seen.clone(), |head, _| ok_for(head));
        async_client(RetryPolicy::none(), 0)
            .send(&Request::post(server.url("/")).body(Vec::new()))
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(
            header_value(&seen[0].0, "content-length").as_deref(),
            Some("0")
        );
        assert!(seen[0].1.is_empty());
    }

    #[tokio::test]
    async fn async_post_json_body_is_unchanged() {
        let seen = Seen::default();
        let server = recording(seen.clone(), |head, _| ok_for(head));
        async_client(RetryPolicy::none(), 0)
            .send(
                &Request::post(server.url("/"))
                    .header("Content-Type", "application/json")
                    .body(br#"{"a":1}"#.to_vec()),
            )
            .await
            .unwrap();
        let seen = seen.lock().unwrap();
        let (head, body) = &seen[0];
        assert_eq!(header_value(head, "content-length").as_deref(), Some("7"));
        assert_eq!(
            header_value(head, "content-type").as_deref(),
            Some("application/json")
        );
        assert_eq!(body.as_slice(), br#"{"a":1}"#);
    }

    #[tokio::test]
    async fn async_retried_get_has_no_length_or_chunked_framing() {
        let seen = Seen::default();
        let server = recording(seen.clone(), |head, index| {
            if index == 0 {
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec()
            } else {
                ok_for(head)
            }
        });
        let r = async_client(no_backoff(), 0)
            .send(&Request::get(server.url("/")))
            .await
            .unwrap();
        assert_eq!(r.status, 200);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the 503 must have been retried");
        for (head, _) in seen.iter() {
            assert_no_framing(head);
        }
    }

    #[tokio::test]
    async fn async_redirected_get_has_no_length_or_chunked_framing() {
        let seen = Seen::default();
        let server = recording(seen.clone(), |head, _| {
            if head.starts_with("GET /start ") {
                b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec()
            } else {
                ok_for(head)
            }
        });
        let r = async_client(RetryPolicy::none(), 5)
            .send(&Request::get(server.url("/start")))
            .await
            .unwrap();
        assert_eq!(r.status, 200);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        for (head, _) in seen.iter() {
            assert_no_framing(head);
        }
    }
}
