//! Runtime budget for rightkit-send (docs/RUNTIME-BUDGETS.md): the nearby-sharing service.
//!
//! Load: start a service (own TLS identity, listener on all interfaces, default features so no
//! multicast or Bonjour), list 50 devices through `peer_add_resolved` (documentation-range addresses
//! 192.0.2.x that never answer: the service refuses loopback and own-interface peers, so a real peer
//! cannot be listed from the same machine), wait 5 s, then idle for the idle window. The idle window
//! includes the 20-second liveness sweep: it finds all 50 devices stale and probes each from its own
//! thread (2 s connect timeout), then counts a miss. The figures are the whole process's; the acceptor
//! blocks in `accept` and the liveness thread sleeps its 20-second period in one timed wait.
//!
//! No file is transferred here: the receive and send paths are exercised by `tests/localsend.rs`.
//!
//! Run: `rightkit cargo bench -p rightkit-send --bench budget`
//! Optional: `-- <devices> <idle_seconds>` (defaults 50 and 30).

use rightkit_process::{BudgetLabels, observe_idle, process_usage};
use rightkit_send::{Config, Event, Identity, Policy, Service, proto};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn config(alias: &str, save: &Path, trust: &Path) -> Config {
    Config {
        product_name: "Bench".into(),
        alias: alias.into(),
        port: 0,
        device_type: "desktop".into(),
        device_model: "Bench".into(),
        save_dir: save.to_path_buf(),
        trust_path: trust.to_path_buf(),
        user_agent: "Bench/1".into(),
        quarantine_agent: "Bench".into(),
        temp_suffix: ".bench-part".into(),
        log_prefix: "bench sharing:".into(),
        policy: Policy::default(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let devices: usize = args.first().and_then(|v| v.parse().ok()).unwrap_or(50);
    let idle_s: u64 = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);

    let root: PathBuf =
        std::env::temp_dir().join(format!("rightkit-send-budget-{}", proto::random_hex(4)));
    std::fs::create_dir_all(&root).unwrap();
    let before = process_usage().expect("process usage");
    let id = Identity::generate("Bench").unwrap();
    let service = Service::start(
        config("Bench", &root.join("save"), &root.join("trust.json")),
        id,
        Arc::new(|_: Event| {}),
    )
    .expect("service");
    for i in 0..devices {
        let fake = format!("{:064x}", i + 1);
        let ip: std::net::IpAddr = format!("192.0.2.{}", (i % 250) + 1).parse().unwrap();
        service
            .peer_add_resolved(&fake, &format!("Device {i}"), ip, 53317, &[])
            .unwrap();
    }
    std::thread::sleep(Duration::from_secs(5));
    let warm = process_usage().expect("process usage");
    println!(
        "warm-up: service on port {}, devices listed {}, resident {:.1} MiB (before start {:.1} MiB)",
        service.bound_port(),
        service.status().devices,
        warm.resident_bytes as f64 / 1048576.0,
        before.resident_bytes as f64 / 1048576.0,
    );
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    let end = service.status();
    println!("RESULT devices_after_idle={} devices", end.devices);
    print!("{}", idle.result_lines(&warm, &BudgetLabels { krate: "rightkit-send", version: env!("CARGO_PKG_VERSION"), scale_unit: "devices", scale: devices as u64, what: "devices listed on a running service (documentation-range addresses that never answer), service idle through one liveness sweep", scope: "synthetic" }));
    service.stop();
    let _ = std::fs::remove_dir_all(&root);
}
