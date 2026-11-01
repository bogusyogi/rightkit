//! Temporary Chromium/Electron AX-tree observation leases, running-application
//! identity, and the Chromium prewake watcher.

use core_foundation::base::TCFType;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread};
use objc2_app_kit::{
    NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey,
    NSWorkspaceDidActivateApplicationNotification,
};
use objc2_foundation::{NSNotification, NSObject};

use super::ffi::{ax_app_element, ax_bool, ax_set_bool, AXIsProcessTrusted, AXUIElementRef};
use super::frontmost::frontmost_pid;

/// Temporary AX tree observation enabled for Chromium/Electron targets.
///
/// Chromium keeps its renderer accessibility tree dormant unless this private
/// AX attribute is enabled. The lease changes only an observed `false` state;
/// an already-enabled target is left alone. Drop restores `false` only after
/// validating that same pid still belongs to same bundle and bundle path.
/// No broad enhanced-accessibility mode escalation is used.
pub(super) struct AccessibilityObservationLease {
    pid: i32,
    bundle_identifier: String,
    bundle_path: String,
    /// Which app-level flag this lease turned on and must turn back off:
    /// `AXManualAccessibility` (Electron) or `AXEnhancedUserInterface`
    /// (Chromium embedded in a native shell, e.g. ChatGPT/Codex desktop,
    /// which reports the Electron flag as unsupported).
    attribute: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ObservationKey {
    pid: i32,
    bundle_identifier: String,
    bundle_path: String,
    attribute: &'static str,
}

pub(super) fn observation_clients(
) -> &'static std::sync::Mutex<std::collections::HashMap<ObservationKey, usize>> {
    static CLIENTS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<ObservationKey, usize>>,
    > = std::sync::OnceLock::new();
    CLIENTS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(super) fn retain_managed_observation(
    identity: &RunningApplicationIdentity,
) -> Option<AccessibilityObservationLease> {
    let mut clients = observation_clients().lock().ok()?;
    let (key, count) = clients.iter_mut().find(|(key, _)| {
        key.pid == identity.pid
            && key.bundle_identifier == identity.bundle_identifier
            && key.bundle_path == identity.bundle_path
    })?;
    *count = count.saturating_add(1);
    Some(AccessibilityObservationLease {
        pid: key.pid,
        bundle_identifier: key.bundle_identifier.clone(),
        bundle_path: key.bundle_path.clone(),
        attribute: key.attribute,
    })
}

/// A lease granted for a recording whose focus pin then MISSED (typically the
/// tree was still waking: ChatGPT/Codex needs ~2.5 s after the flag is set,
/// the start-time pin allows 350 ms). Parked here so the tree stays awake for
/// the recording and the delivery-time probe finds the field; released when
/// the runtime clears its focus pin, or replaced by the next capture.
pub(super) static PENDING_OBSERVATION_HOLD: std::sync::Mutex<
    Option<AccessibilityObservationLease>,
> = std::sync::Mutex::new(None);

/// Drop a parked observation lease (restores the target's flag). Called by
/// the runtime wherever it clears its focus pin.
pub fn release_pending_accessibility_observation() {
    if let Ok(mut slot) = PENDING_OBSERVATION_HOLD.lock() {
        *slot = None;
    }
}

pub(super) struct RunningApplicationIdentity {
    pub(super) pid: i32,
    pub(super) bundle_identifier: String,
    pub(super) bundle_path: String,
}

pub(super) const CHROMIUM_BUNDLE_IDENTIFIERS: &[&str] = &[
    "com.google.Chrome",
    "com.google.Chrome.canary",
    "com.google.Chrome.beta",
    "com.google.Chrome.dev",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "com.microsoft.edgemac.Beta",
    "com.microsoft.edgemac.Dev",
    "com.operasoftware.Opera",
    "com.operasoftware.OperaDeveloper",
    "com.vivaldi.Vivaldi",
    "org.chromium.Chromium",
    "company.thebrowser.Browser",
    "com.arc.Browser",
    // ChatGPT/Codex desktop embeds Chromium in Codex Framework rather than
    // Electron Framework, so it needs explicit renderer observation.
    "com.openai.codex",
];

pub(super) fn app_bundle_path(app: &NSRunningApplication) -> Option<String> {
    app.bundleURL()?.path().map(|path| path.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningApplicationIdentityMiss {
    InvalidPid,
    NotFound,
    Terminated,
    MissingBundleIdentifier,
    MissingBundleUrl,
    MissingBundlePath,
}

impl RunningApplicationIdentityMiss {
    pub fn label(self) -> &'static str {
        match self {
            Self::InvalidPid => "invalid_pid",
            Self::NotFound => "running_app_not_found",
            Self::Terminated => "running_app_terminated",
            Self::MissingBundleIdentifier => "missing_bundle_identifier",
            Self::MissingBundleUrl => "missing_bundle_url",
            Self::MissingBundlePath => "missing_bundle_path",
        }
    }
}

pub(super) fn app_bundle_path_detailed(
    app: &NSRunningApplication,
) -> Result<String, RunningApplicationIdentityMiss> {
    let url = app
        .bundleURL()
        .ok_or(RunningApplicationIdentityMiss::MissingBundleUrl)?;
    url.path()
        .map(|path| path.to_string())
        .ok_or(RunningApplicationIdentityMiss::MissingBundlePath)
}

pub(super) fn running_application_identity_detailed(
    pid: i32,
) -> Result<RunningApplicationIdentity, RunningApplicationIdentityMiss> {
    if pid <= 0 {
        return Err(RunningApplicationIdentityMiss::InvalidPid);
    }
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .ok_or(RunningApplicationIdentityMiss::NotFound)?;
    if app.isTerminated() {
        return Err(RunningApplicationIdentityMiss::Terminated);
    }
    let bundle_identifier = app
        .bundleIdentifier()
        .map(|value| value.to_string())
        .ok_or(RunningApplicationIdentityMiss::MissingBundleIdentifier)?;
    let bundle_path = app_bundle_path_detailed(&app)?;
    Ok(RunningApplicationIdentity {
        pid,
        bundle_identifier,
        bundle_path,
    })
}

/// Compatibility wrapper for callers that only need an identity.
pub(super) fn running_application_identity(pid: i32) -> Option<RunningApplicationIdentity> {
    running_application_identity_detailed(pid).ok()
}

pub(super) fn is_electron_bundle_path(path: &str) -> bool {
    std::path::Path::new(path)
        .join("Contents/Frameworks/Electron Framework.framework")
        .is_dir()
}

pub(super) fn same_running_application(
    pid: i32,
    bundle_identifier: &str,
    bundle_path: &str,
) -> bool {
    let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return false;
    };
    let Some(current_bundle_identifier) = app.bundleIdentifier().map(|value| value.to_string())
    else {
        return false;
    };
    if app.isTerminated() || current_bundle_identifier != bundle_identifier {
        return false;
    }
    app_bundle_path(&app).as_deref() == Some(bundle_path)
}

pub(super) fn frontmost_identity_matches(identity: &RunningApplicationIdentity) -> bool {
    if frontmost_pid() != Some(identity.pid) {
        return false;
    }
    let Some(frontmost) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
        return false;
    };
    frontmost.processIdentifier() == identity.pid
        && frontmost
            .bundleIdentifier()
            .map(|value| value.to_string())
            .as_deref()
            == Some(identity.bundle_identifier.as_str())
        && app_bundle_path(&frontmost).as_deref() == Some(identity.bundle_path.as_str())
}

