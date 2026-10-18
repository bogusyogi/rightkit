//! Standalone single instance with a typed message, usable without the shell plugin.
//!
//! [`SingleInstance::claim`] makes this process the app's primary, or forwards this launch's
//! [`InstanceMessage`] (argv, cwd, an optional action and a JSON argument map) to the live primary
//! and reports that it did. The outcome is typed and fail-closed:
//!
//! * [`ClaimOutcome::Primary`]: this process is the primary. Keep the guard for the process lifetime.
//! * [`ClaimOutcome::Forwarded`]: a live primary accepted the message. The caller must exit.
//! * [`ClaimOutcome::Failed`]: arbitration could not be decided. The caller must not continue as a
//!   second primary; it should report the reason and exit.
//!
//! Arbitration runs over `rightkit-daemon` (current-user socket or named pipe, a probed endpoint and
//! a live pid), so a crashed primary never blocks the next launch. The shell plugin's own
//! arbitration (`instance`, method `second-instance`) is separate; an app uses one or the other.
//!
//! Reference: Pulse hub's `win_bridge::claim_instance` (a named mutex that forwards `--section`).
//! That path fails open when the singleton cannot be secured; this one fails closed.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rightkit_daemon::{
    host, Allowlist, CallContext, Client, Emit, Handler, HostError, Hosted, ServiceConfig,
    ServiceError, Suite,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Service method a forwarded launch calls on the primary.
pub const MESSAGE_METHOD: &str = "instance-message";

/// Attempts to connect to a live primary that may still be starting.
const CONNECT_ATTEMPTS: u32 = 20;
const CONNECT_RETRY: Duration = Duration::from_millis(100);
/// How long hosting the claim may wait for an election.
const HOST_TIMEOUT: Duration = Duration::from_secs(5);

/// The payload one launch hands to the primary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InstanceMessage {
    /// The launch's argv (including the program path).
    pub argv: Vec<String>,
    /// The launch's working directory.
    pub cwd: String,
    /// What the primary should do, e.g. `"show-section"`.
    pub action: Option<String>,
    /// Named arguments for the action, e.g. `{"section": "x"}`.
    #[serde(default)]
    pub args: Map<String, Value>,
}

impl InstanceMessage {
    /// This process's argv and working directory, with no action.
    pub fn from_process() -> Self {
        Self {
            argv: std::env::args().collect(),
            cwd: std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            action: None,
            args: Map::new(),
        }
    }

    /// Set the action.
    pub fn with_action(mut self, action: &str) -> Self {
        self.action = Some(action.to_owned());
        self
    }

    /// Add one named argument.
    pub fn with_arg(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.args.insert(key.to_owned(), value.into());
        self
    }
}

/// Why arbitration could not be decided. Each variant carries the underlying message.
///
/// Every variant is fail-closed: [`SingleInstance::claim`] never turns one into a second primary on
/// its own. A user must still never be locked out of launching the app, so the app decides what
/// happens next, and the crate bounds how long each step can take so that decision is reached quickly:
///
/// * a live-primary forward retries the connect for at most 2 s (20 attempts, 100 ms apart) and sends
///   once;
/// * a legacy forward waits at most 5 s for the old holder to answer (Windows `SendMessageTimeoutW`
///   with abort-if-hung; macOS a 5 s write timeout, then 2 s for the holder to close the connection
///   it was written to, see [`ClaimOutcome::Forwarded`]);
/// * hosting the claim waits at most 5 s for an election;
/// * so one `claim` call returns within about 14 s in the worst case.
///
/// The crate enforces those bounds; the app enforces the rest (below). An app should show or log
/// the reason, and on a variant it cannot recover from, start anyway after at most that bound rather
/// than exit silently, unless a second copy would corrupt data, in which case it should tell the user
/// why it will not start and how to end the other copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimFailure {
    /// The suite root (the per-user service directory) could not be resolved. Nothing was hosted, so
    /// no other copy can have been told about this launch. Safe to log and continue unarbitrated.
    Discovery(String),
    /// The primary endpoint could not be hosted, for a reason other than a live primary. Usually a
    /// permissions or disk problem. Continue unarbitrated only if duplicates are harmless; otherwise
    /// report it.
    Host(String),
    /// A live primary exists, but it could not be reached or did not accept the message within the
    /// bound above (hung, or mid-shutdown). Do not start a second primary silently: tell the user
    /// the running copy is not responding, and offer to retry. A retry after the bound is safe.
    Forward(String),
    /// A live instance started by `tauri-plugin-single-instance` holds the claim and could not take
    /// this launch within the bound (a hung old build). The caller must not start a second primary
    /// silently; report that the older copy is not responding and how to close it.
    Legacy(String),
    /// The id cannot be turned into a service name (empty, or longer than 39 bytes). The id is
    /// normalised first (ASCII lowercase, every character outside `[a-z0-9-]` becomes `-`), so a
    /// dotted bundle identifier is fine. This is a caller bug: nothing was hosted and protection is
    /// off, so fix the id rather than continuing unarbitrated.
    InvalidId(String),
}

