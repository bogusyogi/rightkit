//! Per-generation handshake (`SupervisorConfig::with_handshake`): order against snapshots and
//! `Ready`, a fresh provider call per generation, the Required / BestEffort failure rules, the
//! refusal of malformed params before any write, shutdown during the handshake, and the raw-params
//! client path. Supervisor cases launch the real scriptable `engine_fixture` child with
//! `FX_LOG_METHODS` set, which records the methods it receives (never their params) in
//! `FX_DIR/methods`.
//!
//! No test can inspect the supervisor's memory. That no handshake data is retained after `Ready`
//! is structural: `State` and `Shared` have no field of the handshake's type or params; the
//! provider's return value lives on the boot function's stack and each call is dropped
//! (zeroized) right after its send.

use rightkit_child_process::{OwnedCommand, RestartMode, RestartPolicy};
use rightkit_job_queue::{
    ClientOptions, EngineClient, EngineSupervisor, ErrorOrigin, HandshakeCall, HandshakeMode,
    InitialFailure, JobError, Lifecycle, LifecycleEvent, SupervisorConfig, Terminal, Zeroizing,
};
use serde_json::{json, Value};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
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

/// A config over the fixture that logs received methods and fails a bad first boot at once.
fn rig(plan: &str) -> (tempfile::TempDir, SupervisorConfig) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    let plan = plan.to_string();
    let cfg = SupervisorConfig::new("fixture", move || {
        let mut c = OwnedCommand::new(fixture());
        c.command_mut()
            .env("FX_DIR", &state)
            .env("FX_PLAN", &plan)
            .env("FX_LOG_METHODS", "1");
        Ok(c)
    })
    .with_hello_timeout(Duration::from_secs(10))
    .with_ready_wait(Duration::from_secs(3))
    .with_shutdown_grace(Duration::from_millis(200))
    .with_initial_failure(InitialFailure::Terminal);
    (dir, cfg)
}

