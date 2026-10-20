//! Test resident host for the client lifecycle tests:
//! `svc_resident <suite-root> <service> <role> <rank> [ignore-shutdown]`.
//!
//! Hosts `service` for the app label `shell`, answers `echo` and `whoami`, and ends itself when
//! it is asked to through `shutdown`, unless `ignore-shutdown` is given (a host that does not
//! honour the polite request, so the lifecycle has to kill it).
use rightkit_daemon::*;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct Resident {
    role: String,
    ignore_shutdown: bool,
    stop: Arc<AtomicBool>,
}

impl Handler for Resident {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        match method {
            "echo" => Ok(params.clone()),
            "whoami" => Ok(json!({"role": self.role, "pid": std::process::id()})),
            "shutdown" => {
                if !self.ignore_shutdown {
                    self.stop.store(true, Ordering::SeqCst);
                }
                Ok(json!({"stopping": !self.ignore_shutdown}))
            }
            _ => Err(ServiceError::new("unknown_method", method)),
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 4 {
        eprintln!("usage: svc_resident <suite-root> <service> <role> <rank> [ignore-shutdown]");
        std::process::exit(2);
    }
    let suite = Suite::at(&a[0]).expect("suite");
    let allow = Allowlist::new().allow("shell", ["echo", "whoami", "shutdown"]);
    let mut cfg = ServiceConfig::new(&a[1], &a[2], a[3].parse().unwrap(), "test", allow);
    cfg.takeover_timeout = Duration::from_secs(20);
    cfg.handoff_grace = Duration::from_secs(5);
    let stop = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(Resident {
        role: a[2].clone(),
        ignore_shutdown: a.get(4).map(String::as_str) == Some("ignore-shutdown"),
        stop: stop.clone(),
    });
    match host(&suite, cfg, handler) {
        Ok(hosted) => {
            while !stop.load(Ordering::SeqCst) {
                if hosted.wait_released(Duration::from_millis(20)) {
                    break;
                }
            }
            hosted.shutdown();
        }
        Err(HostError::Held(_)) => std::process::exit(3),
        Err(_) => std::process::exit(4),
    }
}
