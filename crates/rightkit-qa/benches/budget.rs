//! Runtime budget for rightkit-qa (docs/RUNTIME-BUDGETS.md): the harness driving an engine sidecar.
//!
//! Load: start one `--stdio` sidecar through `EngineTarget::start_sidecar` (this bench binary re-run
//! as the engine: it prints a `hello` frame, and answers each `echo` request with a response plus one
//! unsolicited event frame and one stderr line, as a progress-reporting engine does), send 10,000
//! commands, keep the sidecar open, then idle for the idle window. The figures are the harness
//! process's own; the engine child is a separate process.
//!
//! Run: `rightkit cargo bench -p rightkit-qa --bench budget`
//! Optional: `-- <commands> <idle_seconds>` (defaults 10000 and 30).

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels};
use rightkit_qa::engine::{CliOptions, EngineTarget};
use rightkit_qa::process::Tracker;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
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
        eprintln!("progress: handled request {id} with a representative one-line stderr report");
        put(json!({"type":"event","id":id,"event":{"progress":1}}));
        put(json!({"type":"response","id":id,"ok":true,"result":req["params"].clone()}));
    }
}

fn main() {
    if std::env::args().any(|a| a == "--stdio") {
        engine();
        return;
    }
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let commands: u64 = args.first().and_then(|v| v.parse().ok()).unwrap_or(10_000);
    let idle_s: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);

    let before = process_usage().expect("process usage");
    // The guard refuses an engine launch that names no isolated data directory, even for this
    // synthetic engine, which reads and writes nothing: give it a scratch directory under the temp root.
    let scratch = std::env::temp_dir().join(format!("rightkit-qa-budget-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch data dir");
    let target = EngineTarget::new(std::env::current_exe().unwrap())
        .with_data_dir_env("RIGHTKIT_QA_BUDGET_DATA");
    let tracker = Tracker::new();
    let mut sidecar = target
        .start_sidecar(
            &CliOptions {
                data_dir: Some(scratch.clone()),
                ..Default::default()
            },
            &tracker,
        )
        .expect("sidecar");
    let started = Instant::now();
    for i in 0..commands {
        sidecar
            .command("echo", json!({"n": i}), Duration::from_secs(20))
            .expect("echo");
    }
    let load_s = started.elapsed().as_secs_f64();
    let warm = process_usage().expect("process usage");
    let status = sidecar.status();
    println!(
        "warm-up: {commands} commands in {load_s:.2} s ({:.0} cmd/s), buffered events {}, stderr bytes {}, resident {:.1} MiB (before start {:.1} MiB)",
        commands as f64 / load_s,
        status.events_buffered,
        status.stderr_bytes,
        warm.resident_bytes as f64 / 1048576.0,
        before.resident_bytes as f64 / 1048576.0,
    );
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    let end = sidecar.status();
    println!("RESULT events_buffered={} frames", end.events_buffered);
    println!("RESULT stderr_bytes_buffered={} bytes", end.stderr_bytes);
    print!("{}", idle.result_lines(&warm, &BudgetLabels { krate: "rightkit-qa", version: env!("CARGO_PKG_VERSION"), scale_unit: "commands", scale: commands, what: "echo commands to a stdio sidecar (one event frame and one stderr line each), sidecar open and idle", scope: "synthetic" }));
    sidecar.close();
    let _ = std::fs::remove_dir_all(&scratch);
}
