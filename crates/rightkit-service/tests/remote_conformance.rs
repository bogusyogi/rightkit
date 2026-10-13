//! Conformance against the daemon-api v1 contract and its fixtures.

mod common;

use std::collections::BTreeMap;

use common::*;
use rightkit_contracts::types::daemon_api::{AuthClassId, DaemonApi, Method};
use rightkit_contracts::{parse, Contract};
use rightkit_service::remote::events::contract_name;
use rightkit_service::remote::routes::{routes_v1_1, CALL_ROUTE, ROUTES};
use rightkit_service::remote::{DaemonConfig, Request, UploadLimits};
use serde_json::{json, Value};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/fixtures/daemon-api/valid/api-v1.json"
);
/// The crate's embedded copy (ships with `cargo package`); `embedded_schema_equals_repository_contract`
/// fails when it drifts from the repository-root `contracts/` source.
const SCHEMA_TEXT: &str = include_str!("../schemas/daemon-api.v1.schema.json");
const SCHEMA_SOURCE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/daemon-api.v1.schema.json"
);

#[test]
fn embedded_schema_equals_repository_contract() {
    let source = std::fs::read_to_string(SCHEMA_SOURCE).expect("contracts/ schema");
    assert!(
        SCHEMA_TEXT == source,
        "schemas/daemon-api.v1.schema.json differs from contracts/daemon-api.v1.schema.json; copy it over"
    );
}

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

