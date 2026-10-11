//! Single-instance arbitration over `rightkit-service` (no extra plugin, no lock file).
//!
//! The first process hosts the `<service-id>-shell` service (see [`service_name`] for how an app id
//! becomes `<service-id>`) on the suite's current-user-only socket /
//! named pipe. A later process finds a live record, forwards its argv and working directory with
//! `second-instance`, and exits. Because liveness is a probed endpoint plus a live pid, a crashed
//! primary never blocks the next launch (a stale record is discarded).
//!
//! Donors: all three apps used `tauri-plugin-single-instance`; this keeps its contract
//! (callback with argv + cwd) and adds suite isolation for tests.

use std::sync::Arc;

use rightkit_service::{
    host, valid_service_name, Allowlist, CallContext, Client, Emit, Handler, HostError, Hosted,
    ServiceConfig, ServiceError, Suite, MAX_SERVICE_NAME_LEN,
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
    /// The app id cannot be turned into a service name (see [`service_name`]). This is a bug in the
    /// caller, not an environment problem: arbitration did not run, so single-instance protection
    /// would be OFF if this were ignored. Never start as a second primary on it; fix the id.
    InvalidAppId(InvalidAppId),
}

/// Why an app id cannot name a single-instance service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidAppIdReason {
    /// The id is empty.
    Empty,
    /// The derived service name exceeds [`MAX_SERVICE_NAME_LEN`] bytes.
    TooLong { name_len: usize, max: usize },
    /// `rightkit-service` refused the derived name for a reason this crate did not predict.
    Rejected(String),
}

/// An app id that cannot be turned into a valid `rightkit-service` name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidAppId {
    /// The id as the caller passed it.
    pub app_id: String,
    pub reason: InvalidAppIdReason,
}

impl std::fmt::Display for InvalidAppId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.reason {
            InvalidAppIdReason::Empty => {
                write!(
                    f,
                    "app id {:?} is empty; it cannot name a service",
                    self.app_id
                )
            }
            InvalidAppIdReason::TooLong { name_len, max } => write!(
                f,
                "app id {:?} gives a {name_len}-byte service name; the limit is {max}",
                self.app_id
            ),
            InvalidAppIdReason::Rejected(name) => write!(
                f,
                "app id {:?} gives service name {name:?}, which rightkit-service refused",
                self.app_id
            ),
        }
    }
}

impl std::error::Error for InvalidAppId {}

/// Normalise an app id for use inside a service name: ASCII-lowercase it, then replace every
/// character outside `[a-z0-9-]` with `-` (one `-` per character; nothing is collapsed or trimmed).
/// `"app.mailright"` becomes `"app-mailright"`, `"Mail_Right"` becomes `"mail-right"`. An id that is
/// already valid is returned unchanged. Distinct ids can normalise to the same string
/// (`"a.b"` and `"a-b"`), which makes them one app for single-instance purposes.
pub(crate) fn normalize_app_id(app_id: &str) -> String {
    app_id
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' => c,
            _ => '-',
        })
        .collect()
}

/// `<normalised app id><suffix>`, validated against `rightkit-service`'s rule.
pub(crate) fn derive_service_name(app_id: &str, suffix: &str) -> Result<String, InvalidAppId> {
    let name = format!("{}{suffix}", normalize_app_id(app_id));
    let fail = |reason| InvalidAppId {
        app_id: app_id.to_owned(),
        reason,
    };
    if normalize_app_id(app_id).is_empty() {
        return Err(fail(InvalidAppIdReason::Empty));
    }
    if name.len() > MAX_SERVICE_NAME_LEN {
        return Err(fail(InvalidAppIdReason::TooLong {
            name_len: name.len(),
            max: MAX_SERVICE_NAME_LEN,
        }));
    }
    debug_assert!(valid_service_name(&name), "normalisation produced {name:?}");
    Ok(name)
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

/// The service name arbitration hosts for `app_id`: the normalised id plus `-shell`,
/// so an id that is already `[a-z0-9-]` gives exactly `<id>-shell` (running instances keep being
/// found across upgrades) and `"app.mailright"` gives `"app-mailright-shell"`. `rightkit-service`
/// allows only 1 to 48 bytes of `[a-z0-9-]`, so an empty id or one longer than 42 bytes is refused.
pub fn try_service_name(app_id: &str) -> Result<String, InvalidAppId> {
    derive_service_name(app_id, "-shell")
}

/// [`try_service_name`] without the check: the normalised name even when it is empty or too long,
/// which `arbitrate` would refuse. Prefer [`try_service_name`].
pub fn service_name(app_id: &str) -> String {
    format!("{}-shell", normalize_app_id(app_id))
}

/// Arbitrate single-instance for `app_id`.
///
/// Naming rule: the service is named `<app id>-shell` after the id is ASCII-lowercased and every
/// character outside `[a-z0-9-]` becomes `-` (so `"app.mailright"` and `"app-mailright"` are the
/// same app). An empty id, or one whose service name would exceed 48 bytes (id longer than 42),
/// returns [`Arbitration::InvalidAppId`], never [`Arbitration::Unavailable`]: a naming mistake is
/// not an environment failure and must not be treated as "arbitration is off". The caller must not
/// continue as primary on `InvalidAppId` or `Unavailable`.
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
    // Name first: a bad id must surface even when the suite root also cannot be resolved.
    if let Err(e) = try_service_name(app_id) {
        return invalid_app_id(e);
    }
    let suite = match Suite::resolve() {
        Ok(s) => s,
        Err(e) => return Arbitration::Unavailable(e.to_string()),
    };
    arbitrate_in_suite_with(&suite, app_id, version, legacy_identifier, on_second)
}