impl fmt::Display for ClaimFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(m) => write!(f, "single-instance discovery failed: {m}"),
            Self::Host(m) => write!(f, "single-instance host failed: {m}"),
            Self::Forward(m) => write!(f, "single-instance forward failed: {m}"),
            Self::Legacy(m) => write!(f, "single-instance legacy holder refused the launch: {m}"),
            Self::InvalidId(m) => write!(f, "single-instance id is not usable: {m}"),
        }
    }
}

impl std::error::Error for ClaimFailure {}

/// Every primary claim this process holds. Guards refer to an entry by token, so a guard does not
/// own the endpoint directly: [`destroy`] (or dropping the guard) releases it.
static CLAIMS: Mutex<Vec<(u64, Hosted)>> = Mutex::new(Vec::new());
static NEXT_CLAIM: AtomicU64 = AtomicU64::new(1);
/// The legacy claims held and serviced alongside a primary claim, by the primary's token.
static LEGACY_HOLDS: Mutex<Vec<(u64, crate::legacy_hold::LegacyHold)>> = Mutex::new(Vec::new());

fn legacy_holds() -> std::sync::MutexGuard<'static, Vec<(u64, crate::legacy_hold::LegacyHold)>> {
    LEGACY_HOLDS.lock().unwrap_or_else(|e| e.into_inner())
}

fn claims() -> std::sync::MutexGuard<'static, Vec<(u64, Hosted)>> {
    CLAIMS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keep a hosted endpoint alive for the process (or until [`destroy`]). Returns its token.
pub(crate) fn hold(hosted: Hosted) -> u64 {
    let token = NEXT_CLAIM.fetch_add(1, Ordering::Relaxed);
    claims().push((token, hosted));
    token
}

/// Release one claim by token. Unknown tokens are ignored.
pub(crate) fn release(token: u64) {
    let removed = {
        let mut held = claims();
        held.iter()
            .position(|(t, _)| *t == token)
            .map(|i| held.remove(i))
    };
    drop(removed);
    let legacy = {
        let mut held = legacy_holds();
        held.iter()
            .position(|(t, _)| *t == token)
            .map(|i| held.remove(i))
    };
    drop(legacy);
}

/// Release every claim this process holds.
pub(crate) fn release_all() {
    let released: Vec<(u64, Hosted)> = std::mem::take(&mut *claims());
    drop(released);
    let legacy = std::mem::take(&mut *legacy_holds());
    drop(legacy);
}

type DestroyHook = Box<dyn FnOnce() + Send>;

/// Releases registered with [`SingleInstance::on_destroy`], run once by [`SingleInstance::destroy`].
static DESTROY_HOOKS: Mutex<Vec<DestroyHook>> = Mutex::new(Vec::new());

fn run_destroy_hooks() {
    let hooks: Vec<DestroyHook> =
        std::mem::take(&mut *DESTROY_HOOKS.lock().unwrap_or_else(|e| e.into_inner()));
    for hook in hooks {
        // A panicking hook must not stop the remaining releases.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(hook));
    }
}

/// Every name `tauri-plugin-single-instance` 2.x derives from the Tauri identifier
/// (`tauri.conf.json` `identifier`). Read from plugin versions 2.4.2 and 2.4.3
/// (`src/platform_impl/{windows,macos,linux}.rs`), with the plugin's `semver` feature off. HeardRight
/// confirmed the names at its locked 2.4.5 (verified by the consumer; 2.4.5 source was not read here). The
/// derivation is pure, so it is the same on every host.
///
/// For HeardRight (`app.heardright.next`):
///
/// | Platform | Resource | Name |
/// |---|---|---|
/// | Windows | named mutex | `app.heardright.next-sim` |
/// | Windows | window class | `app.heardright.next-sic` |
/// | Windows | window title | `app.heardright.next-siw` |
/// | macOS | unix socket | `/tmp/app_heardright_next_si.sock` |
/// | Linux | D-Bus well-known name | `app.heardright.next.SingleInstance` |
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySingleInstanceNames {
    /// Windows: `<identifier>-sim`, verbatim. The mutex that arbitrates the claim.
    pub windows_mutex: String,
    /// Windows: `<identifier>-sic`, verbatim. The class of the hidden window that receives
    /// `WM_COPYDATA` (`dwData == 1542`).
    pub windows_window_class: String,
    /// Windows: `<identifier>-siw`, verbatim. The title of that window.
    pub windows_window_title: String,
    /// macOS: `/tmp/<identifier>_si.sock`, with every `.` and `-` in the identifier replaced by `_`.
    pub macos_socket: std::path::PathBuf,
    /// Linux: `<identifier>.SingleInstance`, verbatim. This crate does not probe it.
    pub linux_dbus_name: String,
}

