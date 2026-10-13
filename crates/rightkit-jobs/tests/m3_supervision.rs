//! M3 owner-gap coverage (engine supervision): restart policy wiring, snapshot reconciliation,
//! contract requirement, environment policy, deadlines, write stalls and shutdown bounds. Every case
//! launches the real scriptable `engine_fixture` child.

use rightkit_jobs::{
    CallOptions, EngineSupervisor, ErrorOrigin, EventFrame, InitialFailure, Lifecycle,
    ProtocolError, ShutdownOutcome, SupervisorConfig, Terminal, CONTRACT_VERSION,
    CONTRACT_VERSION_MISMATCH,
};
use rightkit_process::{OwnedCommand, RestartMode, RestartPolicy};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    // `--all-targets` also builds a libtest harness named `engine_fixture-<hash>`; the real
    // example is the exact name, so prefer it and only fall back to a hashed binary.
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

fn rig(plan: &str) -> (tempfile::TempDir, SupervisorConfig) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    let plan = plan.to_string();
    let cfg = SupervisorConfig::new("fixture", move || {
        let mut c = OwnedCommand::new(fixture());
        c.command_mut().env("FX_DIR", &state).env("FX_PLAN", &plan);
        Ok(c)
    })
    .with_hello_timeout(Duration::from_secs(10))
    .with_ready_wait(Duration::from_secs(3))
    .with_shutdown_grace(Duration::from_millis(200));
    (dir, cfg)
}

fn wait_until(s: &EngineSupervisor, state: Lifecycle) {
    let until = Instant::now() + Duration::from_secs(4);
    while s.lifecycle() != state && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        s.lifecycle(),
        state,
        "status: {:?}",
        s.status().last_exit_reason
    );
}

fn wait_generation(s: &EngineSupervisor, generation: u64) {
    let until = Instant::now() + Duration::from_secs(4);
    while s.status().generation < generation && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        s.status().generation >= generation,
        "status: {:?}",
        s.status()
    );
}

#[test]
fn consecutive_failures_without_ready_end_the_run_before_the_rolling_budget() {
    // Every boot is refused at the handshake (it never reports ready), so the consecutive cap
    // of 2 ends the run although the rolling budget (10) is far from spent.
    let (_d, cfg) = rig("reject_set");
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        10,
        Duration::from_secs(60),
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .unwrap()
    .with_consecutive_failures(2, Duration::ZERO)
    .unwrap();
    let s = EngineSupervisor::new(cfg.with_restart(policy));
    s.set_snapshot("settings", "set_settings", json!({}))
        .unwrap();
    assert!(s.start().is_err());
    wait_until(&s, Lifecycle::Down);
    let st = s.status();
    assert!(
        matches!(st.terminal, Some(Terminal::Failed { .. })),
        "{st:?}"
    );
    assert_eq!(st.generation, 3, "first boot plus two restarts: {st:?}");
    assert!(st.restarts_in_window < 10);
}

#[test]
fn a_crash_after_ready_does_not_count_as_a_consecutive_failure() {
    // Each generation reaches ready and then crashes. With a cap of 1 and no cooldown a
    // consecutive count that ignored ready would stop at generation 2; it must keep restarting.
    let (_d, cfg) = rig("crash:20");
    let policy = RestartPolicy::new(
        RestartMode::Automatic,
        20,
        Duration::from_secs(60),
        Duration::from_millis(5),
        Duration::from_millis(5),
    )
    .unwrap()
    .with_consecutive_failures(1, Duration::ZERO)
    .unwrap();
    let s = EngineSupervisor::new(cfg.with_restart(policy));
    s.start().unwrap();
    wait_generation(&s, 4);
    assert!(s.status().terminal.is_none(), "{:?}", s.status());
    s.shutdown();
}
