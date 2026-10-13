//! One handler, two transports. `Service::builder` hosts the same `Handler` and `MethodTable` on
//! the local transport (real current-user-only endpoint) and as `POST /v1/call/{method}` on the
//! remote daemon; the parity test walks the method table and runs every `transport: both` method
//! through both paths.
//!
//! What is identical, and what is the documented equivalent:
//! * success: the same result value, and the handler saw the same method and params;
//! * handler error: the same code (`revision_conflict`), and the HTTP status is the status table's;
//! * wrong scope / app: local `request_not_allowed`, remote `forbidden`; both are 403 in the status
//!   table and neither reaches the handler;
//! * unknown device: local has no devices; its analogue is an app absent from the table
//!   (`request_not_allowed`, refused at `hello`); remote is `unauthenticated` (401). Neither reaches
//!   the handler;
//! * replay: only the remote path signs nonces. A replayed signed request is `unauthenticated`
//!   and the handler runs once.

mod common;

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use common::*;
use rightkit_daemon::contract::{status_for_code, Caller, MethodPolicy, MethodTable, Transport};
use rightkit_daemon::remote::DaemonConfig;
use rightkit_daemon::*;
use serde_json::{json, Value};

#[derive(Default)]
struct Rec {
    seen: Mutex<Vec<(String, Value, Caller)>>,
}

impl Handler for Rec {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        self.seen
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone(), cx.origin().clone()));
        if method.starts_with("fail") {
            return Err(ServiceError::new(ERROR_REVISION_CONFLICT, "stale revision"));
        }
        Ok(json!({ "method": method, "params": params }))
    }
}

fn table() -> MethodTable {
    MethodTable::new()
        .method(
            "echo",
            MethodPolicy::local(["alpha"]).and_remote(["venture"]),
        )
        .method(
            "narrow",
            MethodPolicy::local(["alpha"]).and_remote(["account"]),
        )
        .method(
            "fail",
            MethodPolicy::local(["alpha"]).and_remote(["venture"]),
        )
        .method("local_only", MethodPolicy::local(["alpha"]))
        .method("remote_only", MethodPolicy::remote(["venture"]))
}

struct Fx {
    _dir: tempfile::TempDir,
    suite: Suite,
    service: Service,
    rec: Arc<Rec>,
    h: Harness,
}

fn start() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let state = dir.path().join("remote");
    let mut cfg = DaemonConfig::new(&state);
    cfg.limits.chunk_min = 4;
    let clock = Arc::new(TestClock(AtomicU64::new(T0)));
    let rec = Arc::new(Rec::default());
    let service = Service::builder()
        .methods(table())
        .handler(rec.clone())
        .local(
            &suite,
            ServiceConfig::new("dual", "test", 5, "test", Allowlist::new()),
            HostOptions::new(),
        )
        .remote_with(cfg.clone(), clock.clone(), None)
        .start()
        .expect("service starts");
    let h = Harness {
        dir: tempfile::tempdir().unwrap(),
        clock,
        daemon: service.daemon().unwrap().clone(),
        cfg,
        oauth: None,
    };
    Fx {
        _dir: dir,
        suite,
        service,
        rec,
        h,
    }
}

/// `Ok(result)` or `Err((code, status))`, the transport-neutral shape of one outcome.
type Outcome = Result<Value, (String, u16)>;

fn local(c: &Client, m: &str, params: &Value) -> Outcome {
    c.call(m, params.clone())
        .map_err(|e| (e.code.clone(), status_for_code(&e.code).unwrap_or(500)))
}

fn remote(fx: &Fx, dev: &Dev, m: &str, params: &Value) -> Outcome {
    let r = dev.json(&fx.h, "POST", &format!("/v1/call/{m}"), params);
    if r.status == 200 {
        Ok(r.body)
    } else {
        let code = r.body["error"]["code"]
            .as_str()
            .or_else(|| r.body["error"].as_str())
            .unwrap_or("?")
            .to_string();
        Err((code, r.status))
    }
}

fn both_methods(t: &MethodTable) -> Vec<String> {
    t.ids()
        .filter(|m| t.get(m).unwrap().transport() == Transport::Both)
        .map(str::to_string)
        .collect()
}

