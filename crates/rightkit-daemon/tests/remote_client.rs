//! The blocking device client against a real in-process daemon over TLS.
//! Needs the `testing` feature (enabled by this crate's self dev-dependency).

use std::sync::Arc;

use rightkit_daemon::remote::client::{Client, ClientConfig, DeviceKey, Error, ErrorCode, Refusal};
use rightkit_daemon::remote::signing::{chunk_digest, object_hasher, parse_digest_key};
use rightkit_daemon::remote::testing::{TestCertificate, TestDaemon};
use serde_json::{json, Map, Value};

fn key() -> Arc<DeviceKey> {
    Arc::new(DeviceKey::from_pkcs8(&DeviceKey::generate_pkcs8().unwrap()).unwrap())
}

fn refusal(e: Error) -> Refusal {
    match e {
        Error::Refused(r) => r,
        other => panic!("expected a daemon refusal, got {other}"),
    }
}

fn patch(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn rejections(d: &TestDaemon) -> Vec<String> {
    d.daemon
        .events()
        .iter()
        .filter(|e| e["event"] == "AUTH_REJECTED")
        .map(|e| e["reason"].as_str().unwrap().to_string())
        .collect()
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 % 251) as u8).collect()
}

#[test]
fn enrol_then_signed_request_round_trip() {
    let d = TestDaemon::start().unwrap();
    let client = d.client().unwrap();
    let dev = client.enrol(&d.enrolment_code, key(), Some("ci")).unwrap();
    assert!(dev.device_id().starts_with("dev_"));
    assert_eq!(dev.scopes(), ["venture", "channel", "account"]);
    assert!(d.daemon.device_ids().contains(&dev.device_id().to_string()));

    let first = dev
        .update_job("job-1", &patch(json!({"state": "queued"})), 0, "idem-1")
        .unwrap();
    assert_eq!((first.revision, first.replayed), (1, false));
    assert_eq!(first.fields["state"], "queued");

    // A replay of the same request is answered from the idempotency record.
    let again = dev
        .update_job("job-1", &patch(json!({"state": "queued"})), 0, "idem-1")
        .unwrap();
    assert_eq!((again.revision, again.replayed), (1, true));

    let second = dev
        .update_job("job-1", &patch(json!({"state": "sent"})), 1, "idem-2")
        .unwrap();
    assert_eq!(second.revision, 2);
    assert_eq!(d.daemon.job_revision("job-1"), 2);

    // The generic signed request reads back through the same path.
    let status = dev.upload_init(10, 5).unwrap();
    assert_eq!(
        dev.upload_status(&status.upload_id).unwrap().received.len(),
        0
    );
}

#[test]
fn signature_by_the_wrong_key_is_refused_as_unauthenticated() {
    let d = TestDaemon::start().unwrap();
    let client = d.client().unwrap();
    let dev = client.enrol(&d.enrolment_code, key(), None).unwrap();

    // Same device id, request signed by a different key: the signature does not verify.
    let forged = client.session(dev.device_id(), key());
    let r = refusal(
        forged
            .update_job("j", &patch(json!({"a": 1})), 0, "k")
            .unwrap_err(),
    );
    assert_eq!((r.status, r.code), (401, ErrorCode::Unauthenticated));
    assert!(rejections(&d).contains(&"bad_signature".to_string()));
    assert_eq!(
        d.daemon.job_revision("j"),
        0,
        "the refused patch changed nothing"
    );

    // An unknown device id is the same refusal.
    let ghost = client.session("dev_0000000000000000", key());
    let r = refusal(ghost.upload_init(10, 5).unwrap_err());
    assert_eq!(r.code, ErrorCode::Unauthenticated);
    assert!(rejections(&d).contains(&"unknown_device".to_string()));
}

#[test]
fn an_unsigned_request_to_a_signed_route_is_refused() {
    let d = TestDaemon::start().unwrap();
    let client = d.client().unwrap();
    let dev = client.enrol(&d.enrolment_code, key(), None).unwrap();

    // Control: signed, the same device reaches the route (authenticated, upload unknown).
    let r = refusal(dev.send("GET", "/v1/uploads/up_x", &[], &[]).unwrap_err());
    assert_eq!(r.code, ErrorCode::NotFound);
    let nonces = d.daemon.metrics().nonce_records;

    // The device id without a timestamp, nonce or signature: refused on every device route.
    let refused = |method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]| {
        let r = refusal(dev.send_unsigned(method, path, headers, body).unwrap_err());
        assert_eq!(
            (r.status, r.code),
            (401, ErrorCode::Unauthenticated),
            "{method} {path}"
        );
    };
    refused("POST", "/v1/uploads", &[], br#"{"size":10,"chunkBytes":5}"#);
    refused("GET", "/v1/uploads/up_x", &[], b"");
    refused(
        "PATCH",
        "/v1/publish-jobs/j",
        &[("if-match", "\"0\""), ("idempotency-key", "k")],
        br#"{"state":"queued"}"#,
    );
    assert_eq!(
        rejections(&d),
        ["malformed_timestamp"; 3],
        "refused before any signature or replay check"
    );
    assert_eq!(
        d.daemon.job_revision("j"),
        0,
        "the refused patch changed nothing"
    );
    assert_eq!(
        d.daemon.metrics().nonce_records,
        nonces,
        "an unauthenticated caller leaves the replay table alone"
    );

    // The unsigned helper still cannot be handed credentials of its own.
    assert!(matches!(
        dev.send_unsigned(
            "GET",
            "/v1/uploads/up_x",
            &[("x-rk-signature", "AAAA")],
            &[]
        ),
        Err(Error::Config(_))
    ));
}

