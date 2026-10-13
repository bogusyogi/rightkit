//! Single-instance race racer: `svc_race <suite-root> <service> <go-file> <id> <marker-dir>`.
//! Waits for the go file, then calls `host` at equal rank. The winner prints `OWNER`, records
//! every forwarded `echo` payload in `<marker-dir>/received`, and serves until `<marker-dir>/stop`
//! exists. A loser gets `Held`, forwards its `<id>` payload to the winner, prints `FORWARDED`.
use rightkit_daemon::*;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Owner {
    marker: PathBuf,
}

impl Handler for Owner {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => {
                let id = params["id"].as_str().unwrap_or_default().to_string();
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.marker.join("received"))
                    .map_err(|e| ServiceError::new("io", e.to_string()))?;
                // One write call per line: concurrent connections append to the same file.
                f.write_all(format!("{id}\n").as_bytes())
                    .map_err(|e| ServiceError::new("io", e.to_string()))?;
                Ok(json!({"id": id}))
            }
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let suite = Suite::at(&a[0]).expect("suite");
    let service = a[1].clone();
    let go = PathBuf::from(&a[2]);
    let id = a[3].clone();
    let marker = PathBuf::from(&a[4]);

    let wait_go = Instant::now();
    while !go.exists() && wait_go.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(2));
    }
    let allow = Allowlist::new().allow("alpha", ["echo"]);
    let mut cfg = ServiceConfig::new(&service, "racer", 2, "test", allow);
    cfg.takeover_timeout = Duration::from_secs(20);
    match host(
        &suite,
        cfg,
        Arc::new(Owner {
            marker: marker.clone(),
        }),
    ) {
        Ok(hosted) => {
            println!("OWNER {}", hosted.record().pid);
            let _ = std::io::stdout().flush();
            let stop = marker.join("stop");
            let until = Instant::now() + Duration::from_secs(60);
            while !stop.exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(hosted);
        }
        Err(HostError::Held(_)) => {
            let mut last = String::from("no attempt");
            for _ in 0..50 {
                match Client::connect(&suite, &service, "alpha")
                    .and_then(|c| c.call("echo", json!({"id": id})))
                {
                    Ok(_) => {
                        println!("FORWARDED {id}");
                        let _ = std::io::stdout().flush();
                        return;
                    }
                    Err(e) => {
                        last = e.to_string();
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
            println!("ERR forward {last}");
            std::process::exit(1);
        }
        Err(e) => {
            println!("ERR {e}");
            std::process::exit(4);
        }
    }
}
