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
    arbitrate_with_legacy(app_id, version, None, on_second)
}

/// Arbitration that also probes for a live `tauri-plugin-single-instance` holder under
/// `legacy_identifier` (the Tauri identifier) first. A live legacy holder receives this launch and
/// the result is [`Arbitration::Secondary`]; an unreachable one is [`Arbitration::Unavailable`].
pub fn arbitrate_with_legacy(
    app_id: &str,
    version: &str,
    legacy_identifier: Option<&str>,
    on_second: Arc<dyn Fn(SecondInstance) + Send + Sync>,
) -> Arbitration {
    let suite = match Suite::resolve() {
        Ok(s) => s,
        Err(e) => return Arbitration::Unavailable(e.to_string()),
    };
    arbitrate_in_suite_with(&suite, app_id, version, legacy_identifier, on_second)
}

#[cfg(test)]
fn arbitrate_in_suite(
    suite: &Suite,
    app_id: &str,
    version: &str,
    on_second: Arc<dyn Fn(SecondInstance) + Send + Sync>,
) -> Arbitration {
    arbitrate_in_suite_with(suite, app_id, version, None, on_second)
}

fn arbitrate_in_suite_with(
    suite: &Suite,
    app_id: &str,
    version: &str,
    legacy_identifier: Option<&str>,
    on_second: Arc<dyn Fn(SecondInstance) + Send + Sync>,
) -> Arbitration {
    let argv: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(identifier) = legacy_identifier {
        match crate::legacy_instance::forward(identifier, &argv, &cwd) {
            Ok(Some(())) => return Arbitration::Secondary,
            Ok(None) => {}
            Err(e) => {
                return Arbitration::Unavailable(format!(
                    "a legacy single-instance holder refused this launch: {e}"
                ))
            }
        }
    }
    let service = service_name(app_id);
    let cfg = ServiceConfig::new(
        &service,
        &format!("shell:{app_id}"),
        100,
        version,
        Allowlist::new().allow(app_id, [METHOD]),
    );
    match host(suite, cfg, Arc::new(Forward { on_second })) {
        Ok(hosted) => Arbitration::Primary(hosted),
        Err(HostError::Held(_)) => {
            // The incumbent may be mid-startup: retry the connect only. Once a call has reached it,
            // the launch is delivered, so a failed call is never repeated.
            let mut connected = None;
            for _ in 0..20 {
                match Client::connect(suite, &service, app_id) {
                    Ok(client) => {
                        connected = Some(client);
                        break;
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
            let Some(client) = connected else {
                return Arbitration::Unavailable(
                    "live instance did not accept a connection for the forwarded launch".into(),
                );
            };
            match client.call(METHOD, json!({"argv": argv, "cwd": cwd})) {
                Ok(_) => Arbitration::Secondary,
                Err(e) => Arbitration::Unavailable(format!(
                    "live instance did not accept the forwarded launch: {e}"
                )),
            }
        }
        Err(e) => Arbitration::Unavailable(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn arbitration_forwards_secondary_launch_and_releases_primary_guard() {
        let suite = Suite::per_run_temp("shell-instance").unwrap();
        let (tx, rx) = mpsc::channel();
        let primary = match arbitrate_in_suite(
            &suite,
            "test-shell",
            "1.0.0",
            Arc::new(move |second| {
                tx.send(second).unwrap();
            }),
        ) {
            Arbitration::Primary(hosted) => hosted,
            _ => panic!("first launch must host arbitration"),
        };
        // Arbitration checks process identity, so the secondary must be a separate process.
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance::tests::secondary_process",
                "--nocapture",
            ])
            .env("RK_SHELL_INSTANCE_TEST_ROOT", suite.root())
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(child.success());
        let forwarded = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(forwarded
            .argv
            .iter()
            .any(|arg| arg == "instance::tests::secondary_process"));
        assert_eq!(
            forwarded.cwd,
            std::env::current_dir().unwrap().to_string_lossy()
        );
        drop(primary);
        assert!(matches!(
            arbitrate_in_suite(&suite, "test-shell", "1.0.0", Arc::new(|_| {})),
            Arbitration::Primary(_)
        ));
    }

    #[test]
    fn secondary_process() {
        let Some(root) = std::env::var_os("RK_SHELL_INSTANCE_TEST_ROOT") else {
            return;
        };
        let suite = Suite::at(std::path::PathBuf::from(root)).unwrap();
        assert!(matches!(
            arbitrate_in_suite(
                &suite,
                "test-shell",
                "1.0.0",
                Arc::new(|_| panic!("secondary callback must not run"))
            ),
            Arbitration::Secondary
        ));
    }
}
