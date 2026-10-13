mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use common::*;
use rightkit_daemon::remote::signing::{
    H_NONCE, H_PROVIDER_SIGNATURE, H_PROVIDER_TIMESTAMP, H_SIGNATURE,
};
use rightkit_daemon::remote::{OAuthCallbackHandler, OAuthOutcome, Request};
use serde_json::{json, Value};

// ---- enrolment ---------------------------------------------------------------

#[test]
fn enrolment_is_single_use_and_logged() {
    let h = Harness::new();
    let code = h.daemon.issue_enrolment_code(ALL_SCOPES, 600).unwrap();
    let (dev, resp) = h.try_enrol(&code);
    assert_eq!(resp.status, 201);
    assert!(dev.id.starts_with("dev_"));
    assert_eq!(h.events("ENROLLED").len(), 1);
    assert_eq!(h.events("ENROLLED")[0]["contractEvent"], "device_enrolled");

    // The same code never enrols a second key.
    let (_, again) = h.try_enrol(&code);
    assert_eq!(again.status, 401);
    assert!(h.rejections().contains(&"bad_enrolment_code".to_string()));
    assert_eq!(h.daemon.device_ids().len(), 1);
}

#[test]
fn enrolment_rejects_expired_code_and_bad_proof_of_possession() {
    let h = Harness::new();
    let code = h.daemon.issue_enrolment_code(ALL_SCOPES, 10).unwrap();
    h.advance(11);
    let (_, resp) = h.try_enrol(&code);
    assert_eq!(resp.status, 401);
    assert!(h
        .rejections()
        .contains(&"expired_enrolment_code".to_string()));

    // A request signed by a different key than the one it announces is refused,
    // and the code survives for the legitimate holder.
    let code = h.daemon.issue_enrolment_code(ALL_SCOPES, 600).unwrap();
    let (a, b) = (Dev::generate(), Dev::generate());
    let mut req = a.enrol_request(&code, h.now(), "n-pop");
    req.headers.insert(
        H_SIGNATURE.into(),
        b.enrol_request(&code, h.now(), "n-pop").headers[H_SIGNATURE].clone(),
    );
    assert_eq!(h.call(req).status, 401);
    assert!(h.rejections().contains(&"bad_signature".to_string()));
    let (_, ok) = h.try_enrol(&code);
    assert_eq!(ok.status, 201);
}

#[test]
fn enrolment_requires_a_code() {
    let h = Harness::new();
    let dev = Dev::generate();
    let mut req = dev.enrol_request("x", h.now(), "n1");
    req.headers.remove("authorization");
    assert_eq!(h.call(req).status, 401);
    assert!(h.rejections().contains(&"missing_credentials".to_string()));
}

// ---- device-authenticated class ----------------------------------------------

#[test]
fn device_request_positive() {
    let h = Harness::new();
    let dev = h.enrol();
    let (id, _) = dev.upload_init(&h, 100, 50);
    let s = dev.call(&h, "GET", &format!("/v1/uploads/{id}"), b"");
    assert_eq!(s.status, 200);
}

#[test]
fn device_bad_signature() {
    let h = Harness::new();
    let dev = h.enrol();
    let mut req = dev.request(
        &h,
        "POST",
        "/v1/uploads",
        br#"{"size":100,"chunkBytes":50}"#,
    );
    req.body = br#"{"size":999,"chunkBytes":50}"#.to_vec(); // body changed after signing
    assert_eq!(h.call(req).status, 401);
    // A different key under the same device id is also refused.
    let mut imposter = Dev::generate();
    imposter.id = dev.id.clone();
    let r = imposter.call(&h, "GET", "/v1/uploads/up_x", b"");
    assert_eq!(r.status, 401);
    assert_eq!(
        h.rejections()
            .iter()
            .filter(|r| *r == "bad_signature")
            .count(),
        2
    );
}

#[test]
fn device_replayed_nonce() {
    let h = Harness::new();
    let dev = h.enrol();
    let req = dev.request_at("GET", "/v1/uploads/up_x", b"", h.now(), "fixed-nonce", &[]);
    assert_eq!(h.call(req.clone()).status, 404); // authenticated, upload unknown
    assert_eq!(h.call(req).status, 401);
    assert_eq!(h.rejections(), vec!["replayed_nonce"]);
}

#[test]
fn device_expired_and_future_timestamps() {
    let h = Harness::new();
    let dev = h.enrol();
    let old = dev.request_at("GET", "/v1/uploads/up_x", b"", h.now() - 301, "o1", &[]);
    let future = dev.request_at("GET", "/v1/uploads/up_x", b"", h.now() + 301, "o2", &[]);
    let edge = dev.request_at("GET", "/v1/uploads/up_x", b"", h.now() - 300, "o3", &[]);
    assert_eq!(h.call(old).status, 401);
    assert_eq!(h.call(future).status, 401);
    assert_eq!(h.call(edge).status, 404); // inside the window
    assert_eq!(
        h.rejections(),
        vec!["expired_timestamp", "expired_timestamp"]
    );
}

