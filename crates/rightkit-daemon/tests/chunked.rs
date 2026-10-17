//! Chunked calls (issue #294): a request and a result larger than one frame, under the unchanged
//! frame cap and the host's separate message bound. The host here has a tiny frame limit so a few
//! kilobytes are "large".

use rightkit_daemon::*;
use serde_json::{json, Value};
use std::sync::Arc;

const FRAME: usize = 4096;

struct Svc;

impl Handler for Svc {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => Ok(params.clone()),
            // `{"n": 5000}` answers a string of n characters.
            "make" => Ok(json!({"text": "é\"\\x".repeat(params["n"].as_u64().unwrap_or(0) as usize)})),
            "len" => Ok(json!({"len": params["text"].as_str().map(str::len)})),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn start(options: HostOptions) -> (tempfile::TempDir, Suite, Hosted) {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let mut config = ServiceConfig::new(
        "chunk-svc",
        "engine",
        1,
        "1.0",
        Allowlist::new().allow("app", ["echo", "make", "len"]),
    );
    config.frame_limit = FRAME;
    let hosted =
        host_with_options(&suite, config, Arc::new(Svc), options).expect("host starts");
    (dir, suite, hosted)
}

fn connect(suite: &Suite) -> Client {
    Client::connect(suite, "chunk-svc", "app").expect("connects")
}

#[test]
fn hello_advertises_the_chunk_limits() {
    let (_dir, suite, _host) = start(HostOptions::new());
    let client = connect(&suite);
    assert_eq!(client.hello()["frame_limit"], FRAME);
    assert_eq!(client.hello()["chunk_bytes"], FRAME / 4);
    assert_eq!(client.hello()["max_message"], 256 * 1024 * 1024);
}

#[test]
fn a_large_request_and_result_round_trip_in_pieces() {
    let (_dir, suite, _host) = start(HostOptions::new());
    let client = connect(&suite);
    let big = json!({"text": "é\"\\x".repeat(20_000), "n": [1, 2, 3]});
    let echoed = client.call_large("echo", big.clone(), &mut |_| {}).unwrap();
    assert_eq!(echoed, big);
    // The same connection is still on a frame boundary and serves ordinary calls.
    assert_eq!(client.call("echo", json!({"a": 1})).unwrap(), json!({"a": 1}));
}

#[test]
fn a_large_request_with_a_small_result_and_the_reverse() {
    let (_dir, suite, _host) = start(HostOptions::new());
    let client = connect(&suite);
    let text = "x".repeat(50_000);
    let out = client
        .call_large("len", json!({"text": text}), &mut |_| {})
        .unwrap();
    assert_eq!(out, json!({"len": 50_000}));
    let made = client
        .call_large("make", json!({"n": 10_000}), &mut |_| {})
        .unwrap();
    assert_eq!(made["text"].as_str().unwrap().chars().count(), 40_000);
    // A small call through call_large is one ordinary request.
    assert_eq!(
        client.call_large("echo", json!({"a": 1}), &mut |_| {}).unwrap(),
        json!({"a": 1})
    );
}

#[test]
fn without_opting_in_an_oversized_result_is_still_the_typed_error() {
    let (_dir, suite, _host) = start(HostOptions::new());
    let client = connect(&suite);
    let error = client.call("make", json!({"n": 10_000})).unwrap_err();
    assert_eq!(error.code, ERROR_RESPONSE_TOO_LARGE);
}

#[test]
fn the_message_bound_holds_in_both_directions() {
    let (_dir, suite, _host) = start(HostOptions::new().max_message(30_000));
    let client = connect(&suite);
    // Request beyond the bound: refused before any piece is sent.
    let error = client
        .call_large("len", json!({"text": "x".repeat(40_000)}), &mut |_| {})
        .unwrap_err();
    assert_eq!(error.code, ERROR_REQUEST_TOO_LARGE);
    // Result beyond the bound: the typed too-large error, not a chunked result.
    let error = client
        .call_large("make", json!({"n": 10_000}), &mut |_| {})
        .unwrap_err();
    assert_eq!(error.code, ERROR_RESPONSE_TOO_LARGE);
    // Within the bound it works.
    let ok = client
        .call_large("len", json!({"text": "x".repeat(20_000)}), &mut |_| {})
        .unwrap();
    assert_eq!(ok, json!({"len": 20_000}));
}

#[test]
fn a_host_with_chunking_off_refuses_pieces_and_never_chunks_a_result() {
    let (_dir, suite, _host) = start(HostOptions::new().max_message(0));
    let client = connect(&suite);
    let error = client
        .call_large("len", json!({"text": "x".repeat(40_000)}), &mut |_| {})
        .unwrap_err();
    assert_eq!(error.code, ERROR_REQUEST_TOO_LARGE);
    let error = client
        .call_large("make", json!({"n": 10_000}), &mut |_| {})
        .unwrap_err();
    assert_eq!(error.code, ERROR_RESPONSE_TOO_LARGE);
}

#[test]
fn a_chunked_call_is_authorised_on_its_own_method() {
    let (_dir, suite, _host) = start(HostOptions::new());
    let client = connect(&suite);
    let error = client
        .call_large("not_allowed", json!({"text": "x".repeat(40_000)}), &mut |_| {})
        .unwrap_err();
    assert_eq!(error.code, ERROR_NOT_ALLOWED);
    // The reserved prefix cannot be smuggled in as the carried method.
    let piece = json!({"id": "u1", "seq": 0, "last": true, "text": "{}", "method": "rk.fetch"});
    let error = client.call("rk.upload", piece).unwrap_err();
    assert_eq!(error.code, ERROR_BAD_CHUNK);
}

#[test]
fn a_fetch_with_nothing_held_is_a_typed_error() {
    let (_dir, suite, _host) = start(HostOptions::new());
    let client = connect(&suite);
    let error = client
        .call("rk.fetch", json!({"id": "d1", "seq": 0}))
        .unwrap_err();
    assert_eq!(error.code, ERROR_BAD_CHUNK);
}
