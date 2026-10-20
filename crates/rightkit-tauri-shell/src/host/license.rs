//! The native licence commands and event (feature `license`): the shell owns them, apps do not.
//!
//! Five commands over `rightkit_license::LicenseClient` (`status`, `activate`, `refresh`,
//! `deactivate`, `request_trial`) and one event, [`EVENT_LICENSE_CHANGED`], are what
//! `@rightkit/license` 0.3.0 calls and listens to. An app builds its `LicenseClient` (public key,
//! host, device identity), registers it with
//! [`HostServicesBuilder::license_client`](super::HostServicesBuilder::license_client) and lists
//! nothing else: `handler!` carries the commands.
//!
//! # Secrets stay native
//!
//! The licence key and the trial e-mail enter once, as arguments of `activate` and
//! `request_trial`, and are validated here (length and control characters, the limits the renderer
//! client applies) before the client sees them. Every credential the server issues is held by the
//! client in the OS credential store. The commands answer only the client's fixed-schema views
//! (`LicenseView`, `RefreshView`, `TrialView`, none of which carries a token, a key or a device id)
//! and reject with its closed `CommandError` (`{ code, reason?, message }`, message a code, never
//! text). Nothing in this module reads, returns, logs or emits a token.
//!
//! # Who is calling
//!
//! The first call of every command is [`license_for`] over the facts of the invoking `Webview`
//! (label and current URL from Tauri's native context, never an argument). A caller the
//! [`HostCallerPolicy`](super::HostCallerPolicy) does not trust for the action is refused with
//! `forbidden` before the client is touched: no lock is taken, no network is used, no record is
//! read. The service's trust (labels, origins, actions) is the app's; a registered content
//! webview is refused whatever its label.
//!
//! # The event
//!
//! [`EVENT_LICENSE_CHANGED`] (`{ "view": LicenseView }`) is emitted after the client signals a
//! licence change (`LicenseClient::invalidation`): an activation, refresh, deactivation, lease
//! renewal or import, whichever native code ran it, and also an attempt that may have changed the
//! stored record. The payload is a fresh `status()` read taken after the client released its
//! lock. It goes to each open webview the policy trusts for `status` and to no other (a content
//! webview never hears of the licence). A delivery failure never fails an operation.
//!
//! The listener runs on the thread that finished the licence operation (the app's, or an engine
//! verifier's), and `status()` itself can signal again on that same thread (the client flushes a
//! pending change when it reads), so the listener must never block on anything it may already
//! hold. [`ChangeCoalescer`] is how: a signal marks "a read is owed" and at most one thread at a
//! time (the one that wins a `try_lock`) performs read-then-emit rounds until nothing is owed.
//! A signal that finds the turn taken (another thread, or this one re-entered) returns at once and
//! the holder's next round covers it. No lock is held while waiting, and none across a signal, so
//! the main-thread round trip each webview costs (`webview.url()`) cannot deadlock with an
//! operation the app runs on the main thread. Events leave in the order their reads were taken, so
//! the last event a window receives is the newest state; signals that arrive together may share
//! one event.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError, Weak};

use rightkit_license::{
    CommandError, LicenseClient, LicenseError, LicenseInvalidation, LicenseView, RefreshView,
    RejectReason, SecretString, TrialView,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Webview, Wry};

use super::policy::{HostCallerPolicy, ServiceId};
use super::HostServices;
use crate::caller_gate::{self, CallerError, CallerFacts, Denial};

/// The event the licence view changes are announced on.
pub const EVENT_LICENSE_CHANGED: &str = "rightkit://license-changed";

/// The longest licence key (characters) the commands accept, as the renderer client does.
pub const MAX_KEY_CHARS: usize = 4096;

/// The longest trial e-mail address (characters) the commands accept.
pub const MAX_EMAIL_CHARS: usize = 320;

/// What the licence commands need from the client. [`LicenseClient`] is the one real
/// implementation; the seam lets a test count the calls a refused caller must not cause.
pub trait LicenseOps: Send + Sync {
    fn status(&self) -> LicenseView;
    fn activate(&self, key: SecretString) -> Result<LicenseView, LicenseError>;
    fn refresh(&self) -> Result<RefreshView, LicenseError>;
    fn deactivate(&self) -> Result<LicenseView, LicenseError>;
    fn request_trial(&self, email: &str) -> Result<TrialView, LicenseError>;
    /// The change counter the event follows; `None` means no event is ever emitted.
    fn invalidation(&self) -> Option<LicenseInvalidation>;
}