#[test]
fn device_unknown_missing_and_revoked() {
    let h = Harness::new();
    let dev = h.enrol();
    let mut ghost = Dev::generate();
    ghost.id = "dev_0000000000000000".into();
    assert_eq!(ghost.call(&h, "GET", "/v1/uploads/up_x", b"").status, 401);

    let mut req = dev.request(&h, "GET", "/v1/uploads/up_x", b"");
    req.headers.remove("authorization");
    assert_eq!(h.call(req).status, 401);

    assert!(h.daemon.revoke_device(&dev.id).unwrap());
    assert_eq!(dev.call(&h, "GET", "/v1/uploads/up_x", b"").status, 401);
    assert_eq!(
        h.rejections(),
        vec!["unknown_device", "missing_credentials", "revoked_device"]
    );
    assert_eq!(h.events("DEVICE_REVOKED").len(), 1);
}

#[test]
fn device_scope_is_enforced() {
    let h = Harness::new();
    let dev = h.enrol_scopes(&["account"]);
    // upload.init needs venture or channel.
    let r = dev.json(
        &h,
        "POST",
        "/v1/uploads",
        &json!({"size": 100, "chunkBytes": 50}),
    );
    assert_eq!(r.status, 403);
    assert!(h.rejections().contains(&"insufficient_scope".to_string()));
    // job.update accepts account.
    assert_eq!(dev.patch_job(&h, "j", 0, "k", &json!({"a": 1})).status, 200);
}

#[test]
fn wrong_class_for_route() {
    let h = Harness::new();
    h.advance(0);
    let dev = h.enrol();

    // Webhook credentials on a device route.
    let mut req = dev.request(&h, "POST", "/v1/uploads", b"{}");
    req.headers.insert(H_PROVIDER_SIGNATURE.into(), "00".into());
    assert_eq!(h.call(req).status, 401);

    // Device credentials on the webhook route.
    let body = b"{}";
    let mut hook = h.webhook("meta", b"meta-secret", h.now(), body);
    hook.headers
        .insert("authorization".into(), format!("RK-Device {}", dev.id));
    assert_eq!(h.call(hook).status, 401);

    // Device credentials on the OAuth callback route.
    let cb = dev.request(&h, "GET", "/v1/oauth/meta/callback?code=a&state=b", b"");
    assert_eq!(h.call(cb).status, 400);

    // An enrolment code on an ordinary device route.
    let mut req = dev.request(&h, "GET", "/v1/uploads/up_x", b"");
    req.headers
        .insert("authorization".into(), "RK-Enrol abc".into());
    assert_eq!(h.call(req).status, 401);

    assert_eq!(
        h.rejections(),
        vec![
            "wrong_class",
            "wrong_class",
            "wrong_class",
            "enrolment_code_on_non_enrol_route"
        ]
    );
    let classes: Vec<_> = h
        .events("AUTH_REJECTED")
        .iter()
        .map(|e| e["class"].clone())
        .collect();
    assert_eq!(
        classes,
        vec![
            json!("device_authenticated_mutation"),
            json!("signed_webhook"),
            json!("oauth_callback"),
            json!("device_authenticated_mutation")
        ]
    );
}

#[test]
fn unknown_route_and_wrong_method() {
    let h = Harness::new();
    let req = |m: &str, p: &str| Request {
        method: m.into(),
        path_and_query: p.into(),
        ..Default::default()
    };
    assert_eq!(h.call(req("GET", "/v1/nothing")).status, 404);
    assert_eq!(h.call(req("DELETE", "/v1/uploads/up_x")).status, 405);
}

// ---- signed webhook class ----------------------------------------------------

#[test]
fn webhook_positive_then_duplicate() {
    let h = Harness::new();
    let body = br#"{"entry":[{"id":"1"}]}"#;
    let req = h.webhook("meta", b"meta-secret", h.now(), body);
    assert_eq!(h.call(req.clone()).status, 202);
    assert_eq!(h.call(req).status, 409);
    let inbox = h.daemon.drain_webhooks().unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].1, "meta");
    assert_eq!(inbox[0].2, body.to_vec());
    assert!(h.daemon.drain_webhooks().unwrap().is_empty());
}

#[test]
fn webhook_prefix_is_accepted() {
    let h = Harness::new();
    let mut req = h.webhook("meta", b"meta-secret", h.now(), b"{}");
    let sig = req.headers[H_PROVIDER_SIGNATURE].clone();
    req.headers
        .insert(H_PROVIDER_SIGNATURE.into(), format!("sha256={sig}"));
    assert_eq!(h.call(req).status, 202);
}

