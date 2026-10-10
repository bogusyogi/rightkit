//! Conformance against the daemon-api v1 contract and its fixtures.

mod common;

use std::collections::BTreeMap;

use common::*;
use rightkit_contracts::types::daemon_api::{AuthClassId, DaemonApi, Method};
use rightkit_contracts::{parse, Contract};
use rightkit_daemon::events::contract_name;
use rightkit_daemon::routes::ROUTES;
use rightkit_daemon::{DaemonConfig, Request, UploadLimits};
use serde_json::{json, Value};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/fixtures/daemon-api/valid/api-v1.json"
);
const SCHEMA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/daemon-api.v1.schema.json"
);

fn api() -> DaemonApi {
    parse(
        Contract::DaemonApi,
        &std::fs::read_to_string(FIXTURE).unwrap(),
    )
    .unwrap()
}

fn method_str(m: Method) -> &'static str {
    match m {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
    }
}

#[test]
fn route_table_matches_the_contract() {
    let api = api();
    assert_eq!(api.routes.len(), ROUTES.len());
    for c in &api.routes {
        let r = ROUTES
            .iter()
            .find(|r| r.id == c.route_id)
            .unwrap_or_else(|| panic!("{} unimplemented", c.route_id));
        assert_eq!(r.method, method_str(c.method), "{}", c.route_id);
        assert_eq!(r.pattern, c.path, "{}", c.route_id);
        assert_eq!(r.class, c.auth_class, "{}", c.route_id);
        assert_eq!(r.mutation, c.mutation, "{}", c.route_id);
        assert_eq!(r.cas, c.cas_required, "{}", c.route_id);
        assert_eq!(r.scopes, c.scopes.as_slice(), "{}", c.route_id);
        assert_eq!(r.responses, c.responses.as_slice(), "{}", c.route_id);
    }
}

#[test]
fn exactly_three_classes_and_their_rules() {
    let api = api();
    for r in ROUTES {
        // Only device-authenticated routes may mutate; callbacks and webhooks never do.
        if r.class != AuthClassId::DeviceAuthenticatedMutation {
            assert!(!r.mutation, "{}", r.id);
        }
    }
    assert!(
        api.auth_classes
            .device_authenticated_mutation
            .mutation_allowed
    );
    assert!(!api.auth_classes.oauth_callback.mutation_allowed);
    assert!(!api.auth_classes.signed_webhook.mutation_allowed);
    assert_eq!(
        api.auth_classes.signed_webhook.required_headers,
        ["X-Provider-Signature", "X-Provider-Timestamp"]
    );
    assert_eq!(api.mutations.precondition.header, "If-Match");
    assert_eq!(api.mutations.idempotency.header, "Idempotency-Key");
    assert_eq!(api.mutations.conflict.status, 409);
    assert_eq!(api.transport.tls, "required");
}

#[test]
fn defaults_follow_the_contract_limits() {
    let api = api();
    assert_eq!(UploadLimits::from_contract(&api), UploadLimits::default());
    assert_eq!(api.upload.digest.algorithm, "blake3");
    assert_eq!(api.upload.digest.mode, "keyed");
    assert!(api.upload.durable_ack_required);
    let c = DaemonConfig::from_contract(&api, "/tmp/unused");
    assert_eq!(
        c.idempotency_ttl_secs,
        api.mutations.idempotency.ttl_seconds
    );
    // The four upload steps name routes the daemon implements.
    for id in [
        &api.upload.routes.init,
        &api.upload.routes.chunk,
        &api.upload.routes.status,
        &api.upload.routes.complete,
    ] {
        assert!(ROUTES.iter().any(|r| r.id == id));
    }
}

#[test]
fn lifecycle_event_names_are_in_the_schema_enum() {
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(SCHEMA).unwrap()).unwrap();
    let allowed: Vec<&str> = schema["properties"]["lifecycleEvents"]["items"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for ev in [
        "LISTENING",
        "SHUTDOWN",
        "ENROLLED",
        "DEVICE_REVOKED",
        "UPLOAD_STARTED",
        "UPLOAD_COMPLETE",
        "JOB_UPDATED",
    ] {
        let name = contract_name(ev).unwrap();
        assert!(allowed.contains(&name), "{ev} -> {name}");
    }
}