// Kept: source-text contract in
// src-tauri/tests/mac_focus_pin_contract_tests.rs asserts the identity-check-
// before-AX-write ordering starting at this signature. Its only production
// caller (the diagnostic probe) was deleted as dead code (examples/-only);
// the invariant itself still lives in `set_accessibility_flag_for_identity`.
#[allow(dead_code)]
pub(super) unsafe fn set_manual_accessibility_for_identity(
    pid: i32,
    bundle_identifier: &str,
    bundle_path: &str,
    enabled: bool,
) -> bool {
    set_accessibility_flag_for_identity(
        pid,
        bundle_identifier,
        bundle_path,
        "AXManualAccessibility",
        enabled,
    )
}

/// Set an app-level accessibility flag and confirm it by reading it back.
/// The read-back matters: ChatGPT/Codex desktop answers the setter with
/// kAXErrorNotImplemented (-25208) and flips the value anyway, so the return
/// code alone would refuse a wake that worked.
pub(super) unsafe fn set_accessibility_flag_for_identity(
    pid: i32,
    bundle_identifier: &str,
    bundle_path: &str,
    attribute: &'static str,
    enabled: bool,
) -> bool {
    if !same_running_application(pid, bundle_identifier, bundle_path) {
        return false;
    }
    let Some(app) = ax_app_element(pid) else {
        return false;
    };
    let app_ref = app.as_CFTypeRef() as AXUIElementRef;
    // Identity check is deliberately adjacent to AX setter: process identity
    // may change while creating/configuring the AX application element.
    if !same_running_application(pid, bundle_identifier, bundle_path) {
        return false;
    }
    let result = ax_set_bool(app_ref, attribute, enabled);
    let read_back = ax_bool(app_ref, attribute) == Some(enabled);
    result || read_back
}

