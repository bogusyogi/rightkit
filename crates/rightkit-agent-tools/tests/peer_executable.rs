//! `CallContextExt::peer_executable`: the handler learns which program is calling, from the
//! transport's own peer check. Host and client run in this process, so the caller's executable is
//! this test binary.

use rightkit_agent_tools::CallContextExt;
use rightkit_daemon::{
    host, Allowlist, CallContext, Client, Emit, Handler, MethodPolicy, MethodTable, ServiceConfig,
    ServiceError, Suite,
};
use serde_json::{json, Value};
use std::sync::Arc;

struct Who;

impl Handler for Who {
    fn call(
        &self,
        cx: &CallContext,
        _method: &str,
        _params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        Ok(json!({ "exe": cx.peer_executable() }))
    }
}

#[test]
fn the_handler_sees_the_verified_executable_of_the_caller() {
    let table = MethodTable::new().method("who", MethodPolicy::local(["*"]));
    let dir = tempfile::tempdir().unwrap();
    let suite = Suite::at(dir.path().join("suite")).unwrap();
    let cfg = ServiceConfig::new("who", "test", 5, "test", Allowlist::new()).with_methods(&table);
    let _host = host(&suite, cfg, Arc::new(Who)).expect("host starts");
    let client = Client::connect(&suite, "who", "app").unwrap();

    let answer = client.call("who", json!({})).unwrap();
    let reported = std::path::PathBuf::from(answer["exe"].as_str().expect("an executable path"));
    let this = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    let same = |a: &std::path::Path, b: &std::path::Path| {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
            || std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
    };
    assert!(
        same(&reported, &this),
        "reported {reported:?}, expected {this:?}"
    );
}
