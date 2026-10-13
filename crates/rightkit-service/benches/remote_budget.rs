//! Runtime budget for rightkit-service (remote profile) (macOS and Windows).
//!
//! Scenario: one in-process `Daemon` driven through `Daemon::handle` (no transport). One
//! device is enrolled through the real enrolment route, then N device-signed
//! `PATCH /v1/publish-jobs/{id}` submissions run with `If-Match: 0`, a distinct
//! `Idempotency-Key` and a distinct job id each (default N = 10,000). The harness is the
//! one the integration tests use (`tests/common`), so signing and the authorisation path
//! are the production ones. The clock is fixed inside the replay and idempotency windows,
//! so every nonce and idempotency record is retained: the worst case inside the TTL.
//!
//! After the submissions the daemon is held idle (state resident, no request in flight) for the idle
//! window. Reports resident memory, CPU, wake-ups and disk operations through
//! `rightkit_process::process_usage` (the one shared source), submission latency, the size of each
//! `state.json` write (`Daemon::metrics`), and the retained-state counts. The transport (HTTPS
//! listener) is not exercised: the idle figures are the daemon state and runtime without a listener.
//!
//! Run: `rightkit cargo bench -p rightkit-service --features remote --bench remote_budget`
//! Optional: `-- <submissions> <idle_seconds>` (defaults 10000 and 30).

#[path = "../tests/common/mod.rs"]
mod common;

use common::Harness;
use rightkit_process::{observe_idle, process_usage, BudgetLabels, ProcessUsage};
use serde_json::json;
use std::time::{Duration, Instant};

fn mb(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

fn usage() -> ProcessUsage {
    process_usage().expect("process usage")
}

fn report(label: &str, p: ProcessUsage) {
    println!(
        "{label}: resident {:7.1} MB  footprint {:7.1} MB",
        mb(p.resident_bytes),
        mb(p.footprint_bytes)
    );
}

fn main() {
    let nums: Vec<u64> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let n: u64 = nums.first().copied().unwrap_or(10_000);
    let idle_s: u64 = nums.get(1).copied().unwrap_or(30);

    let h = Harness::new();
    let dev = h.enrol();
    let before = h.daemon.metrics();
    let p_before = usage();
    report("before submissions", p_before);

    let cpu0 = usage().cpu_seconds;
    let start = Instant::now();
    let mut max_ms = 0.0f64;
    let mut window_ms = 0.0f64;
    for i in 0..n {
        let t = Instant::now();
        let r = dev.patch_job(
            &h,
            &format!("job-{i:05}"),
            0,
            &format!("idem-{i:05}"),
            &json!({ "caption": format!("caption {i}"), "slot": i % 40, "status": "queued" }),
        );
        let ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(r.status, 200, "submission {i}: {:?}", r.body);
        max_ms = max_ms.max(ms);
        window_ms += ms;
        if (i + 1) % 1_000 == 0 {
            let m = h.daemon.metrics();
            println!(
                "progress {:>6} submissions  {:7.1} s  last-1000 mean {:6.2} ms  state.json {:7.1} KB  resident {:7.1} MB",
                i + 1,
                start.elapsed().as_secs_f64(),
                window_ms / 1_000.0,
                m.last_state_bytes as f64 / 1024.0,
                mb(usage().resident_bytes),
            );
            window_ms = 0.0;
        }
    }
    let wall = start.elapsed().as_secs_f64();
    let cpu = usage().cpu_seconds - cpu0;
    assert_eq!(h.daemon.job_revision("job-00000"), 1);

    let m = h.daemon.metrics();
    let p_after = usage();
    report("after submissions", p_after);
    let saves = m.state_saves - before.state_saves;
    println!(
        "submissions: n={n} wall_s={wall:.2} submissions_per_s={:.1} mean_ms={:.2} max_ms={max_ms:.1} cpu_s={cpu:.2} cpu_percent={:.1} resident_growth_mb={:.1} state_saves_per_submission={:.2} state_last_kb={:.1} state_max_kb={:.1} jobs={} idempotency_records={} nonce_records={} events_retained={} requests={}",
        n as f64 / wall,
        wall * 1e3 / n as f64,
        cpu / wall * 100.0,
        mb(p_after.resident_bytes) - mb(p_before.resident_bytes),
        saves as f64 / n as f64,
        m.last_state_bytes as f64 / 1024.0,
        m.max_state_bytes as f64 / 1024.0,
        m.jobs,
        m.idempotency_records,
        m.nonce_records,
        m.events_retained,
        m.requests - before.requests,
    );
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    let status = h.daemon.status();
    println!("RESULT jobs_retained={} jobs", status.metrics.jobs);
    print!(
        "{}",
        idle.result_lines(
            &p_after,
            &BudgetLabels {
                krate: "rightkit-service",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "submissions",
                scale: n,
                what: "signed job submissions retained in daemon state, daemon idle (no listener)",
                scope: "synthetic"
            }
        )
    );
}
