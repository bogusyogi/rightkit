//! Pro continuity across a person's Apple devices (feature `pro-bridge`).
//!
//! A Pro user gets Pro on their next Apple device without typing a key. Rust cannot call the
//! Security framework's synchronizable-item APIs, so the synchronizable iCloud Keychain is reached
//! through the app's Swift credentials helper (`rightkit_credentials::apple_helper`); the licence
//! half (the `bridge/*` calls, the signed-lease checks, the stored record) is
//! `LicenseClient::{bridge_publish, bridge_exchange}`. This module owns what is left: the local
//! toggle state, the launch algorithm and the status read. The three apps that carried a copy of it
//! now pass a [`ProBridgeConfig`] and a [`ProBridgeHost`] instead.
//!
//! **The bridge token is not an entitlement.** It means only "this Apple Account owns a valid Pro
//! source and may request device activation". The licence key, the activation token and the signed
//! entitlement are never written to the synchronizable item; only the opaque bridge token and the
//! content-free consent marker are. A bridge exchange is verified by the licence client before
//! anything is stored.
//!
//! # State (device-only, never synced)
//!
//! Two independent preference bits, each with its own reported status: the Keychain CREDENTIAL
//! channel (`keychain_sync_enabled`) and the backend Pro RESOLUTION channel
//! (`pro_resolution_enabled`). One can be on while the other is off, and one's failure is never
//! reported as the other's. Absent state defaults BOTH off: nothing is published or consumed until
//! the user opts in. The file is `{ v, keychain_sync_enabled, pro_resolution_enabled,
//! suppress_consumption, migrated }`; an older file with the single `sync_enabled` flag is read in
//! place and migrated into both flags once.
//!
//! `suppress_consumption` is set right before the renderer deactivates this device's licence, so a
//! launch cannot race the deactivation and re-activate the seat that was just freed. It is cleared
//! only by an explicit activation, an explicit reconnect of Pro resolution, or a verified bridge
//! activation, never by time or launch.
//!
//! # Launch algorithm
//!
//! Local entitlement first; then Pro + resolution on publishes this device's activation as a bridge,
//! Free + resolution on tries to consume one. Every failure is logged and swallowed: an offline Mac
//! boots exactly as before. Publish and consume are launch-driven only, there is no "sync now".
//!
//! # What the host supplies
//!
//! [`ProBridgeConfig`] (names, state file, helper) and [`ProBridgeHost`] (the licence client, "is
//! this device Pro", the app's own sync-choice marker, a log sink). The methods are synchronous and
//! block on the helper and the network; a Tauri command wraps one in `spawn_blocking`.
//!
//! # Commands
//!
//! The shell registers the five commands the apps carried, under the same names and shapes, behind
//! [`ServiceId::PRO_BRIDGE`] ([`HostServicesBuilder::pro_bridge`](super::HostServicesBuilder::pro_bridge)):
//! `pro_bridge_status` (the [`ProBridgeStatus`], camelCase), `pro_bridge_set_keychain_sync_enabled`
//! and `pro_bridge_set_pro_resolution_enabled` (`{ enabled }`), `pro_bridge_set_sync_enabled`
//! (`{ enabled }`, both halves) and `pro_bridge_suppress_consumption` (`{ suppressed }`). The four
//! setters answer `null`; every error is a plain string. Each asks the host caller policy first
//! (the action is the command name without the `pro_bridge_` prefix), so a window the app did not
//! trust reaches neither the helper nor the state file.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tauri::{Manager, Webview, Wry};

use rightkit_credentials::apple_helper::{AppleHelper, HelperTrust, Scope};
use rightkit_credentials::Secret;
use rightkit_license::{BridgePlatform, LicenseClient, SecretString};
use serde::{Deserialize, Serialize};

use super::policy::ServiceId;
use super::HostServices;
use crate::caller_gate::{self, CallerError, CallerFacts, Denial};

