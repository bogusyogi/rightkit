//! Runtime budget for rightkit-shell (docs/RUNTIME-BUDGETS.md): the hang watchdog on a live Tauri
//! event loop.
//!
//! Load: build a Wry (native webview runtime) Tauri app with the test context and no windows, install
//! the shell's hang watchdog with its defaults (1 s main-thread heartbeat ping, 250 ms allowed
//! lateness, 5 s threshold; neither thread polls), run the real event loop on the main thread, wait
//! 5 s, then observe the idle window.
//! The figures are the process's: the shell's two watchdog threads plus the Tauri/Wry event loop. The
//! webview and its helper processes are separate processes and are not in these figures.
//!
//! Run: `rightkit cargo bench -p rightkit-shell --bench budget`
//! Optional: `-- <idle_seconds>` (default 30).

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels};
use rightkit_shell::watchdog;
use rightkit_shell::WatchdogConfig;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let idle_s: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(30);

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
                    krate: "rightkit-shell",
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