pub(super) fn begin_accessibility_observation(
    identity: &RunningApplicationIdentity,
) -> Result<Option<AccessibilityObservationLease>, ()> {
    if let Some(lease) = retain_managed_observation(identity) {
        return Ok(Some(lease));
    }
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return Err(());
        }
        let Some(current_identity) = running_application_identity(identity.pid) else {
            return Err(());
        };
        if current_identity.bundle_identifier != identity.bundle_identifier
            || current_identity.bundle_path != identity.bundle_path
        {
            return Err(());
        }
        if !CHROMIUM_BUNDLE_IDENTIFIERS.contains(&identity.bundle_identifier.as_str())
            && !is_electron_bundle_path(&identity.bundle_path)
        {
            return Ok(None);
        }
        // Never turn on renderer observation for an app that is no longer the
        // focused/frontmost target captured for this recording.
        if !frontmost_identity_matches(identity) {
            return Err(());
        }
        let Some(app_element) = ax_app_element(identity.pid) else {
            return Err(());
        };
        let current = ax_bool(
            app_element.as_CFTypeRef() as AXUIElementRef,
            "AXManualAccessibility",
        );
        drop(app_element);
        // An already-enabled target is owned by its own lifecycle; leave it
        // unchanged. A definite false is leased and restored later.
        //
        // Unknown means the Electron flag is unsupported: Chromium embedded
        // in a native shell (ChatGPT/Codex desktop, "Codex Framework") does
        // not implement AXManualAccessibility at all. On a cold
        // ChatGPT process: AXManualAccessibility reads None and
        // its setter fails; AXEnhancedUserInterface (what VoiceOver sets)
        // reads false, flips to true, and ~2.5 s later the AXWebArea and the
        // focused AXTextArea appear. Without this fallback the first
        // recording into ChatGPT after every launch fails focus pin
        // (reason=observation_failed) and falls to the mini clipboard.
        let attribute: &'static str = match current {
            Some(true) => return Ok(None),
            Some(false) => "AXManualAccessibility",
            None => {
                let Some(app_element) = ax_app_element(identity.pid) else {
                    return Err(());
                };
                let enhanced = ax_bool(
                    app_element.as_CFTypeRef() as AXUIElementRef,
                    "AXEnhancedUserInterface",
                );
                drop(app_element);
                match enhanced {
                    Some(true) => return Ok(None),
                    Some(false) => "AXEnhancedUserInterface",
                    None => return Err(()),
                }
            }
        };
        if !set_accessibility_flag_for_identity(
            identity.pid,
            &identity.bundle_identifier,
            &identity.bundle_path,
            attribute,
            true,
        ) {
            return Err(());
        }
        let lease = AccessibilityObservationLease {
            pid: identity.pid,
            bundle_identifier: identity.bundle_identifier.clone(),
            bundle_path: identity.bundle_path.clone(),
            attribute,
        };
        if let Ok(mut clients) = observation_clients().lock() {
            clients.insert(
                ObservationKey {
                    pid: lease.pid,
                    bundle_identifier: lease.bundle_identifier.clone(),
                    bundle_path: lease.bundle_path.clone(),
                    attribute: lease.attribute,
                },
                1,
            );
        }
        if !same_running_application(
            identity.pid,
            &identity.bundle_identifier,
            &identity.bundle_path,
        ) || !frontmost_identity_matches(identity)
        {
            drop(lease);
            return Err(());
        }
        Ok(Some(lease))
    }
}