/// The synchronized account the bridge token lives in (a name the Swift helper allow-lists).
const ACCOUNT_PRO_BRIDGE: &str = "pro.bridge";
/// The synchronized account the consent marker lives in.
const ACCOUNT_SYNC_CONSENT: &str = "sync.consent";

/// Local Keychain calls, not network calls: bounded defense against a wedged helper process.
const DEFAULT_HELPER_TIMEOUT: Duration = Duration::from_secs(15);

/// Everything product-specific.
#[derive(Debug, Clone)]
pub struct ProBridgeConfig {
    /// File name of the Swift helper, e.g. `HeardRightAppleCredentials`. It ships as its own nested
    /// `.app` under `Contents/Library/LoginItems` (the keychain access group needs its profile).
    pub helper_binary_name: String,
    /// Developer ID team the helper must be signed by. `Some` verifies the signature before every
    /// spawn (the helper receives the bridge token); `None` runs a helper found by placement alone.
    pub helper_team_id: Option<String>,
    /// The versioned consent marker written to the synchronized account, e.g.
    /// `heardright.apple-sync.v1`.
    pub consent_marker: String,
    /// The state file (`pro-bridge-state.json` in the app's data directory).
    pub state_file: PathBuf,
    /// Deadline for one helper exchange. Default 15 s.
    pub helper_timeout: Duration,
    /// Development builds only: accept a bare helper file beside the running executable when the
    /// bundle has no nested login-item app (an unpackaged `target/<profile>/` run). Default
    /// `false`: a packaged build never runs an unbundled helper (it carries no provisioning
    /// profile or entitlement). See [`ProBridgeConfig::with_unbundled_sibling`].
    pub(crate) allow_unbundled_sibling: bool,
}

impl ProBridgeConfig {
    pub fn new(
        helper_binary_name: impl Into<String>,
        consent_marker: impl Into<String>,
        state_file: impl Into<PathBuf>,
    ) -> Self {
        Self {
            helper_binary_name: helper_binary_name.into(),
            helper_team_id: None,
            consent_marker: consent_marker.into(),
            state_file: state_file.into(),
            helper_timeout: DEFAULT_HELPER_TIMEOUT,
            allow_unbundled_sibling: false,
        }
    }

    /// Require the helper to be signed by this Developer ID team: the signature is verified before
    /// every spawn and the helper is built with `AppleHelper::new_trusted`. Without it
    /// (`helper_team_id` is `None`, the default) no code signature is enforced and the helper is
    /// built with `AppleHelper::new`: placement alone decides, so set it in every release build.
    pub fn with_helper_team_id(mut self, team_id: impl Into<String>) -> Self {
        self.helper_team_id = Some(team_id.into());
        self
    }

    /// Allow the unpackaged development fallback: when the nested login-item app is not found, a
    /// bare regular file named `helper_binary_name` directly in the running executable's
    /// directory is used (`rightkit_child_process::LoginItemOptions::allow_unbundled_sibling`;
    /// it must canonicalise to a regular file in that directory, so a symlink that leaves it is
    /// refused). Default `false`. Combine it with a team id only if the dev helper is signed.
    pub fn with_unbundled_sibling(mut self, allow: bool) -> Self {
        self.allow_unbundled_sibling = allow;
        self
    }
}

/// A condition of the Apple helper that the Settings row shows as text
/// ([`ProBridgeStatus::keychain_detail`] and the not-bundled refusals). The host words it through
/// [`ProBridgeHost::describe_helper_condition`]; [`HelperCondition::default_text`] is the wording
/// used when it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HelperCondition {
    /// The helper is not where the bundle should hold it (or this is not macOS). `reason` is the
    /// lookup's own explanation, when there is one.
    NotBundled {
        helper_name: String,
        reason: Option<String>,
    },
    /// The helper process could not be started; `detail` is the OS error kind.
    Spawn { helper_name: String, detail: String },
    /// The helper did not answer within the deadline (`secs`).
    TimedOut { helper_name: String, secs: u64 },
    /// The helper ran and reported a failure; `detail` is its text.
    Helper { detail: String },
    /// Any other failure; `message` is the credential layer's own text.
    Other { message: String },
}