/// `"<generation> <method>"` for every request the fixture received, in order.
fn methods(dir: &tempfile::TempDir) -> Vec<String> {
    std::fs::read_to_string(dir.path().join("methods"))
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

fn params(json: &str) -> Zeroizing<Vec<u8>> {
    Zeroizing::new(json.as_bytes().to_vec())
}

/// Failure diagnostics only: one line each time the generation, attempt or lifecycle changes,
/// sampled about every 100 ms, so a failed wait shows whether the supervisor was still making
/// progress (slow boots) or had stopped (a stuck state).
fn note_progress(s: &EngineSupervisor, began: Instant, last: &mut String, trace: &mut Vec<String>) {
    let st = s.status();
    let key = format!(
        "gen {} attempt {} {:?} restarts {}",
        st.generation, st.attempt, st.lifecycle, st.restarts
    );
    if key != *last {
        trace.push(format!("{}ms {key}", began.elapsed().as_millis()));
        *last = key;
    }
}

fn wait_until(s: &EngineSupervisor, state: Lifecycle) {
    let began = Instant::now();
    let until = began + Duration::from_secs(4);
    let (mut trace, mut last, mut next_sample) = (Vec::new(), String::new(), began);
    while s.lifecycle() != state && Instant::now() < until {
        if Instant::now() >= next_sample {
            note_progress(s, began, &mut last, &mut trace);
            next_sample = Instant::now() + Duration::from_millis(100);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        s.lifecycle(),
        state,
        "waited {:?} of a 4 s ceiling for {state:?}; progression: {trace:?}; status: {:?}",
        began.elapsed(),
        s.status()
    );
}

fn wait_generation(s: &EngineSupervisor, generation: u64) {
    let began = Instant::now();
    let until = began + Duration::from_secs(4);
    let (mut trace, mut last, mut next_sample) = (Vec::new(), String::new(), began);
    while s.status().generation < generation && Instant::now() < until {
        if Instant::now() >= next_sample {
            note_progress(s, began, &mut last, &mut trace);
            next_sample = Instant::now() + Duration::from_millis(100);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        s.status().generation >= generation,
        "waited {:?} of a 4 s ceiling for generation {generation}; progression: {trace:?}; status: {:?}",
        began.elapsed(),
        s.status()
    );
}

/// Every text a lifecycle event carries.
fn event_text(e: &LifecycleEvent) -> String {
    format!(
        "{} | {} | {}",
        e.message,
        e.last_exit_reason.as_deref().unwrap_or_default(),
        e.stderr_tail.as_deref().unwrap_or_default()
    )
}

#[test]
fn snapshots_then_handshake_then_ready_for_the_same_generation() {
    let (dir, cfg) = rig("ok");
    let seen: Arc<Mutex<Vec<(u64, Lifecycle)>>> = Arc::default();
    let slot: Arc<OnceLock<Weak<EngineSupervisor>>> = Arc::default();
    let (seen_in, slot_in) = (Arc::clone(&seen), Arc::clone(&slot));
    let cfg = cfg.with_handshake(move |generation| {
        // The provider runs while the boot is still in flight.
        let state = slot_in
            .get()
            .and_then(Weak::upgrade)
            .map(|s| s.lifecycle())
            .unwrap();
        seen_in.lock().unwrap().push((generation, state));
        Ok(vec![
            HandshakeCall::new(
                "set_key_a",
                params(r#"{"key":"SECRET-A"}"#),
                HandshakeMode::Required,
            ),
            HandshakeCall::new(
                "set_key_b",
                params(r#"{"key":"SECRET-B"}"#),
                HandshakeMode::BestEffort,
            ),
        ])
    });
    let s = Arc::new(EngineSupervisor::new(cfg));
    slot.set(Arc::downgrade(&s)).unwrap();
    let events = s.subscribe_lifecycle();
    s.set_snapshot("settings", "set_settings", json!({"theme": "dark"}))
        .unwrap();
    s.start().unwrap();

    assert_eq!(
        methods(&dir),
        ["1 set_settings", "1 set_key_a", "1 set_key_b"]
    );
    assert_eq!(*seen.lock().unwrap(), vec![(1u64, Lifecycle::Booting)]);
    let state = s.call("get_state", json!({})).unwrap();
    assert_eq!(state["set_key_a"]["key"], "SECRET-A");
    assert_eq!(state["set_key_b"]["key"], "SECRET-B");
    assert_eq!(state["set_settings"]["theme"], "dark");

    let events: Vec<LifecycleEvent> = events.try_iter().collect();
    assert!(events.iter().any(|e| e.state == Lifecycle::Ready));
    assert!(events.iter().all(|e| !event_text(e).contains("SECRET")));
    s.shutdown();
}

#[test]
fn a_restart_asks_the_provider_again_with_the_new_generation() {
    let (dir, cfg) = rig("crash_on_call,ok");
    let generations: Arc<Mutex<Vec<u64>>> = Arc::default();
    let generations_in = Arc::clone(&generations);
    let cfg = cfg
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
        .with_handshake(move |generation| {
            generations_in.lock().unwrap().push(generation);
            Ok(vec![HandshakeCall::new(
                "set_key",
                Zeroizing::new(format!(r#"{{"key":"K{generation}"}}"#).into_bytes()),
                HandshakeMode::Required,
            )])
        });
    let s = EngineSupervisor::new(cfg);
    s.start().unwrap();
    assert_eq!(
        s.call("get_state", json!({})).unwrap()["set_key"]["key"],
        "K1"
    );
    assert!(s.call("work", json!({})).is_err());
    wait_generation(&s, 2);
    wait_until(&s, Lifecycle::Ready);
    assert_eq!(
        s.call("get_state", json!({})).unwrap()["set_key"]["key"],
        "K2",
        "generation 2 must receive the value read for generation 2"
    );
    assert_eq!(*generations.lock().unwrap(), vec![1, 2]);
    assert!(methods(&dir).contains(&"2 set_key".to_string()));
    s.shutdown();
}

#[test]
fn best_effort_tolerates_an_engine_error_reply_and_logs_only_a_fixed_code() {
    // `reject_set` answers every `set_*` with an error reply whose message is below.
    let (dir, cfg) = rig("reject_set");
    let cfg = cfg.with_handshake(|_| {
        Ok(vec![HandshakeCall::new(
            "set_key",
            params(r#"{"key":"SECRET-X"}"#),
            HandshakeMode::BestEffort,
        )])
    });
    let s = EngineSupervisor::new(cfg);
    let events = s.subscribe_lifecycle();
    s.start().unwrap();
    assert_eq!(s.lifecycle(), Lifecycle::Ready);
    assert_eq!(methods(&dir), ["1 set_key"]);

    let events: Vec<LifecycleEvent> = events.try_iter().collect();
    assert!(events
        .iter()
        .any(|e| e.message == "handshake_best_effort_error: set_key"));
    for e in &events {
        let text = event_text(e);
        assert!(!text.contains("SECRET-X"), "params leaked: {text}");
        assert!(
            !text.contains("no settings accepted"),
            "engine message leaked: {text}"
        );
    }
    s.shutdown();
}

#[test]
fn required_engine_error_reply_fails_the_boot() {
    let (dir, cfg) = rig("reject_set");
    let cfg = cfg.with_handshake(|_| {
        Ok(vec![HandshakeCall::new(
            "set_key",
            params(r#"{"key":"SECRET-X"}"#),
            HandshakeMode::Required,
        )])
    });
    let s = EngineSupervisor::new(cfg);
    let events = s.subscribe_lifecycle();
    let e = s.start().unwrap_err();
    assert_eq!(e.code, "handshake_failed");
    assert!(e.message.contains("set_key"), "{e}");
    assert!(!e.message.contains("SECRET-X"), "{e}");
    assert!(!e.message.contains("no settings accepted"), "{e}");
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    assert!(matches!(s.status().terminal, Some(Terminal::Failed { .. })));
    assert_eq!(methods(&dir), ["1 set_key"]);
    assert!(events.try_iter().all(|e| e.state != Lifecycle::Ready));
}

#[test]
fn best_effort_never_tolerates_a_dead_transport() {
    // `crash_on_call` exits on method `work`: the engine dies mid-handshake.
    let (dir, cfg) = rig("crash_on_call");
    let cfg = cfg.with_handshake(|_| {
        Ok(vec![HandshakeCall::new(
            "work",
            params(r#"{"key":"SECRET-X"}"#),
            HandshakeMode::BestEffort,
        )])
    });
    let s = EngineSupervisor::new(cfg);
    let events = s.subscribe_lifecycle();
    let e = s.start().unwrap_err();
    assert_eq!(e.code, "handshake_failed");
    assert!(!e.message.contains("SECRET-X"), "{e}");
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    assert!(
        events.try_iter().all(|e| e.state != Lifecycle::Ready),
        "a dead engine must never be reported Ready"
    );
    assert_eq!(methods(&dir), ["1 work"]);
}

#[test]
fn a_failing_provider_fails_the_boot_before_anything_is_sent() {
    let (dir, cfg) = rig("ok");
    let cfg = cfg.with_handshake(|_| {
        Err(JobError::new(
            "no_key",
            "key store locked, hint SECRET-HINT",
        ))
    });
    let s = EngineSupervisor::new(cfg);
    let e = s.start().unwrap_err();
    assert_eq!(e.code, "handshake_failed");
    assert!(e.message.contains("no_key"), "{e}");
    assert!(!e.message.contains("SECRET-HINT"), "{e}");
    assert!(!s.status().last_exit_reason.unwrap().contains("SECRET-HINT"));
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    assert!(methods(&dir).is_empty());
}

#[test]
fn a_panicking_provider_fails_the_boot_instead_of_hanging_it() {
    let (dir, cfg) = rig("ok");
    let cfg = cfg.with_handshake(|_| panic!("provider bug"));
    let s = EngineSupervisor::new(cfg);
    let e = s.start().unwrap_err();
    assert_eq!(e.code, "handshake_failed");
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    assert!(methods(&dir).is_empty());
}

#[test]
fn malformed_params_are_refused_before_any_write_in_either_mode() {
    let cases: [(&str, &[u8]); 7] = [
        ("newline", b"{\"key\":\n\"SECRET-X\"}"),
        ("carriage return", b"{\"key\":\r\"SECRET-X\"}"),
        ("not json", b"SECRET-X"),
        ("two values", b"{} {}"),
        ("trailing garbage", b"{\"key\":\"SECRET-X\"}x"),
        ("truncated", b"{\"key\":\"SECRET-X\""),
        ("empty", b""),
    ];
    for mode in [HandshakeMode::Required, HandshakeMode::BestEffort] {
        for (label, bytes) in cases {
            let (dir, cfg) = rig("ok");
            let bytes = bytes.to_vec();
            let cfg = cfg.with_handshake(move |_| {
                Ok(vec![HandshakeCall::new(
                    "set_key",
                    Zeroizing::new(bytes.clone()),
                    mode,
                )])
            });
            let s = EngineSupervisor::new(cfg);
            let e = s.start().unwrap_err();
            assert_eq!(e.code, "handshake_failed", "{label} / {mode:?}");
            assert!(e.message.contains("bad_input"), "{label} / {mode:?}: {e}");
            assert!(!e.message.contains("SECRET-X"), "{label} / {mode:?}: {e}");
            assert_eq!(s.lifecycle(), Lifecycle::Down, "{label} / {mode:?}");
            assert!(
                methods(&dir).is_empty(),
                "{label} / {mode:?}: the engine must not have received the call"
            );
        }
    }
}

#[test]
fn shutdown_during_the_handshake_sends_nothing_further() {
    let (dir, cfg) = rig("ok");
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let (entered_in, release_in) = (Arc::clone(&entered), Arc::clone(&release));
    let cfg = cfg.with_handshake(move |_| {
        entered_in.store(true, Ordering::SeqCst);
        let until = Instant::now() + Duration::from_secs(5);
        while !release_in.load(Ordering::SeqCst) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(vec![HandshakeCall::new(
            "set_key",
            params(r#"{"key":"SECRET-X"}"#),
            HandshakeMode::Required,
        )])
    });
    let s = EngineSupervisor::new(cfg);
    let ready = s.start_background();
    let until = Instant::now() + Duration::from_secs(5);
    while !entered.load(Ordering::SeqCst) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(entered.load(Ordering::SeqCst), "the provider never ran");
    s.shutdown();
    release.store(true, Ordering::SeqCst);
    let outcome = ready
        .wait_timeout(Duration::from_secs(5))
        .expect("the boot must end");
    assert!(outcome.is_err(), "a shut down boot is not Ready");
    assert_eq!(s.lifecycle(), Lifecycle::Down);
    // The supervisor's own graceful `shutdown` call is expected; no handshake call may follow it.
    let sent = methods(&dir);
    assert!(
        sent.iter().all(|m| !m.ends_with(" set_key")),
        "no handshake call may be sent after shutdown: {sent:?}"
    );
}

// ---- the raw-params client path ------------------------------------------------------------

/// Collects every byte the client writes.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn client(sink: &Sink, frame_limit: usize) -> EngineClient {
    let hello = b"{\"type\":\"hello\",\"engine\":\"t\",\"version\":\"1\",\"contract_version\":1}\n"
        .to_vec();
    EngineClient::connect_opts(
        Cursor::new(hello),
        sink.clone(),
        ClientOptions {
            frame_limit,
            ..ClientOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn raw_params_are_spliced_into_the_frame_byte_for_byte() {
    let sink = Sink::default();
    let c = client(&sink, 1 << 20);
    // The engine never answers (the reader hit EOF): the call ends in an error, after the write.
    let r = c.call_timeout_raw("m", br#"{"a":1}"#, Duration::from_millis(200));
    assert!(r.is_err());
    assert_eq!(
        sink.0.lock().unwrap().as_slice(),
        b"{\"type\":\"request\",\"id\":1,\"method\":\"m\",\"params\":{\"a\":1}}\n"
    );
}

#[test]
fn raw_params_keep_their_own_spelling_and_the_method_is_escaped() {
    let sink = Sink::default();
    let c = client(&sink, 1 << 20);
    let raw = r#"  { "k" : ["v", 1, null, "é \" \\"] }  "#.as_bytes();
    let _ = c.call_timeout_raw("a\"b", raw, Duration::from_millis(200));
    let written = sink.0.lock().unwrap().clone();
    assert_eq!(written.iter().filter(|b| **b == b'\n').count(), 1);
    assert_eq!(written.last(), Some(&b'\n'));
    let frame: Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(frame["type"], "request");
    assert_eq!(frame["method"], "a\"b");
    assert_eq!(
        frame["params"],
        json!({"k": ["v", 1, null, "\u{e9} \" \\"]})
    );
    // Spliced as written, not re-serialized.
    assert!(written.windows(raw.len()).any(|w| w == raw));
}

#[test]
fn malformed_raw_params_are_refused_locally_and_nothing_is_written() {
    let bad: [&[u8]; 9] = [
        b"",
        b"{\n}",
        b"{\r}",
        b"{\"secret\": nope}",
        b"{} {}",
        b"{\"a\":",
        b"\xff",
        b"[1,2",
        b"\"unterminated",
    ];
    for raw in bad {
        let sink = Sink::default();
        let c = client(&sink, 1 << 20);
        let e = c
            .call_timeout_raw("m", raw, Duration::from_millis(200))
            .unwrap_err();
        assert_eq!(e.code, "bad_input", "{raw:?}");
        assert_eq!(e.origin, ErrorOrigin::Supervisor, "{raw:?}");
        assert!(!e.message.contains("secret"), "{raw:?}: {e}");
        assert!(
            sink.0.lock().unwrap().is_empty(),
            "{raw:?}: bytes reached the writer"
        );
    }
}

#[test]
fn raw_params_over_the_frame_limit_are_refused_locally() {
    let sink = Sink::default();
    let c = client(&sink, 64);
    let big = format!(r#"{{"k":"{}"}}"#, "x".repeat(200));
    let e = c
        .call_timeout_raw("m", big.as_bytes(), Duration::from_millis(200))
        .unwrap_err();
    assert_eq!(e.code, "bad_input");
    assert!(e.message.contains("frame limit"), "{e}");
    assert!(sink.0.lock().unwrap().is_empty());
}
