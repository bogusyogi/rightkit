//! `EngineSupervisor` reports abnormal engine exits through `CrashExitObserver`, and an
//! intentional shutdown reports nothing.

use rightkit_crash::CrashConfig;
use rightkit_jobs::{CrashExitObserver, EngineSupervisor, ExitObserver, SupervisorConfig};
use rightkit_process::{OwnedCommand, RestartMode, RestartPolicy};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn fixture() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let examples = exe.parent().unwrap().parent().unwrap().join("examples");
    let exact = if cfg!(windows) {
        "engine_fixture.exe"
    } else {
        "engine_fixture"
    };
    if examples.join(exact).is_file() {
        return examples.join(exact);
    }
    std::fs::read_dir(&examples)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let binary =
                cfg!(windows) && name.ends_with(".exe") || !cfg!(windows) && !name.contains('.');
            name.starts_with("engine_fixture-") && binary && entry.path().is_file()
        })
        .map(|entry| entry.path())
        .unwrap_or_else(|| examples.join(exact))
}

fn config(state: PathBuf, plan: &str) -> SupervisorConfig {
    let plan = plan.to_string();
    SupervisorConfig::new("fixture", move || {
        let mut c = OwnedCommand::new(fixture());
        c.command_mut().env("FX_DIR", &state).env("FX_PLAN", &plan);
        Ok(c)
    })
    .with_hello_timeout(Duration::from_secs(10))
    .with_ready_wait(Duration::from_secs(3))
    .with_shutdown_grace(Duration::from_millis(200))
    .with_restart(
        RestartPolicy::new(
            RestartMode::Automatic,
            2,
            Duration::from_secs(10),
            Duration::from_millis(10),
            Duration::from_millis(20),
        )
        .unwrap(),
    )
}

fn reports(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            if let Ok(bytes) = std::fs::read(&path) {
                if let Ok(value) = serde_json::from_slice(&bytes) {
                    out.push(value);
                }
            }
        }
    }
    out
}

#[test]
fn crash_is_recorded_as_a_helper_exit_with_restart_count() {
    let state = tempfile::tempdir().unwrap();
    let crash = tempfile::tempdir().unwrap();
    let observer = CrashExitObserver::new(CrashConfig::new("App", "1", crash.path()));
    let s = EngineSupervisor::new(
        config(state.path().to_path_buf(), "crash_on_call,ok")
            .with_exit_observer(Arc::new(observer)),
    );
    s.start().unwrap();
    assert!(s.call("work", json!({})).is_err());
    let until = Instant::now() + Duration::from_secs(8);
    let mut found = reports(crash.path());
    while found.is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
        found = reports(crash.path());
    }
    let report = found.first().expect("a crash report");
    assert_eq!(report["kind"], "sidecar_crash");
    assert_eq!(report["process_kind"], "helper");
    let message = report["message"].as_str().unwrap();
    assert!(message.contains("helper fixture exited"), "{message}");
    assert!(message.contains("restarting"), "{message}");
    assert!(message.contains("restarts so far: 0"), "{message}");
    s.shutdown();
}

struct Recorder(Mutex<Vec<(String, String, u32)>>);

impl ExitObserver for Recorder {
    fn engine_exited(&self, helper: &str, exit: &str, restart_count: u32) {
        self.0
            .lock()
            .unwrap()
            .push((helper.into(), exit.into(), restart_count));
    }
}

#[test]
fn intentional_shutdown_is_not_reported() {
    let state = tempfile::tempdir().unwrap();
    let recorder = Arc::new(Recorder(Mutex::new(Vec::new())));
    let s = EngineSupervisor::new(
        config(state.path().to_path_buf(), "ok").with_exit_observer(recorder.clone()),
    );
    s.start().unwrap();
    s.shutdown();
    std::thread::sleep(Duration::from_millis(200));
    assert!(recorder.0.lock().unwrap().is_empty());
}