impl HelperCondition {
    /// The wording RightKit uses when the host does not override it.
    pub fn default_text(&self) -> String {
        match self {
            Self::NotBundled {
                helper_name,
                reason: Some(why),
            } => format!(
                "{helper_name} helper not bundled ({why}) — Apple device continuity unavailable"
            ),
            Self::NotBundled {
                helper_name,
                reason: None,
            } => format!("{helper_name} helper not bundled — Apple device continuity unavailable"),
            Self::Spawn { detail, .. } => {
                format!(
                    "apple helper error: {}{detail}",
                    rightkit_credentials::apple_helper::SPAWN_FAILED_PREFIX
                )
            }
            Self::TimedOut { secs, .. } => {
                format!("credential store operation apple_helper timed out after {secs}s")
            }
            Self::Helper { detail } => format!("apple helper error: {detail}"),
            Self::Other { message } => message.clone(),
        }
    }
}

/// What the bridge needs from the app.
pub trait ProBridgeHost: Send + Sync + 'static {
    /// The licence client; `None` while licensing is not available.
    fn license_client(&self) -> Option<LicenseClient>;
    /// Whether this device holds a Pro entitlement right now (verified offline).
    fn has_pro(&self) -> bool;
    /// Whether the user already made an explicit local choice in the app's other sync setting; the
    /// account consent marker is then never adopted. Default: no such setting.
    fn has_local_sync_choice(&self) -> bool {
        false
    }
    /// A reason the bridge must not run at all (for example a QA mode), or `None`. A `Some` refuses
    /// the setters and every helper call with that text.
    fn unavailable_reason(&self) -> Option<String> {
        None
    }
    /// A diagnostic event. `detail` never holds a token.
    fn log(&self, event: &str, detail: &str);
    /// The text shown for a helper condition: the not-bundled refusals and the status row's
    /// `keychainDetail`. Default: [`HelperCondition::default_text`]. An app that ships its own
    /// wording returns it here (for example `"{helper_name} timed out"` for `TimedOut`).
    fn describe_helper_condition(&self, condition: &HelperCondition) -> String {
        condition.default_text()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProBridgeState {
    v: u8,
    #[serde(default, alias = "sync_enabled")]
    keychain_sync_enabled: bool,
    #[serde(default)]
    pro_resolution_enabled: bool,
    #[serde(default)]
    suppress_consumption: bool,
    /// One-time migration marker from the single `sync_enabled` flag. A new install starts `true`.
    #[serde(default)]
    migrated: bool,
}

impl Default for ProBridgeState {
    fn default() -> Self {
        Self {
            v: 1,
            keychain_sync_enabled: false,
            pro_resolution_enabled: false,
            suppress_consumption: false,
            migrated: true,
        }
    }
}

/// The status the renderer reads (`camelCase`). `sync_enabled` is the pre-split field, kept as it
/// always was: the Keychain toggle.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProBridgeStatus {
    pub sync_enabled: bool,
    pub keychain_ready: bool,
    pub keychain_detail: Option<String>,
    /// True while this device must not consume the shared bridge.
    pub consumption_suppressed: bool,
    pub keychain_sync_enabled: bool,
    pub pro_resolution_enabled: bool,
    /// `"off"`, `"blocked_no_keychain"` (resolution on but the Keychain channel is off) or `"ok"`.
    pub pro_resolution_status: &'static str,
}

/// The Pro bridge of one app. Build once, share through `Arc`.
pub struct ProBridge {
    config: ProBridgeConfig,
    host: Arc<dyn ProBridgeHost>,
    /// Serialises every read-modify-write of the state file: the Keychain and Pro-resolution
    /// setters can fire concurrently, and each must see the other's write.
    state_lock: Mutex<()>,
}

impl ProBridge {
    pub fn new(config: ProBridgeConfig, host: Arc<dyn ProBridgeHost>) -> Self {
        Self {
            config,
            host,
            state_lock: Mutex::new(()),
        }
    }

    // ---- state ------------------------------------------------------------------------

