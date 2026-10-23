//! `SupervisorConfig::with_launch_frame`: the frame is the child's first stdin line (before any
//! snapshot, handshake or request), the provider runs once per generation, a bad provider or
//! frame fails the boot through the restart budget before any spawn, the payload never reaches an
//! error, exit reason or lifecycle event, and a supervisor without a frame writes what it always
//! did. Cases launch the real scriptable `engine_fixture` child with `FX_LAUNCH` set, which
//! reads one line before hello and records it in `FX_DIR/launch`.

use rightkit_child_process::{OwnedCommand, RestartMode, RestartPolicy};
use rightkit_job_queue::{
    EngineSupervisor, HandshakeCall, HandshakeMode, InitialFailure, JobError, LaunchFrame,
    Lifecycle, LifecycleEvent, SupervisorConfig, Terminal, Zeroizing, LAUNCH_FRAME_FAILED,
    MAX_LAUNCH_FRAME_BYTES,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
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

fn rig(plan: &str) -> (tempfile::TempDir, SupervisorConfig) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    let plan = plan.to_string();
    let cfg = SupervisorConfig::new("fixture", move || {
        let mut c = OwnedCommand::new(fixture());
        c.command_mut()
            .env("FX_DIR", &state)
            .env("FX_PLAN", &plan)
            .env("FX_LOG_METHODS", "1")
            .env("FX_LAUNCH", "1")
            .env("FX_RECORD_STDIN", "1");
        Ok(c)
    })
    .with_hello_timeout(Duration::from_secs(10))
    .with_ready_wait(Duration::from_secs(3))
    .with_shutdown_grace(Duration::from_millis(200))
    .with_initial_failure(InitialFailure::Terminal);
    (dir, cfg)
}

