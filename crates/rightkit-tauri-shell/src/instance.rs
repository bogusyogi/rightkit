//! Single-instance arbitration over `rightkit-daemon` (no extra plugin, no lock file).
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

use rightkit_daemon::{
    host, Allowlist, CallContext, Client, Emit, Handler, HostError, Hosted, ServiceConfig,
    ServiceError, Suite, MAX_SERVICE_NAME_LEN,
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
    /// `rightkit-daemon` refused the derived name for a reason this crate did not predict.
    Rejected(String),
}

/// An app id that cannot be turned into a valid `rightkit-daemon` name.
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
                "app id {:?} gives service name {name:?}, which rightkit-daemon refused",
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

/// `<normalised app id><suffix>`, validated against `rightkit-daemon`'s rule.
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
/// found across upgrades) and `"app.mailright"` gives `"app-mailright-shell"`. `rightkit-daemon`
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
/// the result is [`Arbitration::Secondary`]; an unreachable one is [`Arbitration::Unavailable`]. A
/// socket that only a leaked descriptor keeps open is not a live holder, and a macOS holder that
/// stays silent is logged on stderr as unconfirmed (see
/// [`ClaimOutcome::Forwarded`](crate::single_instance::ClaimOutcome::Forwarded)).
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
    eprintln!("rightkit-tauri-shell: single-instance is NOT active, invalid app id: {e}");
    Arbitration::InvalidAppId(e)
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
        match crate::legacy_instance::forward_checked(identifier, &argv, &cwd) {
            Ok(Some(delivery)) => {
                delivery.log_unconfirmed("instance::arbitrate_with_legacy");
                return Arbitration::Secondary;
            }
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
