//! End-to-end proof of the supervision primitives merged from the donors:
//! environment scrub (HeardRight), bounded reap (HeardRight), stderr tail with
//! redaction (ScrapeRight + HeardRight) and the stall watchdog (HeardRight
//! hang/UI-stall). Real child processes and real threads; no mocks.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_process::{Ack, Cooldown, StallConfig, StallEvent, StallWatchdog};

#[cfg(unix)]
mod child {
    use super::*;
    use rightkit_process::{OwnedCommand, ReapOutcome, StderrTail, DEFAULT_ENV_ALLOWLIST};
    use std::io::Read;
    use std::process::Stdio;

    fn sh(script: &str) -> OwnedCommand {
        let mut c = OwnedCommand::new("/bin/sh");
        c.command_mut().args(["-c", script]).stdin(Stdio::null());
        c
    }

    fn run_env(mut c: OwnedCommand) -> String {
        c.command_mut().stdout(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let mut out = String::new();
        child
            .take_stdout()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        child.wait().unwrap();
        out
    }

    #[test]
    fn env_allowlist_and_strip_reach_the_real_child() {
        std::env::set_var("RK_TEST_KEEP", "kept-value");
        std::env::set_var("RK_TEST_HIDDEN", "hidden-value");
        std::env::set_var("RK_TEST_OTHER", "other-value");

        // Allowlist: only named variables survive; the rest are cleared.
        let mut c = OwnedCommand::new("/usr/bin/env");
        c.env_allowlist(
            DEFAULT_ENV_ALLOWLIST
                .iter()
                .copied()
                .chain(["RK_TEST_KEEP"]),
        );
        let out = run_env(c);
        assert!(out.contains("RK_TEST_KEEP=kept-value"), "{out}");
        assert!(
            !out.contains("RK_TEST_HIDDEN") && !out.contains("RK_TEST_OTHER"),
            "{out}"
        );
        assert!(
            out.contains("PATH="),
            "PATH is in the default allowlist: {out}"
        );

        // Explicit env set after the scrub wins and is not filtered.
        let mut c = OwnedCommand::new("/usr/bin/env");
        c.env_allowlist(["PATH"]);
        c.command_mut().env("RK_TEST_ADDED", "added");
        assert!(run_env(c).contains("RK_TEST_ADDED=added"));

        // Strip: only the named variable disappears.
        let mut c = OwnedCommand::new("/usr/bin/env");
        c.env_strip(["RK_TEST_HIDDEN"]);
        let out = run_env(c);
        assert!(
            !out.contains("RK_TEST_HIDDEN") && out.contains("RK_TEST_OTHER=other-value"),
            "{out}"
        );
    }

    #[test]
    fn bounded_reap_kills_a_child_that_ignores_sigterm_within_the_bound() {
        let mut c = sh("trap '' TERM; sleep 60 & wait");
        let mut child = c.spawn().unwrap();
        std::thread::sleep(Duration::from_millis(150));
        let t = Instant::now();
        match child
            .terminate_tree_bounded(Duration::from_secs(5))
            .unwrap()
        {
            ReapOutcome::Exited(status) => assert!(!status.success()),
            ReapOutcome::TimedOut => panic!("a SIGKILLed group must be reaped"),
        }
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "took {:?}",
            t.elapsed()
        );
        // Idempotent on an already-reaped child.
        assert!(matches!(
            child
                .terminate_tree_bounded(Duration::from_millis(50))
                .unwrap(),
            ReapOutcome::Exited(_)
        ));
    }

    #[test]
    fn stderr_tail_is_bounded_redacted_forwarded_and_drains_a_chatty_child() {
        // 400 numbered lines, then a 200 KB burst (more than a pipe buffer) with no
        // newline until its end: a child nobody drains would block forever.
        let mut c = sh(
            "i=0; while [ $i -lt 400 ]; do echo \"line-$i key=PRIVATE$i\" >&2; i=$((i+1)); done; \
             head -c 200000 /dev/zero | tr '\\0' 'x' >&2; echo >&2; echo final-line >&2",
        );
        c.command_mut().stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let forwarded = Arc::new(Mutex::new(0usize));
        let f = Arc::clone(&forwarded);
        let mut tail = StderrTail::capture(
            child.take_stderr().unwrap(),
            2048,
            |l| match l.split_once("key=") {
                Some((head, _)) => Some(format!("{head}key=[redacted]")),
                None => Some(l.to_string()),
            },
            move |_| *f.lock().unwrap() += 1,
        );
        let status = child.wait().unwrap();
        assert!(status.success());
        let text = tail.finish();
        assert!(
            text.ends_with("final-line"),
            "newest line kept: ...{}",
            &text[text.len().saturating_sub(40)..]
        );
        assert!(!text.contains("PRIVATE"), "redacted before retention");
        assert!(!text.contains("line-0 "), "oldest lines fell off");
        assert!(*forwarded.lock().unwrap() >= 400, "every line forwarded");
    }
}

/// A stand-in for the UI thread: runs closures in order, like an event loop.
struct Loop {
    tx: Sender<Box<dyn FnOnce() + Send>>,
}
impl Loop {
    fn new() -> Self {
        let (tx, rx) = channel::<Box<dyn FnOnce() + Send>>();
        std::thread::spawn(move || {
            for job in rx {
                job();
            }
        });
        Loop { tx }
    }
    fn run(&self, f: impl FnOnce() + Send + 'static) {
        self.tx.send(Box::new(f)).unwrap();
    }
    fn ping(&self) -> impl Fn(Ack) -> Result<(), String> + Send + 'static {
        let tx = self.tx.clone();
        move |ack| {
            tx.send(Box::new(move || ack.done()))
                .map_err(|e| e.to_string())
        }
    }
}

