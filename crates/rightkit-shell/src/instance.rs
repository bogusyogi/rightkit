//! Single-instance arbitration over `rightkit-service` (no extra plugin, no lock file).
//!
//! The first process hosts the `<app-id>-shell` service on the suite's current-user-only socket /
//! named pipe. A later process finds a live record, forwards its argv and working directory with
//! `second-instance`, and exits. Because liveness is a probed endpoint plus a live pid, a crashed
//! primary never blocks the next launch (a stale record is discarded).
//!
//! Donors: all three apps used `tauri-plugin-single-instance`; this keeps its contract
//! (callback with argv + cwd) and adds suite isolation for tests.

use std::sync::Arc;

use rightkit_service::{
    host, Allowlist, CallContext, Client, Emit, Handler, HostError, Hosted, ServiceConfig,
    ServiceError, Suite,
};
use serde_json::{json, Value};

pub const METHOD: &str = "second-instance";

#[derive(Debug, Clone)]
pub struct SecondInstance {
    pub argv: Vec<String>,
    pub cwd: String,
}

pub enum Arbitration {
    /// This process is the instance; keep the guard alive for the process lifetime.
    Primary(Hosted),
    /// Another live instance exists and was told about this launch.
    Secondary,
    /// No suite root could be resolved (fail closed on discovery, proceed without arbitration).
    Unavailable(String),
}

struct Forward {
    on_second: Arc<dyn Fn(SecondInstance) + Send + Sync>,
}

impl Handler for Forward {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        if method != METHOD {
            return Err(ServiceError::new("unknown_method", method.to_string()));
        }
        let argv = params["argv"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let cwd = params["cwd"].as_str().unwrap_or_default().to_string();
        (self.on_second)(SecondInstance { argv, cwd });
        Ok(json!({"ok": true}))
    }
}

pub fn service_name(app_id: &str) -> String {
    format!("{app_id}-shell")
}

pub fn arbitrate(
    app_id: &str,
    version: &str,
    on_second: Arc<dyn Fn(SecondInstance) + Send + Sync>,
) -> Arbitration {
    let suite = match Suite::resolve() {
        Ok(s) => s,
        Err(e) => return Arbitration::Unavailable(e.to_string()),
    };
    let service = service_name(app_id);
    let cfg = ServiceConfig::new(
        &service,
        &format!("shell:{app_id}"),
        100,
        version,
        Allowlist::new().allow(app_id, [METHOD]),
    );
    match host(&suite, cfg, Arc::new(Forward { on_second })) {
        Ok(hosted) => Arbitration::Primary(hosted),
        Err(HostError::Held(_)) => {
            let argv: Vec<String> = std::env::args().collect();
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            // The incumbent may be mid-startup: retry briefly before giving up.
            for _ in 0..20 {
                match Client::connect(&suite, &service, app_id)
                    .and_then(|c| c.call(METHOD, json!({"argv": argv, "cwd": cwd})))
                {
                    Ok(_) => return Arbitration::Secondary,
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
            Arbitration::Unavailable("live instance did not accept the forwarded launch".into())
        }
        Err(e) => Arbitration::Unavailable(e.to_string()),
    }
}