impl LicenseOps for LicenseClient {
    fn status(&self) -> LicenseView {
        LicenseClient::status(self)
    }
    fn activate(&self, key: SecretString) -> Result<LicenseView, LicenseError> {
        LicenseClient::activate(self, key)
    }
    fn refresh(&self) -> Result<RefreshView, LicenseError> {
        LicenseClient::refresh(self)
    }
    fn deactivate(&self) -> Result<LicenseView, LicenseError> {
        LicenseClient::deactivate(self)
    }
    fn request_trial(&self, email: &str) -> Result<TrialView, LicenseError> {
        LicenseClient::request_trial(self, email)
    }
    fn invalidation(&self) -> Option<LicenseInvalidation> {
        Some(LicenseClient::invalidation(self))
    }
}

/// The registered licence service: the client behind the five commands and the event.
pub struct LicenseHostService {
    ops: Arc<dyn LicenseOps>,
}

impl LicenseHostService {
    pub fn new(client: LicenseClient) -> Self {
        Self {
            ops: Arc::new(client),
        }
    }

    /// A service over any [`LicenseOps`] (a test double).
    pub fn with_ops(ops: Arc<dyn LicenseOps>) -> Self {
        Self { ops }
    }

    /// The licence as it stands. No network.
    pub fn status(&self) -> LicenseView {
        self.ops.status()
    }

    /// Activate `key`. A key that is empty, too long or holds a control character is refused as
    /// `keyNotAccepted` without a client call.
    pub fn activate(&self, key: String) -> Result<LicenseView, LicenseError> {
        if !plausible(&key, MAX_KEY_CHARS) {
            return Err(LicenseError::Rejected {
                reason: RejectReason::KeyNotAccepted,
            });
        }
        self.ops.activate(SecretString::new(key))
    }

    pub fn refresh(&self) -> Result<RefreshView, LicenseError> {
        self.ops.refresh()
    }

    pub fn deactivate(&self) -> Result<LicenseView, LicenseError> {
        self.ops.deactivate()
    }

    /// Ask for a trial key by e-mail. An address that is empty, too long or holds a control
    /// character is answered `accepted: false` without a client call.
    pub fn request_trial(&self, email: &str) -> Result<TrialView, LicenseError> {
        if !plausible(email, MAX_EMAIL_CHARS) {
            return Ok(TrialView {
                accepted: false,
                message: rightkit_license::MessageCode::TrialRefused,
            });
        }
        self.ops.request_trial(email)
    }

    /// Announce every licence change on [`EVENT_LICENSE_CHANGED`] (see the module documentation).
    pub(crate) fn attach_emitter(&self, app: AppHandle<Wry>) {
        self.on_change(move |view| emit_changed(&app, view));
    }

    /// Call `emit` with a fresh `status()` read after every change the client signals, ordered
    /// and coalesced by [`ChangeCoalescer`]: `emit` never runs twice at once, never runs for a
    /// read older than one already emitted, and a re-entrant or concurrent signal neither blocks
    /// nor is lost. [`attach_emitter`](Self::attach_emitter) is this with the Tauri emit; a test
    /// passes its own.
    pub fn on_change(&self, emit: impl Fn(&LicenseView) + Send + Sync + 'static) {
        let Some(invalidation) = self.ops.invalidation() else {
            return;
        };
        // Weak, so the listener the client keeps does not keep the client alive.
        let ops: Weak<dyn LicenseOps> = Arc::downgrade(&self.ops);
        let pump = ChangeCoalescer::default();
        invalidation.subscribe(move |_generation| {
            pump.signal(|| {
                if let Some(ops) = ops.upgrade() {
                    emit(&ops.status());
                }
            });
        });
    }
}

/// Serialises "read the licence, then emit it" without ever blocking a signal.
///
/// A plain mutex around the read and the emit deadlocks: `status()` can signal again on the same
/// thread (the client flushes a pending change at the end of every read), and the emit makes a
/// main-thread round trip per webview. This holds the turn only through `try_lock`, and records
/// that a read is owed in `pending` BEFORE trying, so a signal is never lost: whoever holds the
/// turn keeps running rounds while `pending` is set, and re-checks it once after letting go.
#[derive(Default)]
pub struct ChangeCoalescer {
    turn: Mutex<()>,
    pending: AtomicBool,
}

impl ChangeCoalescer {
    /// Record that a read is owed and, when no other round is running (on this thread or another),
    /// run `round` (one read plus its emit) until nothing is owed. Returns at once when another
    /// round holds the turn: that round will perform the owed read after it finishes its own.
    pub fn signal(&self, mut round: impl FnMut()) {
        self.pending.store(true, Ordering::SeqCst);
        loop {
            let turn = match self.turn.try_lock() {
                Ok(turn) => turn,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                // Another round (or this thread, re-entered from inside `round`) holds the turn.
                Err(TryLockError::WouldBlock) => return,
            };
            while self.pending.swap(false, Ordering::SeqCst) {
                round();
            }
            drop(turn);
            // A signal between the last swap and the unlock saw the turn taken and returned.
            if !self.pending.load(Ordering::SeqCst) {
                return;
            }
        }
    }
}

