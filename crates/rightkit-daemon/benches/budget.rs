//! Runtime budget for rightkit-daemon (docs/RUNTIME-BUDGETS.md): one resident local host.
//!
//! Load: host an echo service on a temp suite root (current-user-only socket on macOS, named pipe on
//! Windows), open 64 client connections, send 20,000 `echo` requests spread over them from 8
//! threads, keep all 64 connections open, then idle for the idle window. The host's takeover
//! monitor (blocked on a one-second timed wait, one stat of `takeover.json` per wake) runs the
//! whole time; its cost is what the idle figures show.
//!
//! Run: `rightkit cargo bench -p rightkit-daemon --bench budget`
//! Optional: `-- <connections> <requests> <idle_seconds>` (defaults 64, 20000 and 30).

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels};
use rightkit_daemon::*;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Echo;
impl Handler for Echo {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => Ok(params.clone()),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let conns: usize = args.first().and_then(|v| v.parse().ok()).unwrap_or(64);
    let requests: usize = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(20_000);
    let idle_s: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(30);

    let tmp = tempfile::tempdir().unwrap();
    let suite = Suite::at(tmp.path()).expect("suite");
    let before = process_usage().expect("process usage");
    let allow = Allowlist::new().allow("bench", ["echo"]);
    let mut cfg = ServiceConfig::new("budget", "bench-host", 10, "bench", allow);
    cfg.takeover_timeout = Duration::from_secs(20);
    // Windows named pipes cap server instances; allow every connection this bench holds open.
    let options = HostOptions::new().max_pipe_instances(conns as u32 + 8);
    let hosted = host_with_options(&suite, cfg, Arc::new(Echo), options).expect("host");

    let clients: Vec<Arc<Client>> = (0..conns)
        .map(|_| Arc::new(Client::connect(&suite, "budget", "bench").expect("connect")))
        .collect();
    let started = Instant::now();
    let threads = 8usize.min(conns.max(1));
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let mine: Vec<Arc<Client>> = clients.iter().skip(t).step_by(threads).cloned().collect();
            let share = requests / threads;
            std::thread::spawn(move || {
                for i in 0..share {
                    let c = &mine[i % mine.len()];
                    c.call("echo", json!({"n": i})).expect("echo");
                }
                share
            })
        })
        .collect();
    let sent: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let load_s = started.elapsed().as_secs_f64();
    let status = hosted.status();
    let warm = process_usage().expect("process usage");
    println!(
        "warm-up: {sent} requests over {conns} connections in {load_s:.2} s ({:.0} req/s), open connections {}, inflight {}, resident {:.1} MiB (before host {:.1} MiB)",
        sent as f64 / load_s,
        status.connections,
        status.inflight,
        warm.resident_bytes as f64 / 1048576.0,
        before.resident_bytes as f64 / 1048576.0,
    );
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    let end = hosted.status();
    println!("RESULT open_connections={} connections", end.connections);
    print!(
        "{}",
        idle.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-daemon",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "connections",
                scale: conns as u64,
                what: "open connections after echo requests, then idle",
                scope: "synthetic"
            }
        )
    );
    drop(clients);
    drop(hosted);
}
