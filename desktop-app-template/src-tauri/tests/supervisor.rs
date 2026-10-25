//! Real-process supervision through rightkit-job-queue's EngineSupervisor, using the shell's engine
//! configuration. A hung engine cannot hold a call past its deadline, the timed-out command is never
//! replayed, the next call restarts the engine, and a crash loop ends at the restart budget.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use rightapp::engine_config;
use rightkit_job_queue::EngineSupervisor;
use serde_json::json;

fn script(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("engine.sh");
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

/// The rightkit-job-queue serve contract's `hello` frame, the first line of every generation.
const HELLO: &str = r#"echo '{"type":"hello","engine":"fake","version":"0","contract_version":1}'"#;

// rk-test: contract
#[test]
fn hung_engine_times_out_then_restarts_without_replay() {
    let dir = std::env::temp_dir().join(format!("rightapp-sup-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("first-run");
    let log = dir.join("requests.log");
    // The first generation reads one request and hangs (`exec` so the kill reaches the sleep itself);
    // later generations answer every request in the serve contract's response shape.
    let engine = script(
        &dir,
        &format!(
            r#"{HELLO}
if [ ! -e '{marker}' ]; then : > '{marker}'; read line; echo "$line" >> '{log}'; exec sleep 60; fi
while read line; do
  echo "$line" >> '{log}'
  id=$(echo "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
  echo "{{\"type\":\"response\",\"id\":$id,\"ok\":true,\"result\":{{\"served\":true}}}}"
done"#,
            marker = marker.display(),
            log = log.display()
        ),
    );
    let sup = EngineSupervisor::new(
        engine_config(engine)
            .unwrap()
            .with_rpc_timeout(Duration::from_millis(600)),
    );
    sup.start().unwrap();

    let started = Instant::now();
    let err = sup.call("slow", json!({"n": 1})).unwrap_err();
    assert_eq!(err.code, "timeout", "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "call held {:?}",
        started.elapsed()
    );

    let ok = sup.call("echo", json!({"n": 2})).unwrap();
    assert_eq!(ok, json!({"served": true}));

    let seen = fs::read_to_string(&log).unwrap();
    assert_eq!(
        seen.matches("\"method\":\"slow\"").count(),
        1,
        "timed-out command must not be replayed: {seen}"
    );
    assert_eq!(seen.matches("\"method\":\"echo\"").count(), 1, "{seen}");
    drop(sup);
    let _ = fs::remove_dir_all(&dir);
}

// rk-test: contract
#[test]
fn restart_budget_is_bounded() {
    let dir = std::env::temp_dir().join(format!("rightapp-sup-budget-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    // An engine that says hello, then dies on its first request: every call fails, and the restart
    // budget (5 per minute) ends the crash loop with a `down` error. Each granted restart costs two
    // calls (the first hits the dead pipe, the next triggers the restart), so the loop needs more than
    // 2 * 5 + 2 iterations to reach the refusal.
    let engine = script(&dir, &format!("{HELLO}\nread line\nexit 1"));
    let sup = EngineSupervisor::new(
        engine_config(engine)
            .unwrap()
            .with_hello_timeout(Duration::from_secs(5))
            .with_rpc_timeout(Duration::from_secs(5)),
    );
    sup.start().unwrap();
    let started = Instant::now();
    let mut last = String::new();
    for _ in 0..30 {
        last = sup.call("x", json!({})).unwrap_err().message;
        if last.contains("budget") {
            break;
        }
    }
    assert!(last.contains("budget"), "{last}");
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    let _ = fs::remove_dir_all(&dir);
}