/// Every status the daemon returns for a contract-visible outcome is declared
/// for that route, and error bodies use the contract's error codes.
#[test]
fn statuses_and_error_codes_are_declared() {
    let api = api();
    let declared = |route: &str, status: u16| {
        let r = api.routes.iter().find(|r| r.route_id == route).unwrap();
        assert!(
            r.responses.contains(&status),
            "{route} returned undeclared {status}"
        );
    };
    let h = Harness::new();
    let code = h.daemon.issue_enrolment_code(ALL_SCOPES, 600).unwrap();
    let (dev, r) = h.try_enrol(&code);
    declared("device.enrol", r.status);
    declared("device.enrol", h.try_enrol(&code).1.status); // spent code
    let (_, readonly) = (0, h.enrol_scopes(&["account"]));

    let init = dev.json(
        &h,
        "POST",
        "/v1/uploads",
        &json!({"size": 100, "chunkBytes": 50}),
    );
    declared("upload.init", init.status);
    declared(
        "upload.init",
        dev.json(
            &h,
            "POST",
            "/v1/uploads",
            &json!({"size": 9999, "chunkBytes": 50}),
        )
        .status,
    );
    declared(
        "upload.init",
        readonly
            .json(
                &h,
                "POST",
                "/v1/uploads",
                &json!({"size": 10, "chunkBytes": 10}),
            )
            .status,
    );
    let mut unauth = dev.request(&h, "POST", "/v1/uploads", b"{}");
    unauth.headers.remove("authorization");
    let r = h.call(unauth);
    declared("upload.init", r.status);
    assert_eq!(r.body["error"], "unauthenticated");
    assert_eq!(
        api.errors
            .iter()
            .find(|e| e.code == "unauthenticated")
            .unwrap()
            .status,
        r.status
    );

    let id = init.body["uploadId"].as_str().unwrap().to_string();
    declared(
        "upload.chunk",
        dev.put_chunk(&h, &id, 0, &pattern_bytes(50)).status,
    );
    declared(
        "upload.chunk",
        dev.put_chunk(&h, &id, 0, &pattern_bytes(51)).status,
    );
    let mut diff = pattern_bytes(50);
    diff[3] ^= 9;
    declared("upload.chunk", dev.put_chunk(&h, &id, 0, &diff).status);
    declared(
        "upload.status",
        dev.call(&h, "GET", &format!("/v1/uploads/{id}"), b"")
            .status,
    );
    declared(
        "upload.status",
        dev.call(&h, "GET", "/v1/uploads/up_none", b"").status,
    );
    declared(
        "upload.complete",
        dev.complete(&h, &id, &"0".repeat(64)).status,
    ); // incomplete -> 400
    declared(
        "upload.complete",
        dev.json(
            &h,
            "POST",
            &format!("/v1/uploads/{id}/complete"),
            &json!({}),
        )
        .status,
    );

    declared(
        "job.update",
        dev.patch_job(&h, "j", 0, "k1", &json!({"a": 1})).status,
    );
    let conflict = dev.patch_job(&h, "j", 0, "k2", &json!({"a": 2}));
    declared("job.update", conflict.status);
    let entry = api
        .errors
        .iter()
        .find(|e| e.code == "revision_conflict")
        .unwrap();
    assert_eq!(
        (entry.status, conflict.body["error"].as_str()),
        (conflict.status, Some("revision_conflict"))
    );
    assert_eq!(conflict.body["currentRevision"], 1);
    for field in &api.mutations.conflict.returns {
        assert!(
            conflict.body.get(field).is_some(),
            "conflict must return {field}"
        );
    }
    declared(
        "job.update",
        readonly
            .patch_job(&h, "j2", 0, "k3", &json!({"a": 1}))
            .status,
    );

    let hook_ok = h.webhook("meta", b"meta-secret", h.now(), b"{}");
    declared("webhook.inbound", h.call(hook_ok.clone()).status);
    declared("webhook.inbound", h.call(hook_ok).status);
    declared(
        "webhook.inbound",
        h.call(h.webhook("meta", b"bad", h.now(), b"{}")).status,
    );

    let cb = Request {
        method: "GET".into(),
        path_and_query: "/v1/oauth/meta/callback?state=x".into(),
        headers: BTreeMap::new(),
        body: vec![],
    };
    let r = h.call(cb);
    assert_eq!(r.status, 502); // no handler wired in this harness: not a contract outcome
}

#[test]
fn fixture_exchange_replays_byte_for_byte() {
    let api = api();
    let ex = &api.exchanges[0];
    assert_eq!(ex.route_id, "job.update");
    let h = Harness::new();
    let dev = h.enrol();
    // Bring the job to revision 8, then replay the fixture request (If-Match 7).
    for i in 0..8 {
        assert_eq!(
            dev.patch_job(&h, "job-42", i, &format!("seed-{i}"), &json!({"n": i}))
                .status,
            200
        );
    }
    let body = serde_json::to_vec(ex.request.body.as_ref().unwrap()).unwrap();
    let mut extra: Vec<(String, String)> = Vec::new();
    for (k, v) in &ex.request.headers {
        extra.push((k.to_ascii_lowercase(), v.clone()));
    }
    let extra_ref: Vec<(&str, &str)> = extra
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let req = dev.request_at(
        "PATCH",
        "/v1/publish-jobs/job-42",
        &body,
        h.now(),
        &next_nonce(),
        &extra_ref,
    );
    let resp = h.call(req);
    assert_eq!(resp.status, ex.response.status);
    assert_eq!(Some(&resp.body), ex.response.body.as_ref());
}

#[test]
fn every_valid_and_invalid_fixture_still_checks() {
    // The shared fixture checker is the contract crate's; running it here pins
    // the fixtures this daemon was built against.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures");
    let report = rightkit_contracts::fixtures::check_tree(&root).unwrap();
    assert!(report.is_green(), "{:?}", report.failures);
}

// ---- contract/code drift guards ----------------------------------------------

