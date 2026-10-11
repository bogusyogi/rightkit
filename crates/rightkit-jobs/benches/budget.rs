//! Runtime budget for rightkit-jobs (docs/RUNTIME-BUDGETS.md): an engine supervisor with a live engine.
//!
//! Load: supervise one engine child (this bench binary re-run in engine mode, speaking the stdio
//! contract: a `hello` line, then `ping` and `echo` responses) with the default 2 s health ping,
//! hold 100 event subscribers, send 10,000 `echo` calls from 4 threads, then idle for the idle
//! window with the engine alive and the health ping running.
//!
//! The figures are the supervising process's own (the engine child is a separate process).
//!
//! Run: `rightkit cargo bench -p rightkit-jobs --bench budget`
//! Optional: `-- <calls> <idle_seconds>` (defaults 10000 and 30).

use rightkit_jobs::{EngineSupervisor, HealthCheck, SupervisorConfig};
use rightkit_process::{observe_idle, process_usage, BudgetLabels, OwnedCommand};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn engine() {
    let stdout = std::io::stdout();
    let put = |v: Value| {
        let mut o = stdout.lock();
        writeln!(o, "{v}").unwrap();
        o.flush().unwrap();
    };
    put(json!({"type":"hello","engine":"budget","version":"1.0","contract_version":1}));
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = req["id"].clone();
        let result = match req["method"].as_str().unwrap_or("") {
            "ping" => json!({"pong": true}),
            _ => req["params"].clone(),
        };
        put(json!({"type":"response","id":id,"ok":true,"result":result}));
    }
}

fn main() {
    if std::env::var_os("RK_BUDGET_ENGINE").is_some() {
        engine();
        return;
    }
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let calls: usize = args.first().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let idle_s: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);

    let exe = std::env::current_exe().unwrap();
    let before = process_usage().expect("process usage");
    let cfg = SupervisorConfig::new("budget", move || {
        let mut c = OwnedCommand::new(&exe);
        c.command_mut().env("RK_BUDGET_ENGINE", "1");
        Ok(c)
    })
    .with_hello_timeout(Duration::from_secs(20))
    .with_health(HealthCheck::default());
    let sup = Arc::new(EngineSupervisor::new(cfg));
    sup.start().expect("engine start");
    let _subs: Vec<_> = (0..100).map(|_| sup.subscribe_events()).collect();

    let started = Instant::now();
    let threads = 4usize;
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let sup = Arc::clone(&sup);
            std::thread::spawn(move || {
                let share = calls / threads;
                for i in 0..share {
                    sup.call("echo", json!({"t": t, "n": i})).expect("echo");
                }
                share
            })
        })
        .collect();
    let sent: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let load_s = started.elapsed().as_secs_f64();
    let warm = process_usage().expect("process usage");
    println!(
        "warm-up: {sent} calls in {load_s:.2} s ({:.0} calls/s), lifecycle {:?}, resident {:.1} MiB (before start {:.1} MiB)",
        sent as f64 / load_s,
        sup.lifecycle(),
        warm.resident_bytes as f64 / 1048576.0,
        before.resident_bytes as f64 / 1048576.0,
    );
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    let end = sup.status();
    println!("RESULT restarts={} restarts", end.restarts);
    println!(
        "RESULT event_subscribers={} subscribers",
        end.event_subscribers
    );
    print!("{}", idle.result_lines(&warm, &BudgetLabels { krate: "rightkit-jobs", version: env!("CARGO_PKG_VERSION"), scale_unit: "calls", scale: sent as u64, what: "echo calls to a live engine with 100 event subscribers and a 2 s health ping, then idle", scope: "synthetic" }));
}
