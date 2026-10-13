//! A result that does not fit the frame limit is answered with a typed error, not dropped, and
//! a handler can read the peer identity the OS verified. Host and client run in this process,
//! over the real current-user-only endpoint.

use rightkit_service::*;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LIMIT: usize = 4096;

#[derive(Default)]
struct Probe {
    peer: Mutex<Option<PeerIdentity>>,
}

impl Handler for Probe {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        emit: &Emit,
    ) -> Result<Value, ServiceError> {
        let text = |params: &Value| "x".repeat(params["bytes"].as_u64().unwrap_or(0) as usize);
        match method {
            "fill" => Ok(Value::String(text(params))),
            "flood" => {
                emit(Value::String(text(params)));
                Ok(json!("done"))
            }
            "peer" => {
                *self.peer.lock().unwrap() = cx.peer().cloned();
                Ok(json!({ "app": cx.app() }))
            }
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    _hosted: Hosted,
    suite: Suite,
    probe: Arc<Probe>,
    events: Arc<Mutex<Vec<ServiceEvent>>>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let mut cfg = ServiceConfig::new(
        "probe",
        "test",
        5,
        "test",
        Allowlist::new().allow("*", ["fill", "flood", "peer"]),
    );
    cfg.frame_limit = LIMIT;
    let events = Arc::new(Mutex::new(Vec::new()));
    cfg.on_event = Some({
        let events = events.clone();
        Arc::new(move |event: &ServiceEvent| events.lock().unwrap().push(event.clone()))
    });
    let probe = Arc::new(Probe::default());
    let hosted = host(&suite, cfg, probe.clone()).expect("host starts");
    Fixture {
        _dir: dir,
        _hosted: hosted,
        suite,
        probe,
        events,
    }
}

fn too_large(events: &Mutex<Vec<ServiceEvent>>, kind: &str) -> Vec<ServiceEvent> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| matches!(event, ServiceEvent::FrameTooLarge { kind: k, .. } if *k == kind))
        .cloned()
        .collect()
}

#[test]
fn a_result_one_byte_over_the_frame_limit_is_a_typed_error_and_the_connection_stays_usable() {
    let fx = fixture();
    let client = Client::connect(&fx.suite, "probe", "caller").unwrap();
    let started = Instant::now();

    // Grow the result one byte at a time across the limit. Every size up to some `n` fits and
    // every size after it does not, so the first refusal is exactly one byte over.
    let mut last_fit = None;
    let mut first_refused = None;
    for bytes in (LIMIT - 300)..=(LIMIT + 100) {
        match client.call("fill", json!({ "bytes": bytes })) {
            Ok(value) => {
                assert!(
                    first_refused.is_none(),
                    "{bytes} bytes fitted after {first_refused:?} was refused"
                );
                assert_eq!(value.as_str().map(str::len), Some(bytes));
                last_fit = Some(bytes);
            }
            Err(error) => {
                assert_eq!(error.code, ERROR_RESPONSE_TOO_LARGE, "{error:?}");
                assert!(!error.retryable);
                first_refused.get_or_insert(bytes);
            }
        }
    }
    let fit = last_fit.expect("a result under the limit is delivered");
    assert_eq!(first_refused, Some(fit + 1));

    // The refusal is immediate, not the client's 120 s deadline, and nothing was half-written:
    // the same connection still answers.
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        client.call("fill", json!({ "bytes": 3 })).unwrap(),
        json!("xxx")
    );

    let reported = too_large(&fx.events, "response");
    assert_eq!(reported.len(), LIMIT + 100 - fit);
    assert!(matches!(
        &reported[0],
        ServiceEvent::FrameTooLarge { method, limit, .. } if method == "fill" && *limit == LIMIT
    ));
}

#[test]
fn an_oversized_event_is_not_sent_and_is_reported_while_the_response_still_arrives() {
    let fx = fixture();
    let client = Client::connect(&fx.suite, "probe", "caller").unwrap();
    assert_eq!(
        client.call("flood", json!({ "bytes": LIMIT * 2 })).unwrap(),
        json!("done")
    );
    let reported = too_large(&fx.events, "event");
    assert_eq!(reported.len(), 1);
    assert!(matches!(
        &reported[0],
        ServiceEvent::FrameTooLarge { method, .. } if method == "flood"
    ));
    assert!(too_large(&fx.events, "response").is_empty());
}

#[test]
fn the_handler_sees_the_verified_peer_beside_the_declared_label() {
    let fx = fixture();
    let client = Client::connect(&fx.suite, "probe", "any-label-i-like").unwrap();
    // The label is whatever the caller declared.
    assert_eq!(
        client.call("peer", json!({})).unwrap(),
        json!({ "app": "any-label-i-like" })
    );
    // The peer identity is what the OS reported: this very process, as the current user.
    let peer = fx
        .probe
        .peer
        .lock()
        .unwrap()
        .clone()
        .expect("peer recorded");
    #[cfg(unix)]
    assert!(peer.uid.is_some(), "{peer:?}");
    #[cfg(windows)]
    assert!(
        peer.sid
            .as_deref()
            .is_some_and(|sid| sid.starts_with("S-1-")),
        "{peer:?}"
    );
    // Linux (SO_PEERCRED), macOS (LOCAL_PEERPID) and Windows (the pipe's client process).
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    assert_eq!(peer.pid, Some(std::process::id()), "{peer:?}");
}
