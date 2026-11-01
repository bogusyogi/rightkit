//! Signed owner approvals for privileged and reserved actions, the redaction of secrets from text
//! an agent may read, and the audit trail of both.
//!
//! * [`approval_sig`]: owner-check keys and approver keys (P-256, Windows Hello RSA), the wire
//!   format an owner's device signs, key bindings, and verification of a signature against an
//!   enrolled key. Nothing is accepted on a caller's word.
//! * [`redact`]: masks API keys, tokens, private-key blocks and the like in text before it leaves
//!   this computer.
//! * [`audit`]: the Activity file and the security audit file, written one append per line.
//! * [`protected_guard`]: what each platform adds to a protected credential item (the keychain
//!   partition check on macOS, the sealed key on macOS and Windows).
//!
//! The crate asks its host app for a few answers through [`rightkit_app_host`] (the state folder,
//! the product name) and through [`ApprovalHooks`] (the PIN and the unlock sessions, which belong
//! to the agent gateway). Both are installed once at start; until then every answer is neutral and
//! fails closed.

pub mod approval_sig;
pub mod audit;
pub mod protected_guard;
pub mod redact;

use serde_json::{Value, json};
use std::sync::OnceLock;

/// A refusal or failure, with a stable `code` a caller can rely on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Refusal {
        Refusal {
            code,
            message: message.into(),
        }
    }

    /// `{"error": message, "code": code}`.
    pub fn to_json(&self) -> Value {
        json!({"error": self.message, "code": self.code})
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Refusal {}

/// `Result` with a [`Refusal`].
pub type Result<T> = std::result::Result<T, Refusal>;

/// What the approvals need from the agent gateway's PIN and unlock state. Install one with
/// [`set_hooks`]; the gateway does it from its own `init`.
pub trait ApprovalHooks: Send + Sync {
    /// Whether an owner PIN is set.
    fn pin_is_set(&self) -> Result<bool>;
    /// Verifies `pin` (counted like any attempt) and opens the seal key for a write.
    fn confirm_pin(&self, pin: &str) -> Result<()>;
    /// Voids every unlock session, as a revoked owner-check key must.
    fn void_unlocks(&self) -> Result<()>;
    /// Whether this process is the resident engine, the only one that reads the key files.
    fn is_engine_process(&self) -> bool;
}

/// The hooks used before [`set_hooks`]: no PIN, nothing to void, not the engine.
struct NoHooks;

impl ApprovalHooks for NoHooks {
    fn pin_is_set(&self) -> Result<bool> {
        Ok(false)
    }
    fn confirm_pin(&self, _pin: &str) -> Result<()> {
        Err(Refusal::new(
            "pin_unavailable",
            "no PIN service is installed in this process",
        ))
    }
    fn void_unlocks(&self) -> Result<()> {
        Ok(())
    }
    fn is_engine_process(&self) -> bool {
        false
    }
}

static HOOKS: OnceLock<Box<dyn ApprovalHooks>> = OnceLock::new();

/// Installs the hooks. Only the first call takes effect; returns `false` otherwise.
pub fn set_hooks(hooks: Box<dyn ApprovalHooks>) -> bool {
    HOOKS.set(hooks).is_ok()
}

pub(crate) fn hooks() -> &'static dyn ApprovalHooks {
    HOOKS.get_or_init(|| Box::new(NoHooks)).as_ref()
}

/// The elevated Windows service that holds the owner-check keys (a folder only administrators can
/// write). Install one with [`set_owner_key_service`] on Windows; elsewhere it is never asked.
pub trait OwnerKeyService: Send + Sync {
    /// The service's keys as `(key id, device, public key hex)`; `None` when it is not installed.
    fn owner_keys(&self) -> std::result::Result<Option<Vec<(String, String, String)>>, String>;
    /// Whether the service program is installed.
    fn service_installed(&self) -> bool;
    /// Runs one elevated change of the service's key list (`owner-enrol ...`, `owner-revoke ...`).
    fn change_owner_keys(&self, args: &str) -> std::result::Result<(), String>;
}

static KEY_SERVICE: OnceLock<Box<dyn OwnerKeyService>> = OnceLock::new();

/// Installs the owner-key service. Only the first call takes effect; returns `false` otherwise.
pub fn set_owner_key_service(service: Box<dyn OwnerKeyService>) -> bool {
    KEY_SERVICE.set(service).is_ok()
}

#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn owner_key_service() -> Option<&'static dyn OwnerKeyService> {
    KEY_SERVICE.get().map(|service| service.as_ref())
}

static SIGNING_TEAM: OnceLock<String> = OnceLock::new();

/// Sets the code-signing team whose signed code may create protected keychain items (macOS). Without
/// it a protected keychain item is never trusted. Only the first call takes effect.
pub fn set_signing_team(team: &str) -> bool {
    SIGNING_TEAM.set(team.to_string()).is_ok()
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn signing_team() -> Option<&'static str> {
    SIGNING_TEAM.get().map(String::as_str)
}
