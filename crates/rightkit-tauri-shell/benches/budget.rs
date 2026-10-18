//! Runtime budget for rightkit-tauri-shell (docs/RUNTIME-BUDGETS.md): the hang watchdog on a live Tauri
//! event loop.
//!
//! Load: build a Wry (native webview runtime) Tauri app with the test context and no windows, install
//! the shell's hang watchdog with its defaults (1 s main-thread heartbeat ping, 250 ms allowed
//! lateness, 5 s threshold; neither thread polls), run the real event loop on the main thread, wait
//! 5 s, then observe the idle window.
//! The figures are the process's: the shell's two watchdog threads plus the Tauri/Wry event loop. The
//! webview and its helper processes are separate processes and are not in these figures.
//!
//! The wake-up gate is the watchdog's cost over a bare Wry loop, not the whole-process rate, which host
//! and window-server activity move by 2x within a day (runtime-budget.json ceilingBasis). Wry allows one
//! event loop per process, so the default mode is a parent that runs each workload in a child process of
//! this binary, interleaved bare, watchdog, bare, watchdog, ... so drift hits both sides. It reports the
//! median pair by delta: that pair's watchdog run supplies every figure of the `RESULT budget` line
//! (resident memory, CPU, wake-ups, disk), and its bare run supplies `bare_wakeups_per_s`.
//!
//! Run: `rustright cargo test -p rightkit-tauri-shell --profile release-iterate --bench budget -- 30`
//! Optional: `-- <idle_seconds> [--pairs <n>]` (defaults 30 and 3, pairs odd and at least 3; a pair is one bare and one watchdog run,
//! each 5 s warm-up plus the idle window).
//! Internal: `-- --single <idle_seconds>` runs one watchdog workload in this process (the parent's child).
//! Diagnostic only: `-- --diagnostic <bare|heartbeat-1s|heartbeat-2s|full> [idle_seconds]`.
//! Diagnostic modes retain the native app, 5 s warm-up and process observation window, but never
//! emit a qualifying `RESULT budget` line. Run each mode in a fresh process.

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels};
use rightkit_tauri_shell::watchdog;
use rightkit_tauri_shell::WatchdogConfig;
use std::process::{Command, Stdio};
use std::time::Duration;

const DEFAULT_IDLE_SECONDS: u64 = 30;
const DEFAULT_PAIRS: usize = 3;

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    if args.first().is_some_and(|arg| arg == "--diagnostic") {
        diagnostic(&args[1..]);
        return;
    }
    assert!(
        !args.iter().any(|arg| arg.starts_with("--diagnostic")),
        "diagnostic usage: --diagnostic <bare|heartbeat-1s|heartbeat-2s|full> [idle_seconds]"
    );
    if args.first().is_some_and(|arg| arg == "--single") {
        let idle_s: u64 = args
            .get(1)
            .and_then(|v| v.parse().ok())
            .expect("--single needs idle_seconds");
        single(idle_s);
        return;
    }
    let mut idle_s = DEFAULT_IDLE_SECONDS;
    let mut pairs = DEFAULT_PAIRS;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == "--pairs" {
            pairs = rest
                .next()
                .and_then(|v| v.parse().ok())
                .expect("--pairs needs a positive integer");
        } else {
            idle_s = arg.parse().expect("idle_seconds must be an integer");
        }
    }
    assert!(
        pairs >= DEFAULT_PAIRS && pairs % 2 == 1,
        "--pairs must be odd and at least {DEFAULT_PAIRS}, so the median is one real pair"
    );
    assert!(idle_s > 0, "idle_seconds must be positive");
    paired(idle_s, pairs);
}