impl LegacySingleInstanceNames {
    /// Derive the names for a Tauri `identifier`.
    pub fn for_identifier(identifier: &str) -> Self {
        Self {
            windows_mutex: format!("{identifier}-sim"),
            windows_window_class: format!("{identifier}-sic"),
            windows_window_title: format!("{identifier}-siw"),
            macos_socket: std::path::PathBuf::from(format!(
                "/tmp/{}_si.sock",
                crate::legacy_instance::socket_stem(identifier)
            )),
            linux_dbus_name: format!("{identifier}.SingleInstance"),
        }
    }
}

/// Holds one primary claim. Dropping it releases the claim, the same as [`destroy`] does for all.
pub struct SingleInstanceGuard {
    token: u64,
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        release(self.token);
    }
}

/// Options for [`SingleInstance::claim_with_options`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimOptions {
    /// The Tauri identifier (`tauri.conf.json`) the legacy `tauri-plugin-single-instance` keyed its
    /// endpoint by. When set, a live legacy holder is probed first and receives this launch, and an
    /// unreachable one is a [`ClaimFailure::Legacy`]. `None` skips the probe.
    pub legacy_identifier: Option<String>,
    /// With `legacy_identifier`: also HOLD and SERVICE the legacy claim while this process is the
    /// primary, so an older build (on the plugin) launched later finds a live holder, forwards its
    /// launch and exits, instead of starting a second instance. Default `false`.
    ///
    /// The launch the old build forwards (its cwd and argv) reaches the same `on_message` callback,
    /// on a dedicated thread, as an [`InstanceMessage`] with no action. The claim is released by
    /// [`SingleInstance::destroy`] or by dropping the guard: the service thread is stopped and
    /// joined, and the mutex, window or socket is gone when that returns.
    ///
    /// Order, which makes it atomic with becoming primary: the primary endpoint is hosted first; only
    /// the process that won it takes the legacy claim, so two new builds cannot both hold it. If a
    /// live old build already holds the legacy claim, the primary is released and this launch is
    /// forwarded to it, as without this option. A second launch of the new build never probes the
    /// legacy claim; it forwards over the typed channel.
    ///
    /// Do not combine with `tauri_plugin_single_instance` registered in the same process: both would
    /// claim the same names. Windows and macOS only; on Linux (D-Bus) the hold is a no-op.
    pub hold_legacy: bool,
}

impl ClaimOptions {
    /// Probe, and forward to, a live legacy holder keyed by `identifier`.
    pub fn with_legacy(identifier: &str) -> Self {
        Self {
            legacy_identifier: Some(identifier.to_owned()),
            hold_legacy: false,
        }
    }

    /// Also hold and service the legacy claim (see [`ClaimOptions::hold_legacy`]).
    pub fn holding_legacy(mut self) -> Self {
        self.hold_legacy = true;
        self
    }
}

/// The typed result of [`SingleInstance::claim`].
#[must_use = "a Forwarded or Failed launch must exit; ignoring the outcome continues the launch"]
pub enum ClaimOutcome {
    /// This process is the primary. Keep the guard alive.
    Primary(SingleInstanceGuard),
    /// A live primary accepted this launch's message. Exit.
    ///
    /// For a legacy `tauri-plugin-single-instance` holder on macOS the plugin has no reply, so
    /// "accepted" means it read the launch to its end and closed the connection, or, when it stayed
    /// silent for 2 s, that its socket belongs to this app's own process. That silent case cannot be
    /// told from a slow callback, so it is still `Forwarded` but is logged loudly on stderr as
    /// "forwarded but NOT confirmed". A socket whose owning process has exited, or belongs to another
    /// program and never answers, is not a holder (a descriptor leaked into a sidecar can keep such a
    /// socket accepting after the app is gone): the claim carries on and this process can become the
    /// primary.
    Forwarded,
    /// Arbitration could not be decided. Do not continue as a primary.
    Failed(ClaimFailure),
}

