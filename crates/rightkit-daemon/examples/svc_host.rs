//! Test host: `svc_host <suite-root> <service> <role> <rank> <marker-dir>`, or
//! `svc_host --resolve` to print the resolved suite root (fail-closed check).
use rightkit_daemon::*;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct H {
    role: String,
    marker: std::path::PathBuf,
}

impl Handler for H {
    fn call(
        &self,
        cx: &CallContext,
        method: &str,
        params: &Value,
        emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => Ok(params.clone()),
            "whoami" => Ok(json!({"role": self.role, "pid": std::process::id(), "app": cx.app()})),
            "slow" => {
                let total = Duration::from_millis(params["ms"].as_u64().unwrap_or(500));
                let t = Instant::now();
                let mut step = 0;
                while t.elapsed() < total {
                    std::thread::sleep(Duration::from_millis(20));
                    step += 1;
                    emit(json!({"step": step}));
                    if cx.handoff_requested() {
                        return Err(ServiceError::handoff());
                    }
                    if cx.peer_closed() {
                        let _ = std::fs::write(self.marker.join("cancelled"), self.role.as_bytes());
                        return Err(ServiceError::new("cancelled", "client went away"));
                    }
                }
                Ok(json!({"role": self.role, "steps": step}))
            }
            "hang" => {
                // Never replies; records each invocation so tests can prove no replay.
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.marker.join("hang_calls"))
                    .map_err(|e| ServiceError::new("io", e.to_string()))?;
                let _ = f.write_all(b"x");
                let t = Instant::now();
                while t.elapsed() < Duration::from_secs(60) {
                    if cx.peer_closed() {
                        let _ = std::fs::write(self.marker.join("hang_aborted"), b"1");
                        return Err(ServiceError::new("cancelled", "client went away"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(json!({}))
            }
            "secret" => Ok(json!({"secret": true})),
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) == Some("--resolve") {
        match Suite::resolve() {
            Ok(s) => println!("ROOT {}", s.root().display()),
            Err(e) => {
                println!("ERR {e}");
                std::process::exit(5);
            }
        }
        return;
    }
    let suite = Suite::at(&a[0]).expect("suite");
    let allow = Allowlist::new()
        .allow("alpha", ["echo", "slow", "whoami", "hang"])
        .allow("beta", ["echo"]);
    let mut cfg = ServiceConfig::new(&a[1], &a[2], a[3].parse().unwrap(), "test", allow);
    cfg.takeover_timeout = Duration::from_secs(20);
    cfg.handoff_grace = Duration::from_secs(5);
    let h = Arc::new(H {
        role: a[2].clone(),
        marker: a[4].clone().into(),
    });
    match host(&suite, cfg, h) {
        Ok(hosted) => {
            println!("READY {}", hosted.record().pid);
            hosted.wait_released(Duration::from_secs(120));
            println!("RELEASED");
        }
        Err(HostError::Held(r)) => {
            println!("HELD {}", r.role);
            std::process::exit(3);
        }
        Err(e) => {
            println!("ERR {e}");
            std::process::exit(4);
        }
    }
}
