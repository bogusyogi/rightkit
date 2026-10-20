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
    let nums: Vec<u64> = args.iter().filter_map(|v| v.parse().ok()).collect();
    let conns = nums.first().copied().unwrap_or(64) as usize;
    let requests = nums.get(1).copied().unwrap_or(20_000) as usize;
    let idle_s = nums.get(2).copied().unwrap_or(30);
    assert!(conns > 0 && requests > 0 && idle_s > 0);
    if args.iter().any(|arg| arg == "--loopback") {
        #[cfg(feature = "remote-loopback")]
        return loopback_budget(conns, requests, idle_s);
        #[cfg(not(feature = "remote-loopback"))]
        panic!("--loopback requires remote-loopback");
    }

    let tmp = tempfile::tempdir_in(
        std::env::var_os("RIGHTKIT_RUNTIME_SCRATCH_ROOT")
            .expect("RIGHTKIT_RUNTIME_SCRATCH_ROOT must name isolated scratch storage"),
    )
    .expect("isolated suite root");
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
    for client in &clients {
        assert_eq!(
            client.call("echo", json!({"warmup": true})).unwrap(),
            json!({"warmup": true})
        );
    }
    let started = Instant::now();
    let threads = 8usize.min(conns.max(1));
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let mine: Vec<Arc<Client>> = clients.iter().skip(t).step_by(threads).cloned().collect();
            let share = requests / threads + usize::from(t < requests % threads);
            std::thread::spawn(move || {
                for i in 0..share {
                    let c = &mine[i % mine.len()];
                    assert_eq!(
                        c.call("echo", json!({"n": i})).expect("echo"),
                        json!({"n": i})
                    );
                }
                share
            })
        })
        .collect();
    let sent: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(sent, requests);
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
    assert_eq!(
        end.connections as usize, conns,
        "idle workload lost held connections"
    );
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

#[cfg(feature = "remote-loopback")]
fn loopback_budget(conns: usize, requests: usize, idle_s: u64) {
    use rightkit_daemon::loopback::{
        Authenticated, Limits, Loopback, LoopbackApp, LoopbackConfig, Request, Response,
    };
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Mutex;

    struct App;
    impl LoopbackApp for App {
        fn handle(&self, request: &Request, _auth: &Authenticated) -> Response {
            assert_eq!(request.path, "/budget/echo");
            let body: Value = serde_json::from_slice(&request.body).expect("request JSON");
            Response::json(200, &body)
        }
    }
    fn echo(stream: &mut TcpStream, port: u16, token: &str, payload: &Value) {
        let body = serde_json::to_vec(payload).unwrap();
        write!(stream, "POST /budget/echo HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
        stream.write_all(&body).unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            assert!(head.len() < 16_384, "response head bound");
            let mut byte = [0];
            stream.read_exact(&mut byte).expect("response head");
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
        let length: usize = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().unwrap())
            })
            .expect("bounded response body");
        assert!(length <= 4096);
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), *payload);
    }

    // Hold idle connections throughout measurement instead of measuring timeout cleanup.
    let limits = Limits {
        idle_timeout: Duration::from_secs(idle_s.saturating_add(120)),
        ..Limits::default()
    };
    let host = Loopback::start(
        LoopbackConfig::new()
            .limits(limits)
            .max_connections(conns + 8)
            // Retain production token-bucket path with enough declared burst for finite warmup/load.
            .rate_limit(Some((200.0, (requests + conns + 1) as f64))),
        Arc::new(App),
    )
    .expect("isolated loopback host");
    let port = host.port();
    let token = host.token().expose().to_owned();
    let clients: Vec<_> = (0..conns)
        .map(|_| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            echo(&mut stream, port, &token, &json!({"warmup": true}));
            Arc::new(Mutex::new(stream))
        })
        .collect();
    let threads = 8usize.min(conns);
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let mine: Vec<_> = clients.iter().skip(t).step_by(threads).cloned().collect();
            let token = token.clone();
            let share = requests / threads + usize::from(t < requests % threads);
            std::thread::spawn(move || {
                for i in 0..share {
                    echo(
                        &mut mine[i % mine.len()].lock().unwrap(),
                        port,
                        &token,
                        &json!({"n": i}),
                    );
                }
                share
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .sum::<usize>(),
        requests
    );
    assert_eq!(host.connections(), conns);
    let warm = process_usage().expect("process usage");
    let idle = observe_idle(Duration::from_secs(idle_s)).expect("idle window");
    assert_eq!(
        host.connections(),
        conns,
        "idle workload lost held TCP connections"
    );
    println!("RESULT open_connections={} connections", host.connections());
    print!(
        "{}",
        idle.result_lines(
            &warm,
            &BudgetLabels {
                krate: "rightkit-daemon",
                version: env!("CARGO_PKG_VERSION"),
                scale_unit: "connections",
                scale: conns as u64,
                what: "loopback HTTP bearer echo requests then held TCP connections idle",
                scope: "synthetic",
            }
        )
    );
    drop(clients);
    host.shutdown();
    assert!(host.wait_stopped(Duration::from_secs(10)));
}