impl ClaimOutcome {
    /// True only for [`ClaimOutcome::Primary`].
    pub fn is_primary(&self) -> bool {
        matches!(self, Self::Primary(_))
    }
}

/// Standalone single-instance arbitration.
pub struct SingleInstance;

impl SingleInstance {
    /// Release every single-instance claim this process holds, so a relaunched process (for an update,
    /// say) can become the primary before this one exits. Guards stay valid and own nothing afterwards.
    ///
    /// What it releases:
    ///
    /// * every claim made through this type (the rightkit-daemon endpoint), and
    /// * every release registered with [`SingleInstance::on_destroy`], each run exactly once.
    ///
    /// It is idempotent: a second call finds nothing left and does nothing. It does not touch any
    /// `tauri-plugin-single-instance` claim unless a hook for it was registered, because
    /// [`ClaimOptions::legacy_identifier`] only *probes* for a legacy holder; this type never creates
    /// a legacy claim, and the plugin's mutex and window (Windows), unix-socket listener (macOS) and
    /// D-Bus name (Linux) are owned by the plugin's own state inside the app. A process that still
    /// registers the plugin must release it too (see [`SingleInstance::on_destroy`]), or the
    /// relaunched process is refused as a second instance.
    pub fn destroy() {
        run_destroy_hooks();
        release_all();
    }

    /// Register a release that [`SingleInstance::destroy`] runs once, before it drops this type's
    /// own claims. This is how an app that still registers `tauri-plugin-single-instance` makes one
    /// `destroy()` call release both claims:
    ///
    /// ```text
    /// let handle = app.handle().clone();
    /// SingleInstance::on_destroy(move || tauri_plugin_single_instance::destroy(&handle));
    /// // relaunch-for-update:
    /// SingleInstance::destroy(); // plugin claim, then rightkit claim
    /// app.exit(0);
    /// ```
    ///
    /// Without the hook, call `tauri_plugin_single_instance::destroy(&app)` and
    /// `SingleInstance::destroy()` both, in either order, before spawning or exiting.
    ///
    /// The plugin's `destroy` closes its mutex handle (Windows) or removes its socket file (macOS),
    /// which frees the name for the relaunch. On Windows it also calls `DestroyWindow` for its hidden
    /// window; Win32 allows that only on the thread that created the window, so from another thread
    /// the window lingers until the process exits. The relaunched process is unaffected, because
    /// arbitration there is the mutex, and a new mutex is created.
    pub fn on_destroy(hook: impl FnOnce() + Send + 'static) {
        DESTROY_HOOKS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(hook));
    }

    /// Claim the primary for `id` (lowercase letters, digits and `-`). `on_message` receives each
    /// launch that is forwarded to this primary. The outgoing message is this process's argv and cwd.
    pub fn claim(
        id: &str,
        on_message: impl Fn(InstanceMessage) + Send + Sync + 'static,
    ) -> ClaimOutcome {
        Self::claim_with(id, InstanceMessage::from_process(), on_message)
    }

    /// Like [`SingleInstance::claim`], but the outgoing message (used when this launch is forwarded)
    /// is `message`, for example one carrying an action and arguments.
    pub fn claim_with(
        id: &str,
        message: InstanceMessage,
        on_message: impl Fn(InstanceMessage) + Send + Sync + 'static,
    ) -> ClaimOutcome {
        Self::claim_with_options(id, ClaimOptions::default(), message, on_message)
    }

    /// [`SingleInstance::claim_with`] with options, such as the legacy identifier to probe.
    pub fn claim_with_options(
        id: &str,
        options: ClaimOptions,
        message: InstanceMessage,
        on_message: impl Fn(InstanceMessage) + Send + Sync + 'static,
    ) -> ClaimOutcome {
        match Suite::resolve() {
            Ok(suite) => Self::claim_in_with(&suite, id, options, message, on_message),
            Err(e) => ClaimOutcome::Failed(ClaimFailure::Discovery(e.to_string())),
        }
    }

    /// Claim inside an explicit suite root. Tests use this; apps normally call [`SingleInstance::claim`].
    pub fn claim_in(
        suite: &Suite,
        id: &str,
        message: InstanceMessage,
        on_message: impl Fn(InstanceMessage) + Send + Sync + 'static,
    ) -> ClaimOutcome {
        Self::claim_in_with(suite, id, ClaimOptions::default(), message, on_message)
    }

    /// [`SingleInstance::claim_in`] with options.
    pub fn claim_in_with(
        suite: &Suite,
        id: &str,
        options: ClaimOptions,
        message: InstanceMessage,
        on_message: impl Fn(InstanceMessage) + Send + Sync + 'static,
    ) -> ClaimOutcome {
        let service = match crate::instance::derive_service_name(id, "-instance") {
            Ok(name) => name,
            Err(e) => {
                eprintln!("rightkit-tauri-shell: single-instance is NOT active, invalid id: {e}");
                return ClaimOutcome::Failed(ClaimFailure::InvalidId(e.to_string()));
            }
        };
        let client_id = crate::instance::normalize_app_id(id);
        let hold_identifier = options
            .legacy_identifier
            .clone()
            .filter(|_| options.hold_legacy);
        if hold_identifier.is_none() {
            if let Some(identifier) = options.legacy_identifier.as_deref() {
                let argv = message.argv.clone();
                match crate::legacy_instance::forward_checked(identifier, &argv, &message.cwd) {
                    Ok(Some(delivery)) => {
                        delivery.log_unconfirmed("SingleInstance::claim");
                        return ClaimOutcome::Forwarded;
                    }
                    Ok(None) => {}
                    Err(e) => return ClaimOutcome::Failed(ClaimFailure::Legacy(e)),
                }
            }
        }
        let mut cfg = ServiceConfig::new(
            &service,
            &format!("instance:{id}"),
            100,
            env!("CARGO_PKG_VERSION"),
            Allowlist::new().allow(&client_id, [MESSAGE_METHOD]),
        );
        // A claim must come back quickly (see `ClaimFailure`): bound the election.
        cfg.takeover_timeout = HOST_TIMEOUT;
        let on_message: crate::legacy_hold::OnMessage = Arc::new(on_message);
        let handler = Arc::new(Receive {
            on_message: on_message.clone(),
        });
        match host(suite, cfg, handler) {
            Ok(hosted) => {
                let Some(identifier) = hold_identifier else {
                    return ClaimOutcome::Primary(SingleInstanceGuard {
                        token: hold(hosted),
                    });
                };
                match crate::legacy_hold::acquire(
                    &identifier,
                    &message.argv,
                    &message.cwd,
                    on_message,
                ) {
                    Ok(crate::legacy_hold::Acquired::Held(legacy)) => {
                        let token = hold(hosted);
                        legacy_holds().push((token, legacy));
                        ClaimOutcome::Primary(SingleInstanceGuard { token })
                    }
                    Ok(crate::legacy_hold::Acquired::Forwarded) => {
                        drop(hosted);
                        ClaimOutcome::Forwarded
                    }
                    Err(e) => {
                        drop(hosted);
                        ClaimOutcome::Failed(ClaimFailure::Legacy(e))
                    }
                }
            }
            Err(HostError::Held(_)) => forward(suite, &client_id, &service, &message),
            Err(e) => ClaimOutcome::Failed(ClaimFailure::Host(e.to_string())),
        }
    }
}

