//! Runtime budget for rightkit-file-watch (docs/RUNTIME-BUDGETS.md), macOS and Windows.
//!
//! Scenario, through the public runtime-free API `TypedWatcher::watch(root)`:
//! * a real tree of 50,000 files in 1,000 directories is built under a temp root, plus a
//!   400-file churn pool in its own directories (50,400 files in total);
//! * a recursive typed watch is started on the root and the churn pool is rewritten at
//!   400 files per second for 60 s (400 modify operations per second). The caller drains
//!   the stream on every tick, as an app's consumer would, and samples the backlog;
//! * after the churn stops the stream is drained until it is quiet for 3 s, then the watch is
//!   held idle (tree watched, nothing changing) for the idle window.
//!
//! Reports process resident memory, CPU, wake-ups and disk operations through
//! `rightkit_child_process::process_usage` (the one shared source), delivered record rates from
//! `TypedWatcher::metrics()`, and the maximum backlog observed. The churn writes are the bench's own
//! file operations in the same process, so churn-window disk figures include them; only the idle
//! window is a budget figure.
//!
//! Run: `rightkit cargo bench -p rightkit-file-watch --bench budget`
//! Optional: `-- <static_files> <churn_seconds> <idle_seconds>` (defaults 50000, 60 and 30).

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels, ProcessUsage};
use rightkit_file_watch::{TypedWatcher, WatchEvent};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn mb(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn report(label: &str, p: ProcessUsage) {
    println!(
        "{label}: resident {:7.1} MB  footprint {:7.1} MB  cpu {:.2} s",
        mb(p.resident_bytes),
        mb(p.footprint_bytes),
        p.cpu_seconds
    );
}

fn usage() -> ProcessUsage {
    process_usage().expect("process usage")
}

fn write_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
}

/// Builds the static tree and the churn pool; returns (root, churn pool paths).
fn build_tree(root: &Path, static_files: u64) -> Vec<PathBuf> {
    let dirs = (static_files / 50).max(1);
    for d in 0..dirs {
        let dir = root.join(format!("pkg{d:04}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..50 {
            write_file(&dir.join(format!("m{f:02}.rs")), &[b'x'; 64]);
        }
    }
    let churn = root.join("churn");
    let mut pool = Vec::new();
    for d in 0..20 {
        let dir = churn.join(format!("c{d:02}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..20 {
            let p = dir.join(format!("hot{f:02}.o"));
            write_file(&p, &[7u8; 64]);
            pool.push(p);
        }
    }
    pool
}

/// Consumer step: take everything queued right now and count it.
fn drain(watch: &TypedWatcher, seen: &mut u64) {
    while watch.try_recv().is_ok() {
        *seen += 1;
    }
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let static_files: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(50_000);
    let secs: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(60);
    let idle_s: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(30);
    const RATE: usize = 400;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("tree");
    fs::create_dir_all(&root).unwrap();
    let t = Instant::now();
    let pool = build_tree(&root, static_files);
    let total = static_files / 50 * 50 + pool.len() as u64;
    println!(
        "built {total} files ({} churned) in {:.1?}",
        pool.len(),
        t.elapsed()
    );
    let before = usage();
    report("before watch", before);

    let t = Instant::now();
    let watch = TypedWatcher::watch(&root).unwrap();
    let start_ms = t.elapsed().as_secs_f64() * 1e3;
    // Let the backend finish registering the recursive watch before churn.
    std::thread::sleep(Duration::from_secs(2));
    let mut drained = 0u64;
    drain(&watch, &mut drained);
    report("watch started (recursive, idle 2 s)", usage());
    println!("watch start: {start_ms:.1} ms");

    let m0 = watch.metrics();
    let u0 = usage();
    let mut max_backlog = 0u64;
    let mut overflow_seen = false;
    let start = Instant::now();
    for sec in 0..secs {
        let tick = start + Duration::from_secs(sec);
        if let Some(wait) = tick.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        for i in 0..RATE {
            let p = &pool[(sec as usize * RATE + i) % pool.len()];
            write_file(p, &[(sec % 251) as u8; 64]);
        }
        drain(&watch, &mut drained);
        let backlog = watch.metrics().queue_depth;
        max_backlog = max_backlog.max(backlog);
    }
    let wall = start.elapsed().as_secs_f64();
    let u1 = usage();
    let churn_rates = u1.rates_since(&u0, Duration::from_secs_f64(wall));
    let cpu = u1.cpu_seconds - u0.cpu_seconds;
    let m1 = watch.metrics();
    report("after churn (watch on)", usage());

    // Quiet period: drain until no record arrives for 3 s.
    let mut quiet_since = Instant::now();
    while quiet_since.elapsed() < Duration::from_secs(3) {
        if let Ok(event) = watch.recv_timeout(Duration::from_millis(200)) {
            drained += 1;
            if matches!(event, WatchEvent::Overflow) {
                overflow_seen = true;
            }
            quiet_since = Instant::now();
        }
    }
    let end = watch.metrics();
    report("after quiet period", usage());
    let queued_in_window = m1.queued_total - m0.queued_total;
    let received_in_window = m1.received_total - m0.received_total;
    let warm = usage();
    println!(
        "churn window: files={total} churn_ops_per_s={RATE} churn_s={secs} wall_s={wall:.1} cpu_s={cpu:.2} cpu_percent={:.1} records_queued_per_s={:.1} records_received_per_s={:.1} queue_depth_max={max_backlog} queue_depth_end={} overflow_total={} error_total={} start_ms={start_ms:.1} watch_drained={drained} quiet_overflow={overflow_seen}",
        cpu / wall * 100.0,
        queued_in_window as f64 / wall,
        received_in_window as f64 / wall,
        end.queue_depth,
        end.overflow_total,
        end.error_total,
    );
    println!(
        "churn window rates (include the bench's own file writes): {}",
        churn_rates_line(&churn_rates)
    );

    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    let status = watch.status();
    println!(
        "RESULT queue_depth_after_idle={} records",
        status.metrics.queue_depth
    );
    print!(
        "{}",
        idle.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-file-watch",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "files",
                scale: total,
                what: "files watched recursively after 400 modify ops/s churn, then idle",
                scope: "synthetic"
            }
        )
    );
}

fn churn_rates_line(r: &rightkit_child_process::UsageRates) -> String {
    let f = |v: Option<f64>| v.map_or("n/a".to_string(), |v| format!("{v:.1}"));
    format!(
        "wakeups_per_s={} disk_ops_per_s={}",
        f(r.wakeups_per_second),
        f(r.disk_ops_per_second)
    )
}