type Events = Arc<Mutex<Vec<StallEvent>>>;

fn wait_for(events: &Events, what: &str, pred: impl Fn(&[StallEvent]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if pred(&events.lock().unwrap()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {:?}",
            events.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn collector() -> (Events, impl Fn(StallEvent) + Send + 'static) {
    let events: Events = Arc::default();
    let sink = Arc::clone(&events);
    (events, move |e| sink.lock().unwrap().push(e))
}

#[test]
fn stall_is_reported_once_per_episode_then_recovers_and_rearms() {
    let ev_loop = Loop::new();
    let (events, on_event) = collector();
    let cfg = StallConfig {
        poll: Duration::from_millis(10),
        threshold: Duration::from_millis(100),
        observer_gap: Duration::from_secs(5),
    };
    let _wd = StallWatchdog::spawn("test-stall", cfg, ev_loop.ping(), on_event).unwrap();

    // Healthy loop: nothing reported across many polls.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        events.lock().unwrap().is_empty(),
        "{:?}",
        events.lock().unwrap()
    );

    // Freeze the loop for 700 ms.
    ev_loop.run(|| std::thread::sleep(Duration::from_millis(700)));
    wait_for(&events, "recovery", |e| {
        e.iter().any(|x| matches!(x, StallEvent::Recovered { .. }))
    });
    {
        let e = events.lock().unwrap();
        let stalls: Vec<_> = e
            .iter()
            .filter(|x| matches!(x, StallEvent::Stalled { .. }))
            .collect();
        assert_eq!(stalls.len(), 1, "edge-triggered: {e:?}");
        match stalls[0] {
            StallEvent::Stalled {
                episode: 1,
                gap,
                recovered: false,
            } => {
                assert!(
                    *gap >= Duration::from_millis(100) && *gap < Duration::from_millis(650),
                    "{gap:?}"
                )
            }
            other => panic!("{other:?}"),
        }
        match e
            .iter()
            .find(|x| matches!(x, StallEvent::Recovered { .. }))
            .unwrap()
        {
            StallEvent::Recovered { episode: 1, gap } => {
                assert!(*gap >= Duration::from_millis(600), "{gap:?}")
            }
            other => panic!("{other:?}"),
        }
    }

    // A second freeze is a second episode.
    ev_loop.run(|| std::thread::sleep(Duration::from_millis(400)));
    wait_for(&events, "second episode", |e| {
        e.iter()
            .filter(|x| matches!(x, StallEvent::Recovered { .. }))
            .count()
            == 2
    });
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|x| matches!(x, StallEvent::Stalled { episode: 2, .. })));
}

#[test]
fn a_descheduled_observer_is_not_mistaken_for_a_stalled_target() {
    // The ping closure blocks the *watchdog* thread for 400 ms (as a suspended
    // process or sleeping laptop would) while the target is perfectly healthy.
    let ev_loop = Loop::new();
    let inner = ev_loop.ping();
    let n = Arc::new(Mutex::new(0));
    let ping = move |ack: Ack| {
        let mut k = n.lock().unwrap();
        *k += 1;
        if *k == 2 {
            std::thread::sleep(Duration::from_millis(400));
        }
        inner(ack)
    };
    let (events, on_event) = collector();
    let cfg = StallConfig {
        poll: Duration::from_millis(10),
        threshold: Duration::from_millis(100),
        observer_gap: Duration::from_millis(150),
    };
    let mut wd = StallWatchdog::spawn("test-observer", cfg, ping, on_event).unwrap();
    wait_for(&events, "observer gap", |e| {
        e.iter()
            .any(|x| matches!(x, StallEvent::ObserverGap { .. }))
    });
    std::thread::sleep(Duration::from_millis(200));
    wd.stop();
    let e = events.lock().unwrap();
    assert!(
        !e.iter().any(|x| matches!(x, StallEvent::Stalled { .. })),
        "false stall: {e:?}"
    );
    match e
        .iter()
        .find(|x| matches!(x, StallEvent::ObserverGap { .. }))
        .unwrap()
    {
        StallEvent::ObserverGap { gap } => assert!(*gap >= Duration::from_millis(350), "{gap:?}"),
        _ => unreachable!(),
    }
}

#[test]
fn a_dead_target_stops_the_watchdog_with_a_reason() {
    let (tx, rx) = channel::<Box<dyn FnOnce() + Send>>();
    drop(rx); // the "main thread" is gone: dispatch fails
    let (events, on_event) = collector();
    let cfg = StallConfig {
        poll: Duration::from_millis(10),
        ..StallConfig::default()
    };
    let _wd = StallWatchdog::spawn(
        "test-dead",
        cfg,
        move |ack| {
            tx.send(Box::new(move || ack.done()))
                .map_err(|_| "event loop closed".to_string())
        },
        on_event,
    )
    .unwrap();
    wait_for(&events, "stop", |e| {
        e.iter()
            .any(|x| matches!(x, StallEvent::Stopped { reason } if reason == "event loop closed"))
    });
}

#[test]
fn capture_cooldown_rate_limits_expensive_samples() {
    let mut c = Cooldown::new(Duration::from_secs(60));
    assert!(c.reserve(Duration::from_millis(200)));
    assert!(!c.reserve(Duration::from_millis(500)));
    assert!(!c.reserve(Duration::from_millis(60_199)));
    assert!(c.reserve(Duration::from_millis(60_200)));
}