#[test]
fn webhook_negatives() {
    let h = Harness::new();
    // Wrong secret.
    assert_eq!(
        h.call(h.webhook("meta", b"other", h.now(), b"{}")).status,
        401
    );
    // Body tampered after signing.
    let mut tampered = h.webhook("meta", b"meta-secret", h.now(), b"{\"a\":1}");
    tampered.body = b"{\"a\":2}".to_vec();
    assert_eq!(h.call(tampered).status, 401);
    // Timestamp outside tolerance, correctly signed for that timestamp.
    assert_eq!(
        h.call(h.webhook("meta", b"meta-secret", h.now() - 301, b"{}"))
            .status,
        401
    );
    assert_eq!(
        h.call(h.webhook("meta", b"meta-secret", h.now() + 301, b"{}"))
            .status,
        401
    );
    // A provider with no configured secret.
    assert_eq!(
        h.call(h.webhook("tiktok", b"x", h.now(), b"{}")).status,
        401
    );
    // Missing and malformed signature headers.
    let mut none = h.webhook("meta", b"meta-secret", h.now(), b"{}");
    none.headers.remove(H_PROVIDER_SIGNATURE);
    assert_eq!(h.call(none).status, 401);
    let mut junk = h.webhook("meta", b"meta-secret", h.now(), b"{}");
    junk.headers
        .insert(H_PROVIDER_SIGNATURE.into(), "zz".into());
    assert_eq!(h.call(junk).status, 401);
    let mut no_ts = h.webhook("meta", b"meta-secret", h.now(), b"{}");
    no_ts.headers.remove(H_PROVIDER_TIMESTAMP);
    assert_eq!(h.call(no_ts).status, 401);
    // An RK device signature header on a webhook route.
    let mut dev_sig = h.webhook("meta", b"meta-secret", h.now(), b"{}");
    dev_sig.headers.insert(H_SIGNATURE.into(), "AAAA".into());
    assert_eq!(h.call(dev_sig).status, 401);

    assert_eq!(
        h.rejections(),
        vec![
            "bad_signature",
            "bad_signature",
            "expired_timestamp",
            "expired_timestamp",
            "unknown_provider",
            "missing_credentials",
            "malformed_signature",
            "malformed_timestamp",
            "wrong_class"
        ]
    );
    assert!(h
        .events("AUTH_REJECTED")
        .iter()
        .all(|e| e["contractEvent"] == "webhook_rejected"));
    assert!(h.daemon.drain_webhooks().unwrap().is_empty());
}

#[test]
fn webhook_inbox_is_bounded() {
    let h = Harness::with(|c| c.max_webhook_inbox = 2);
    for i in 0..2 {
        let body = format!("{{\"i\":{i}}}");
        assert_eq!(
            h.call(h.webhook("meta", b"meta-secret", h.now(), body.as_bytes()))
                .status,
            202
        );
    }
    assert_eq!(
        h.call(h.webhook("meta", b"meta-secret", h.now(), b"{\"i\":9}"))
            .status,
        429
    );
}

// ---- OAuth callback class ----------------------------------------------------

struct FakeOAuth(Mutex<Vec<(String, Option<String>)>>, OAuthOutcome);
impl OAuthCallbackHandler for FakeOAuth {
    fn complete(
        &self,
        provider: &str,
        _: Option<String>,
        state: Option<String>,
        _: Option<String>,
    ) -> OAuthOutcome {
        self.0.lock().unwrap().push((provider.into(), state));
        self.1
    }
}

fn get(path: &str) -> Request {
    Request {
        method: "GET".into(),
        path_and_query: path.into(),
        headers: BTreeMap::new(),
        body: vec![],
    }
}

#[test]
fn oauth_callback_delegates_and_redirects() {
    let fake = Arc::new(FakeOAuth(Mutex::new(vec![]), OAuthOutcome::Completed));
    let h = Harness::with_oauth(
        |c| c.oauth_success_redirect = "/done".into(),
        Some(fake.clone()),
    );
    let r = h.call(get("/v1/oauth/meta/callback?code=c%2B1&state=s-1"));
    assert_eq!(r.status, 302);
    assert!(r.headers.contains(&("Location".into(), "/done".into())));
    assert_eq!(
        fake.0.lock().unwrap()[0],
        ("meta".to_string(), Some("s-1".to_string()))
    );
    assert_eq!(h.events("OAUTH_COMPLETED").len(), 1);
}

#[test]
fn oauth_rejection_and_missing_handler() {
    let fake = Arc::new(FakeOAuth(Mutex::new(vec![]), OAuthOutcome::Rejected));
    let h = Harness::with_oauth(|_| {}, Some(fake));
    assert_eq!(h.call(get("/v1/oauth/meta/callback?state=x")).status, 400);
    assert_eq!(h.rejections(), vec!["oauth_rejected"]);

    let none = Harness::new();
    assert_eq!(
        none.call(get("/v1/oauth/meta/callback?state=x")).status,
        502
    );
}