#[test]
fn parity_over_the_method_table() {
    let fx = start();
    let t = table();
    let methods = both_methods(&t);
    assert_eq!(
        methods,
        ["echo", "fail", "narrow"],
        "the table's both-methods"
    );

    let alpha = Client::connect(&fx.suite, "dual", "alpha").unwrap();
    // `venture` covers echo and fail; `narrow` needs `account`.
    let dev = fx.h.enrol_scopes(&["venture", "account"]);
    let venture_only = fx.h.enrol_scopes(&["venture"]);
    let channel_only = fx.h.enrol_scopes(&["channel"]);

    for m in &methods {
        let params = json!({ "n": 7, "m": m });

        // success / handler error: identical outcome on both paths.
        let l = local(&alpha, m, &params);
        let r = remote(&fx, &dev, m, &params);
        match (&l, &r) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{m} result"),
            (Err(a), Err(b)) => {
                assert_eq!(a.0, b.0, "{m} handler error code");
                assert_eq!(a.1, b.1, "{m} status");
                assert_eq!(b.1, 409);
            }
            other => panic!("{m}: paths disagree: {other:?}"),
        }
        assert_eq!(m == "fail", l.is_err(), "{m}");

        // wrong scope (device) / wrong app (local): both 403, handler untouched.
        let before = fx.rec.seen.lock().unwrap().len();
        let denied_remote = remote(&fx, &channel_only, m, &params).unwrap_err();
        assert_eq!(denied_remote, ("forbidden".to_string(), 403), "{m}");
        let ghost = Client::connect(&fx.suite, "dual", "ghost")
            .err()
            .expect("refused");
        assert_eq!(ghost.code, ERROR_NOT_ALLOWED);
        assert_eq!(status_for_code(&ghost.code), Some(denied_remote.1));
        if m == "narrow" {
            // a device with the wrong one of two valid scopes is refused the same way
            assert_eq!(
                remote(&fx, &venture_only, m, &params).unwrap_err(),
                ("forbidden".to_string(), 403)
            );
        }

        // unknown device: refused before the handler.
        let mut stranger = Dev::generate();
        stranger.id = "dev_0000000000000000".into();
        assert_eq!(
            remote(&fx, &stranger, m, &params).unwrap_err(),
            ("unauthenticated".to_string(), 401),
            "{m}"
        );
        assert_eq!(
            fx.rec.seen.lock().unwrap().len(),
            before,
            "{m}: handler ran on a refusal"
        );

        // replay: the second delivery of one signed request is refused, the handler ran once.
        let body = serde_json::to_vec(&params).unwrap();
        let req = dev.request_at(
            "POST",
            &format!("/v1/call/{m}"),
            &body,
            fx.h.now(),
            &format!("replay-{m}"),
            &[],
        );
        let first = fx.h.call(req.clone());
        let second = fx.h.call(req);
        assert!(
            first.status == 200 || first.status == 409,
            "{m} {}",
            first.status
        );
        assert_eq!(second.status, 401, "{m}");
        assert_eq!(second.body["error"], "unauthenticated");
        assert_eq!(
            fx.rec.seen.lock().unwrap().len(),
            before + 1,
            "{m}: the replayed request ran once"
        );
    }

    // The handler saw both caller kinds with the same method and params.
    let seen = fx.rec.seen.lock().unwrap();
    let echo: Vec<_> = seen.iter().filter(|(m, ..)| m == "echo").collect();
    assert!(echo
        .iter()
        .any(|(_, _, c)| matches!(c, Caller::LocalApp { .. })));
    assert!(echo
        .iter()
        .any(|(_, _, c)| matches!(c, Caller::Device { id, scopes } if id == &dev.id && scopes.contains("venture"))));
    drop(seen);
    fx.service.shutdown();
}

#[test]
fn only_both_methods_are_exposed_as_calls() {
    let fx = start();
    let dev = fx.h.enrol_scopes(&["venture", "account"]);
    let p = json!({});
    for m in ["local_only", "remote_only", "unlisted", "*", "echo/../x"] {
        let r = dev.json(
            &fx.h,
            "POST",
            &format!("/v1/call/{}", m.replace('/', "%2F")),
            &p,
        );
        assert_eq!(r.status, 403, "{m}");
        assert_eq!(r.body["error"]["code"], "forbidden", "{m}");
    }
    assert!(fx.rec.seen.lock().unwrap().is_empty());
    // The refusal is auditable and carries no params.
    assert!(fx
        .h
        .rejections()
        .iter()
        .any(|r| r == "method_not_permitted"));
}

#[test]
fn a_daemon_without_calls_has_no_call_route() {
    let h = Harness::new();
    let dev = h.enrol();
    let r = dev.json(&h, "POST", "/v1/call/echo", &json!({}));
    assert_eq!((r.status, &r.body["error"]), (404, &json!("not_found")));
    // Wrong verb on the call path.
    let g = dev.call(&h, "GET", "/v1/call/echo", b"");
    assert_eq!(g.status, 405);
}

#[test]
fn existing_v1_routes_still_serve_beside_the_call_route() {
    let fx = start();
    let dev = fx.h.enrol();
    let r = dev.patch_job(&fx.h, "j1", 0, "k1", &json!({"a": 1}));
    assert_eq!(r.status, 200);
    assert_eq!(r.body["revision"], 1);
}

#[test]
fn builder_refuses_incomplete_configuration() {
    assert!(matches!(
        Service::builder().start(),
        Err(BuildError::Missing("methods"))
    ));
    assert!(matches!(
        Service::builder().methods(table()).start(),
        Err(BuildError::Missing("handler"))
    ));
    assert!(matches!(
        Service::builder()
            .methods(table())
            .handler(Arc::new(Rec::default()))
            .start(),
        Err(BuildError::NoTransport)
    ));
}