    fn read_state(&self) -> ProBridgeState {
        let path = &self.config.state_file;
        if !path.exists() {
            return ProBridgeState::default();
        }
        let mut state: ProBridgeState = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => ProBridgeState::default(),
        };
        if !state.migrated {
            // One-time migration from the single `sync_enabled` flag; a failed write just runs the
            // same idempotent migration again on the next read.
            state.pro_resolution_enabled = state.keychain_sync_enabled;
            state.migrated = true;
            let _ = self.write_state(&state);
        }
        state
    }

    fn write_state(&self, state: &ProBridgeState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|err| format!("serialize pro bridge state: {err}"))?;
        let options = rightkit_fs::atomic::AtomicWriteOptions {
            create_parents: true,
            ..Default::default()
        };
        rightkit_fs::atomic::atomic_write_with_options(&self.config.state_file, options, |file| {
            std::io::Write::write_all(file, &bytes)
        })
        .map_err(|err| format!("write pro bridge state: {err}"))
    }

    fn update_state(&self, f: impl FnOnce(&mut ProBridgeState)) -> Result<ProBridgeState, String> {
        let _guard = self
            .state_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut state = self.read_state();
        f(&mut state);
        self.write_state(&state)?;
        Ok(state)
    }

    // ---- helper -----------------------------------------------------------------------

    fn refuse_if_unavailable(&self) -> Result<(), String> {
        match self.host.unavailable_reason() {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    fn not_bundled(&self, why: Option<String>) -> String {
        self.host
            .describe_helper_condition(&HelperCondition::NotBundled {
                helper_name: self.config.helper_binary_name.clone(),
                reason: why,
            })
    }

    /// A failed helper exchange, worded by the host.
    fn describe_credential_error(&self, err: &rightkit_credentials::Error) -> String {
        use rightkit_credentials::apple_helper::SPAWN_FAILED_PREFIX;
        use rightkit_credentials::Error;
        let helper_name = self.config.helper_binary_name.clone();
        let condition = match err {
            Error::Timeout {
                op: "apple_helper",
                secs,
            } => HelperCondition::TimedOut {
                helper_name,
                secs: *secs,
            },
            Error::Helper(text) => match text.strip_prefix(SPAWN_FAILED_PREFIX) {
                Some(detail) => HelperCondition::Spawn {
                    helper_name,
                    detail: detail.to_string(),
                },
                None => HelperCondition::Helper {
                    detail: text.clone(),
                },
            },
            other => HelperCondition::Other {
                message: other.to_string(),
            },
        };
        self.host.describe_helper_condition(&condition)
    }

    #[cfg(target_os = "macos")]
    fn helper_path(&self) -> Result<PathBuf, String> {
        use rightkit_child_process::{resolve_bundled_login_item_with, LoginItemOptions};
        use rightkit_credentials::apple_helper::resolve_bundled;
        let name = &self.config.helper_binary_name;
        let options = LoginItemOptions::default()
            .allow_unbundled_sibling(self.config.allow_unbundled_sibling);
        match resolve_bundled_login_item_with(name, options) {
            Ok(path) => Ok(path),
            // A bare sibling binary only for an unsigned (no team id) dev run.
            Err(err) if self.config.helper_team_id.is_none() => {
                resolve_bundled(name).ok_or_else(|| self.not_bundled(Some(err.to_string())))
            }
            Err(err) => Err(self.not_bundled(Some(err.to_string()))),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn helper_path(&self) -> Result<PathBuf, String> {
        Err(self.not_bundled(None))
    }

    fn helper(&self) -> Result<AppleHelper, String> {
        self.refuse_if_unavailable()?;
        let path = self.helper_path()?;
        let helper = match &self.config.helper_team_id {
            Some(team) => {
                let trust = HelperTrust::team(team).map_err(|err| err.to_string())?;
                AppleHelper::new_trusted(path, trust)
            }
            None => AppleHelper::new(path),
        };
        Ok(helper.timeout(self.config.helper_timeout))
    }

    fn license_client(&self) -> Result<LicenseClient, String> {
        self.host
            .license_client()
            .ok_or_else(|| "licensing is not available".to_string())
    }

    // ---- consent marker ---------------------------------------------------------------

    fn publish_account_sync_consent(&self) -> Result<(), String> {
        self.helper()?
            .write(
                Scope::Synced,
                ACCOUNT_SYNC_CONSENT,
                &Secret::from_string(&self.config.consent_marker),
            )
            .map_err(|err| format!("Could not publish Apple sync consent: {err}"))
    }

    /// Reads the account-scoped marker only when this Mac has no prior local decision in either
    /// Apple-sync state machine. A synchronized marker is historical account state, not local
    /// consent (it may be stale after a reset), so it never enables anything: always `Ok(false)`.
    /// The read is kept for its failure report.
    fn adopt_account_sync_consent_if_unset(&self) -> Result<bool, String> {
        if self.config.state_file.exists() || self.host.has_local_sync_choice() {
            return Ok(false);
        }
        self.helper()?
            .read(Scope::Synced, ACCOUNT_SYNC_CONSENT)
            .map_err(|err| format!("Could not read Apple sync consent: {err}"))?;
        Ok(false)
    }

    // ---- publish / consume ------------------------------------------------------------

    /// Publish this Mac's existing activation as a bridge, if it is Pro. The licence client never
    /// writes the stored licence when publishing, so a failed or offline attempt cannot downgrade
    /// a valid local entitlement.
    fn publish_inner(&self) -> Result<(), String> {
        let Some(bridge_token) = self
            .license_client()?
            .bridge_publish()
            .map_err(|err| err.to_string())?
        else {
            // No licence granting access, or no activation credential to mint a bridge from.
            return Ok(());
        };
        self.helper()?
            .write(
                Scope::Synced,
                ACCOUNT_PRO_BRIDGE,
                &Secret::from_string(bridge_token.expose()),
            )
            .map_err(|err| format!("Could not store the synced Pro bridge: {err}"))
    }

    /// Consume an existing bridge from iCloud Keychain and, if it is still valid, activate this
    /// device through it. A missing bridge, an offline backend or a rejected exchange leaves this
    /// Mac exactly as Free (or Pro) as it already was.
    fn consume_inner(&self) -> Result<(), String> {
        if self.read_state().suppress_consumption {
            // Suppressed ahead of a deactivation (or not yet reconnected since): never consume,
            // however many launches happen.
            return Ok(());
        }
        let read = self
            .helper()?
            .read(Scope::Synced, ACCOUNT_PRO_BRIDGE)
            .map_err(|err| format!("Could not read the synced Pro bridge: {err}"))?;
        let bridge_token = read
            .as_ref()
            .map(Secret::expose_str)
            .transpose()
            .map_err(|err| format!("Could not read the synced Pro bridge: {err}"))?
            .filter(|value| !value.trim().is_empty());
        let Some(bridge_token) = bridge_token else {
            // No bridge yet (or not propagated here yet): the licence-entry UI stays the fallback.
            return Ok(());
        };
        self.license_client()?
            .bridge_exchange(SecretString::new(bridge_token), BridgePlatform::MacOs)
            .map_err(|err| err.to_string())?;
        // A verified bridge activation is itself an explicit activation: clear any suppression.
        let _ = self.update_state(|state| state.suppress_consumption = false);
        Ok(())
    }

    /// The Mac launch algorithm, backend-resolution half only: gated on the user's own
    /// `pro_resolution_enabled`, never on the Keychain flag directly. The Keychain channel being
    /// off is a runtime precondition reported as `pro_bridge_resolution_blocked`, not folded into
    /// "off". Every failure is logged and swallowed.
    pub fn run_launch_algorithm(&self) {
        let state = self.read_state();
        if !state.pro_resolution_enabled {
            return;
        }
        if !state.keychain_sync_enabled {
            self.host.log(
                "pro_bridge_resolution_blocked",
                "Pro resolution is enabled but Keychain credential sync is off",
            );
            return;
        }
        if self.host.has_pro() {
            if let Err(err) = self.publish_inner() {
                self.host.log("pro_bridge_publish_error", &err);
            }
        } else if let Err(err) = self.consume_inner() {
            self.host.log("pro_bridge_consume_error", &err);
        }
    }

    /// The launch step: read the consent marker for its failure report, then run the algorithm.
    /// Blocking; call it off the main thread, once, from setup.
    pub fn run_launch(&self) {
        if let Err(err) = self.adopt_account_sync_consent_if_unset() {
            self.host.log("apple_sync_consent_adopt_failed", &err);
        }
        self.run_launch_algorithm();
    }

    // ---- commands' bodies -------------------------------------------------------------

    /// The status read. Blocking (one helper exchange).
    pub fn status(&self) -> ProBridgeStatus {
        let state = self.read_state();
        let pro_resolution_status = if !state.pro_resolution_enabled {
            "off"
        } else if !state.keychain_sync_enabled {
            "blocked_no_keychain"
        } else {
            "ok"
        };
        let (keychain_ready, keychain_detail) = match self
            .helper()
            .and_then(|h| h.status().map_err(|e| self.describe_credential_error(&e)))
        {
            Ok(status) => (
                status.ready,
                if status.ready { None } else { status.detail },
            ),
            Err(err) => (false, Some(err)),
        };
        ProBridgeStatus {
            sync_enabled: state.keychain_sync_enabled,
            keychain_ready,
            keychain_detail,
            consumption_suppressed: state.suppress_consumption,
            keychain_sync_enabled: state.keychain_sync_enabled,
            pro_resolution_enabled: state.pro_resolution_enabled,
            pro_resolution_status,
        }
    }

    /// CREDENTIAL half only. Turning it off never deletes the synced item (Apple propagates the
    /// deletion to every device of the Apple Account): what this device needs is copied into a
    /// device-only item and the synced scope is simply no longer touched.
    pub fn set_keychain_sync_enabled(&self, enabled: bool) -> Result<(), String> {
        self.refuse_if_unavailable()?;
        let state = self.update_state(|state| state.keychain_sync_enabled = enabled)?;
        if !enabled {
            let copied = self.helper().and_then(|helper| {
                helper
                    .copy_synced_to_device_only(ACCOUNT_PRO_BRIDGE)
                    .map_err(|err| err.to_string())
            });
            if let Err(err) = copied {
                self.host.log("pro_bridge_copy_local_error", &err);
            }
        } else {
            if let Err(err) = self.publish_account_sync_consent() {
                self.host.log("apple_sync_consent_publish_failed", &err);
            }
            if state.pro_resolution_enabled {
                // The Keychain channel is available again and resolution already wants to run.
                self.run_launch_algorithm();
            }
        }
        Ok(())
    }

    /// BACKEND (Pro-resolution) half only. Enabling it while Keychain sync is off persists the
    /// preference and reports `blocked_no_keychain`. Turning it on is an explicit reconnect and
    /// clears a standing consumption suppression.
    pub fn set_pro_resolution_enabled(&self, enabled: bool) -> Result<(), String> {
        self.refuse_if_unavailable()?;
        self.update_state(|state| {
            state.pro_resolution_enabled = enabled;
            if enabled {
                state.suppress_consumption = false;
            }
        })?;
        if enabled {
            // Best-effort immediate attempt; nothing local changes except on a verified success.
            self.run_launch_algorithm();
        }
        Ok(())
    }

    /// Compatibility composition for a single "Sync with Apple devices" control: both halves, in
    /// that order.
    pub fn set_sync_enabled(&self, enabled: bool) -> Result<(), String> {
        self.set_keychain_sync_enabled(enabled)?;
        self.set_pro_resolution_enabled(enabled)
    }

    /// Set or clear consumption suppression. The renderer sets it BEFORE it deactivates this
    /// device's licence and clears it after a successful activation. It touches only the local flag.
    pub fn suppress_consumption(&self, suppressed: bool) -> Result<(), String> {
        self.refuse_if_unavailable()?;
        self.update_state(|state| state.suppress_consumption = suppressed)
            .map(|_| ())
    }
}

/// The registered Pro bridge: what [`HostServicesBuilder::pro_bridge`](super::HostServicesBuilder::pro_bridge)
/// stores behind [`ServiceId::PRO_BRIDGE`]. The app keeps its own `Arc` to call
/// [`ProBridge::run_launch`] at start.
pub struct ProBridgeHostService {
    bridge: Arc<ProBridge>,
}

impl ProBridgeHostService {
    pub fn new(bridge: Arc<ProBridge>) -> Self {
        Self { bridge }
    }
}

/// The bridge for `action`, after the caller policy allowed it. Errors are the plain strings the
/// commands answer.
pub fn pro_bridge_for(
    host: &HostServices,
    action: &str,
    facts: &CallerFacts<'_>,
) -> Result<Arc<ProBridge>, String> {
    match host.policy().decide(ServiceId::PRO_BRIDGE, action, facts) {
        Ok(()) => {}
        Err(CallerError::Forbidden(Denial::ServiceNotRegistered)) => {
            return Err("unsupported".to_string())
        }
        Err(CallerError::Forbidden(_)) => return Err("forbidden".to_string()),
    }
    host.service::<ProBridgeHostService>(ServiceId::PRO_BRIDGE)
        .map(|service| service.bridge.clone())
        .ok_or_else(|| "unsupported".to_string())
}

fn bridge_service(webview: &Webview<Wry>, action: &str) -> Result<Arc<ProBridge>, String> {
    let host = super::managed(webview.app_handle()).map_err(|_| "unsupported".to_string())?;
    let owned = caller_gate::gather(webview).map_err(|_| "forbidden".to_string())?;
    pro_bridge_for(&host, action, &owned.facts())
}

async fn blocking<T: Send + 'static>(
    command: &'static str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|err| format!("join {command}: {err}"))?
}

/// The status: both halves, the helper's readiness and the suppression flag.
#[tauri::command]
pub async fn pro_bridge_status(webview: Webview<Wry>) -> Result<ProBridgeStatus, String> {
    let bridge = bridge_service(&webview, "status")?;
    blocking("pro_bridge_status", move || Ok(bridge.status())).await
}

/// The Keychain credential half only.
#[tauri::command]
pub async fn pro_bridge_set_keychain_sync_enabled(
    webview: Webview<Wry>,
    enabled: bool,
) -> Result<(), String> {
    let bridge = bridge_service(&webview, "set_keychain_sync_enabled")?;
    blocking("pro_bridge_set_keychain_sync_enabled", move || {
        bridge.set_keychain_sync_enabled(enabled)
    })
    .await
}

/// The backend Pro-resolution half only.
#[tauri::command]
pub async fn pro_bridge_set_pro_resolution_enabled(
    webview: Webview<Wry>,
    enabled: bool,
) -> Result<(), String> {
    let bridge = bridge_service(&webview, "set_pro_resolution_enabled")?;
    blocking("pro_bridge_set_pro_resolution_enabled", move || {
        bridge.set_pro_resolution_enabled(enabled)
    })
    .await
}

/// Both halves together (the single "Sync with Apple devices" control).
#[tauri::command]
pub async fn pro_bridge_set_sync_enabled(
    webview: Webview<Wry>,
    enabled: bool,
) -> Result<(), String> {
    let bridge = bridge_service(&webview, "set_sync_enabled")?;
    blocking("pro_bridge_set_sync_enabled", move || {
        bridge.set_sync_enabled(enabled)
    })
    .await
}

/// Set or clear consumption suppression (the renderer sets it before a deactivation).
#[tauri::command]
pub async fn pro_bridge_suppress_consumption(
    webview: Webview<Wry>,
    suppressed: bool,
) -> Result<(), String> {
    let bridge = bridge_service(&webview, "suppress_consumption")?;
    blocking("pro_bridge_suppress_consumption", move || {
        bridge.suppress_consumption(suppressed)
    })
    .await
}