#[test]
fn oauth_class_refuses_other_credentials() {
    let fake = Arc::new(FakeOAuth(Mutex::new(vec![]), OAuthOutcome::Completed));
    let h = Harness::with_oauth(|_| {}, Some(fake.clone()));
    for (k, v) in [
        ("authorization", "RK-Device dev_x"),
        (H_SIGNATURE, "AA"),
        (H_PROVIDER_SIGNATURE, "00"),
    ] {
        let mut r = get("/v1/oauth/meta/callback?code=a&state=b");
        r.headers.insert(k.into(), v.into());
        assert_eq!(h.call(r).status, 400, "{k}");
    }
    assert!(
        fake.0.lock().unwrap().is_empty(),
        "handler must never run for the wrong class"
    );
}

mod real_flow {
    use super::*;
    use rightkit_daemon::remote::oauth::FlowOAuthHandler;
    use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
    use rightkit_http::Client;
    use rightkit_oauth::server::{
        MemoryPendingStateStore, SealedFileTokenStore, ServerAuthFlow, ServerProvider, TokenStore,
    };
    use rightkit_oauth::OAuthConfig;
    use std::time::Duration;

    #[test]
    fn rightkit_oauth_flow_end_to_end() {
        // The flow stores tokens in rightkit-oauth's sealed file store, which exists on Unix
        // only: its private-directory check has no Windows form and it refuses to open there.
        if !cfg!(unix) {
            println!(
                "SKIP rightkit_oauth_flow_end_to_end: SealedFileTokenStore is Unix-only, so \
                 the server flow has no token store to run against on this OS"
            );
            return;
        }
        let server = MockServer::start(|_, _| {
            MockResponse::json(
                200,
                &json!({"access_token":"a-secret","refresh_token":"r-secret","expires_in":3600}),
            )
        });
        let provider = ServerProvider::new(
            "test",
            OAuthConfig::new("client", server.url("/authorize"), server.url("/token"))
                .with_client_secret("client-secret"),
            "https://postright.orthiclabs.com/v1/oauth/test/callback",
        );
        let flow = Arc::new(
            ServerAuthFlow::new(
                // The mock token endpoint is on 127.0.0.1; the default public-web policy would
                // deny the exchange (502) before the route check that must answer 400.
                Client::new(loopback_config()),
                [provider],
                Arc::new(MemoryPendingStateStore::default()),
                Duration::from_secs(300),
            )
            .unwrap(),
        );
        let tokens_dir = tempfile::tempdir().unwrap();
        let tokens: Arc<dyn TokenStore> = Arc::new(
            SealedFileTokenStore::new(tokens_dir.path().join("tokens"), [7u8; 32]).unwrap(),
        );
        let handler = Arc::new(FlowOAuthHandler {
            flow: flow.clone(),
            tokens: tokens.clone(),
        });
        let h = Harness::with_oauth(|_| {}, Some(handler));

        // Wrong provider route: the flow completes for "test" but the route says "other".
        let wrong = flow.begin("test", "acct", &[]).unwrap();
        let r = h.call(get(&format!(
            "/v1/oauth/other/callback?code=c&state={}",
            wrong.state
        )));
        assert_eq!(r.status, 400);
        assert!(
            tokens.get("acct", "test").unwrap().is_none(),
            "tokens must not attach to a mismatched route"
        );

        // Correct route stores tokens once; the state cannot be replayed.
        let ok = flow.begin("test", "acct", &[]).unwrap();
        let path = format!("/v1/oauth/test/callback?code=c&state={}", ok.state);
        assert_eq!(h.call(get(&path)).status, 302);
        assert!(tokens.get("acct", "test").unwrap().is_some());
        assert_eq!(h.call(get(&path)).status, 400);

        // Unknown state.
        assert_eq!(
            h.call(get("/v1/oauth/test/callback?code=c&state=nope"))
                .status,
            400
        );
    }
}

// ---- uploads -----------------------------------------------------------------

#[test]
fn upload_out_of_order_complete_and_commit() {
    let h = Harness::new();
    let dev = h.enrol();
    let data = pattern_bytes(150);
    let (id, key) = dev.upload_init(&h, 150, 64);
    assert_eq!(h.events("UPLOAD_STARTED").len(), 1);

    assert_eq!(dev.put_chunk(&h, &id, 2, &data[128..]).status, 204);
    assert_eq!(dev.put_chunk(&h, &id, 0, &data[..64]).status, 204);
    // Retrying a delivered chunk is idempotent.
    assert_eq!(dev.put_chunk(&h, &id, 0, &data[..64]).status, 204);
    let st = dev.call(&h, "GET", &format!("/v1/uploads/{id}"), b"");
    assert_eq!(st.body["received"], json!([0, 2]));
    assert_eq!(st.body["nextOffset"], 64);

    // Not complete yet.
    let early = dev.complete(&h, &id, &keyed_digest(&key, &data));
    assert_eq!(early.status, 400);
    assert_eq!(early.body["missing"], json!([1]));

    assert_eq!(dev.put_chunk(&h, &id, 1, &data[64..128]).status, 204);
    let done = dev.complete(&h, &id, &keyed_digest(&key, &data));
    assert_eq!(done.status, 200, "{:?}", done.body);
    assert_eq!(std::fs::read(h.daemon.object_path(&id)).unwrap(), data);
    assert!(!h
        .dir
        .path()
        .join("uploads")
        .join(format!("{id}.part"))
        .exists());
    let ev = &h.events("UPLOAD_COMPLETE")[0];
    assert_eq!(ev["uploadId"], id.as_str());
    assert_eq!(ev["contractEvent"], "upload_completed");

    // Completion is idempotent for the same digest and refused for another.
    assert_eq!(
        dev.complete(&h, &id, &keyed_digest(&key, &data)).status,
        200
    );
    assert_eq!(
        dev.complete(&h, &id, &keyed_digest(&key, b"other")).status,
        400
    );
    assert_eq!(dev.put_chunk(&h, &id, 0, &data[..64]).status, 409);
}