/// Connect to the live primary (retrying while it starts), then deliver the message once. Only the
/// connect is retried: a call that failed after the primary received it is never repeated, so the
/// callback cannot run twice for one launch.
fn forward(suite: &Suite, id: &str, service: &str, message: &InstanceMessage) -> ClaimOutcome {
    let params = match serde_json::to_value(message) {
        Ok(params) => params,
        Err(e) => return ClaimOutcome::Failed(ClaimFailure::Forward(e.to_string())),
    };
    let mut last = String::from("live primary did not accept a connection");
    for _ in 0..CONNECT_ATTEMPTS {
        match Client::connect(suite, service, id) {
            Ok(client) => {
                return match client.call(MESSAGE_METHOD, params) {
                    Ok(_) => ClaimOutcome::Forwarded,
                    Err(e) => ClaimOutcome::Failed(ClaimFailure::Forward(e.to_string())),
                };
            }
            Err(e) => {
                last = e.to_string();
                std::thread::sleep(CONNECT_RETRY);
            }
        }
    }
    ClaimOutcome::Failed(ClaimFailure::Forward(last))
}

struct Receive {
    on_message: crate::legacy_hold::OnMessage,
}

impl Handler for Receive {
    fn call(
        &self,
        _cx: &CallContext,
        method: &str,
        params: &Value,
        _emit: &Emit,
    ) -> Result<Value, ServiceError> {
        if method != MESSAGE_METHOD {
            return Err(ServiceError::new("unknown_method", method.to_string()));
        }
        // A malformed payload (wrong types, args that are not an object) is refused, not guessed at.
        let message: InstanceMessage = serde_json::from_value(params.clone())
            .map_err(|e| ServiceError::new("invalid_message", e.to_string()))?;
        (self.on_message)(message);
        Ok(json!({"ok": true}))
    }
}
