//! Service registration (PK07-5): identity, method descriptors and handlers declared as one
//! value, hosted on the local transport only. Host and client run in this process over the real
//! current-user-only endpoint.

use rightkit_daemon::contract::MethodPolicy;
use rightkit_daemon::*;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn identity() -> ServiceIdentity {
    ServiceIdentity::new("reg-svc", "engine", 5, "1.0").expect("identity")
}

fn suite() -> (tempfile::TempDir, Suite) {
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    (dir, suite)
}

#[test]
fn a_registration_hosts_its_methods_for_the_apps_it_names_and_nobody_else() {
    let (_dir, suite) = suite();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let registration = Registration::new(identity())
        .method(
            MethodDescriptor::local("echo", ["alpha", "beta"]),
            move |cx, params, _emit| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(json!({ "app": cx.app(), "params": params }))
            },
        )
        .unwrap()
        .method(
            MethodDescriptor::local("narrow", ["alpha"]),
            |_cx, _params, _emit| Ok(json!("narrow")),
        )
        .unwrap();
    assert_eq!(registration.methods().ids().collect::<Vec<_>>(), ["echo", "narrow"]);

    let service = Service::builder()
        .register(&suite, registration, HostOptions::new())
        .expect("register")
        .start()
        .expect("start");
    assert!(service.local().is_some());

    let alpha = Client::connect(&suite, "reg-svc", "alpha").unwrap();
    let beta = Client::connect(&suite, "reg-svc", "beta").unwrap();
    let echoed = alpha.call("echo", json!({"n": 1})).unwrap();
    assert_eq!(echoed["app"], json!("alpha"));
    assert_eq!(echoed["params"], json!({"n": 1}));
    assert_eq!(alpha.call("narrow", json!({})).unwrap(), json!("narrow"));
    assert!(beta.call("echo", json!({})).is_ok());
    assert_eq!(
        beta.call("narrow", json!({})).unwrap_err().code,
        ERROR_NOT_ALLOWED
    );
    assert_eq!(
        alpha.call("unregistered", json!({})).unwrap_err().code,
        ERROR_NOT_ALLOWED
    );
    assert!(alpha.call("ping", json!({})).is_ok());
    // Refused calls never reached a handler.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_handler_error_is_the_error_the_client_sees() {
    let (_dir, suite) = suite();
    let registration = Registration::new(identity())
        .method(
            MethodDescriptor::local("fail", ["alpha"]),
            |_cx, _params, _emit| Err(ServiceError::new("my_code", "nope")),
        )
        .unwrap();
    let _service = Service::builder()
        .register(&suite, registration, HostOptions::new())
        .unwrap()
        .start()
        .unwrap();
    let alpha = Client::connect(&suite, "reg-svc", "alpha").unwrap();
    let err = alpha.call("fail", json!({})).unwrap_err();
    assert_eq!(err.code, "my_code");
}

#[test]
fn registration_refuses_what_cannot_work_before_anything_is_hosted() {
    let ok = |id: &str| MethodDescriptor::local(id, ["alpha"]);
    let handler = |_cx: &CallContext, _p: &Value, _e: &Emit| Ok(json!(null));

    // `ping` belongs to the host.
    assert_eq!(
        Registration::new(identity()).method(ok("ping"), handler).err(),
        Some(RegistrationError::Reserved("ping".to_string()))
    );
    // A method registered twice.
    assert_eq!(
        Registration::new(identity())
            .method(ok("a"), handler)
            .unwrap()
            .method(ok("a"), handler)
            .err(),
        Some(RegistrationError::Duplicate("a".to_string()))
    );
    // A bad id is the method table's refusal.
    assert!(matches!(
        Registration::new(identity()).method(ok("a/b"), handler).err(),
        Some(RegistrationError::Table(_))
    ));
    // A registration with no method hosts nothing.
    assert_eq!(
        Registration::new(identity()).into_parts().err(),
        Some(RegistrationError::NoMethods)
    );
}

#[test]
fn a_local_profile_refuses_methods_that_remote_devices_could_reach() {
    let remote_reachable =
        MethodDescriptor::new("both", MethodPolicy::local(["alpha"]).and_remote(["scope"]));
    let handler = |_cx: &CallContext, _p: &Value, _e: &Emit| Ok(json!(null));
    assert_eq!(
        Registration::new(identity())
            .method(remote_reachable.clone(), handler)
            .err(),
        Some(RegistrationError::RemoteMethodInLocalProfile("both".to_string()))
    );
    // The same method is fine once the registration says it offers a remote profile.
    let registration = Registration::new(identity())
        .transport(TransportProfile::LocalAndRemote)
        .method(remote_reachable, handler)
        .unwrap();
    assert_eq!(registration.profile(), TransportProfile::LocalAndRemote);
}

#[test]
fn an_identity_is_validated() {
    assert_eq!(
        ServiceIdentity::new("Not A Name", "engine", 5, "1.0"),
        Err(RegistrationError::BadIdentity("service name"))
    );
    assert_eq!(
        ServiceIdentity::new("ok-name", "", 5, "1.0"),
        Err(RegistrationError::BadIdentity("role"))
    );
    assert_eq!(
        ServiceIdentity::new("ok-name", "engine", 5, "bad\nversion"),
        Err(RegistrationError::BadIdentity("version"))
    );
    let id = ServiceIdentity::new("ok-name", "engine", 3, "2.1").unwrap();
    assert_eq!((id.service(), id.role(), id.rank(), id.version()), ("ok-name", "engine", 3, "2.1"));
}

#[cfg(feature = "remote")]
#[test]
fn a_local_only_registration_refuses_a_remote_daemon() {
    let (dir, suite) = suite();
    let registration = Registration::new(identity())
        .method(MethodDescriptor::local("m", ["alpha"]), |_c, _p, _e| Ok(json!(1)))
        .unwrap();
    let built = Service::builder()
        .register(&suite, registration, HostOptions::new())
        .unwrap()
        .remote(rightkit_daemon::remote::DaemonConfig::new(dir.path().join("remote")))
        .start();
    match built {
        Err(BuildError::Registration(RegistrationError::RemoteNotDeclared)) => {}
        Err(other) => panic!("wrong refusal: {other}"),
        Ok(_) => panic!("a local-only registration must not start a remote daemon"),
    }
}

/// The registration module opens no network address: it names no socket, listener or server
/// crate. (The one transport is the local host `Service::builder` already runs.)
#[test]
fn the_registration_source_opens_no_listener() {
    const SOURCE: &str = include_str!("../src/registration.rs");
    for forbidden in [
        "TcpListener",
        "UdpSocket",
        "std::net",
        "0.0.0.0",
        "tokio",
        "axum",
        "hyper",
        "rustls",
    ] {
        assert!(
            !SOURCE.contains(forbidden),
            "registration.rs must not mention {forbidden}"
        );
    }
}
