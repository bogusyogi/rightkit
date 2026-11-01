//! rightkit-daemon: host a real local service in an isolated suite, discover it through service.json, call it over the
//! current-user-only socket or pipe; an app the registration does not name is refused.
use rightkit_daemon::{
    Client, HostOptions, MethodDescriptor, Registration, Service, ServiceIdentity, Suite,
    ERROR_NOT_ALLOWED,
};
use serde_json::{json, Value};

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let dir = ctx.dir("daemon")?;
    let suite = Suite::at(dir.join("suite")).map_err(|e| format!("suite: {e}"))?;
    let identity = ServiceIdentity::new("rke2e-probe", "engine", 1, "1.0")
        .map_err(|e| format!("identity: {e}"))?;
    let registration = Registration::new(identity)
        .method(
            MethodDescriptor::local("echo", ["rke2e"]),
            |cx, params, _emit| Ok(json!({ "app": cx.app(), "params": params })),
        )
        .map_err(|e| format!("method: {e}"))?;
    let service = Service::builder()
        .register(&suite, registration, HostOptions::new())
        .map_err(|e| format!("register: {e}"))?
        .start()
        .map_err(|e| format!("start: {e}"))?;
    let client =
        Client::connect(&suite, "rke2e-probe", "rke2e").map_err(|e| format!("connect: {e}"))?;
    let echoed = client
        .call("echo", json!({ "n": 7 }))
        .map_err(|e| format!("call: {e}"))?;
    if echoed["app"] != json!("rke2e") || echoed["params"] != json!({ "n": 7 }) {
        return Err(format!("echo answered {echoed}"));
    }
    // An app the registration does not name is refused: at connect (the hello) or, failing that, at its first call.
    let refused = match Client::connect(&suite, "rke2e-probe", "stranger") {
        Err(e) => e.code == ERROR_NOT_ALLOWED,
        Ok(stranger) => stranger
            .call("echo", json!({}))
            .err()
            .map(|e| e.code == ERROR_NOT_ALLOWED)
            .unwrap_or(false),
    };
    if !refused {
        return Err("an app the registration does not name was served".into());
    }
    drop(service);
    Ok(json!({ "echo": echoed, "unnamed_app_refused": refused }))
}