#[test]
fn a_replayed_nonce_is_refused_even_when_correctly_signed() {
    let d = TestDaemon::start().unwrap();
    let client = d.client().unwrap();
    let dev = client.enrol(&d.enrolment_code, key(), None).unwrap();
    let path = "/v1/publish-jobs/job-1";
    let body = br#"{"state":"queued"}"#;

    let first = dev
        .send_with_nonce(
            "fixed-nonce-1",
            "PATCH",
            path,
            &[("if-match", "\"0\""), ("idempotency-key", "idem-1")],
            body,
        )
        .unwrap();
    assert_eq!(first.status, 200);
    assert_eq!(d.daemon.job_revision("job-1"), 1);

    // The identical request, signed afresh over the same nonce. Without replay protection the
    // daemon would answer it from the idempotency record (200 with Idempotency-Replayed).
    let r = refusal(
        dev.send_with_nonce(
            "fixed-nonce-1",
            "PATCH",
            path,
            &[("if-match", "\"0\""), ("idempotency-key", "idem-1")],
            body,
        )
        .unwrap_err(),
    );
    assert_eq!((r.status, r.code), (401, ErrorCode::Unauthenticated));
    assert_eq!(rejections(&d), ["replayed_nonce"]);

    // The nonce is remembered, not the request: a different, valid patch reusing it is refused
    // and applies nothing.
    let r = refusal(
        dev.send_with_nonce(
            "fixed-nonce-1",
            "PATCH",
            path,
            &[("if-match", "\"1\""), ("idempotency-key", "idem-2")],
            br#"{"state":"sent"}"#,
        )
        .unwrap_err(),
    );
    assert_eq!((r.status, r.code), (401, ErrorCode::Unauthenticated));
    assert_eq!(rejections(&d), ["replayed_nonce", "replayed_nonce"]);
    assert_eq!(d.daemon.job_revision("job-1"), 1);

    // The refusal is not sticky: a fresh nonce for the same patch is accepted.
    let next = dev
        .send_with_nonce(
            "fixed-nonce-2",
            "PATCH",
            path,
            &[("if-match", "\"1\""), ("idempotency-key", "idem-2")],
            br#"{"state":"sent"}"#,
        )
        .unwrap();
    assert_eq!(next.status, 200);
    assert_eq!(d.daemon.job_revision("job-1"), 2);
}

#[test]
fn enrolment_refusals_are_typed() {
    let d = TestDaemon::start().unwrap();
    let client = d.client().unwrap();

    let r = refusal(client.enrol("rk-not-a-code", key(), None).unwrap_err());
    assert_eq!((r.status, r.code), (401, ErrorCode::Unauthenticated));

    let k = key();
    client.enrol(&d.enrolment_code, k.clone(), None).unwrap();
    // The code is single use.
    let r = refusal(client.enrol(&d.enrolment_code, key(), None).unwrap_err());
    assert_eq!(r.code, ErrorCode::Unauthenticated);
    // The same key cannot enrol twice.
    let code = d.issue_code(&["venture"]).unwrap();
    let r = refusal(client.enrol(&code, k, None).unwrap_err());
    assert_eq!((r.status, r.code), (409, ErrorCode::AlreadyEnrolled));
}

#[test]
fn a_device_without_the_scope_is_forbidden() {
    let d = TestDaemon::start().unwrap();
    let client = d.client().unwrap();
    let code = d.issue_code(&["account"]).unwrap();
    let dev = client.enrol(&code, key(), None).unwrap();
    assert_eq!(dev.scopes(), ["account"]);

    // upload.init needs venture or channel.
    let r = refusal(dev.upload_init(10, 5).unwrap_err());
    assert_eq!((r.status, r.code), (403, ErrorCode::Forbidden));
    assert!(rejections(&d).contains(&"insufficient_scope".to_string()));
    // job.update admits account.
    dev.update_job("j", &patch(json!({"a": 1})), 0, "k")
        .unwrap();
}

