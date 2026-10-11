//! Typed entitlement downloads: `ModelStore::try_ensure_entitled` takes a raw token, refuses a
//! `Bearer `-prefixed value before any request, and maps each failure to its own
//! `EntitlementError` variant. Loopback mock server in the style of `grant_rate_limit.rs`.

use rightkit_http::AddressPolicy;
use rightkit_models::http::{http_client_with_options, ModelHttp, ModelHttpOptions, ProxyMode};
use rightkit_models::suite::catalog::{ModelSpec, ModelTier, SpecFile, SpecSource};
use rightkit_models::suite::grant::GrantError;
use rightkit_models::suite::store::EntryState;
use rightkit_models::suite::{EntitlementError, ModelStore};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WEIGHTS: &[u8] = b"weights-bytes";

/// One canned answer. `status == 0` closes the connection without writing a response.
#[derive(Clone)]
struct Reply {
    status: u16,
    headers: &'static str,
    body: Vec<u8>,
}

fn reply(status: u16, headers: &'static str, body: Vec<u8>) -> Reply {
    Reply {
        status,
        headers,
        body,
    }
}

struct Mock {
    base: String,
    /// The `Authorization` header of every request, in arrival order.
    seen: Arc<Mutex<Vec<Option<String>>>>,
}

/// Answers requests in `script` order; the last answer repeats.
fn serve(script: Vec<Reply>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for (answered, stream) in listener.incoming().flatten().enumerate() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut authorization = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if line
                    .get(..14)
                    .is_some_and(|p| p.eq_ignore_ascii_case("authorization:"))
                {
                    authorization = Some(line[14..].trim().to_string());
                }
            }
            log.lock().unwrap().push(authorization);
            let answer = script[answered.min(script.len() - 1)].clone();
            let mut out = stream;
            if answer.status == 0 {
                continue;
            }
            let _ = out.write_all(
                format!(
                    "HTTP/1.1 {} X\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    answer.status,
                    answer.headers,
                    answer.body.len()
                )
                .as_bytes(),
            );
            let _ = out.write_all(&answer.body);
        }
    });
    Mock { base, seen }
}

/// Loopback-capable client: production clients refuse 127.0.0.1.
fn loopback_client() -> ModelHttp {
    http_client_with_options(ModelHttpOptions {
        proxy: ProxyMode::None,
        address_policy: AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        ..Default::default()
    })
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn leak(s: impl Into<String>) -> &'static str {
    Box::leak(s.into().into_boxed_str())
}

