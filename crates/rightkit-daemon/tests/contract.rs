//! The method table drives the local allowlist, the handler sees a typed `Caller`, and the status
//! table is the contract's error mapping. Host and client run in this process over the real
//! current-user-only endpoint. Runs in both feature states (`cargo test -p rightkit-daemon` and
//! `--features test-mode`): nothing here depends on the override switches.

use rightkit_daemon::contract::{
    status_for_code, status_for_error, Caller, MethodPolicy, MethodTable, TableError, Transport,
};
use rightkit_daemon::*;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Rec {
    seen: Mutex<Vec<(String, Option<PeerIdentity>, Caller)>>,
}

impl Handler for Rec {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        self.seen.lock().unwrap().push((
            method.to_string(),
            cx.peer().cloned(),
            cx.origin().clone(),
        ));
        Ok(params.clone())
    }
}

fn table() -> MethodTable {
    MethodTable::new()
        .method("open", MethodPolicy::local(["*"]))
        .method("narrow", MethodPolicy::local(["alpha"]).and_remote(["s"]))
        .method("remote_only", MethodPolicy::remote(["s"]))
}

fn start() -> (tempfile::TempDir, Suite, Hosted, Arc<Rec>) {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let cfg = ServiceConfig::new("tbl", "test", 5, "test", Allowlist::new()).with_methods(&table());
    let rec = Arc::new(Rec::default());
    let hosted = host(&suite, cfg, rec.clone()).expect("host starts");
    (dir, suite, hosted, rec)
}

#[test]
fn the_method_table_drives_the_local_allowlist() {
    let (_d, suite, _h, rec) = start();
    let alpha = Client::connect(&suite, "tbl", "alpha").unwrap();
    let beta = Client::connect(&suite, "tbl", "beta").unwrap();

    // hello advertises what the table allows each app.
    let methods = |c: &Client| -> Vec<String> {
        c.hello()["methods"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap().to_string())
            .collect()
    };
    // An app with its own entry does not inherit the "*" entry.
    assert_eq!(methods(&alpha), ["narrow"]);
    assert_eq!(methods(&beta), ["open"]);

    assert_eq!(alpha.call("narrow", json!(1)).unwrap(), json!(1));
    assert_eq!(beta.call("open", json!(2)).unwrap(), json!(2));
    assert_eq!(
        alpha.call("open", json!(0)).unwrap_err().code,
        ERROR_NOT_ALLOWED
    );
    assert_eq!(
        beta.call("narrow", json!(3)).unwrap_err().code,
        ERROR_NOT_ALLOWED
    );
    // A remote-only method is not reachable by any local app, even a wildcard one.
    assert_eq!(
        alpha.call("remote_only", json!(4)).unwrap_err().code,
        ERROR_NOT_ALLOWED
    );
    assert_eq!(
        alpha.call("unlisted", json!(5)).unwrap_err().code,
        ERROR_NOT_ALLOWED
    );
    assert!(beta.call("ping", json!({})).is_ok());

    // The handler saw exactly the allowed calls, each with a typed local caller and a peer.
    let seen = rec.seen.lock().unwrap();
    let names: Vec<_> = seen.iter().map(|(m, _, _)| m.as_str()).collect();
    assert_eq!(names, ["narrow", "open"]);
    for (_, peer, caller) in seen.iter() {
        assert!(peer.is_some());
        assert!(matches!(caller, Caller::LocalApp { .. }));
        assert_eq!(caller.peer(), peer.as_ref());
    }
}

#[test]
fn table_and_allowlist_agree_for_every_app_and_method() {
    let t = table();
    let list = t.allowlist();
    for app in ["alpha", "beta"] {
        let caller = Caller::local_app(app, PeerIdentity::default());
        for m in ["open", "narrow", "remote_only", "unlisted", "ping"] {
            assert_eq!(
                t.authorize(&caller, m).is_ok(),
                list.permits(app, m),
                "{app} {m}"
            );
        }
    }
    assert_eq!(t.get("remote_only").unwrap().transport(), Transport::Remote);
    assert_eq!(t.get("narrow").unwrap().transport(), Transport::Both);
}

#[test]
fn a_device_is_checked_against_scopes_not_apps() {
    let t = table();
    let dev = Caller::device("dev_1", ["s"]);
    assert!(dev.peer().is_none());
    assert!(t.authorize(&dev, "narrow").is_ok());
    assert!(t.authorize(&dev, "remote_only").is_ok());
    assert_eq!(t.authorize(&dev, "open").unwrap_err().code, ERROR_FORBIDDEN);
    let none = Caller::device("dev_2", Vec::<String>::new());
    assert_eq!(none.label(), "dev_2");
    assert_eq!(
        t.authorize(&none, "remote_only").unwrap_err().code,
        ERROR_FORBIDDEN
    );
}

#[test]
fn error_codes_map_to_statuses() {
    assert_eq!(status_for_code(ERROR_UNAUTHENTICATED), Some(401));
    assert_eq!(status_for_code(ERROR_FORBIDDEN), Some(403));
    assert_eq!(status_for_code(ERROR_REVISION_CONFLICT), Some(409));
    assert_eq!(status_for_error(&ServiceError::handoff()), 503);
}

#[test]
fn wildcard_glob_and_empty_method_names_are_refused_at_construction() {
    for bad in [
        "*", "", "a*", "a?", "[a]", "{a}", "a b", "a/b", "a%2Fb", "a#b", "a\\b", "a\n",
    ] {
        let r = MethodTable::new().try_method(bad, MethodPolicy::local(["*"]));
        assert!(
            matches!(r, Err(TableError::EmptyMethod | TableError::BadMethod(_))),
            "{bad:?} accepted"
        );
    }
    for ok in ["echo", "job.update", "a-b_c:d", "speech.transcribe"] {
        assert!(MethodTable::new()
            .try_method(ok, MethodPolicy::local(["a"]))
            .is_ok());
    }
    // A table cannot carry a `*` method, so the allowlist cannot grow a method wildcard from it.
    let t = MethodTable::new().method("echo", MethodPolicy::local(["a"]));
    assert!(!t.allowlist().permits("a", "anything"));
}

#[test]
#[should_panic(expected = "MethodTable::method")]
fn the_infallible_builder_panics_on_a_wildcard_method() {
    let _ = MethodTable::new().method("*", MethodPolicy::local(["a"]));
}

#[test]
fn and_remote_with_no_scope_is_an_error_not_any_device() {
    for policy in [
        MethodPolicy::local(["a"]).and_remote(Vec::<String>::new()),
        MethodPolicy::remote(Vec::<String>::new()),
        MethodPolicy::local(["a"]).and_remote([] as [&str; 0]),
    ] {
        assert_eq!(
            MethodTable::new()
                .try_method("m", policy.clone())
                .unwrap_err(),
            TableError::NoDeviceScopes("m".into())
        );
    }
    // A local-only method needs no scope.
    assert!(MethodTable::new()
        .try_method("m", MethodPolicy::local(["a"]))
        .is_ok());
}

#[test]
#[should_panic(expected = "names no scope")]
fn the_infallible_builder_panics_on_an_unscoped_device_policy() {
    let _ = MethodTable::new().method(
        "m",
        MethodPolicy::local(["a"]).and_remote(Vec::<String>::new()),
    );
}