/// How many times one activation retries the wake while the same app is
/// still the latest activation. The system-wide AX focused application can
/// trail the NSWorkspace activation notification by a few tens of
/// milliseconds, and `begin_accessibility_observation` refuses a wake until
/// both agree. The retries are a bounded burst after an event, never a timer.
const PREWAKE_ATTEMPTS: u32 = 5;
const PREWAKE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// Held-lease bookkeeping for the Chromium prewake, pure so the pid-change
/// and stale-wake rules are testable without AppKit. `L` is the lease type;
/// dropping an `L` releases it.
///
/// Every activation of a pid other than the held or pending one takes a new
/// sequence number. A wake that finishes under an older number is stale
/// (another app activated while it ran) and its lease is handed back to be
/// dropped.
pub(super) struct PrewakeState<L> {
    held: Option<(i32, L)>,
    pending: Option<i32>,
    latest_seq: u64,
}

pub(super) struct PrewakeActivation<L> {
    /// Sequence number to wake under, or `None` when nothing needs to run
    /// (this pid already holds a lease, or its wake is already in flight).
    pub(super) wake_seq: Option<u64>,
    /// Lease held for a different pid, to drop outside the state lock.
    pub(super) released: Option<L>,
}

impl<L> PrewakeState<L> {
    pub(super) const fn new() -> Self {
        Self {
            held: None,
            pending: None,
            latest_seq: 0,
        }
    }

    /// Record that `pid` became the active application.
    pub(super) fn activate(&mut self, pid: i32) -> PrewakeActivation<L> {
        if self
            .held
            .as_ref()
            .is_some_and(|(held_pid, _)| *held_pid == pid)
            || self.pending == Some(pid)
        {
            return PrewakeActivation {
                wake_seq: None,
                released: None,
            };
        }
        // A different pid is now frontmost: the held lease is done.
        let released = self.held.take().map(|(_, lease)| lease);
        self.latest_seq += 1;
        self.pending = Some(pid);
        PrewakeActivation {
            wake_seq: Some(self.latest_seq),
            released,
        }
    }

    /// Whether the wake started under `seq` is still the latest activation.
    pub(super) fn is_current(&self, seq: u64) -> bool {
        self.latest_seq == seq
    }

    /// Record a finished wake. Returns a lease that must be dropped because
    /// the wake went stale while it ran.
    pub(super) fn finish(&mut self, seq: u64, pid: i32, lease: Option<L>) -> Option<L> {
        if seq != self.latest_seq {
            return lease;
        }
        self.pending = None;
        self.held = lease.map(|lease| (pid, lease));
        None
    }
}

static PREWAKE_STATE: std::sync::Mutex<PrewakeState<AccessibilityObservationLease>> =
    std::sync::Mutex::new(PrewakeState::new());

fn with_prewake_state<R>(
    f: impl FnOnce(&mut PrewakeState<AccessibilityObservationLease>) -> R,
) -> R {
    let mut state = PREWAKE_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut state)
}

/// Wake `pid` if it is Chromium-class, retrying while it stays the latest
/// activation. Runs on a background thread: AX calls can stall on a slow
/// target and must never run on the AppKit main thread.
fn prewake_worker(pid: i32, seq: u64) {
    objc2::rc::autoreleasepool(|_| {
        let mut lease = None;
        if let Some(identity) = running_application_identity(pid) {
            if is_chromium_class(&identity) {
                for attempt in 0..PREWAKE_ATTEMPTS {
                    if attempt > 0 {
                        std::thread::sleep(PREWAKE_RETRY_DELAY);
                    }
                    if !with_prewake_state(|state| state.is_current(seq)) {
                        break;
                    }
                    // `Ok(None)`: already awake, nothing to hold. `Err`: the
                    // AX focused app has not caught up yet; try again.
                    if let Ok(taken) = begin_accessibility_observation(&identity) {
                        lease = taken;
                        break;
                    }
                }
            }
        }
        let stale = with_prewake_state(|state| state.finish(seq, pid, lease));
        drop(stale);
    });
}