fn lines(dir: &tempfile::TempDir, name: &str) -> Vec<String> {
    std::fs::read_to_string(dir.path().join(name))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

fn restart_policy(max_restarts: u32) -> RestartPolicy {
    RestartPolicy::new(
        RestartMode::Automatic,
        max_restarts,
        Duration::from_secs(10),
        Duration::from_millis(1),
        Duration::from_millis(1),
    )
    .unwrap()
}

fn wait_until(s: &EngineSupervisor, state: Lifecycle) {
    let until = Instant::now() + Duration::from_secs(8);
    while s.lifecycle() != state && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(s.lifecycle(), state, "status: {:?}", s.status());
}

fn event_text(e: &LifecycleEvent) -> String {
    format!(
        "{} | {} | {}",
        e.message,
        e.last_exit_reason.as_deref().unwrap_or_default(),
        e.stderr_tail.as_deref().unwrap_or_default()
    )
}

fn frame_for(generation: u64) -> Result<LaunchFrame, JobError> {
    LaunchFrame::new(
        "hr.engine",
        1,
        &json!({"backend": "SECRET-BACKEND", "generation": generation}),
    )
}

/// Everything a boot failure shows a caller: the error, the status and every lifecycle event.
fn visible_text(
    s: &EngineSupervisor,
    err: &JobError,
    events: &std::sync::mpsc::Receiver<LifecycleEvent>,
) -> String {
    let mut all = format!("{err} | {err:?} | {:?}", s.status());
    for e in events.try_iter() {
        all.push_str(&event_text(&e));
        all.push_str(&format!(" {e:?}"));
    }
    all
}

#[test]
fn the_frame_is_the_first_stdin_line_before_snapshots_and_handshake() {
    let (dir, cfg) = rig("ok");
    let cfg = cfg.with_launch_frame(frame_for).with_handshake(|_| {
        Ok(vec![HandshakeCall::new(
            "set_key",
            Zeroizing::new(br#"{"key":"K"}"#.to_vec()),
            HandshakeMode::Required,
        )])
    });
    let s = EngineSupervisor::new(cfg);
    s.set_snapshot("settings", "set_settings", json!({"theme": "dark"}))
        .unwrap();
    s.start().unwrap();
    assert_eq!(s.lifecycle(), Lifecycle::Ready);

    let launch = lines(&dir, "launch");
    assert_eq!(launch.len(), 1, "{launch:?}");
    let (generation, frame) = launch[0].split_once(' ').unwrap();
    assert_eq!(generation, "1");
    let frame: Value = serde_json::from_str(frame).unwrap();
    assert_eq!(
        frame,
        json!({"type":"launch","schema":"hr.engine","version":1,
               "params":{"backend":"SECRET-BACKEND","generation":1}})
    );
    // Everything after the launch line is the snapshot, then the handshake call.
    let after = lines(&dir, "stdin_lines");
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(after[0].contains("set_settings") && after[1].contains("set_key"));
    assert_eq!(
        lines(&dir, "methods"),
        ["1 set_settings", "1 set_key"],
        "requests ran after the launch frame"
    );
    s.shutdown();
}

#[test]
fn a_restart_calls_the_provider_again_and_sends_the_new_frame() {
    let (dir, cfg) = rig("crash_on_call,ok");
    let calls: Arc<Mutex<Vec<u64>>> = Arc::default();
    let calls_in = Arc::clone(&calls);
    let cfg = cfg
        .with_restart(restart_policy(2))
        .with_launch_frame(move |g| {
            calls_in.lock().unwrap().push(g);
            frame_for(g)
        });
    let s = EngineSupervisor::new(cfg);
    s.start().unwrap();
    assert!(s.call("work", json!({})).is_err());
    let until = Instant::now() + Duration::from_secs(8);
    while s.status().generation < 2 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    wait_until(&s, Lifecycle::Ready);
    assert_eq!(*calls.lock().unwrap(), vec![1, 2]);
    let launch = lines(&dir, "launch");
    assert_eq!(launch.len(), 2, "{launch:?}");
    assert!(launch[0].starts_with("1 ") && launch[0].contains(r#""generation":1"#));
    assert!(launch[1].starts_with("2 ") && launch[1].contains(r#""generation":2"#));
    s.shutdown();
}

fn assert_refused_before_spawn(cfg: SupervisorConfig, dir: &tempfile::TempDir, secret: &str) {
    let s = EngineSupervisor::new(cfg);
    let events = s.subscribe_lifecycle();
    let e = s.start().unwrap_err();
    assert_eq!(e.code, LAUNCH_FRAME_FAILED, "{e}");
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    assert!(
        !dir.path().join("gen").exists(),
        "no engine process may be started"
    );
    let text = visible_text(&s, &e, &events);
    assert!(!text.contains(secret), "payload leaked: {text}");
    assert!(
        text.contains(LAUNCH_FRAME_FAILED),
        "failure names its code: {text}"
    );
}

#[test]
fn a_provider_error_fails_the_boot_without_the_error_text() {
    let (dir, cfg) = rig("ok");
    let cfg =
        cfg.with_launch_frame(|_| Err(JobError::new("no_settings", "SECRET-HINT unreadable")));
    assert_refused_before_spawn(cfg, &dir, "SECRET-HINT");
}

#[test]
fn a_panicking_provider_fails_the_boot() {
    let (dir, cfg) = rig("ok");
    let cfg = cfg.with_launch_frame(|_| panic!("SECRET-PANIC"));
    assert_refused_before_spawn(cfg, &dir, "SECRET-PANIC");
}

#[test]
fn a_multi_line_frame_is_refused_before_spawn() {
    let (dir, cfg) = rig("ok");
    let cfg = cfg.with_launch_frame(|_| {
        Ok(LaunchFrame::from_json_line(Zeroizing::new(
            b"{\"type\":\"launch\",\n\"SECRET-LINE\":1}".to_vec(),
        )))
    });
    assert_refused_before_spawn(cfg, &dir, "SECRET-LINE");
    let (dir, cfg) = rig("ok");
    let cfg = cfg.with_launch_frame(|_| {
        Ok(LaunchFrame::from_json_line(Zeroizing::new(
            b"{\"SECRET-CR\":1}\r".to_vec(),
        )))
    });
    assert_refused_before_spawn(cfg, &dir, "SECRET-CR");
}

#[test]
fn an_oversize_frame_is_refused_before_spawn() {
    let big = format!(
        "{{\"SECRET-BIG\":\"{}\"}}",
        "x".repeat(MAX_LAUNCH_FRAME_BYTES)
    );
    let (dir, cfg) = rig("ok");
    let line = big.clone();
    let cfg = cfg.with_launch_frame(move |_| {
        Ok(LaunchFrame::from_json_line(Zeroizing::new(
            line.clone().into_bytes(),
        )))
    });
    assert_refused_before_spawn(cfg, &dir, "SECRET-BIG");
    // `LaunchFrame::new` refuses the same size itself, without echoing the params.
    let e = LaunchFrame::new(
        "hr.engine",
        1,
        &json!({"SECRET-BIG": "x".repeat(MAX_LAUNCH_FRAME_BYTES)}),
    )
    .unwrap_err();
    assert_eq!(e.code, "bad_input");
    assert!(!e.to_string().contains("SECRET-BIG"));
    // An empty line is refused too.
    let (dir, cfg) = rig("ok");
    let cfg =
        cfg.with_launch_frame(|_| Ok(LaunchFrame::from_json_line(Zeroizing::new(Vec::new()))));
    assert_refused_before_spawn(cfg, &dir, "SECRET-NONE");
}

#[test]
fn the_limit_is_exact() {
    // Build a frame of exactly MAX bytes, then one byte over.
    let overhead = LaunchFrame::new("s", 1, &json!({"p": ""})).unwrap().len();
    let fill = MAX_LAUNCH_FRAME_BYTES - overhead;
    let exact = LaunchFrame::new("s", 1, &json!({"p": "x".repeat(fill)})).unwrap();
    assert_eq!(exact.len(), MAX_LAUNCH_FRAME_BYTES);
    assert!(LaunchFrame::new("s", 1, &json!({"p": "x".repeat(fill + 1)})).is_err());
    let (dir, cfg) = rig("ok");
    let s = EngineSupervisor::new(
        cfg.with_launch_frame(move |_| LaunchFrame::new("s", 1, &json!({"p": "x".repeat(fill)}))),
    );
    s.start().unwrap();
    assert_eq!(lines(&dir, "launch")[0].len(), 2 + MAX_LAUNCH_FRAME_BYTES);
    s.shutdown();
}

#[test]
fn a_failing_frame_spends_restart_budget_on_every_boot() {
    let (dir, cfg) = rig("ok");
    let calls = Arc::new(AtomicU64::new(0));
    let calls_in = Arc::clone(&calls);
    let cfg = cfg
        .with_initial_failure(InitialFailure::Retry)
        .with_restart(restart_policy(2))
        .with_launch_frame(move |_| {
            calls_in.fetch_add(1, Ordering::SeqCst);
            Err(JobError::new("no_settings", "SECRET-HINT"))
        });
    let s = EngineSupervisor::new(cfg);
    let e = s.start().unwrap_err();
    assert_eq!(e.code, LAUNCH_FRAME_FAILED);
    wait_until(&s, Lifecycle::Down);
    let status = s.status();
    assert_eq!(status.restarts, 2, "{status:?}");
    assert!(matches!(
        status.terminal,
        Some(Terminal::Failed { ref reason }) if reason.contains("budget")
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "first boot plus two retries"
    );
    assert!(!dir.path().join("gen").exists());
    s.shutdown();
}

#[test]
fn a_frame_to_a_child_that_never_says_hello_ends_at_hello_timeout() {
    let (dir, cfg) = rig("no_hello");
    let cfg = cfg
        .with_hello_timeout(Duration::from_millis(400))
        .with_launch_frame(frame_for);
    let s = EngineSupervisor::new(cfg);
    let began = Instant::now();
    let e = s.start().unwrap_err();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    assert_eq!(e.code, "hello_timeout", "{e}");
    assert_eq!(
        lines(&dir, "launch").len(),
        1,
        "the frame was still sent first"
    );
}

#[test]
fn without_a_frame_the_first_stdin_line_is_the_first_request() {
    let (dir, _) = rig("ok");
    // The fixture would swallow one line as its launch frame; leave that off here.
    let state = dir.path().to_path_buf();
    let cfg = SupervisorConfig::new("fixture", move || {
        let mut c = OwnedCommand::new(fixture());
        c.command_mut()
            .env("FX_DIR", &state)
            .env("FX_PLAN", "ok")
            .env("FX_LOG_METHODS", "1")
            .env("FX_RECORD_STDIN", "1");
        Ok(c)
    })
    .with_hello_timeout(Duration::from_secs(10))
    .with_ready_wait(Duration::from_secs(3))
    .with_shutdown_grace(Duration::from_millis(200))
    .with_initial_failure(InitialFailure::Terminal);
    let s = EngineSupervisor::new(cfg);
    s.set_snapshot("settings", "set_settings", json!({"theme": "dark"}))
        .unwrap();
    s.start().unwrap();
    assert_eq!(s.call("ping", json!({})).unwrap()["pong"], true);
    let written = lines(&dir, "stdin_lines");
    assert_eq!(written.len(), 2, "{written:?}");
    let first: Value = serde_json::from_str(&written[0]).unwrap();
    assert_eq!(first["type"], "request");
    assert_eq!(first["method"], "set_settings");
    assert!(written.iter().all(|l| !l.contains("\"launch\"")));
    assert!(!dir.path().join("launch").exists());
    s.shutdown();
}

#[test]
fn the_payload_is_not_in_events_or_the_stderr_tail_after_a_boot_and_a_crash() {
    let (_dir, cfg) = rig("crash:30");
    let cfg = cfg.with_launch_frame(frame_for);
    let s = EngineSupervisor::new(cfg);
    let events = s.subscribe_lifecycle();
    s.start().unwrap();
    wait_until(&s, Lifecycle::Down);
    let mut all = format!("{:?}", s.status());
    for e in events.try_iter() {
        all.push_str(&event_text(&e));
        all.push_str(&format!(" {e:?}"));
    }
    assert!(
        all.contains("PRIVATE1"),
        "the fixture's own stderr is captured: {all}"
    );
    assert!(!all.contains("SECRET-BACKEND"), "payload leaked: {all}");
    s.shutdown();
}
