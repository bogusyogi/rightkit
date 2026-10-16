//! Runtime budget for rightkit-logs (docs/RUNTIME-BUDGETS.md): the tracing file writer.
//!
//! Load: install the daily-JSONL tracing writer on a temp directory, write 200,000 info lines
//! of about 120 bytes through `tracing` (a burst the writer drains from its 128,000-line queue),
//! wait for the backlog to reach zero, then hold the guard idle for the idle window.
//! Prints `RESULT` lines (see `rightkit_child_process::IdleObservation::result_lines`) after the idle
//! window, plus warm-up and status figures.
//!
//! Run: `rightkit cargo bench -p rightkit-logs --features tracing --bench budget`
//! Optional: `-- <lines> <idle_seconds>` (defaults 200000 and 30).

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels};
use rightkit_logs::{install_tracing, TracingFileConfig};
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let lines: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(200_000);
    let idle_s: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);

    let tmp = tempfile::tempdir().unwrap();
    let before = process_usage().expect("process usage");
    let guard = install_tracing(TracingFileConfig::new(tmp.path(), "budget")).unwrap();
    let started = Instant::now();
    for i in 0..lines {
        tracing::info!(
            sequence = i,
            payload = "0123456789abcdef0123456789abcdef0123456789abcdef",
            "budget line"
        );
    }
    while guard.status().pending_lines > 0 {
        std::thread::sleep(Duration::from_millis(10));
    }
    let drain_s = started.elapsed().as_secs_f64();
    let status = guard.status();
    let warm = process_usage().expect("process usage");
    println!(
        "warm-up: {lines} lines in {drain_s:.2} s ({:.0} lines/s), dropped {}, resident {:.1} MiB (before install {:.1} MiB)",
        lines as f64 / drain_s,
        status.dropped_lines,
        warm.resident_bytes as f64 / 1048576.0,
        before.resident_bytes as f64 / 1048576.0,
    );
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    let end = guard.status();
    println!(
        "RESULT pending_lines_after_idle={} lines",
        end.pending_lines
    );
    println!("RESULT dropped_lines={} lines", end.dropped_lines);
    print!(
        "{}",
        idle.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-logs",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "lines",
                scale: lines,
                what: "tracing lines written then idle",
                scope: "synthetic"
            }
        )
    );
    drop(guard);
}
