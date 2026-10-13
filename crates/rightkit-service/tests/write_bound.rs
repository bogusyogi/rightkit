//! The host bounds each frame write. A client that stops reading fills the transport buffer; the
//! host closes that connection after `HostOptions::write_timeout` instead of blocking on it
//! forever. Unix only: it needs a raw socket that handshakes and then never reads.
#![cfg(unix)]

use rightkit_service::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LIMIT: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct Big {
    closed: AtomicBool,
}

impl Handler for Big {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        _params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            // Far more than any socket buffer holds.
            "big" => Ok(Value::String("x".repeat(32 * 1024 * 1024))),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
    fn connection_closed(&self, _conn: u64) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

fn start(options: HostOptions) -> (tempfile::TempDir, Suite, Arc<Big>, Hosted) {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let mut cfg = ServiceConfig::new(
        "wb",
        "test",
        5,
        "test",
        Allowlist::new().allow("*", ["big"]),
    );
    cfg.frame_limit = LIMIT;
    let handler = Arc::new(Big::default());
    let hosted = host_with_options(&suite, cfg, handler.clone(), options).expect("host starts");
    (dir, suite, handler, hosted)
}

/// Handshake on a raw socket, send `big`, and return the socket without reading the response.
fn request_and_stall(suite: &Suite) -> UnixStream {
    let rec = read_record(suite, "wb").expect("record");
    let mut s = UnixStream::connect(&rec.endpoint).unwrap();
    let mut hello = serde_json::to_vec(
        &json!({"type": "hello", "app": "stall", "protocol_major": PROTOCOL_MAJOR}),
    )
    .unwrap();
    hello.push(b'\n');
    s.write_all(&hello).unwrap();
    // Read exactly the hello reply (one line), byte by byte.
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while s.read(&mut b).unwrap() == 1 && b[0] != b'\n' {
        line.push(b[0]);
    }
    assert!(String::from_utf8_lossy(&line).contains("\"ok\":true"));
    let mut req =
        serde_json::to_vec(&json!({"type": "request", "id": 1, "method": "big", "params": {}}))
            .unwrap();
    req.push(b'\n');
    s.write_all(&req).unwrap();
    s
}

fn wait_for(flag: &AtomicBool, within: Duration) -> bool {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if flag.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    flag.load(Ordering::SeqCst)
}

#[test]
fn a_client_that_never_reads_is_cut_off_after_the_write_timeout() {
    let (_d, suite, handler, _hosted) =
        start(HostOptions::new().write_timeout(Some(Duration::from_millis(400))));
    let _stalled = request_and_stall(&suite);
    assert!(
        wait_for(&handler.closed, Duration::from_secs(10)),
        "the host closed the stalled connection"
    );
}

#[test]
fn a_reader_that_keeps_up_within_the_bound_gets_the_whole_response() {
    let (_d, suite, handler, _hosted) =
        start(HostOptions::new().write_timeout(Some(Duration::from_secs(20))));
    let client = Client::connect(&suite, "wb", "reader").unwrap();
    let v = client.call("big", json!({})).unwrap();
    assert_eq!(v.as_str().map(str::len), Some(32 * 1024 * 1024));
    assert!(!handler.closed.load(Ordering::SeqCst));
    // The connection is still usable.
    assert_eq!(
        client.call("nope", json!({})).unwrap_err().code,
        "unknown_method"
    );
}

#[test]
fn without_a_bound_the_stalled_connection_stays_open() {
    let (_d, suite, handler, _hosted) = start(HostOptions::new().write_timeout(None));
    let _stalled = request_and_stall(&suite);
    assert!(
        !wait_for(&handler.closed, Duration::from_millis(1500)),
        "unbounded write keeps waiting for the reader"
    );
}

#[test]
fn the_default_is_bounded_and_zero_is_raised_to_one_millisecond() {
    assert_eq!(
        HostOptions::default().get_write_timeout(),
        Some(DEFAULT_WRITE_TIMEOUT)
    );
    assert_eq!(
        HostOptions::new()
            .write_timeout(Some(Duration::ZERO))
            .get_write_timeout(),
        Some(Duration::from_millis(1))
    );
    assert_eq!(
        HostOptions::new().write_timeout(None).get_write_timeout(),
        None
    );
}
