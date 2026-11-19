//! CloudKit transport for the transcript sync host: the signed CloudKit helper process, the
//! signed-environment pin, environment-scoped state file names and the native observer hook.
//!
//! macOS only. Container, zone and subscription names, the helper's bundle name (through the
//! profile) and the expected signing team are inputs; nothing about them is defaulted.
//!
//! # How the two sides know each other
//!
//! * App to helper: the helper is found only as the nested login-item app of the running
//!   bundle (`resolve_bundled_login_item`: canonical path, regular file, inside its own nested
//!   `.app` inside the canonical bundle; there is no sibling-file fallback), then
//!   `verify_team_signature` runs `codesign --verify --strict` against the configured team and
//!   fails closed, then the helper's signed CloudKit environment must match the requested one.
//!   Placement, signature and environment are three separate checks; any doubt refuses the
//!   helper. The check and the later spawn use the same canonical path, so a process that can
//!   write inside the signed bundle between the two could still substitute the program (the
//!   bundle is the trust boundary).
//! * Helper to app: the helper is the app's own child. It speaks over a stdin/stdout pipe pair
//!   that only the app holds, so no other process can send it requests or answer for it. The
//!   crate's `helper` module bounds every response line (128 MiB), matches responses to request
//!   ids, kills and respawns the helper on any protocol failure or timeout, and rejects a
//!   response whose account scope differs from the one the request carried.
//! * Account identity: the helper reports an opaque scope; every request carries the scope the
//!   app expects (`expected_scope`), and a response whose scope differs cancels the call and
//!   fences the in-flight pass.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::traits::{NativeObserver, SyncTransport};
use crate::helper::{CloudKitHelper, HelperConfig};
use crate::transcripts::{self as sync_core, CloudKitEnvironment, TranscriptSyncProfile};
use crate::{AuthProvider, Backend, SyncObserver};

/// Everything the host chooses for the CloudKit transport. No field has a default.
pub struct CloudKitConfig {
    /// Names of the helper bundles (through `helper_bundle_name`).
    pub profile: TranscriptSyncProfile,
    /// CloudKit container identifier, e.g. `iCloud.<reverse-dns>`.
    pub container: String,
    /// Record zone name.
    pub zone: String,
    /// Subscription id. Must equal the id the main-process native observer saves, so the
    /// helper's `prepare` is idempotent against it.
    pub subscription: String,
    /// Apple team identifier the helper must be signed by (checked with
    /// `rightkit_child_process::is_team_id`).
    pub team_id: String,
    /// Per-request deadline for the helper, in seconds. Clamped to 5..=600.
    pub request_timeout_secs: u64,
    /// The main-process native observer, if the app has one. It is used only when the requested
    /// environment allows it.
    pub native: Option<Arc<dyn NativeObserver>>,
    /// Diagnostics sink `(event name, detail)`; details are sanitized here.
    pub event_sink: fn(&str, &str),
}

fn check_ident(what: &str, value: &str) -> Result<(), String> {
    let ok = !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "cloudkit config: {what} must be 1..=255 ASCII letters, digits, '.', '_' or '-', not starting with '-'"
        ))
    }
}

fn environment() -> Result<CloudKitEnvironment, String> {
    sync_core::requested_environment()
}

fn env_or_production() -> CloudKitEnvironment {
    environment().unwrap_or(CloudKitEnvironment::Production)
}

struct TelemetryObserver {
    sink: fn(&str, &str),
    native: Option<Arc<dyn NativeObserver>>,
}

impl SyncObserver for TelemetryObserver {
    fn event(&self, name: &'static str, detail: &str) {
        if name == "zone_recreated" {
            if let Some(native) = &self.native {
                // The zone (and its subscription) was recreated: ask the native observer to
                // re-establish its cached subscription.
                native.invalidate_subscription();
            }
        }
        (self.sink)(&format!("cloud_sync_{name}"), &super::safe_detail(detail));
    }
}

/// Read the helper's SIGNED CloudKit environment (`codesign -d --entitlements :-`) and enforce
/// the pin: a helper signed Development is refused by a Production expectation.
fn verify_signed_environment(env: CloudKitEnvironment, path: &Path) -> Result<(), String> {
    let mut command = rightkit_child_process::OwnedCommand::new("/usr/bin/codesign");
    command
        .command_mut()
        .args(["-d", "--entitlements", ":-"])
        .arg(path);
    let options =
        rightkit_child_process::CaptureOptions::default().timeout(Duration::from_secs(15));
    let signed = rightkit_child_process::run_command_capture(command, None, &options)
        .ok()
        .filter(|output| !output.timed_out && !output.stdout.truncated)
        .and_then(|output| {
            sync_core::parse_entitlement_environment(&String::from_utf8_lossy(&output.stdout.bytes))
        });
    sync_core::check_helper_environment(env, signed)
}

pub struct CloudKitTransport {
    profile: TranscriptSyncProfile,
    container: String,
    zone: String,
    subscription: String,
    team_id: String,
    request_timeout: Duration,
    sink: fn(&str, &str),
    native: Option<Arc<dyn NativeObserver>>,
    helper: Mutex<Option<CloudKitHelper>>,
}