/// The schema example carries the concrete route list; the valid fixture repeats
/// it. Both must equal the code's table, route by route and status by status.
#[test]
fn schema_example_routes_match_fixture_and_code() {
    let schema = schema();
    let example = schema["examples"][0]["routes"].as_array().unwrap();
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    assert_eq!(
        example,
        fixture["routes"].as_array().unwrap(),
        "schema example and fixture disagree"
    );
    assert_eq!(example.len(), ROUTES.len());
    for r in ROUTES {
        let c = example
            .iter()
            .find(|c| c["routeId"] == r.id)
            .unwrap_or_else(|| panic!("{} missing from the schema example", r.id));
        let responses: Vec<u64> = c["responses"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        let code: Vec<u64> = r.responses.iter().map(|s| u64::from(*s)).collect();
        assert_eq!(responses, code, "{} responses", r.id);
        assert_eq!(c["method"], r.method, "{}", r.id);
        assert_eq!(c["path"], r.pattern, "{}", r.id);
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
    let schema: Value = serde_json::from_str(SCHEMA_TEXT).unwrap();
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
    // Statuses added to the contract in 0.1.2 that only another scope or an
    // unknown upload reach: 400 bad chunk size, 403 scope, 404 unknown upload.
    declared(
        "upload.init",
        dev.json(
            &h,
            "POST",
            "/v1/uploads",
            &json!({"size": 100, "chunkBytes": 1}),
        )
        .status,
    );
    declared(
        "upload.status",
        readonly
            .call(&h, "GET", &format!("/v1/uploads/{id}"), b"")
            .status,
    );
    declared(
        "upload.chunk",
        readonly.put_chunk(&h, &id, 0, &pattern_bytes(50)).status,
    );
    declared(
        "upload.chunk",
        dev.put_chunk(&h, "up_none", 0, &pattern_bytes(50)).status,
    );
    declared(
        "upload.complete",
        readonly.complete(&h, &id, &"0".repeat(64)).status,
    );
    declared(
        "upload.complete",
        dev.complete(&h, "up_none", &"0".repeat(64)).status,
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
    // This harness wires no OAuth handler: the callback answers 502 oauth_unavailable.
    let r = h.call(cb);
    declared("oauth.callback", r.status);
    assert_eq!(
        (r.status, &r.body["error"]),
        (502, &json!("oauth_unavailable"))
    );

    let full = Harness::with(|c| c.max_webhook_inbox = 0);
    let r = full.call(full.webhook("meta", b"meta-secret", full.now(), b"{}"));
    declared("webhook.inbound", r.status);
    assert_eq!((r.status, &r.body["error"]), (429, &json!("rate_limited")));
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
    ("remote/core.rs", include_str!("../src/remote/core.rs")),
    ("remote/http.rs", include_str!("../src/remote/http.rs")),
];

fn schema() -> Value {
    serde_json::from_str(SCHEMA_TEXT).unwrap()
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

/// The schema's `errors` enum is exactly the set of codes the daemon returns,
/// plus `x-reservedErrors`: names in the vocabulary that no route returns yet.
#[test]
fn every_error_code_the_daemon_returns_is_in_the_schema_enum() {
    let schema = schema();
    let enum_codes: std::collections::BTreeSet<String> = schema["properties"]["errors"]["items"]
        ["properties"]["code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let reserved: std::collections::BTreeSet<String> = schema["properties"]["errors"]
        ["x-reservedErrors"]
        .as_array()
        .expect("schema marks reserved error codes")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let returned = source_error_codes();
    // The scanner must actually see the codes it guards.
    for known in [
        "unauthenticated",
        "precondition_required",
        "idempotency_key_reuse",
        "rate_limited",
    ] {
        assert!(
            returned.contains(known),
            "scanner missed {known}: {returned:?}"
        );
    }
    let missing: Vec<_> = returned.difference(&enum_codes).collect();
    assert!(
        missing.is_empty(),
        "codes the daemon returns but the schema enum lacks: {missing:?}"
    );
    let unlisted: Vec<_> = enum_codes
        .difference(&returned)
        .filter(|c| !reserved.contains(*c))
        .collect();
    assert!(
        unlisted.is_empty(),
        "enum codes no route returns and not reserved: {unlisted:?}"
    );
    let stray: Vec<_> = reserved.difference(&enum_codes).collect();
    assert!(
        stray.is_empty(),
        "reserved codes not in the enum: {stray:?}"
    );
    let emitted: Vec<_> = reserved.intersection(&returned).collect();
    assert!(
        emitted.is_empty(),
        "reserved codes the daemon now returns; unreserve them: {emitted:?}"
    );
}

/// Every internal event name passed as a literal to `emit(`.
fn emitted_event_literals() -> std::collections::BTreeSet<String> {
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
    internal
}

/// Internal events with no lifecycle-vocabulary name, by design: they are
/// diagnostics in `events.jsonl`. Naming one means adding it to the schema's
/// `lifecycleEvents` enum and to `events::contract_name`.
/// AUTH_REJECTED is mapped to `webhook_rejected` for the signed-webhook class
/// only; every other AUTH_REJECTED stays internal.
const INTERNAL_EVENTS: &[&str] = &[
    "AUTH_REJECTED",
    "OAUTH_COMPLETED",
    "STORAGE_ERROR",
    "UPLOAD_RECORD_SKIPPED",
    "UPLOAD_REJECTED",
    "WEBHOOK_ACCEPTED",
];

#[test]
fn every_emitted_event_is_mapped_or_classified_internal() {
    let internal = emitted_event_literals();
    for ev in &internal {
        assert!(
            contract_name(ev).is_some() || INTERNAL_EVENTS.contains(&ev.as_str()),
            "{ev} has no contract name and is not classified internal"
        );
    }
    for ev in INTERNAL_EVENTS {
        assert!(
            contract_name(ev).is_none(),
            "{ev} is mapped to a contract name; drop it from INTERNAL_EVENTS"
        );
        assert!(
            internal.contains(*ev),
            "{ev} is classified internal but never emitted"
        );
    }
}

/// Contract names of the emitted events, plus `webhook_rejected`.
fn emitted_contract_events() -> std::collections::BTreeSet<String> {
    let internal = emitted_event_literals();
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
    let api = api();
    let contract = api
        .routes
        .iter()
        .find(|r| r.route_id == "job.update")
        .unwrap();
    let r = ROUTES.iter().find(|r| r.id == "job.update").unwrap();
    assert_eq!(contract.responses.as_slice(), r.responses);
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

/// daemon-api 1.1.0 adds the `device_call` class. The v1 fixture and `ROUTES` stay exactly the
/// 1.0.0 table (tests above); the 1.1.0 fixture is that table plus the call route, and the code
/// serves exactly those.
#[test]
fn schema_1_1_adds_device_call_without_touching_v1() {
    let v1 = api();
    assert_eq!(v1.contract_version, "1.0.0");
    assert!(v1.auth_classes.device_call.is_none());
    assert!(v1
        .routes
        .iter()
        .all(|r| r.auth_class != AuthClassId::DeviceCall));

    let text = std::fs::read_to_string(FIXTURE_V1_1).unwrap();
    let v11: DaemonApi = parse(Contract::DaemonApi, &text).unwrap();
    assert_eq!(v11.contract_version, "1.1.0");
    let class = v11
        .auth_classes
        .device_call
        .as_ref()
        .expect("device_call class");
    assert!(class.mutation_allowed);
    assert_eq!(class.required_headers, ["Authorization"]);

    // 1.1.0 routes = the 1.0.0 routes unchanged, then the call route.
    assert_eq!(v11.routes.len(), ROUTES.len() + 1);
    assert_eq!(v11.routes[..ROUTES.len()], v1.routes[..]);
    assert_eq!(v11.routes.len(), routes_v1_1().count());
    let c = v11.routes.last().unwrap();
    assert_eq!(c.route_id, CALL_ROUTE.id);
    assert_eq!(method_str(c.method), CALL_ROUTE.method);
    assert_eq!(c.path, CALL_ROUTE.pattern);
    assert_eq!(c.auth_class, CALL_ROUTE.class);
    assert_eq!(c.mutation, CALL_ROUTE.mutation);
    assert_eq!(c.cas_required, CALL_ROUTE.cas);
    assert_eq!(c.scopes, CALL_ROUTE.scopes);
    assert_eq!(c.responses, CALL_ROUTE.responses);
    // Handler errors surface through the status table; each status is declared.
    for s in [200u16, 400, 401, 403, 409, 500, 502, 503, 504] {
        assert!(CALL_ROUTE.responses.contains(&s), "{s}");
    }
}

#[test]
fn schema_1_1_device_call_rules_are_enforced() {
    let base: Value =
        serde_json::from_str(&std::fs::read_to_string(FIXTURE_V1_1).unwrap()).unwrap();
    let check = |edit: &dyn Fn(&mut Value)| {
        let mut doc = base.clone();
        edit(&mut doc);
        parse::<DaemonApi>(Contract::DaemonApi, &doc.to_string())
    };
    assert!(check(&|_| {}).is_ok());
    // A call route must be POST on /v1/call/{name} without compare-and-swap.
    assert!(check(&|d| d["routes"][8]["method"] = json!("PATCH")).is_err());
    assert!(check(&|d| d["routes"][8]["path"] = json!("/v1/elsewhere/{method}")).is_err());
    assert!(check(&|d| d["routes"][8]["casRequired"] = json!(true)).is_err());
    // The class entry is optional, but when present it carries a device token only.
    assert!(check(&|d| {
        d["authClasses"]
            .as_object_mut()
            .unwrap()
            .remove("device_call");
    })
    .is_ok());
    assert!(
        check(&|d| d["authClasses"]["device_call"]["credentials"] = json!(["hmac_sha256"]))
            .is_err()
    );
    assert!(check(&|d| d["authClasses"]["device_call"]["mutationAllowed"] = json!(false)).is_err());
    // device_call does not widen the other classes: a webhook still may not mutate.
    assert!(check(&|d| d["routes"][7]["mutation"] = json!(true)).is_err());
}