fn tar_gz(wrapper: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        builder
            .append_data(&mut header, format!("{wrapper}/{name}"), *data)
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

/// An entitlement-gated archive model served from `url`, whose archive pin is `archive`.
fn gated_spec(url: &'static str, archive: &[u8], entitlement: bool) -> ModelSpec {
    ModelSpec {
        tier: ModelTier::AddOn,
        entitlement,
        id: "entitled-model",
        label: "Entitled model",
        version: leak(sha(archive)),
        size_hint_bytes: WEIGHTS.len() as u64,
        files: Box::leak(
            vec![SpecFile {
                path: "weights.bin",
                sha256: Some(leak(sha(WEIGHTS))),
                size: Some(WEIGHTS.len() as u64),
            }]
            .into_boxed_slice(),
        ),
        source: SpecSource::Archive {
            url: Some(url),
            sha256: Some(leak(sha(archive))),
            size: None,
            requires_auth: true,
        },
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

fn store(root: &std::path::Path) -> ModelStore {
    ModelStore::at_models_root(root, "scraperight").with_http_client(loopback_client())
}

/// A spec whose URL is the mock server; the body is never reached on the failure paths.
fn spec_for(mock: &Mock) -> ModelSpec {
    let archive = tar_gz("wrap", &[("weights.bin", WEIGHTS)]);
    gated_spec(
        leak(format!("{}/m/model.tar.gz", mock.base)),
        &archive,
        true,
    )
}

fn try_call(
    store: &ModelStore,
    spec: &ModelSpec,
    token: &str,
) -> Result<rightkit_models::suite::store::ModelStatus, EntitlementError> {
    store.try_ensure_entitled(spec, token, &|| false, |_| {})
}

#[test]
fn a_raw_token_is_sent_as_the_bearer_and_the_model_installs() {
    let archive = tar_gz("wrap", &[("weights.bin", WEIGHTS)]);
    let mock = serve(vec![reply(200, "", archive.clone())]);
    let spec = gated_spec(
        leak(format!("{}/m/model.tar.gz", mock.base)),
        &archive,
        true,
    );
    let root = tempfile::tempdir().unwrap();

    let status = try_call(&store(root.path()), &spec, "tok-123").unwrap();

    assert_eq!(status.state, EntryState::Ready);
    assert_eq!(
        mock.seen.lock().unwrap().as_slice(),
        &[Some("Bearer tok-123".to_string())]
    );
}

#[test]
fn a_token_with_the_bearer_prefix_is_refused_before_any_request() {
    let mock = serve(vec![reply(200, "", Vec::new())]);
    let spec = spec_for(&mock);
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());

    for token in ["Bearer tok-123", "bearer tok-123", "BEARER tok-123"] {
        let err = try_call(&store, &spec, token).unwrap_err();
        assert!(
            matches!(&err, EntitlementError::InvalidRequest(m) if m.contains("Bearer")),
            "{token}: {err:?}"
        );
    }
    assert!(mock.seen.lock().unwrap().is_empty(), "no request was sent");
}

#[test]
fn an_empty_or_malformed_token_is_refused_before_any_request() {
    let mock = serve(vec![reply(200, "", Vec::new())]);
    let spec = spec_for(&mock);
    let root = tempfile::tempdir().unwrap();
    let store = store(root.path());

    for token in ["", "tok 123", "tok\n123", "tök"] {
        let err = try_call(&store, &spec, token).unwrap_err();
        assert!(
            matches!(err, EntitlementError::InvalidRequest(_)),
            "{token:?}: {err:?}"
        );
    }
    assert!(mock.seen.lock().unwrap().is_empty());
}

#[test]
fn a_model_that_is_not_entitlement_gated_is_an_invalid_request() {
    let mock = serve(vec![reply(200, "", Vec::new())]);
    let spec = gated_spec(leak(format!("{}/m/model.tar.gz", mock.base)), b"x", false);
    let root = tempfile::tempdir().unwrap();

    let err = try_call(&store(root.path()), &spec, "tok-123").unwrap_err();
    assert!(
        matches!(&err, EntitlementError::InvalidRequest(m) if m.contains("does not use an entitlement bearer")),
        "{err:?}"
    );
    assert!(mock.seen.lock().unwrap().is_empty());
}

#[test]
fn http_401_and_403_are_not_entitled_with_the_status() {
    for status in [401u16, 403] {
        let mock = serve(vec![reply(status, "", b"nope".to_vec())]);
        let spec = spec_for(&mock);
        let root = tempfile::tempdir().unwrap();

        let err = try_call(&store(root.path()), &spec, "tok-123").unwrap_err();
        match &err {
            EntitlementError::NotEntitled {
                status: got,
                message,
            } => {
                assert_eq!(*got, status);
                assert!(message.contains(&status.to_string()), "{message}");
            }
            other => panic!("{status}: expected NotEntitled, got {other:?}"),
        }
        // Not retried: an authorization refusal is final.
        assert_eq!(mock.seen.lock().unwrap().len(), 1);
    }
}

#[test]
fn other_http_statuses_are_refused_with_the_status() {
    let mock = serve(vec![reply(404, "", b"gone".to_vec())]);
    let spec = spec_for(&mock);
    let root = tempfile::tempdir().unwrap();

    let err = try_call(&store(root.path()), &spec, "tok-123").unwrap_err();
    assert!(
        matches!(err, EntitlementError::Refused { status: 404, .. }),
        "{err:?}"
    );
}

#[test]
fn a_persistent_server_error_is_refused_with_the_last_status() {
    // 5xx is transient: the download retries, then reports the last status.
    let mock = serve(vec![reply(503, "", b"busy".to_vec())]);
    let spec = spec_for(&mock);
    let root = tempfile::tempdir().unwrap();

    let err = try_call(&store(root.path()), &spec, "tok-123").unwrap_err();
    assert!(
        matches!(err, EntitlementError::Refused { status: 503, .. }),
        "{err:?}"
    );
}

#[test]
fn http_429_is_rate_limited_with_the_retry_after_delay() {
    let mock = serve(vec![reply(429, "Retry-After: 7\r\n", b"slow".to_vec())]);
    let spec = spec_for(&mock);
    let root = tempfile::tempdir().unwrap();

    let err = try_call(&store(root.path()), &spec, "tok-123").unwrap_err();
    match &err {
        EntitlementError::RateLimited(grant @ GrantError::RateLimited { .. }) => {
            assert_eq!(grant.retry_after(), Some(Duration::from_secs(7)));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
    assert!(err.to_string().contains("429"), "{err}");
}

#[test]
fn http_429_without_retry_after_has_no_delay() {
    let mock = serve(vec![reply(429, "", b"slow".to_vec())]);
    let spec = spec_for(&mock);
    let root = tempfile::tempdir().unwrap();

    let err = try_call(&store(root.path()), &spec, "tok-123").unwrap_err();
    assert!(
        matches!(
            &err,
            EntitlementError::RateLimited(GrantError::RateLimited {
                retry_after: None,
                ..
            })
        ),
        "{err:?}"
    );
}

#[test]
fn a_dropped_connection_is_a_transport_failure() {
    let mock = serve(vec![reply(0, "", Vec::new())]);
    let spec = spec_for(&mock);
    let root = tempfile::tempdir().unwrap();

    let err = try_call(&store(root.path()), &spec, "tok-123").unwrap_err();
    assert!(matches!(err, EntitlementError::Transport(_)), "{err:?}");
}

#[test]
fn a_body_that_fails_verification_is_an_install_failure_not_a_refusal() {
    // The server answers 200 with bytes that do not match the archive pin.
    let mock = serve(vec![reply(200, "", b"not the archive".to_vec())]);
    let spec = spec_for(&mock);
    let root = tempfile::tempdir().unwrap();

    let err = try_call(&store(root.path()), &spec, "tok-123").unwrap_err();
    assert!(matches!(err, EntitlementError::Install(_)), "{err:?}");
}

#[test]
fn the_string_api_still_sends_the_header_value_unchanged() {
    let archive = tar_gz("wrap", &[("weights.bin", WEIGHTS)]);
    let mock = serve(vec![reply(200, "", archive.clone())]);
    let spec = gated_spec(
        leak(format!("{}/m/model.tar.gz", mock.base)),
        &archive,
        true,
    );
    let root = tempfile::tempdir().unwrap();

    let status = store(root.path())
        .ensure_entitled(&spec, "Bearer tok-456", &|| false, |_| {})
        .unwrap();

    assert_eq!(status.state, EntryState::Ready);
    assert_eq!(
        mock.seen.lock().unwrap().as_slice(),
        &[Some("Bearer tok-456".to_string())]
    );
}