/// One bare run or one watchdog run of this binary, as a child process; its stdout.
fn run_child(args: &[&str]) -> String {
    let exe = std::env::current_exe().expect("bench executable path");
    let output = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .expect("start bench child");
    assert!(
        output.status.success(),
        "bench child {args:?} failed: {}",
        output.status
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A rate printed with three decimals, so the delta below equals the difference of the printed figures.
fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// The wake-up rate of a bare child (`DIAGNOSTIC wakeups_per_second=<n>`).
fn bare_wakeups(output: &str) -> f64 {
    let value = output
        .lines()
        .find_map(|line| line.strip_prefix("DIAGNOSTIC wakeups_per_second="))
        .expect("bare child printed no wake-up rate");
    round3(
        value
            .trim()
            .parse()
            .expect("the bare wake-up rate is unavailable on this host; a delta needs it"),
    )
}

/// The `RESULT budget` line of a watchdog child and its wake-up rate.
fn watchdog_wakeups(output: &str) -> (&str, f64) {
    let line = output
        .lines()
        .find(|line| line.starts_with("RESULT budget "))
        .expect("watchdog child printed no RESULT budget line");
    let value = line
        .split_whitespace()
        .find_map(|token| token.strip_prefix("wakeups_per_s="))
        .expect("RESULT budget line has no wakeups_per_s");
    (
        line,
        value
            .parse()
            .expect("the watchdog wake-up rate is unavailable on this host; a delta needs it"),
    )
}

/// Runs `pairs` of (bare, watchdog) children back to back and prints the median pair by delta, with the
/// bare rate and the delta appended to its `RESULT budget` line. The runtime-budget gate recomputes the
/// delta from the two raw rates and refuses a record whose reported delta disagrees.
fn paired(idle_s: u64, pairs: usize) {
    let idle = idle_s.to_string();
    let mut runs: Vec<(f64, f64, f64, String)> = Vec::new();
    for pair in 1..=pairs {
        let bare = bare_wakeups(&run_child(&["--diagnostic", "bare", idle.as_str()]));
        let output = run_child(&["--single", idle.as_str()]);
        let watchdog = round3(watchdog_wakeups(&output).1);
        let delta = round3(watchdog - bare);
        println!(
            "PAIR {pair}/{pairs} bare_wakeups_per_s={bare:.3} watchdog_wakeups_per_s={watchdog:.3} delta_per_s={delta:.3}"
        );
        runs.push((delta, bare, watchdog, output));
    }
    runs.sort_by(|a, b| a.0.partial_cmp(&b.0).expect("finite delta"));
    let (delta, bare, _, output) = runs.swap_remove((pairs - 1) / 2);
    println!("bench: median of {pairs} pair(s) by delta, idle window {idle_s} s each");
    for line in output.lines() {
        if line.starts_with("RESULT budget ") {
            println!("{line} bare_wakeups_per_s={bare:.3} wakeups_delta_per_s={delta:.3}");
        } else {
            println!("{line}");
        }
    }
}

/// One watchdog workload in this process (a child of `paired`, or run directly for a whole-process figure).
fn single(idle_s: u64) {
    let before = process_usage().expect("process usage");
    // Only the macOS activation-policy call below mutates the app.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut app = tauri::Builder::<tauri::Wry>::new()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("build the Tauri app");
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
    let handle = app.handle().clone();
    watchdog::install(handle.clone(), WatchdogConfig::default());

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        let warm = process_usage().expect("process usage");
        println!(
            "warm-up: event loop running, watchdog installed, resident {:.1} MiB (before app build {:.1} MiB)",
            warm.resident_bytes as f64 / 1048576.0,
            before.resident_bytes as f64 / 1048576.0
        );
        let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
        let end = watchdog::status();
        println!(
            "RESULT watchdog_stalls={} stalls",
            end.watchdog.stalls_total
        );
        println!(
            "RESULT watchdog_last_beat_age_ms={} ms",
            end.watchdog.last_beat_age_ms
        );
        print!(
            "{}",
            idle.result_lines(
                &warm,
                &BudgetLabels {
                    krate: "rightkit-tauri-shell",
                    version: env!("CARGO_PKG_VERSION"),
                    scale_unit: "watchdogs",
                    scale: 1,
                    what: "default hang watchdog on an idle Wry event loop with no windows",
                    scope: "synthetic"
                }
            )
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();
        handle.exit(0);
    });
    app.run(|_, _| {});
}

/// Isolate native event-loop cost without changing the qualified default workload. Counters are
/// atomic increments only while the app runs; the observer reads them only at the window endpoints.
fn diagnostic(args: &[String]) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let mode = args.first().map(String::as_str).expect(
        "diagnostic usage: --diagnostic <bare|heartbeat-1s|heartbeat-2s|full> [idle_seconds]",
    );
    let heartbeat_interval = match mode {
        "bare" | "full" => None,
        "heartbeat-1s" => Some(Duration::from_secs(1)),
        "heartbeat-2s" => Some(Duration::from_secs(2)),
        _ => panic!("unknown diagnostic mode: {mode}"),
    };
    assert!(args.len() <= 2, "unexpected diagnostic arguments");
    let idle_s: u64 = args
        .get(1)
        .map(|arg| {
            arg.parse()
                .expect("diagnostic idle_seconds must be an integer")
        })
        .unwrap_or(30);
    assert!(idle_s > 0, "diagnostic idle_seconds must be positive");
    // Own the label before the observer thread takes it.
    let mode = mode.to_owned();

    let before = process_usage().expect("diagnostic process usage");
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut app = tauri::Builder::<tauri::Wry>::new()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("build the diagnostic Tauri app");
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Prohibited);
    let handle = app.handle().clone();
    let main_iterations = Arc::new(AtomicU64::new(0));
    let attempts = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicU64::new(0));
    let dispatch_failures = Arc::new(AtomicU64::new(0));

    if mode == "full" {
        watchdog::install(handle.clone(), WatchdogConfig::default());
    }
    if let Some(interval) = heartbeat_interval {
        let heartbeat_handle = handle.clone();
        let (attempts, completed, dispatch_failures) = (
            Arc::clone(&attempts),
            Arc::clone(&completed),
            Arc::clone(&dispatch_failures),
        );
        std::thread::Builder::new()
            .name("rk-shell-diagnostic-heartbeat".into())
            .spawn(move || loop {
                std::thread::sleep(interval);
                let completed = Arc::clone(&completed);
                attempts.fetch_add(1, Ordering::Relaxed);
                if heartbeat_handle
                    .run_on_main_thread(move || {
                        completed.fetch_add(1, Ordering::Relaxed);
                    })
                    .is_err()
                {
                    dispatch_failures.fetch_add(1, Ordering::Relaxed);
                    break;
                }
            })
            .expect("start diagnostic heartbeat");
    }

    let observed_iterations = Arc::clone(&main_iterations);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        let warm = process_usage().expect("diagnostic process usage");
        println!(
            "DIAGNOSTIC mode={mode} qualified=false warm_up_seconds=5 resident_bytes={} before_app_resident_bytes={}",
            warm.resident_bytes, before.resident_bytes
        );
        let start = (
            observed_iterations.load(Ordering::Relaxed),
            attempts.load(Ordering::Relaxed),
            completed.load(Ordering::Relaxed),
            dispatch_failures.load(Ordering::Relaxed),
        );
        let idle = observe_idle(Duration::from_secs(idle_s)).expect("diagnostic idle window");
        let end = (
            observed_iterations.load(Ordering::Relaxed),
            attempts.load(Ordering::Relaxed),
            completed.load(Ordering::Relaxed),
            dispatch_failures.load(Ordering::Relaxed),
        );
        println!(
            "DIAGNOSTIC idle_window_seconds={:.3}",
            idle.elapsed.as_secs_f64()
        );
        println!("DIAGNOSTIC idle_cpu_percent={:.6}", idle.rates.cpu_percent);
        let rate = |value: Option<f64>| {
            value.map_or_else(|| "unavailable".to_owned(), |value| format!("{value:.6}"))
        };
        println!(
            "DIAGNOSTIC wakeups_per_second={}",
            rate(idle.rates.wakeups_per_second)
        );
        println!(
            "DIAGNOSTIC disk_ops_per_second={}",
            rate(idle.rates.disk_ops_per_second)
        );
        println!("DIAGNOSTIC idle_resident_bytes={}", idle.end.resident_bytes);
        println!("DIAGNOSTIC main_iterations={}", end.0 - start.0);
        if heartbeat_interval.is_some() {
            println!("DIAGNOSTIC heartbeat_attempts={}", end.1 - start.1);
            println!("DIAGNOSTIC heartbeat_completed={}", end.2 - start.2);
            println!("DIAGNOSTIC heartbeat_dispatch_failures={}", end.3 - start.3);
        } else if mode == "full" {
            // Production watchdog internals remain untouched by this bench-only probe.
            let status = watchdog::status();
            println!("DIAGNOSTIC heartbeat_counts=unavailable watchdog_internals_unchanged=true");
            println!(
                "DIAGNOSTIC watchdog_stalls={}",
                status.watchdog.stalls_total
            );
            println!(
                "DIAGNOSTIC watchdog_last_beat_age_ms={}",
                status.watchdog.last_beat_age_ms
            );
        }
        use std::io::Write;
        let _ = std::io::stdout().flush();
        handle.exit(0);
    });
    app.run(move |_, event| {
        if matches!(event, tauri::RunEvent::MainEventsCleared) {
            main_iterations.fetch_add(1, Ordering::Relaxed);
        }
    });
}