/// Handle one activation: update the held lease and run the wake off the
/// notification thread.
fn on_application_activated(pid: i32) {
    if pid <= 0 {
        return;
    }
    let activation = with_prewake_state(|state| state.activate(pid));
    drop(activation.released);
    let Some(seq) = activation.wake_seq else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("chromium-prewake".into())
        .spawn(move || prewake_worker(pid, seq));
    if let Err(error) = spawned {
        eprintln!("chromium_prewake_spawn_error {error}");
        let stale = with_prewake_state(|state| state.finish(seq, pid, None));
        drop(stale);
    }
}

struct ChromiumPrewakeObserverIvars;

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RightKitDesktopChromiumPrewakeObserver"]
    #[ivars = ChromiumPrewakeObserverIvars]
    struct ChromiumPrewakeObserver;

    impl ChromiumPrewakeObserver {
        #[unsafe(method(workspaceDidActivateApplication:))]
        fn workspace_did_activate_application(&self, notification: &NSNotification) {
            if let Some(pid) = activated_application_pid(notification) {
                on_application_activated(pid);
            }
        }
    }
);

impl ChromiumPrewakeObserver {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(ChromiumPrewakeObserverIvars);
        unsafe { msg_send![super(this), init] }
    }
}

/// Pid of the application a `NSWorkspaceDidActivateApplicationNotification`
/// announces, from `userInfo[NSWorkspaceApplicationKey]`.
fn activated_application_pid(notification: &NSNotification) -> Option<i32> {
    #[allow(unused_unsafe)]
    unsafe {
        let user_info = notification.userInfo()?;
        let key: &AnyObject = NSWorkspaceApplicationKey;
        let value = user_info.objectForKey(key)?;
        let app = value.downcast::<NSRunningApplication>().ok()?;
        Some(app.processIdentifier())
    }
}

/// Wake a Chromium-class app's accessibility tree the moment it comes to
/// the front, so the tree is already alive when the hotkey lands and the
/// start-time pin captures inside its 350 ms. Takes the same lease
/// `begin_accessibility_observation` takes and holds it until a different
/// application activates. Non-Chromium apps are never touched.
///
/// Event-driven, zero idle cost: one `NSWorkspaceDidActivateApplicationNotification`
/// observer, installed once, and no thread or timer while nothing activates
/// (issue #293: no the app thread may wake periodically at idle). Each
/// activation reads the pid from the notification and runs the AX work on a
/// short-lived background thread, never on the notification (AppKit main)
/// thread. The center's observer API is thread-safe and delivery follows the
/// poster, which is the main run loop; the observer is process-lifetime, so
/// it is deliberately never removed. The app that is already frontmost at
/// install is seeded once, since no activation will announce it.
pub fn start_chromium_prewake_watcher() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let workspace = NSWorkspace::sharedWorkspace();
        let observer = ChromiumPrewakeObserver::new();
        unsafe {
            workspace
                .notificationCenter()
                .addObserver_selector_name_object(
                    &observer,
                    sel!(workspaceDidActivateApplication:),
                    Some(NSWorkspaceDidActivateApplicationNotification),
                    None,
                );
        }
        // The center does not retain a selector-based observer; keep it
        // alive for the life of the process.
        std::mem::forget(observer);
        if let Some(app) = workspace.frontmostApplication() {
            on_application_activated(app.processIdentifier());
        }
    });
}

pub(super) fn is_chromium_class(identity: &RunningApplicationIdentity) -> bool {
    CHROMIUM_BUNDLE_IDENTIFIERS.contains(&identity.bundle_identifier.as_str())
        || is_electron_bundle_path(&identity.bundle_path)
}

impl Drop for AccessibilityObservationLease {
    fn drop(&mut self) {
        let key = ObservationKey {
            pid: self.pid,
            bundle_identifier: self.bundle_identifier.clone(),
            bundle_path: self.bundle_path.clone(),
            attribute: self.attribute,
        };
        if let Ok(mut clients) = observation_clients().lock() {
            if let Some(count) = clients.get_mut(&key) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    clients.remove(&key);
                }
            }
        }
        // A true flag does not reveal whether another accessibility client
        // acquired a dependency while the app held it. Ownership is now
        // uncertain, so leave it enabled instead of risking disabling AT.
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
