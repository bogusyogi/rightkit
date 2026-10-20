use rightkit_job_queue::{
    ActivityOptions, ActivityRegistry, EngineSupervisor, HandshakeCall, HandshakeMode,
    InitialFailure, Lifecycle, OwnedCommand, SupervisorConfig, Zeroizing,
};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::atomic::Ordering;
use std::time::Duration;

// Same executable supplies its child engine; no workspace example or external interpreter.
fn engine() {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{}", json!({
        "type": "hello", "engine": "pk06-consumer", "version": "1",
        "contract_version": 1
    })).unwrap();
    out.flush().unwrap();
    let mut initialized = false;
    let mut calls = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let method = request["method"].as_str().unwrap();
        calls.push(method.to_string());
        let result = match method {
            "initialize_session" => {
                assert_eq!(request["params"]["sample"], "public-fixture");
                initialized = true;
                json!({})
            }
            "state" => json!({"initialized": initialized, "calls": calls}),
            "shutdown" => json!({}),
            other => panic!("unexpected method: {other}"),
        };
        writeln!(out, "{}", json!({
            "type": "response", "id": request["id"], "ok": true, "result": result
        })).unwrap();
        out.flush().unwrap();
        if method == "shutdown" {
            return;
        }
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--engine") {
        engine();
        return;
    }
    let registry = ActivityRegistry::new();
    let (export, flag) = registry.register_with(
        "export", ActivityOptions::new().label("Export").progress(0.25)
    ).unwrap();
    let (index, _) = registry.register_with(
        "index", ActivityOptions::new().label("Index")
    ).unwrap();
    let listed = registry.list();
    assert_eq!(listed.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
        vec!["export", "index"]);
    assert!(listed[0].seq < listed[1].seq);
    export.report(Some(0.75), Some("encoding"));
    assert_eq!(registry.get("export").unwrap().progress, Some(0.75));
    assert!(registry.cancel("export"));
    assert!(flag.load(Ordering::SeqCst));
    assert!(registry.get("export").unwrap().cancelled);
    drop(export);
    drop(index);
    assert!(registry.is_empty());

    let executable = std::env::current_exe().unwrap();
    let config = SupervisorConfig::new("pk06-consumer", move || {
        let mut command = OwnedCommand::new(&executable);
        command.command_mut().arg("--engine");
        Ok(command)
    })
    .with_initial_failure(InitialFailure::Terminal)
    .with_hello_timeout(Duration::from_secs(10))
    .with_rpc_timeout(Duration::from_secs(5))
    .with_shutdown_grace(Duration::from_secs(2))
    .with_handshake(|generation| {
        assert_eq!(generation, 1);
        Ok(vec![HandshakeCall::new(
            "initialize_session",
            Zeroizing::new(br#"{"sample":"public-fixture"}"#.to_vec()),
            HandshakeMode::Required,
        )])
    });
    let supervisor = EngineSupervisor::new(config);
    supervisor.start().unwrap();
    assert_eq!(supervisor.status().lifecycle, Lifecycle::Ready);
    let state = supervisor.call("state", json!({})).unwrap();
    supervisor.shutdown();
    assert_eq!(state["initialized"], true);
    assert_eq!(state["calls"], json!(["initialize_session", "state"]));
    println!("PK06 consumer: handshake before Ready; activity order/progress/cancellation/cleanup verified");
}