const DAEMON_SRC: &[(&str, &str)] = &[
    ("daemon.rs", include_str!("../src/daemon.rs")),
    ("http.rs", include_str!("../src/http.rs")),
];

fn schema() -> Value {
    serde_json::from_str(&std::fs::read_to_string(SCHEMA).unwrap()).unwrap()
}

/// The first string literal in `s`, if any.
fn first_literal(s: &str) -> Option<&str> {
    let start = s.find('"')? + 1;
    let len = s[start..].find('"')?;
    Some(&s[start..start + len])
}

/// The last string literal in `s`, if any (the code argument of a call, even
/// when rustfmt spreads the call over several lines).
fn last_literal(s: &str) -> Option<&str> {
    let end = s.rfind('"')?;
    let start = s[..end].rfind('"')? + 1;
    Some(&s[start..end])
}

/// Every string literal passed as the code of `Response::err(status, "code")`,
/// as the last argument of `reject(.., status, "code")`, or as an inline
/// `"error": "code"` body field.
fn source_error_codes() -> std::collections::BTreeSet<String> {
    let mut codes = std::collections::BTreeSet::new();
    for (_, src) in DAEMON_SRC {
        for (i, _) in src.match_indices("Response::err(") {
            let call = &src[i..];
            let call = &call[..call.find(')').unwrap()];
            if let Some(lit) = last_literal(call) {
                codes.insert(lit.to_string());
            }
        }
        for (i, _) in src.match_indices(".reject(") {
            let call = &src[i..];
            let call = &call[..call.find(')').unwrap()];
            if let Some(lit) = last_literal(call) {
                codes.insert(lit.to_string());
            }
        }
        for (i, _) in src.match_indices("\"error\": \"") {
            let tail = &src[i + "\"error\": ".len()..];
            codes.insert(first_literal(tail).unwrap().to_string());
        }
    }
    codes
}

#[test]
fn every_error_code_the_daemon_returns_is_in_the_schema_enum() {
    let schema = schema();
    let allowed: Vec<&str> = schema["properties"]["errors"]["items"]["properties"]["code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let codes = source_error_codes();
    // The scanner must actually see the codes it guards.
    for known in [
        "unauthenticated",
        "precondition_required",
        "idempotency_key_reuse",
    ] {
        assert!(codes.contains(known), "scanner missed {known}: {codes:?}");
    }
    let missing: Vec<_> = codes
        .iter()
        .filter(|c| !allowed.contains(&c.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "codes missing from the schema enum: {missing:?}"
    );
}

/// Internal event names passed to `emit(`, mapped to the contract vocabulary.
fn emitted_contract_events() -> std::collections::BTreeSet<String> {
    let mut internal = std::collections::BTreeSet::new();
    for (_, src) in DAEMON_SRC {
        for (i, _) in src.match_indices(".emit(") {
            let tail = &src[i + ".emit(".len()..];
            // `emit(event, fields)` forwarders pass a variable, not a literal.
            if tail.trim_start().starts_with('"') {
                internal.insert(first_literal(tail).unwrap().to_string());
            }
        }
    }
    let mut out = std::collections::BTreeSet::new();
    for ev in &internal {
        if let Some(n) = contract_name(ev) {
            out.insert(n.to_string());
        }
    }
    // AUTH_REJECTED on the signed-webhook class carries `webhook_rejected`.
    if internal.contains("AUTH_REJECTED") {
        out.insert("webhook_rejected".to_string());
    }
    out
}

#[test]
fn every_schema_event_has_an_emitter_or_is_reserved() {
    let schema = schema();
    let prop = &schema["properties"]["lifecycleEvents"];
    let listed: Vec<String> = prop["items"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let reserved: Vec<String> = prop["x-reservedEvents"]
        .as_array()
        .expect("schema marks reserved events")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let emitted = emitted_contract_events();
    assert!(emitted.contains("daemon_started") && emitted.contains("job_updated"));
    for ev in &listed {
        assert!(
            emitted.contains(ev) || reserved.contains(ev),
            "{ev} has no emitter and is not reserved"
        );
    }
    for ev in &reserved {
        assert!(listed.contains(ev), "reserved {ev} is not in the enum");
        assert!(
            !emitted.contains(ev),
            "reserved {ev} is emitted; unreserve it"
        );
    }
    for ev in &emitted {
        assert!(listed.contains(ev), "emitted {ev} is not in the schema");
    }
}

#[test]
fn job_update_documents_every_status_it_returns() {
    let r = ROUTES.iter().find(|r| r.id == "job.update").unwrap();
    for status in [200, 400, 401, 403, 409, 422, 428] {
        assert!(r.responses.contains(&status), "{status}");
    }
    let h = Harness::new();
    let dev = h.enrol();
    let mut no_if_match = dev.patch_job(&h, "j", 0, "k1", &json!({"a": 1}));
    assert_eq!(no_if_match.status, 200);
    // Same key, different body: 422.
    no_if_match = dev.patch_job(&h, "j", 0, "k1", &json!({"a": 2}));
    assert_eq!(
        (no_if_match.status, &no_if_match.body["error"]),
        (422, &json!("idempotency_key_reuse"))
    );
}