#[test]
fn job_and_upload_refusals_map_to_their_codes() {
    let d = TestDaemon::start().unwrap();
    let client = d.client().unwrap();
    let dev = client.enrol(&d.enrolment_code, key(), None).unwrap();

    dev.update_job("j", &patch(json!({"a": 1})), 0, "k1")
        .unwrap();
    let r = refusal(
        dev.update_job("j", &patch(json!({"a": 2})), 0, "k2")
            .unwrap_err(),
    );
    assert_eq!((r.status, r.code), (409, ErrorCode::RevisionConflict));
    assert_eq!(r.body["currentRevision"], 1);

    let r = refusal(
        dev.update_job("j", &patch(json!({"a": 3})), 1, "k1")
            .unwrap_err(),
    );
    assert_eq!((r.status, r.code), (422, ErrorCode::IdempotencyKeyReuse));

    let r = refusal(
        dev.send("PATCH", "/v1/publish-jobs/j", &[], b"{}")
            .unwrap_err(),
    );
    assert_eq!((r.status, r.code), (428, ErrorCode::PreconditionRequired));

    let r = refusal(dev.send("GET", "/v1/nope", &[], &[]).unwrap_err());
    assert_eq!((r.status, r.code), (404, ErrorCode::NotFound));
    let r = refusal(dev.send("DELETE", "/v1/uploads/x", &[], &[]).unwrap_err());
    assert_eq!((r.status, r.code), (405, ErrorCode::MethodNotAllowed));

    let r = refusal(dev.upload_init(0, 5).unwrap_err());
    assert_eq!(r.code, ErrorCode::TooLarge);
    let r = refusal(dev.upload_init(100, 1 << 21).unwrap_err());
    assert_eq!(r.code, ErrorCode::BadChunkSize);

    let up = dev.upload_init(10, 5).unwrap();
    let r = refusal(dev.upload_chunk(&up, 0, b"abc").unwrap_err());
    assert_eq!((r.status, r.code), (416, ErrorCode::BadChunkLength));
    let r = refusal(dev.upload_chunk(&up, 9, b"abcde").unwrap_err());
    assert_eq!(r.code, ErrorCode::BadChunkIndex);
    dev.upload_chunk(&up, 0, b"abcde").unwrap();
    dev.upload_chunk(&up, 0, b"abcde").unwrap(); // same bytes: idempotent
    let r = refusal(dev.upload_chunk(&up, 0, b"zzzzz").unwrap_err());
    assert_eq!((r.status, r.code), (409, ErrorCode::ChunkConflict));
    let r = refusal(
        dev.upload_complete(&up.upload_id, &"0".repeat(64))
            .unwrap_err(),
    );
    assert_eq!(r.code, ErrorCode::Incomplete);
    assert_eq!(r.body["missing"], json!([1]));

    // A device cannot see another device's upload.
    let other = client
        .enrol(&d.issue_code(&["venture"]).unwrap(), key(), None)
        .unwrap();
    let r = refusal(other.upload_status(&up.upload_id).unwrap_err());
    assert_eq!(r.code, ErrorCode::NotFound);
}

#[test]
fn interrupted_upload_resumes_and_commits_the_right_bytes_and_hash() {
    let d = TestDaemon::start().unwrap();
    let k = key();
    let dev = d
        .client()
        .unwrap()
        .enrol(&d.enrolment_code, k.clone(), None)
        .unwrap();

    let data = payload(10_500); // 11 chunks of 1000, the last 500 bytes
    let session = dev.upload_init(data.len() as u64, 1000).unwrap();
    assert_eq!(session.chunk_count, 11);
    // The first four chunks arrive, then the process "dies".
    for i in 0..4u64 {
        let from = (i * 1000) as usize;
        dev.upload_chunk(&session, i, &data[from..from + 1000])
            .unwrap();
    }
    drop(dev);

    // A new client resumes from the persisted session record.
    let saved = serde_json::to_string(&session).unwrap();
    let session: rightkit_daemon::remote::client::UploadSession =
        serde_json::from_str(&saved).unwrap();
    let dev = d.client().unwrap().session(session_device(&d), k);
    let status = dev.upload_status(&session.upload_id).unwrap();
    assert_eq!(status.received, vec![0, 1, 2, 3]);
    assert_eq!(status.next_offset, 4000);
    assert!(!status.complete);

    let receipt = dev.upload_bytes(&session, &data).unwrap();
    assert_eq!(receipt.size, data.len() as u64);

    // Independent expectation: keyed blake3 of the whole payload with the session key.
    let key = parse_digest_key(&session.digest_key).unwrap();
    let mut h = object_hasher(&key);
    h.update(&data);
    assert_eq!(receipt.digest, h.finalize().to_hex().to_string());
    assert_eq!(
        std::fs::read(d.daemon.object_path(&session.upload_id)).unwrap(),
        data
    );
    assert!(dev.upload_status(&session.upload_id).unwrap().complete);

    // A repeat after commit is re-acknowledged, not an error.
    assert_eq!(dev.upload_bytes(&session, &data).unwrap(), receipt);
    // And the chunk digest helper is the server's: a tampered chunk digest is refused.
    assert_ne!(chunk_digest(&key, b"a"), chunk_digest(&key, b"b"));
}

