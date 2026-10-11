//! The crate's side of `docs/model-grants-contract.md`, driven by
//! `tests/vectors/model-grants.json`. The worker owner's tests consume the same vectors, so
//! the worker and this crate cannot drift apart on field names or on what a client accepts.

use rightkit_http::AddressPolicy;
use rightkit_models::http::{http_client_with_options, ModelHttpOptions, ProxyMode};
use rightkit_models::suite::grant::{acquire_grant_with_client, ModelGrantRequest};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

fn vectors() -> Value {
    serde_json::from_str(include_str!("vectors/model-grants.json")).unwrap()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Replaces the `$FUTURE` / `$PAST` placeholders in a response body.
fn resolve(mut body: Value) -> Value {
    if let Some(expiry) = body.get_mut("expiresAt") {
        match expiry.as_str() {
            Some("$FUTURE") => *expiry = Value::from(now() + 3600),
            Some("$PAST") => *expiry = Value::from(now() - 3600),
            _ => {}
        }
    }
    body
}

/// Serves one fixed response and records the request body.
fn serve(status: u16, body: Value) -> (String, Arc<Mutex<Option<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(None));
    let record = seen.clone();
    let payload = body.to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /v1/model-grants "), "{line}");
            let (mut length, mut content_type) = (0usize, String::new());
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                let lower = h.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                } else if let Some(v) = lower.strip_prefix("content-type:") {
                    content_type = v.trim().to_string();
                }
            }
            assert!(
                content_type.starts_with("application/json"),
                "{content_type}"
            );
            let mut request = vec![0u8; length];
            reader.read_exact(&mut request).unwrap();
            *record.lock().unwrap() = serde_json::from_slice(&request).ok();
            let mut out = stream;
            let _ = write!(
                out,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
        }
    });
    (base, seen)
}

fn client() -> rightkit_models::http::ModelHttp {
    http_client_with_options(ModelHttpOptions {
        proxy: ProxyMode::None,
        address_policy: AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        ..Default::default()
    })
}

fn call(base: &str, request: &Value) -> Result<rightkit_models::suite::grant::ModelGrant, String> {
    let field = |name: &str| request[name].as_str().unwrap().to_string();
    let (route, model, platform, sha256, device_id) = (
        field("route"),
        field("model"),
        field("platform"),
        field("sha256"),
        field("deviceId"),
    );
    acquire_grant_with_client(
        client().client(),
        ModelGrantRequest {
            worker_base: base,
            route: &route,
            model: &model,
            platform: &platform,
            sha256: &sha256,
            device_id: &device_id,
        },
    )
}

#[test]
fn the_request_the_crate_sends_matches_the_vector_exactly() {
    let v = vectors();
    let (base, seen) = serve(200, resolve(v["accepted"][0]["body"].clone()));
    call(&base, &v["request"]).unwrap();
    // Same keys, same values: no extra field, none renamed.
    assert_eq!(seen.lock().unwrap().clone().unwrap(), v["request"]);
}

#[test]
fn accepted_responses_yield_a_grant() {
    let v = vectors();
    for case in v["accepted"].as_array().unwrap() {
        let (base, _) = serve(
            case["status"].as_u64().unwrap() as u16,
            resolve(case["body"].clone()),
        );
        let grant = call(&base, &v["request"]).unwrap_or_else(|e| panic!("{}: {e}", case["name"]));
        assert_eq!(grant.token, case["body"]["token"].as_str().unwrap());
        assert!(!grant.expires_within(60), "{}", case["name"]);
        assert_eq!(grant.bearer(), format!("Bearer {}", grant.token));
    }
}

#[test]
fn rejected_responses_never_yield_a_grant() {
    let v = vectors();
    for case in v["rejected"].as_array().unwrap() {
        let (base, _) = serve(
            case["status"].as_u64().unwrap() as u16,
            resolve(case["body"].clone()),
        );
        let err = call(&base, &v["request"]).unwrap_err();
        assert!(
            err.contains(case["error_contains"].as_str().unwrap()),
            "{}: {err}",
            case["name"]
        );
    }
}

/// A grant issuance is not replayed after a 5xx: the first POST may have issued a lease.
#[test]
fn grant_post_is_not_replayed_after_a_5xx() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                if h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let mut out = stream;
            let _ = write!(
                out,
                "HTTP/1.1 503 X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
        }
    });
    let retrying = rightkit_http::Client::new(rightkit_http::ClientConfig {
        retry: rightkit_http::RetryPolicy {
            max_attempts: 3,
            initial_backoff: std::time::Duration::from_millis(5),
            max_backoff: std::time::Duration::from_millis(10),
            jitter: 0.0,
            ..rightkit_http::RetryPolicy::default()
        },
        address_policy: AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        ..rightkit_http::ClientConfig::default()
    });
    let v = vectors();
    let r = &v["request"];
    let f = |n: &str| r[n].as_str().unwrap().to_string();
    let _ = acquire_grant_with_client(
        &retrying,
        ModelGrantRequest {
            worker_base: &base,
            route: &f("route"),
            model: &f("model"),
            platform: &f("platform"),
            sha256: &f("sha256"),
            device_id: &f("deviceId"),
        },
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
