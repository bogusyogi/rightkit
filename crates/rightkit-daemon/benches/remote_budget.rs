//! Native remote budget: real TLS listener, ephemeral enrolled device, signed job writes.
//! Run `rustright cargo test --locked --profile release-iterate -p rightkit-daemon
//! --features remote --bench remote_budget -- 10000 30`.
//! Producer supplies RIGHTKIT_RUNTIME_SCRATCH_ROOT plus per-run localhost certificate/key
//! paths in RIGHTKIT_DAEMON_BUDGET_TLS_CERT / RIGHTKIT_DAEMON_BUDGET_TLS_KEY. Missing or
//! invalid TLS material fails; no `testing` feature, committed key, trust bypass or remote host.

use rightkit_child_process::{observe_idle, process_usage, BudgetLabels};
use rightkit_daemon::remote::client::{Client, ClientConfig, DeviceKey};
use rightkit_daemon::remote::{
    load_tls, serve_blocking, Daemon, DaemonConfig, StopHandle, SystemClock,
};
use serde_json::json;
use std::net::TcpListener;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

struct Server {
    stop: StopHandle,
    thread: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(thread) = self.thread.take() {
            let outcome = thread.join();
            if !std::thread::panicking() {
                outcome
                    .expect("server thread")
                    .expect("graceful server shutdown");
            }
        }
    }
}

fn main() {
    let nums: Vec<u64> = std::env::args()
        .skip(1)
        .filter_map(|arg| arg.parse().ok())
        .collect();
    let n = nums.first().copied().unwrap_or(10_000);
    let idle_s = nums.get(1).copied().unwrap_or(30);
    assert!(n > 0 && idle_s > 0);
    let dir = tempfile::tempdir_in(
        std::env::var_os("RIGHTKIT_RUNTIME_SCRATCH_ROOT")
            .expect("RIGHTKIT_RUNTIME_SCRATCH_ROOT must name isolated scratch storage"),
    )
    .expect("isolated daemon state");
    let cert = std::fs::read(
        std::env::var_os("RIGHTKIT_DAEMON_BUDGET_TLS_CERT")
            .expect("producer must supply per-run localhost certificate"),
    )
    .expect("read per-run certificate");
    let key = std::fs::read(
        std::env::var_os("RIGHTKIT_DAEMON_BUDGET_TLS_KEY")
            .expect("producer must supply per-run TLS private-key path"),
    )
    .expect("read per-run private key");
    let tls = load_tls(&cert, &key).expect("matching per-run TLS certificate/key");
    drop(key);
    let daemon = Arc::new(
        Daemon::open(DaemonConfig::new(dir.path()), Arc::new(SystemClock), None)
            .expect("open production-default daemon"),
    );
    let listener = TcpListener::bind("127.0.0.1:0").expect("isolated ephemeral loopback listener");
    let addr = listener.local_addr().unwrap();
    assert!(addr.ip().is_loopback());
    let (ready_tx, ready_rx) = mpsc::channel();
    let stop = StopHandle::new();
    let server = Server {
        stop: stop.clone(),
        thread: Some({
            let daemon = daemon.clone();
            std::thread::spawn(move || serve_blocking(daemon, listener, tls, ready_tx, stop))
        }),
    };
    assert_eq!(
        ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("TLS listener ready"),
        addr
    );
    let client = Client::new(
        ClientConfig::new(addr)
            .server_name("localhost")
            .root_pem(&cert)
            .expect("pin per-run root")
            .timeout(Duration::from_secs(30)),
    )
    .expect("production signed client");
    let device_key = DeviceKey::from_pkcs8(&DeviceKey::generate_pkcs8().expect("ephemeral key"))
        .expect("ephemeral device key");
    let code = daemon
        .issue_enrolment_code(&["venture", "channel", "account"], 600)
        .expect("single-use enrolment code");
    let device = client
        .enrol(&code, Arc::new(device_key), Some("runtime-budget"))
        .expect("real signed TLS enrolment");
    let warmup = json!({"caption": "warmup", "status": "queued"});
    assert_eq!(
        device
            .update_job(
                "budget-warmup",
                warmup.as_object().unwrap(),
                0,
                "budget-warmup"
            )
            .expect("warmup over TLS")
            .revision,
        1
    );
    let before = daemon.metrics();
    let start = Instant::now();
    for i in 0..n {
        let patch = json!({"caption": format!("caption {i}"), "slot": i % 40, "status": "queued"});
        let reply = device
            .update_job(
                &format!("budget-job-{i:05}"),
                patch.as_object().unwrap(),
                0,
                &format!("budget-idem-{i:05}"),
            )
            .expect("signed TLS job submission");
        assert_eq!(reply.revision, 1);
        assert!(!reply.replayed);
    }
    assert_eq!(daemon.job_revision("budget-job-00000"), 1);
    let after = daemon.metrics();
    assert_eq!(after.jobs - before.jobs, n);
    println!(
        "submissions: n={n} wall_s={:.3} jobs={} requests={} state_saves={}",
        start.elapsed().as_secs_f64(),
        after.jobs,
        after.requests - before.requests,
        after.state_saves - before.state_saves
    );
    let warm = process_usage().expect("process usage");
    // Server, TLS runtime, device state & durable records remain alive throughout idle sampling.
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle listener window");
    assert_eq!(daemon.metrics().jobs, after.jobs);
    println!("RESULT jobs_retained={} jobs", daemon.metrics().jobs);
    print!(
        "{}",
        idle.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-daemon",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "submissions",
                scale: n,
                what: "signed TLS job submissions retained with production listener idle",
                scope: "synthetic",
            }
        )
    );
    drop(device);
    drop(client);
    drop(server);
    drop(daemon);
    drop(dir);
}