fn invalid_app_id(e: InvalidAppId) -> Arbitration {
    eprintln!("rightkit-shell: single-instance is NOT active, invalid app id: {e}");
    Arbitration::InvalidAppId(e)
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
    let service = match try_service_name(app_id) {
        Ok(name) => name,
        Err(e) => return invalid_app_id(e),
    };
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
    // Caller identity on both ends is the normalised id, so equivalent spellings agree.
    let client_id = normalize_app_id(app_id);
    let cfg = ServiceConfig::new(
        &service,
        &format!("shell:{app_id}"),
        100,
        version,
        Allowlist::new().allow(&client_id, [METHOD]),
    );
    match host(suite, cfg, Arc::new(Forward { on_second })) {
        Ok(hosted) => Arbitration::Primary(hosted),
        Err(HostError::Held(_)) => {
            // The incumbent may be mid-startup: retry the connect only. Once a call has reached it,
            // the launch is delivered, so a failed call is never repeated.
            let mut connected = None;
            for _ in 0..20 {
                match Client::connect(suite, &service, &client_id) {
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
        Err(HostError::InvalidName(name)) => invalid_app_id(InvalidAppId {
            app_id: app_id.to_owned(),
            reason: InvalidAppIdReason::Rejected(name),
        }),
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

    fn kind(a: &Arbitration) -> &'static str {
        match a {
            Arbitration::Primary(_) => "primary",
            Arbitration::Secondary => "secondary",
            Arbitration::Unavailable(_) => "unavailable",
            Arbitration::InvalidAppId(_) => "invalid-app-id",
        }
    }

    #[test]
    fn valid_ids_keep_their_exact_service_name() {
        assert_eq!(try_service_name("mailright").unwrap(), "mailright-shell");
        assert_eq!(
            try_service_name("view-right2").unwrap(),
            "view-right2-shell"
        );
        assert_eq!(service_name("mailright"), "mailright-shell");
        assert_eq!(normalize_app_id("test-shell"), "test-shell");
    }

    #[test]
    fn dotted_uppercase_and_underscore_ids_normalise() {
        assert_eq!(
            try_service_name("app.mailright").unwrap(),
            "app-mailright-shell"
        );
        assert_eq!(try_service_name("Mail_Right").unwrap(), "mail-right-shell");
        assert_eq!(try_service_name("a..b").unwrap(), "a--b-shell");
        assert_eq!(try_service_name("é").unwrap(), "--shell");
        assert!(valid_service_name(
            &try_service_name("App.Mail_Right").unwrap()
        ));
    }

    #[test]
    fn unnameable_ids_are_typed_errors_not_unavailable() {
        let empty = try_service_name("").unwrap_err();
        assert_eq!(empty.reason, InvalidAppIdReason::Empty);
        let long = "a".repeat(43);
        let err = try_service_name(&long).unwrap_err();
        assert_eq!(
            err.reason,
            InvalidAppIdReason::TooLong {
                name_len: 49,
                max: 48
            }
        );
        assert!(try_service_name(&"a".repeat(42)).is_ok());
        let suite = Suite::per_run_temp("shell-instance-invalid").unwrap();
        assert_eq!(
            kind(&arbitrate_in_suite(&suite, "", "1.0.0", Arc::new(|_| {}))),
            "invalid-app-id"
        );
        assert_eq!(
            kind(&arbitrate_in_suite(
                &suite,
                &long,
                "1.0.0",
                Arc::new(|_| {})
            )),
            "invalid-app-id"
        );
        assert_eq!(
            kind(&arbitrate("", "1.0.0", Arc::new(|_| {}))),
            "invalid-app-id"
        );
    }

    #[test]
    fn dotted_id_second_launch_sees_the_primary() {
        let suite = Suite::per_run_temp("shell-instance-dotted").unwrap();
        let (tx, rx) = mpsc::channel();
        let primary = arbitrate_in_suite(
            &suite,
            "app.mailright",
            "1.0.0",
            Arc::new(move |second| {
                tx.send(second).unwrap();
            }),
        );
        assert_eq!(kind(&primary), "primary");
        // The secondary asks with a differently spelled but equivalent id.
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "instance::tests::dotted_secondary_process",
                "--nocapture",
            ])
            .env("RK_SHELL_INSTANCE_DOTTED_ROOT", suite.root())
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(primary);
    }

    #[test]
    fn dotted_secondary_process() {
        let Some(root) = std::env::var_os("RK_SHELL_INSTANCE_DOTTED_ROOT") else {
            return;
        };
        let suite = Suite::at(std::path::PathBuf::from(root)).unwrap();
        let outcome = arbitrate_in_suite(
            &suite,
            "App.MailRight",
            "1.0.0",
            Arc::new(|_| panic!("secondary callback must not run")),
        );
        assert_eq!(kind(&outcome), "secondary");
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