#[test]
fn upload_final_hash_mismatch_discards_session() {
    let h = Harness::new();
    let dev = h.enrol();
    let data = pattern_bytes(100);
    let (id, key) = dev.upload_init(&h, 100, 50);
    dev.put_chunk(&h, &id, 0, &data[..50]);
    dev.put_chunk(&h, &id, 1, &data[50..]);
    let wrong = keyed_digest(&key, b"not the object");
    let r = dev.complete(&h, &id, &wrong);
    assert_eq!(
        (r.status, r.body["error"].as_str()),
        (400, Some("hash_mismatch"))
    );
    assert!(
        !h.daemon.object_path(&id).exists(),
        "nothing may be committed"
    );
    assert_eq!(
        dev.call(&h, "GET", &format!("/v1/uploads/{id}"), b"")
            .status,
        404
    );
    assert!(h.events("UPLOAD_COMPLETE").is_empty());
}

#[test]
fn upload_key_binds_digest_to_session() {
    let h = Harness::new();
    let dev = h.enrol();
    let data = pattern_bytes(60);
    let (id, _) = dev.upload_init(&h, 60, 60);
    dev.put_chunk(&h, &id, 0, &data);
    // An unkeyed or wrongly keyed digest of the right bytes does not verify.
    let unkeyed = blake3::hash(&data).to_hex().to_string();
    assert_eq!(dev.complete(&h, &id, &unkeyed).status, 400);
}

#[test]
fn upload_chunk_digest_header_is_verified() {
    let h = Harness::new();
    let dev = h.enrol();
    let data = pattern_bytes(60);
    let (id, key) = dev.upload_init(&h, 60, 60);
    let pq = format!("/v1/uploads/{id}/chunks/0");
    let bad = dev.request_at(
        "PUT",
        &pq,
        &data,
        h.now(),
        &next_nonce(),
        &[("x-chunk-digest", &"0".repeat(64))],
    );
    assert_eq!(h.call(bad).status, 400);
    let good = dev.request_at(
        "PUT",
        &pq,
        &data,
        h.now(),
        &next_nonce(),
        &[("x-chunk-digest", &keyed_digest(&key, &data))],
    );
    assert_eq!(h.call(good).status, 204);
}

#[test]
fn upload_oversize_is_refused() {
    let h = Harness::new();
    let dev = h.enrol();
    // Object over the cap.
    let r = dev.json(
        &h,
        "POST",
        "/v1/uploads",
        &json!({"size": 1001, "chunkBytes": 64}),
    );
    assert_eq!(
        (r.status, r.body["error"].as_str()),
        (403, Some("too_large"))
    );
    // Chunk size outside the bounds.
    assert_eq!(
        dev.json(
            &h,
            "POST",
            "/v1/uploads",
            &json!({"size": 500, "chunkBytes": 65})
        )
        .status,
        400
    );
    assert_eq!(
        dev.json(
            &h,
            "POST",
            "/v1/uploads",
            &json!({"size": 500, "chunkBytes": 3})
        )
        .status,
        400
    );
    assert_eq!(
        dev.json(
            &h,
            "POST",
            "/v1/uploads",
            &json!({"size": 0, "chunkBytes": 64})
        )
        .status,
        403
    );

    // A chunk larger than the declared chunk size, and one past the hard cap.
    let (id, _) = dev.upload_init(&h, 500, 32);
    assert_eq!(dev.put_chunk(&h, &id, 0, &pattern_bytes(33)).status, 416);
    assert_eq!(dev.put_chunk(&h, &id, 0, &pattern_bytes(65)).status, 416);
    assert_eq!(dev.put_chunk(&h, &id, 0, &pattern_bytes(66)).status, 413);
    // Short chunk where a full one is required, and an index past the end.
    assert_eq!(dev.put_chunk(&h, &id, 0, &pattern_bytes(31)).status, 416);
    assert_eq!(dev.put_chunk(&h, &id, 16, &pattern_bytes(32)).status, 416);
    // The last chunk must be exactly the remainder (500 = 15*32 + 20).
    assert_eq!(dev.put_chunk(&h, &id, 15, &pattern_bytes(32)).status, 416);
    assert_eq!(dev.put_chunk(&h, &id, 15, &pattern_bytes(20)).status, 204);
}