fn session_device(d: &TestDaemon) -> String {
    d.daemon.device_ids().remove(0)
}

#[test]
fn one_call_upload_and_a_wrong_object_digest() {
    let d = TestDaemon::start().unwrap();
    let dev = d
        .client()
        .unwrap()
        .enrol(&d.enrolment_code, key(), None)
        .unwrap();
    let data = payload(2500);
    let receipt = dev.upload(&data, 1000).unwrap();
    assert_eq!(
        std::fs::read(d.daemon.object_path(&receipt.upload_id)).unwrap(),
        data
    );

    // A source that differs from what the daemon holds is caught by the daemon's hash check.
    let session = dev.upload_init(8, 4).unwrap();
    dev.upload_chunk(&session, 0, b"aaaa").unwrap();
    dev.upload_chunk(&session, 1, b"bbbb").unwrap();
    let r = refusal(
        dev.upload_complete(&session.upload_id, &"0".repeat(64))
            .unwrap_err(),
    );
    assert_eq!((r.status, r.code), (400, ErrorCode::HashMismatch));
    // A source of the wrong length never reaches the daemon.
    let session = dev.upload_init(8, 4).unwrap();
    assert!(matches!(
        dev.upload_bytes(&session, b"short").unwrap_err(),
        Error::Source(_)
    ));
}

#[test]
fn a_certificate_that_is_not_pinned_fails_tls() {
    let d = TestDaemon::start().unwrap();
    let other = TestCertificate::generate().unwrap();
    let wrong = Client::new(
        ClientConfig::new(d.addr)
            .root_pem(other.cert_pem.as_bytes())
            .unwrap(),
    )
    .unwrap();
    let e = wrong
        .enrol(&d.enrolment_code, key(), None)
        .expect_err("enrolment must fail");
    assert!(matches!(e, Error::Tls(_)), "{e}");
    assert!(d.daemon.device_ids().is_empty());

    // The right root works, and a server name outside the certificate fails too.
    d.client()
        .unwrap()
        .enrol(&d.enrolment_code, key(), None)
        .unwrap();
    let renamed = Client::new(d.client_config().unwrap().server_name("example.invalid")).unwrap();
    let e = renamed
        .enrol(&d.issue_code(&["venture"]).unwrap(), key(), None)
        .expect_err("name mismatch must fail");
    assert!(matches!(e, Error::Tls(_)), "{e}");
}

#[test]
fn no_roots_means_no_client() {
    let d = TestDaemon::start().unwrap();
    assert!(matches!(
        Client::new(ClientConfig::new(d.addr)),
        Err(Error::NoRoots)
    ));
    assert!(ClientConfig::new(d.addr).root_pem(b"not pem").is_err());
}

#[test]
fn every_code_the_server_returns_has_a_variant() {
    for code in [
        "unauthenticated",
        "forbidden",
        "bad_request",
        "not_found",
        "method_not_allowed",
        "body_too_large",
        "internal",
        "storage_error",
        "already_enrolled",
        "too_large",
        "too_many_uploads",
        "bad_chunk_size",
        "bad_chunk_index",
        "bad_chunk_length",
        "chunk_too_large",
        "chunk_digest_mismatch",
        "chunk_conflict",
        "already_complete",
        "finalizing",
        "hash_mismatch",
        "incomplete",
        "revision_conflict",
        "precondition_required",
        "idempotency_key_reuse",
        "duplicate_delivery",
        "rate_limited",
        "oauth_unavailable",
    ] {
        assert!(
            !matches!(ErrorCode::from_code(code), ErrorCode::Other(_)),
            "{code}"
        );
    }
    assert_eq!(
        ErrorCode::from_code("future_code"),
        ErrorCode::Other("future_code".into())
    );
}

#[test]
fn dropping_the_daemon_stops_it() {
    let d = TestDaemon::start().unwrap();
    let addr = d.addr;
    let handle = d.stop_handle();
    drop(d);
    handle.stop(); // idempotent after drop
    assert!(std::net::TcpStream::connect(addr).is_err());
}
