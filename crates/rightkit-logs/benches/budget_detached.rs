//! Runtime budget for the public detached-stderr background drainer.
//!
//! Load: write 200,000 complete lines (about 120 bytes each) through `stderr_file`, then
//! let `spawn_drainer` scrub and rotate them using default compaction and a 1 MiB sink.
//! Keep the drainer alive throughout `observe_idle`, in a separate process from `budget`.
//! The two-second interval is an explicit measurement parameter, not an API default:
//! `spawn_drainer` requires its caller to choose an interval.
//! Qualification covers this chosen two-second synthetic workload only.
//!
//! Run: `rightkit cargo test --locked --profile release-iterate -p rightkit-logs
//! --features tracing,log-bridge --bench budget_detached`
//! Optional: `-- <lines> <idle_seconds>` (defaults 200000 and 60).

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels};
use rightkit_logs::{DetachedLog, RotatingTextWriter};
use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DRAIN_INTERVAL: Duration = Duration::from_secs(2);
const MIN_IDLE_INTERVALS: u32 = 30;

fn wait_until_drained(log: &DetachedLog) {
    let deadline = Instant::now() + Duration::from_secs(300);
    while log.has_pending() {
        assert!(Instant::now() < deadline, "detached drain timed out");
        assert_eq!(log.last_error(), None, "detached drain failed");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(log.last_error(), None, "detached drain failed");
    let status = log.sink().status();
    assert_eq!(status.failed_appends, 0, "sink append failed");
    assert_eq!(status.failed_rotations, 0, "sink rotation failed");
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let lines: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(200_000);
    let minimum_idle = DRAIN_INTERVAL * MIN_IDLE_INTERVALS;
    let idle_s: u64 = args
        .get(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(minimum_idle.as_secs());
    let idle_duration = Duration::from_secs(idle_s);
    assert!(lines >= 200_000, "required scale is at least 200,000 lines");
    assert!(
        idle_duration >= minimum_idle,
        "idle window must cover at least {MIN_IDLE_INTERVALS} intervals"
    );

    let tmp = tempfile::tempdir().unwrap();
    let before = process_usage().expect("process usage");
    let sink = Arc::new(RotatingTextWriter::new(
        tmp.path().join("budget.log"),
        1 << 20,
    ));
    let log = Arc::new(DetachedLog::new(sink).with_prefix("[budget] "));
    let started = Instant::now();
    {
        let mut raw = BufWriter::new(log.stderr_file().expect("raw stderr file"));
        for i in 0..lines {
            writeln!(
                raw,
                "budget line {i:06} payload=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .expect("write raw line");
        }
        raw.flush().expect("flush raw lines");
    }
    let drainer = log.spawn_drainer(DRAIN_INTERVAL).expect("spawn drainer");
    wait_until_drained(&log);
    let drain_s = started.elapsed().as_secs_f64();
    let warm = process_usage().expect("process usage");
    println!(
        "warm-up: {lines} detached lines in {drain_s:.2} s, interval {} ms (chosen workload parameter), resident {:.1} MiB (before install {:.1} MiB)",
        DRAIN_INTERVAL.as_millis(),
        warm.resident_bytes as f64 / 1048576.0,
        before.resident_bytes as f64 / 1048576.0,
    );
    let idle = observe_idle(idle_duration).expect("idle window");
    assert_eq!(log.last_error(), None, "detached idle drain failed");
    assert!(!log.has_pending(), "pending detached bytes after idle");

    // Confirm the same worker still drains after the entire measured window. Two lines
    // allow the default post-compaction guard to omit its first line as documented.
    {
        let mut raw = log.stderr_file().expect("raw stderr file");
        raw.write_all(b"after idle guard\nafter idle worker probe\n")
            .expect("write worker probe");
    }
    wait_until_drained(&log);
    drainer.stop();
    assert_eq!(log.last_error(), None, "detached final drain failed");
    let tail = rightkit_logs::tail_bytes(log.sink().path(), 256).expect("read worker probe");
    assert!(
        tail.ends_with(b"[budget] after idle worker probe\n"),
        "worker did not write post-idle probe"
    );
    log.finish().expect("finish detached log");
    println!("RESULT pending_bytes_after_idle=0 bytes");
    print!(
        "{}",
        idle.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-logs",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "lines",
                scale: lines,
                what: "detached stderr lines drained then idle at chosen 2000 ms interval",
                scope: "synthetic"
            }
        )
    );
}