#[test]
fn upload_conflicting_chunk_and_foreign_device() {
    let h = Harness::new();
    let (a, b) = (h.enrol(), h.enrol());
    let (id, _) = a.upload_init(&h, 100, 50);
    assert_eq!(a.put_chunk(&h, &id, 0, &pattern_bytes(50)).status, 204);
    let mut other = pattern_bytes(50);
    other[0] ^= 1;
    assert_eq!(a.put_chunk(&h, &id, 0, &other).status, 409);
    // Another device cannot see or write the session.
    assert_eq!(
        b.call(&h, "GET", &format!("/v1/uploads/{id}"), b"").status,
        404
    );
    assert_eq!(b.put_chunk(&h, &id, 1, &pattern_bytes(50)).status, 404);
}

#[test]
fn upload_sessions_expire_and_are_bounded() {
    let h = Harness::with(|c| c.limits.max_open_per_device = 2);
    let dev = h.enrol();
    let (id, _) = dev.upload_init(&h, 100, 50);
    dev.put_chunk(&h, &id, 0, &pattern_bytes(50));
    dev.upload_init(&h, 100, 50);
    let r = dev.json(
        &h,
        "POST",
        "/v1/uploads",
        &json!({"size": 100, "chunkBytes": 50}),
    );
    assert_eq!(r.status, 403);
    h.advance(101);
    assert_eq!(
        dev.call(&h, "GET", &format!("/v1/uploads/{id}"), b"")
            .status,
        404
    );
    // Expired sessions are swept, freeing the quota and the partial file.
    dev.upload_init(&h, 100, 50);
    assert!(!h
        .dir
        .path()
        .join("uploads")
        .join(format!("{id}.part"))
        .exists());
}

// ---- compare-and-swap --------------------------------------------------------

#[test]
fn cas_conflict_and_progress() {
    let h = Harness::new();
    let dev = h.enrol();
    let r = dev.patch_job(&h, "job-1", 0, "k1", &json!({"caption": "a", "slot": 1}));
    assert_eq!((r.status, r.body["revision"].clone()), (200, json!(1)));
    assert!(r.headers.contains(&("ETag".into(), "\"1\"".into())));

    // A stale writer sees the current revision and changes nothing.
    let stale = dev.patch_job(&h, "job-1", 0, "k2", &json!({"caption": "stale"}));
    assert_eq!(stale.status, 409);
    assert_eq!(
        stale.body,
        json!({"error": "revision_conflict", "currentRevision": 1})
    );

    let ok = dev.patch_job(&h, "job-1", 1, "k3", &json!({"caption": "b", "slot": null}));
    assert_eq!(ok.status, 200);
    assert_eq!(ok.body["fields"], json!({"caption": "b"}));
    assert_eq!(h.daemon.job_revision("job-1"), 2);
}

#[test]
fn cas_two_devices_one_wins() {
    let h = Harness::new();
    let (mac, xps) = (h.enrol(), h.enrol());
    assert_eq!(
        mac.patch_job(&h, "j", 0, "m1", &json!({"t": "mac"})).status,
        200
    );
    let lost = xps.patch_job(&h, "j", 0, "x1", &json!({"t": "xps"}));
    assert_eq!(
        (lost.status, lost.body["currentRevision"].clone()),
        (409, json!(1))
    );
}

#[test]
fn cas_idempotency_and_preconditions() {
    let h = Harness::new();
    let dev = h.enrol();
    let first = dev.patch_job(&h, "j", 0, "same", &json!({"a": 1}));
    // The retry of an applied request replays the response, not a conflict.
    let retry = dev.patch_job(&h, "j", 0, "same", &json!({"a": 1}));
    assert_eq!(retry.status, 200);
    assert_eq!(retry.body, first.body);
    assert!(retry
        .headers
        .contains(&("Idempotency-Replayed".into(), "true".into())));
    assert_eq!(h.daemon.job_revision("j"), 1);
    // The same key with a different request is refused.
    assert_eq!(
        dev.patch_job(&h, "j", 0, "same", &json!({"a": 2})).status,
        422
    );

    // If-Match and Idempotency-Key are required.
    let raw = br#"{"a":1}"#;
    let no_if = dev.request_at(
        "PATCH",
        "/v1/publish-jobs/j",
        raw,
        h.now(),
        &next_nonce(),
        &[("idempotency-key", "z")],
    );
    assert_eq!(h.call(no_if).status, 428);
    let no_key = dev.request_at(
        "PATCH",
        "/v1/publish-jobs/j",
        raw,
        h.now(),
        &next_nonce(),
        &[("if-match", "1")],
    );
    assert_eq!(h.call(no_key).status, 428);
    let junk = dev.request_at(
        "PATCH",
        "/v1/publish-jobs/j",
        raw,
        h.now(),
        &next_nonce(),
        &[("if-match", "abc"), ("idempotency-key", "z")],
    );
    assert_eq!(h.call(junk).status, 428);
}