impl CloudKitTransport {
    /// Validate `config`. No process starts until the first call that needs the helper.
    pub fn new(config: CloudKitConfig) -> Result<Self, String> {
        check_ident("container", &config.container)?;
        check_ident("zone", &config.zone)?;
        check_ident("subscription", &config.subscription)?;
        if !rightkit_child_process::is_team_id(&config.team_id) {
            return Err("cloudkit config: team_id is not an Apple team identifier".to_string());
        }
        // The native observer is signed for Production; it never runs in a Development session.
        let native = config
            .native
            .filter(|_| sync_core::native_observer_allowed(env_or_production()));
        Ok(Self {
            profile: config.profile,
            container: config.container,
            zone: config.zone,
            subscription: config.subscription,
            team_id: config.team_id,
            request_timeout: Duration::from_secs(config.request_timeout_secs.clamp(5, 600)),
            sink: config.event_sink,
            native,
            helper: Mutex::new(None),
        })
    }

    fn emit(&self, name: &str, detail: &str) {
        (self.sink)(name, &super::safe_detail(detail));
    }

    /// The helper if one was already constructed. Holds the lock only for the clone.
    fn existing_helper(&self) -> Result<Option<CloudKitHelper>, String> {
        Ok(self
            .helper
            .lock()
            .map_err(|_| "cloud sync helper lock poisoned".to_string())?
            .clone())
    }

    /// Construct (once) the helper handle, DISABLED. The placement, signature and environment checks run
    /// OUTSIDE the lock (they run `codesign` for up to 35 seconds), so a caller that only wants
    /// the existing helper never waits behind them. Constructing the handle starts no process;
    /// the first request does.
    fn helper(&self) -> Result<CloudKitHelper, String> {
        if let Some(helper) = self.existing_helper()? {
            return Ok(helper);
        }
        let env = environment()?;
        let name = self.profile.helper_bundle_name(env);
        let placed =
            rightkit_child_process::resolve_bundled_login_item(name).map_err(|reason| {
                self.emit(
                    "helper_path_rejected",
                    &format!("helper={name} reason={reason}"),
                );
                "CloudKit helper not bundled — CloudKit sync unavailable".to_string()
            })?;
        let path = rightkit_child_process::verify_team_signature(&placed, &self.team_id).map_err(
            |reason| {
                self.emit(
                    "helper_untrusted",
                    &format!("helper={name} reason={reason}"),
                );
                "CloudKit helper is not signed by the expected team".to_string()
            },
        )?;
        verify_signed_environment(env, &path)?;
        let config = HelperConfig::new(path)
            .cloudkit(&self.container, &self.zone, &self.subscription)
            .timeout(self.request_timeout);
        let helper = CloudKitHelper::new(
            config,
            Arc::new(TelemetryObserver {
                sink: self.sink,
                native: self.native.clone(),
            }),
        );
        // The crate constructs a helper ENABLED. Turn it off here, before the value is published
        // to any other thread (the lock below is the only way out of this function), so the
        // account can be observed (`status`) without any transfer being allowed (rev 68).
        helper.auth().set_enabled(false);
        let mut guard = self
            .helper
            .lock()
            .map_err(|_| "cloud sync helper lock poisoned".to_string())?;
        if let Some(winner) = guard.as_ref() {
            // Another thread finished first; ours was never spawned and is dropped unused.
            return Ok(winner.clone());
        }
        *guard = Some(helper.clone());
        Ok(helper)
    }
}

impl SyncTransport for CloudKitTransport {
    fn state_file_name(&self, base: &str) -> String {
        sync_core::env_file_name(base, env_or_production())
    }

    fn legacy_import(&self) -> bool {
        env_or_production() == CloudKitEnvironment::Production
    }

    fn prepare(&self) -> Result<(), String> {
        self.helper().map(|_| ())
    }

    fn backend(&self) -> Result<Arc<dyn Backend>, String> {
        Ok(Arc::new(self.helper()?.backend()))
    }

    fn auth(&self) -> Result<Arc<dyn AuthProvider>, String> {
        Ok(Arc::new(self.helper()?.auth()))
    }

    fn observer(&self) -> Arc<dyn SyncObserver> {
        Arc::new(TelemetryObserver {
            sink: self.sink,
            native: self.native.clone(),
        })
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        if !enabled {
            // Cancelling needs a helper only if one exists (nothing is in flight otherwise).
            // Never constructs one. A helper constructed later starts DISABLED (`helper`).
            if let Some(helper) = self.existing_helper()? {
                helper.auth().set_enabled(false);
            }
            return Ok(());
        }
        self.helper()?.auth().set_enabled(true);
        Ok(())
    }

    fn confirmed_stopped(&self) -> Option<bool> {
        // The existing helper only; an absent helper or a poisoned lock is unknown, not a stop.
        match self.existing_helper() {
            Ok(Some(helper)) => Some(!helper.auth().enabled()),
            _ => None,
        }
    }

    fn native_observer(&self) -> Option<Arc<dyn NativeObserver>> {
        self.native.clone()
    }
}
