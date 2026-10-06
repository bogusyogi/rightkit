//! Real-process supervisor behaviour: a hung engine cannot hold a call past its deadline, the
//! timed-out command is never replayed, and the next call restarts the engine.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use rightapp::engine_supervisor::EngineSupervisor;
use serde_json::json;

fn script(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let p = dir.join("engine.sh");
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn hung_engine_times_out_then_restarts_without_replay() {
    let dir = std::env::temp_dir().join(format!("rightapp-sup-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("first-run");
    let log = dir.join("requests.log");
    // First generation reads a request and hangs; later generations answer every request.
    let engine = script(
        &dir,
        &format!(
            r#"read line
echo "$line" >> '{log}'
if [ ! -e '{marker}' ]; then : > '{marker}'; sleep 60; fi
while true; do
  id=$(echo "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
  echo "{{\"id\":$id,\"result\":{{\"served\":true}}}}"
  read line || exit 0
  echo "$line" >> '{log}'
done"#,
            log = log.display(),
            marker = marker.display()
        ),
    );
    let mut sup = EngineSupervisor::with_path(engine, dir.clone()).unwrap();

    let started = Instant::now();
    let err = sup.call_with_deadline("slow", json!({"n": 1}), Duration::from_millis(600)).unwrap_err();
    assert!(err.contains("deadline"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "call held {:?}", started.elapsed());

    let ok = sup.call_with_deadline("echo", json!({"n": 2}), Duration::from_secs(10)).unwrap();
    assert_eq!(ok, json!({"served": true}));

    let seen = fs::read_to_string(&log).unwrap();
    assert_eq!(seen.matches("\"cmd\":\"slow\"").count(), 1, "timed-out command must not be replayed: {seen}");
    assert_eq!(seen.matches("\"cmd\":\"echo\"").count(), 1);
    drop(sup);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn restart_budget_is_bounded() {
    let dir = std::env::temp_dir().join(format!("rightapp-sup-budget-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    // An engine that dies immediately: every call fails, and the budget (5 per minute) ends the loop.
    let engine = script(&dir, "exit 1");
    let mut sup = EngineSupervisor::with_path(engine, dir.clone()).unwrap();
    let started = Instant::now();
    let mut last = String::new();
    for _ in 0..12 {
        last = sup.call_with_deadline("x", json!({}), Duration::from_secs(2)).unwrap_err();
        if last.contains("budget exhausted") {
            break;
        }
    }
    assert!(last.contains("budget exhausted"), "{last}");
    assert!(started.elapsed() < Duration::from_secs(30));
    let _ = fs::remove_dir_all(&dir);
}