// ---- restart -----------------------------------------------------------------

#[test]
fn restart_preserves_enrolments_nonces_codes_jobs_and_webhooks() {
    let mut h = Harness::new();
    let dev = h.enrol();
    let spare = h.daemon.issue_enrolment_code(ALL_SCOPES, 600).unwrap();
    assert_eq!(dev.patch_job(&h, "j", 0, "k", &json!({"a": 1})).status, 200);
    let used = dev.request_at(
        "GET",
        "/v1/uploads/up_x",
        b"",
        h.now(),
        "persisted-nonce",
        &[],
    );
    assert_eq!(h.call(used.clone()).status, 404);
    let hook = h.webhook("meta", b"meta-secret", h.now(), b"{\"x\":1}");
    assert_eq!(h.call(hook.clone()).status, 202);

    h.restart();

    // The enrolled device still authenticates; a replay of a pre-restart nonce is refused.
    assert_eq!(dev.call(&h, "GET", "/v1/uploads/up_x", b"").status, 404);
    assert_eq!(h.call(used).status, 401);
    assert_eq!(h.rejections().last().unwrap(), "replayed_nonce");
    // Jobs keep their revision; the idempotency record still replays.
    assert_eq!(h.daemon.job_revision("j"), 1);
    assert_eq!(dev.patch_job(&h, "j", 0, "k", &json!({"a": 1})).status, 200);
    assert_eq!(h.daemon.job_revision("j"), 1);
    // Webhook dedupe and inbox survive.
    assert_eq!(h.call(hook).status, 409);
    assert_eq!(h.daemon.drain_webhooks().unwrap().len(), 1);
    // The unused enrolment code is still valid; a spent one is not.
    let (_, ok) = h.try_enrol(&spare);
    assert_eq!(ok.status, 201);
    let (_, spent) = h.try_enrol(&spare);
    assert_eq!(spent.status, 401);
    assert_eq!(h.daemon.device_ids().len(), 2);
}

#[test]
fn restart_preserves_revocation() {
    let mut h = Harness::new();
    let dev = h.enrol();
    h.daemon.revoke_device(&dev.id).unwrap();
    h.restart();
    assert_eq!(dev.call(&h, "GET", "/v1/uploads/up_x", b"").status, 401);
}

#[test]
fn a_job_submission_is_one_state_write_and_survives_a_crash() {
    let mut h = Harness::new();
    let dev = h.enrol();
    let body = serde_json::to_vec(&json!({ "caption": "a" })).unwrap();
    let signed = dev.request_at(
        "PATCH",
        "/v1/publish-jobs/job-1",
        &body,
        h.now(),
        "submission-nonce",
        &[("if-match", "0"), ("idempotency-key", "k1")],
    );

    // The replay nonce and the job change share one state.json write (the old code wrote twice).
    let before = h.daemon.metrics().state_saves;
    assert_eq!(h.call(signed.clone()).status, 200);
    assert_eq!(h.daemon.metrics().state_saves - before, 1);
    let before = h.daemon.metrics().state_saves;
    assert_eq!(
        dev.patch_job(&h, "job-1", 1, "k2", &json!({ "caption": "b" }))
            .status,
        200
    );
    assert_eq!(h.daemon.metrics().state_saves - before, 1);

    // Reopen without any shutdown step: both acknowledged submissions are already on disk.
    h.restart();
    assert_eq!(h.daemon.job_revision("job-1"), 2);
    // The first submission's nonce is durable too, so its signed request is refused as a replay.
    assert_eq!(h.call(signed).status, 401);
    assert_eq!(h.rejections().last().unwrap(), "replayed_nonce");
}

#[test]
fn a_bad_upload_record_is_skipped_and_reported_and_the_daemon_opens() {
    let mut h = Harness::new();
    let uploads = h.cfg.state_dir.join("uploads");
    std::fs::write(uploads.join("damaged.json"), b"{ damaged").unwrap();
    // A directory with a record's name: it exists and cannot be read as a file.
    std::fs::create_dir(uploads.join("unreadable.json")).unwrap();

    h.restart();

    let skipped = h.events("UPLOAD_RECORD_SKIPPED");
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    let reason_of = |file: &str| {
        skipped
            .iter()
            .find(|event| event["file"] == file)
            .map(|event| event["reason"].as_str().unwrap().to_string())
    };
    assert_eq!(reason_of("damaged.json").as_deref(), Some("unparsable"));
    assert_eq!(reason_of("unreadable.json").as_deref(), Some("unreadable"));
    // The unparsable record is set aside with its bytes; the unreadable one is not moved.
    assert!(!uploads.join("damaged.json").exists());
    assert_eq!(
        std::fs::read(uploads.join("damaged.json.bad")).unwrap(),
        b"{ damaged"
    );
    assert!(uploads.join("unreadable.json").is_dir());
    assert!(!skipped
        .iter()
        .any(|event| event.to_string().contains("damaged\"")));

    // The daemon works: a device can still enrol.
    let code = h.daemon.issue_enrolment_code(ALL_SCOPES, 600).unwrap();
    let (_, resp) = h.try_enrol(&code);
    assert_eq!(resp.status, 201);
}