/// Non-empty, within `max` characters, no control character: the shape the renderer client also
/// requires. The client applies its own trimming and plausibility checks on top.
fn plausible(text: &str, max: usize) -> bool {
    !text.is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// `{ "view": LicenseView }`, the event payload.
#[derive(Clone, Serialize)]
struct LicenseChanged {
    view: LicenseView,
}

/// True when a webview with `facts` may hear [`EVENT_LICENSE_CHANGED`]: the policy would let it
/// call `status`.
pub fn hears_license_changes(policy: &HostCallerPolicy, facts: &CallerFacts<'_>) -> bool {
    policy.decide(ServiceId::LICENSE, "status", facts).is_ok()
}

fn emit_changed(app: &AppHandle<Wry>, view: &LicenseView) {
    let Some(host) = app.try_state::<HostServices>() else {
        return;
    };
    for (label, webview) in app.webviews() {
        let Ok(owned) = caller_gate::gather(&webview) else {
            continue;
        };
        if hears_license_changes(host.policy(), &owned.facts()) {
            let _ = app.emit_to(
                label,
                EVENT_LICENSE_CHANGED,
                LicenseChanged { view: view.clone() },
            );
        }
    }
}

/// The licence service `facts` may use for `action` (`status`, `activate`, `refresh`,
/// `deactivate`, `request_trial`); the first call of every licence command. A refused caller is
/// `forbidden`; an app with no licence service registered, or one registered as another type, is
/// `unsupported`.
pub fn license_for(
    host: &HostServices,
    action: &str,
    facts: &CallerFacts<'_>,
) -> Result<Arc<LicenseHostService>, LicenseError> {
    match host.policy().decide(ServiceId::LICENSE, action, facts) {
        Ok(()) => {}
        Err(CallerError::Forbidden(Denial::ServiceNotRegistered)) => {
            return Err(LicenseError::Unsupported)
        }
        Err(CallerError::Forbidden(_)) => return Err(LicenseError::Forbidden),
    }
    host.service::<LicenseHostService>(ServiceId::LICENSE)
        .ok_or(LicenseError::Unsupported)
}

// ---- commands -------------------------------------------------------------------------------

fn license_service(
    webview: &Webview<Wry>,
    action: &str,
) -> Result<Arc<LicenseHostService>, CommandError> {
    let app = webview.app_handle();
    // No host services, or an unreadable caller, is never a licence caller.
    let host = super::managed(app).map_err(|_| LicenseError::Unsupported.to_command_error())?;
    let owned =
        caller_gate::gather(webview).map_err(|_| LicenseError::Forbidden.to_command_error())?;
    license_for(&host, action, &owned.facts()).map_err(|e| e.to_command_error())
}

/// Run blocking client work off the command thread. A panic in it leaves the stored record
/// unproven, so it is answered as `unresolved`.
async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, LicenseError> + Send + 'static,
) -> Result<T, CommandError> {
    match tauri::async_runtime::spawn_blocking(work).await {
        Ok(result) => result.map_err(|e| e.to_command_error()),
        Err(_) => Err(LicenseError::Unresolved.to_command_error()),
    }
}

/// The licence as it stands.
#[tauri::command]
pub async fn rightkit_license_status(webview: Webview<Wry>) -> Result<LicenseView, CommandError> {
    let service = license_service(&webview, "status")?;
    off_thread(move || Ok(service.status())).await
}

/// Activate a licence key (it is zeroed after use). Answers the new view.
#[tauri::command]
pub async fn rightkit_license_activate(
    webview: Webview<Wry>,
    key: String,
) -> Result<LicenseView, CommandError> {
    let service = license_service(&webview, "activate")?;
    off_thread(move || service.activate(key)).await
}

/// Re-validate with the licence service. Answers the new view and what happened.
#[tauri::command]
pub async fn rightkit_license_refresh(webview: Webview<Wry>) -> Result<RefreshView, CommandError> {
    let service = license_service(&webview, "refresh")?;
    off_thread(move || service.refresh()).await
}

/// Free this device's seat and tombstone the stored licence. Answers the new view.
#[tauri::command]
pub async fn rightkit_license_deactivate(
    webview: Webview<Wry>,
) -> Result<LicenseView, CommandError> {
    let service = license_service(&webview, "deactivate")?;
    off_thread(move || service.deactivate()).await
}

/// Ask the licence service to e-mail a trial key. Issues no credential.
#[tauri::command]
pub async fn rightkit_license_request_trial(
    webview: Webview<Wry>,
    email: String,
) -> Result<TrialView, CommandError> {
    let service = license_service(&webview, "request_trial")?;
    off_thread(move || service.request_trial(&email)).await
}
