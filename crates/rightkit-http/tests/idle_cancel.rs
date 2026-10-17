//! Stream idle timeout (`ClientConfig::idle_timeout`) and `CancelToken` flag sharing
//! (`from_flag` / `flag`), against the loopback mock server.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
use rightkit_http::*;

fn config(idle: Option<Duration>) -> ClientConfig {
    ClientConfig {
        retry: RetryPolicy::none(),
        request_timeout: Duration::from_secs(10),
        response_timeout: Some(Duration::from_secs(5)),
        idle_timeout: idle,
        ..loopback_config()
    }
}

fn chunk(delay_ms: u64, bytes: &[u8]) -> (Duration, Vec<u8>) {
    (Duration::from_millis(delay_ms), bytes.to_vec())
}

#[test]
fn a_stalled_body_fails_with_timed_out_after_the_idle_limit() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![chunk(0, b"first"), chunk(1500, b"late")])
    });
    let c = Client::new(config(Some(Duration::from_millis(250))));
    let mut reader = c
        .stream(&Request::get(server.url("/")))
        .unwrap()
        .into_reader();
    let started = Instant::now();
    let mut out = Vec::new();
    let err = reader.read_to_end(&mut out).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
    assert_eq!(out, b"first");
    assert!(
        started.elapsed() < Duration::from_millis(1400),
        "gave up at the idle limit, not when the server woke: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_slow_but_steady_body_outlives_the_idle_limit() {
    // Six chunks 120 ms apart: about 720 ms in all, longer than the 400 ms idle limit.
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks((0..6).map(|_| chunk(120, b"abc")).collect())
    });
    let c = Client::new(config(Some(Duration::from_millis(400))));
    let mut reader = c
        .stream(&Request::get(server.url("/")))
        .unwrap()
        .into_reader();
    let mut out = Vec::new();
    reader.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"abcabcabcabcabcabc");
}

#[test]
fn the_idle_limit_is_separate_from_the_header_timeout() {
    // Headers arrive after 500 ms, longer than the idle limit: the idle clock only runs on the body.
    let server =
        MockServer::start(|_, _| MockResponse::text(200, "ok").delay(Duration::from_millis(500)));
    let c = Client::new(config(Some(Duration::from_millis(200))));
    let mut reader = c
        .stream(&Request::get(server.url("/")))
        .unwrap()
        .into_reader();
    let mut s = String::new();
    reader.read_to_string(&mut s).unwrap();
    assert_eq!(s, "ok");
}

#[test]
fn without_an_idle_limit_a_pause_is_tolerated() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![chunk(0, b"a"), chunk(500, b"b")])
    });
    let c = Client::new(config(None));
    let mut reader = c
        .stream(&Request::get(server.url("/")))
        .unwrap()
        .into_reader();
    let mut out = Vec::new();
    reader.read_to_end(&mut out).unwrap();
    assert_eq!(out, b"ab");
}

#[test]
fn cancelling_during_an_idle_wait_returns_promptly() {
    let server = MockServer::start(|_, _| {
        MockResponse::new(200).chunks(vec![chunk(0, b"a"), chunk(3000, b"b")])
    });
    let token = CancelToken::new();
    let c = Client::new(config(Some(Duration::from_secs(30))));
    let mut reader = c
        .stream(&Request::get(server.url("/")).cancel(token.clone()))
        .unwrap()
        .into_reader();
    let flipper = {
        let token = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            token.cancel();
        })
    };
    let started = Instant::now();
    let mut out = Vec::new();
    let err = reader.read_to_end(&mut out).unwrap_err();
    flipper.join().unwrap();
    assert!(is_cancelled_io(&err), "{err}");
    assert!(started.elapsed() < Duration::from_millis(2000));
}

#[test]
fn from_flag_shares_the_callers_flag_both_ways() {
    let flag = Arc::new(AtomicBool::new(false));
    let token = CancelToken::from_flag(flag.clone());
    assert!(Arc::ptr_eq(token.flag(), &flag));
    assert!(!token.is_cancelled());

    // The owner sets the flag: the token sees it, and so do its clones.
    flag.store(true, Ordering::SeqCst);
    assert!(token.is_cancelled());
    assert!(token.clone().is_cancelled());

    // The token cancels: the owner's flag is set.
    let other = Arc::new(AtomicBool::new(false));
    let token = CancelToken::from_flag(other.clone());
    token.clone().cancel();
    assert!(other.load(Ordering::SeqCst));
    assert!(std::ptr::eq(token.as_atomic(), &*other));
}

#[test]
fn a_token_from_an_owned_flag_cancels_a_request() {
    let server = MockServer::start(|_, _| MockResponse::text(200, "never sent"));
    let flag = Arc::new(AtomicBool::new(true));
    let c = Client::new(config(None));
    let err = c
        .send(&Request::get(server.url("/")).cancel(CancelToken::from_flag(flag)))
        .unwrap_err();
    assert!(matches!(err, HttpError::Cancelled), "{err:?}");
    assert_eq!(server.request_count(), 0);
}