#[test]
fn interrupted_upload_resumes_after_restart_and_verifies() {
    let mut h = Harness::new();
    let dev = h.enrol();
    let data = pattern_bytes(200);
    let (id, key) = dev.upload_init(&h, 200, 50);
    assert_eq!(dev.put_chunk(&h, &id, 0, &data[..50]).status, 204);
    assert_eq!(dev.put_chunk(&h, &id, 1, &data[50..100]).status, 204);

    h.restart(); // crash between chunk 1 and chunk 2

    let st = dev.call(&h, "GET", &format!("/v1/uploads/{id}"), b"");
    assert_eq!(st.status, 200);
    assert_eq!(st.body["received"], json!([0, 1]));
    assert_eq!(st.body["nextOffset"], 100, "resume by offset");

    assert_eq!(dev.put_chunk(&h, &id, 2, &data[100..150]).status, 204);
    assert_eq!(dev.put_chunk(&h, &id, 3, &data[150..]).status, 204);
    let done = dev.complete(&h, &id, &keyed_digest(&key, &data));
    assert_eq!(done.status, 200);
    assert_eq!(std::fs::read(h.daemon.object_path(&id)).unwrap(), data);

    // A committed object also survives a further restart.
    h.restart();
    let st = dev.call(&h, "GET", &format!("/v1/uploads/{id}"), b"");
    assert_eq!(st.body["complete"], true);
}

#[test]
fn restart_finishes_an_interrupted_commit() {
    let mut h = Harness::new();
    let dev = h.enrol();
    let data = pattern_bytes(60);
    let (id, key) = dev.upload_init(&h, 60, 60);
    dev.put_chunk(&h, &id, 0, &data);
    assert_eq!(
        dev.complete(&h, &id, &keyed_digest(&key, &data)).status,
        200
    );
    // Simulate a crash after the intent was saved but before the rename.
    std::fs::rename(
        h.daemon.object_path(&id),
        h.dir.path().join("uploads").join(format!("{id}.part")),
    )
    .unwrap();
    h.restart();
    assert_eq!(std::fs::read(h.daemon.object_path(&id)).unwrap(), data);
}

#[test]
fn restart_drops_chunks_whose_data_is_gone() {
    let mut h = Harness::new();
    let dev = h.enrol();
    let (id, _) = dev.upload_init(&h, 100, 50);
    dev.put_chunk(&h, &id, 0, &pattern_bytes(50));
    std::fs::remove_file(h.dir.path().join("uploads").join(format!("{id}.part"))).unwrap();
    h.restart();
    let st = dev.call(&h, "GET", &format!("/v1/uploads/{id}"), b"");
    assert_eq!(
        st.body["received"],
        json!([]),
        "an acknowledged chunk is never claimed without its bytes"
    );
}

// ---- events ------------------------------------------------------------------

#[test]
fn events_are_written_as_json_lines_without_secrets() {
    let h = Harness::new();
    let dev = h.enrol();
    let mut req = dev.request(&h, "GET", "/v1/uploads/up_x", b"");
    req.headers
        .insert(H_NONCE.into(), "bad nonce with spaces".into());
    h.call(req);
    let log = std::fs::read_to_string(h.dir.path().join("events.jsonl")).unwrap();
    let lines: Vec<Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(lines.iter().any(|l| l["event"] == "ENROLLED"));
    assert!(lines
        .iter()
        .any(|l| l["event"] == "AUTH_REJECTED" && l["reason"] == "malformed_nonce"));
    assert!(!log.contains("RK-Device") && !log.contains("meta-secret"));
}

#[test]
fn provider_outside_the_slug_rule_never_reaches_a_handler() {
    let fake = Arc::new(FakeOAuth(Mutex::new(vec![]), OAuthOutcome::Completed));
    let h = Harness::with_oauth(
        |c| c.oauth_success_redirect = "/done".into(),
        Some(fake.clone()),
    );
    let long = "a".repeat(65);
    for p in ["Instagram", "a_b", "", long.as_str()] {
        let r = h.call(get(&format!("/v1/oauth/{p}/callback?code=c&state=s")));
        assert_eq!(
            (r.status, &r.body["error"]),
            (404, &json!("not_found")),
            "oauth {p:?}"
        );
        let r = h.call(h.webhook(p, b"meta-secret", h.now(), b"{}"));
        assert_eq!(
            (r.status, &r.body["error"]),
            (404, &json!("not_found")),
            "webhook {p:?}"
        );
    }
    assert!(fake.0.lock().unwrap().is_empty());
    assert!(h.events("OAUTH_COMPLETED").is_empty());
    // A valid slug dispatches normally.
    assert_eq!(
        h.call(get("/v1/oauth/instagram-login/callback?code=c&state=s"))
            .status,
        302
    );
}
