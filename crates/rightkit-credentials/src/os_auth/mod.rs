//! OS user-presence authentication (PK08-7): one blocking "prove the device owner is here" prompt.
//!
//! * macOS: `LocalAuthentication` (`LAContext`, policy `deviceOwnerAuthentication`: Touch ID,
//!   Apple Watch or the account password).
//! * Windows: Windows Hello (`UserConsentVerifier`; PIN, fingerprint or face). Hello only: there is
//!   no CredUI or password fallback, so a user with no Hello method set up gets
//!   [`Availability::NotEnrolled`] / [`OsAuthError::NotEnrolled`] and no prompt is shown; the
//!   caller (the app-lock coordinator) supplies any other fallback.
//! * Android: an injected [`BiometricBridge`] over `BiometricPrompt`, wired like
//!   [`crate::android::KeystoreBridge`] (the JNI side belongs to the `rightkit-android` crate).
//! * Everything else: [`Availability::Unsupported`] and [`OsAuthError::Unsupported`].
//!
//! This module gates a UI action; it is not a secret store. It reads, releases and returns no
//! credential, exposes nothing to a webview, and has no serde surface. A success is a
//! [`Verified`] token for one prompt that the OS answered positively, nothing more: the caller
//! decides how long that is fresh and what it unlocks. Every path other than an explicit OS
//! success is an error, so a caller that treats `Err` as "stay locked" fails closed.
//!
//! A prompt is only ever shown by [`OsAuthenticator::authenticate`], which a person's action must
//! trigger; [`OsAuthenticator::availability`] is passive and never prompts. The calls block, so
//! run them off the UI thread. At most one prompt is open per process at a time (a second
//! concurrent call is [`OsAuthError::Busy`], never queued). The test guards behave as for the
//! credential store: a tripped guard is [`OsAuthError::Guarded`] and the OS is never asked.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::store::{env_tripped, UNIVERSAL_GUARD_ENV};

#[cfg(target_os = "macos")]
mod apple;
#[cfg(windows)]
mod hello;

/// Whether the OS can authenticate the device owner right now. Never prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Availability {
    /// A prompt can be shown.
    Available,
    /// The device has no passcode, PIN, Hello or biometric set up for this user. On Windows this
    /// is also the answer for a user with no Hello method (there is no password fallback).
    NotEnrolled,
    /// Policy (MDM, group policy) disables the method.
    DisabledByPolicy,
    /// The authentication hardware or service is busy; try again shortly.
    Busy,
    /// No usable authenticator (no hardware, no UI session, no bridge installed, OS too old).
    Unavailable,
    /// This platform has no OS authentication in this crate.
    Unsupported,
    /// A test/dev guard variable is set, so the OS is not consulted.
    Guarded,
}

impl Availability {
    pub fn is_available(self) -> bool {
        self == Availability::Available
    }

    /// A stable kebab-case code, safe to log or send across an IPC boundary.
    pub fn code(self) -> &'static str {
        match self {
            Availability::Available => "available",
            Availability::NotEnrolled => "not-enrolled",
            Availability::DisabledByPolicy => "disabled-by-policy",
            Availability::Busy => "busy",
            Availability::Unavailable => "unavailable",
            Availability::Unsupported => "unsupported",
            Availability::Guarded => "guarded",
        }
    }
}

/// Why no [`Verified`] was produced. No variant carries user text, a message from the OS, or any
/// secret; `Os` holds an operation name and the OS status code only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum OsAuthError {
    #[error("OS authentication is not supported on this platform")]
    Unsupported,
    #[error("OS authentication is unavailable")]
    Unavailable,
    #[error("no passcode, PIN or biometric is set up on this device")]
    NotEnrolled,
    #[error("OS authentication is disabled by policy")]
    DisabledByPolicy,
    #[error("OS authentication is busy")]
    Busy,
    #[error("OS authentication is disabled by a test guard")]
    Guarded,
    /// The person dismissed the prompt (cancel, or the fallback the prompt offered). A prompt
    /// cancelled by the system or the app is [`OsAuthError::SystemCancelled`] instead.
    #[error("OS authentication was cancelled")]
    Cancelled,
    /// The system or the app cancelled the prompt (app moved to the background, another prompt
    /// took over, the OS interrupted it). The person made no decision.
    #[error("OS authentication was cancelled by the system or the app")]
    SystemCancelled,
    /// The person was shown the prompt and did not authenticate.
    #[error("OS authentication failed")]
    Failed,
    /// Too many failed attempts; the OS refuses further tries until its own unlock.
    #[error("OS authentication is locked out")]
    LockedOut,
    #[error("OS authentication timed out")]
    Timeout,
    #[error("invalid OS authentication request: {0}")]
    Invalid(&'static str),
    #[error("OS authentication error during {op} (code {code})")]
    Os { op: &'static str, code: i64 },
}

impl OsAuthError {
    /// A stable kebab-case code, safe to log or send across an IPC boundary.
    pub fn code(&self) -> &'static str {
        match self {
            OsAuthError::Unsupported => "unsupported",
            OsAuthError::Unavailable => "unavailable",
            OsAuthError::NotEnrolled => "not-enrolled",
            OsAuthError::DisabledByPolicy => "disabled-by-policy",
            OsAuthError::Busy => "busy",
            OsAuthError::Guarded => "guarded",
            OsAuthError::Cancelled => "cancelled",
            OsAuthError::SystemCancelled => "system-cancelled",
            OsAuthError::Failed => "failed",
            OsAuthError::LockedOut => "locked-out",
            OsAuthError::Timeout => "timeout",
            OsAuthError::Invalid(_) => "invalid",
            OsAuthError::Os { .. } => "os-error",
        }
    }

    /// True when the person was in front of the prompt and declined or failed it (a caller that
    /// counts failed attempts counts only these), as opposed to the OS being unable to ask or the
    /// system or app cancelling it ([`OsAuthError::SystemCancelled`] is not a user decision).
    pub fn is_user_decision(&self) -> bool {
        matches!(
            self,
            OsAuthError::Cancelled | OsAuthError::Failed | OsAuthError::LockedOut
        )
    }
}

/// Proof that one OS prompt was answered positively. It cannot be built outside this crate, is
/// not serialisable and carries no secret.
#[must_use = "a Verified is the only success value of OS authentication; drop it deliberately"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verified {
    at: Instant,
}

impl Verified {
    pub(crate) fn now() -> Self {
        Verified { at: Instant::now() }
    }

    /// When the OS reported success (monotonic clock).
    pub fn verified_at(&self) -> Instant {
        self.at
    }

    /// Time since the OS reported success, for the caller's own freshness window.
    pub fn elapsed(&self) -> Duration {
        self.at.elapsed()
    }
}

/// One authentication request.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    reason: &'a str,
    timeout: Duration,
    owner_window: Option<isize>,
    guard_env: Option<&'a str>,
}

impl<'a> Request<'a> {
    /// Longest accepted `reason`, in bytes.
    pub const MAX_REASON_BYTES: usize = 200;
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
    pub const MIN_TIMEOUT: Duration = Duration::from_secs(1);
    pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);

    /// `reason` is shown by the OS prompt ("Unlock Pulse"): non-empty after trimming, at most
    /// [`Self::MAX_REASON_BYTES`], no control characters. Plain text only.
    pub fn new(reason: &'a str) -> Self {
        Request {
            reason,
            timeout: Self::DEFAULT_TIMEOUT,
            owner_window: None,
            guard_env: None,
        }
    }

    /// How long to wait for the person (default [`Self::DEFAULT_TIMEOUT`]; from
    /// [`Self::MIN_TIMEOUT`] to [`Self::MAX_TIMEOUT`]). On expiry the prompt is cancelled and the
    /// call is [`OsAuthError::Timeout`]; a late success is discarded.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The native handle of the window the prompt belongs to: an `HWND` on Windows, where an
    /// unpackaged desktop app must supply it so Windows Hello appears in front of that window. A
    /// zero handle is rejected. Ignored on other platforms.
    pub fn owner_window(mut self, handle: isize) -> Self {
        self.owner_window = Some(handle);
        self
    }

    /// An extra guard variable (an app's own `*_NO_KEYCHAIN`) honoured in addition to
    /// [`UNIVERSAL_GUARD_ENV`], with the credential store's truthiness rules.
    pub fn guard_env(mut self, name: &'a str) -> Self {
        self.guard_env = Some(name);
        self
    }

    pub fn reason(&self) -> &str {
        self.reason
    }

    fn validate(&self) -> Result<(), OsAuthError> {
        if self.reason.trim().is_empty() {
            return Err(OsAuthError::Invalid("reason is empty"));
        }
        if self.reason.len() > Self::MAX_REASON_BYTES {
            return Err(OsAuthError::Invalid("reason is too long"));
        }
        if self.reason.chars().any(char::is_control) {
            return Err(OsAuthError::Invalid("reason has a control character"));
        }
        if !(Self::MIN_TIMEOUT..=Self::MAX_TIMEOUT).contains(&self.timeout) {
            return Err(OsAuthError::Invalid("timeout is out of range"));
        }
        if self.owner_window == Some(0) {
            return Err(OsAuthError::Invalid("owner window handle is zero"));
        }
        if let Some(name) = self.guard_env {
            if name.is_empty() || name.contains('\0') || name.contains('=') {
                return Err(OsAuthError::Invalid("guard variable name is invalid"));
            }
        }
        Ok(())
    }

    fn guarded(&self) -> bool {
        env_tripped(UNIVERSAL_GUARD_ENV) || self.guard_env.is_some_and(env_tripped)
    }
}

/// The seam an app-lock coordinator holds so its logic is testable without a real prompt.
/// [`SystemAuthenticator`] is the OS implementation.
pub trait OsAuthenticator: Send + Sync {
    /// Passive: never shows UI.
    fn availability(&self) -> Availability;

    /// Shows the OS prompt and blocks until the person answers, the request times out or the OS
    /// fails. `Ok` only when the OS reported success.
    fn authenticate(&self, request: &Request<'_>) -> Result<Verified, OsAuthError>;
}

/// The real OS authenticator for this platform.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemAuthenticator;

/// One prompt per process: a second concurrent call is `Busy`.
static PROMPT: AtomicBool = AtomicBool::new(false);

/// Shared with a native worker: timing out the caller does not free an unfinished prompt.
struct PromptLease;

impl PromptLease {
    fn acquire() -> Result<Arc<Self>, OsAuthError> {
        PROMPT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| OsAuthError::Busy)?;
        Ok(Arc::new(Self))
    }
}

impl Drop for PromptLease {
    fn drop(&mut self) {
        PROMPT.store(false, Ordering::Release);
    }
}

impl OsAuthenticator for SystemAuthenticator {
    fn availability(&self) -> Availability {
        if env_tripped(UNIVERSAL_GUARD_ENV) {
            return Availability::Guarded;
        }
        platform::availability()
    }

    fn authenticate(&self, request: &Request<'_>) -> Result<Verified, OsAuthError> {
        request.validate()?;
        let one_prompt = PromptLease::acquire()?;
        if request.guarded() {
            return Err(OsAuthError::Guarded);
        }
        #[cfg(windows)]
        {
            platform::authenticate(request, one_prompt)
        }
        #[cfg(not(windows))]
        {
            let _one_prompt = one_prompt;
            platform::authenticate(request)
        }
    }
}

/// [`SystemAuthenticator::availability`].
pub fn availability() -> Availability {
    SystemAuthenticator.availability()
}

/// [`SystemAuthenticator::authenticate`].
pub fn authenticate(request: &Request<'_>) -> Result<Verified, OsAuthError> {
    SystemAuthenticator.authenticate(request)
}

// ---- Android: injected bridge ---------------------------------------------------------------

/// The Android side: `androidx.biometric.BiometricPrompt` with `BIOMETRIC_STRONG |
/// DEVICE_CREDENTIAL`, no `CryptoObject` (this primitive gates a UI action; it does not release a
/// key). The `rightkit-android` bridge crate implements it, as for
/// [`crate::android::KeystoreBridge`].
///
/// Contract: `availability` never shows UI. `authenticate` shows the prompt, blocks until the
/// callback fires and returns `Ok(())` only from `onAuthenticationSucceeded`; every error,
/// cancel and lockout callback maps to the matching [`OsAuthError`] (a cancel the person did not
/// make is [`OsAuthError::SystemCancelled`], which `is_user_decision` does not count); a missing
/// `Activity` or `JavaVM` is [`OsAuthError::Unavailable`]. It must return within `timeout`
/// (cancelling the prompt and returning [`OsAuthError::Timeout`]); the crate cannot interrupt a
/// bridge call.
///
/// PK08-7 Android policy (rightkit#771): `BIOMETRIC_STRONG | DEVICE_CREDENTIAL` is only a
/// supported prompt combination from API 30; on API 28/29 — or when the level is unknown — this
/// crate returns [`Availability::Unavailable`]/[`OsAuthError::Unavailable`] before any prompt
/// opens, with no `BIOMETRIC_WEAK` downgrade. Bridges report the device level through
/// [`BiometricBridge::android_api_level`] (the future rightkit-android bridge reports
/// `Build.VERSION.SDK_INT`); the refusal lives here so no bridge can prompt below API 30.
pub trait BiometricBridge: Send + Sync + 'static {
    fn availability(&self) -> Availability;
    fn authenticate(&self, reason: &str, timeout: Duration) -> Result<(), OsAuthError>;
    /// The device's Android API level (`Build.VERSION.SDK_INT`), when the bridge can read it.
    /// The default `None` (unknown) fails closed under the PK08-7 policy above.
    fn android_api_level(&self) -> Option<u32> {
        None
    }
}

/// The lowest Android API level whose `BiometricPrompt` supports the strong-authenticator
/// combination this primitive requires (PK08-7).
const ANDROID_MIN_API_LEVEL: u32 = 30;

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn android_prompt_allowed(api_level: Option<u32>) -> bool {
    api_level.is_some_and(|level| level >= ANDROID_MIN_API_LEVEL)
}

static BRIDGE: Mutex<Option<Arc<dyn BiometricBridge>>> = Mutex::new(None);

/// Wire the process-wide Android bridge. Call once at startup. A second call replaces the first.
/// Compiles on every platform; only an Android build consults it.
pub fn install_biometric_bridge(bridge: Arc<dyn BiometricBridge>) {
    *BRIDGE.lock().unwrap_or_else(PoisonError::into_inner) = Some(bridge);
}

#[cfg(target_os = "android")]
fn installed_bridge() -> Option<Arc<dyn BiometricBridge>> {
    BRIDGE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn bridge_availability(bridge: Option<Arc<dyn BiometricBridge>>) -> Availability {
    match bridge {
        Some(b) if android_prompt_allowed(b.android_api_level()) => b.availability(),
        _ => Availability::Unavailable,
    }
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn bridge_authenticate(
    bridge: Option<Arc<dyn BiometricBridge>>,
    request: &Request<'_>,
) -> Result<Verified, OsAuthError> {
    let bridge = bridge.ok_or(OsAuthError::Unavailable)?;
    if !android_prompt_allowed(bridge.android_api_level()) {
        return Err(OsAuthError::Unavailable);
    }
    bridge
        .authenticate(request.reason.trim(), request.timeout)
        .map(|()| Verified::now())
}

// ---- Pure OS-code mappings (tested on every host) -------------------------------------------

/// `LAError` codes (`LocalAuthentication/LAError.h`) in the `com.apple.LocalAuthentication`
/// domain.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn map_la_code(code: i64) -> OsAuthError {
    match code {
        // LAErrorAuthenticationFailed
        -1 => OsAuthError::Failed,
        // LAErrorUserCancel, LAErrorUserFallback (no fallback button is offered): the person acted
        -2 | -3 => OsAuthError::Cancelled,
        // LAErrorSystemCancel, LAErrorAppCancel: no person decision
        -4 | -9 => OsAuthError::SystemCancelled,
        // LAErrorPasscodeNotSet, LAErrorBiometryNotEnrolled
        -5 | -7 => OsAuthError::NotEnrolled,
        // LAErrorBiometryNotAvailable, LAErrorNotInteractive
        -6 | -1004 => OsAuthError::Unavailable,
        // LAErrorBiometryLockout
        -8 => OsAuthError::LockedOut,
        other => OsAuthError::Os {
            op: "evaluatePolicy",
            code: other,
        },
    }
}

/// `Windows.Security.Credentials.UI.UserConsentVerificationResult`. Only `Verified` is success;
/// an unknown value is an error, never a pass.
#[cfg_attr(not(windows), allow(dead_code))]
fn map_hello_result(value: i32) -> Result<(), OsAuthError> {
    match value {
        // Verified
        0 => Ok(()),
        // DeviceNotPresent
        1 => Err(OsAuthError::Unavailable),
        // NotConfiguredForUser
        2 => Err(OsAuthError::NotEnrolled),
        // DisabledByPolicy
        3 => Err(OsAuthError::DisabledByPolicy),
        // DeviceBusy
        4 => Err(OsAuthError::Busy),
        // RetriesExhausted
        5 => Err(OsAuthError::LockedOut),
        // Canceled
        6 => Err(OsAuthError::Cancelled),
        other => Err(OsAuthError::Os {
            op: "RequestVerification",
            code: i64::from(other),
        }),
    }
}

/// `Windows.Security.Credentials.UI.UserConsentVerifierAvailability`.
#[cfg_attr(not(windows), allow(dead_code))]
fn map_hello_availability(value: i32) -> Availability {
    match value {
        0 => Availability::Available,
        2 => Availability::NotEnrolled,
        3 => Availability::DisabledByPolicy,
        4 => Availability::Busy,
        // 1 (DeviceNotPresent) and anything unknown
        _ => Availability::Unavailable,
    }
}

// ---- Platform dispatch ----------------------------------------------------------------------

#[cfg(target_os = "macos")]
use apple as platform;
#[cfg(windows)]
use hello as platform;

#[cfg(target_os = "android")]
mod platform {
    use super::{
        bridge_authenticate, bridge_availability, installed_bridge, Availability, OsAuthError,
        Request, Verified,
    };

    pub fn availability() -> Availability {
        bridge_availability(installed_bridge())
    }

    pub fn authenticate(request: &Request<'_>) -> Result<Verified, OsAuthError> {
        bridge_authenticate(installed_bridge(), request)
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "android")))]
mod platform {
    use super::{Availability, OsAuthError, Request, Verified};

    pub fn availability() -> Availability {
        Availability::Unsupported
    }

    pub fn authenticate(_request: &Request<'_>) -> Result<Verified, OsAuthError> {
        Err(OsAuthError::Unsupported)
    }
}

// ---- Debug/test authenticator ---------------------------------------------------------------

/// A scripted authenticator for coordinator tests. Compiled only under the `memory-backend` feature,
/// like [`crate::MemoryBackend`]: a release build has no way to mint a [`Verified`]
/// without the OS.
#[cfg(feature = "memory-backend")]
pub struct ScriptedAuthenticator {
    availability: Availability,
    script: Mutex<std::collections::VecDeque<Result<(), OsAuthError>>>,
    prompts: std::sync::atomic::AtomicUsize,
}

#[cfg(feature = "memory-backend")]
impl ScriptedAuthenticator {
    /// `outcomes` are consumed one per `authenticate` call; an empty script answers
    /// `Err(OsAuthError::Cancelled)`.
    pub fn new(availability: Availability, outcomes: Vec<Result<(), OsAuthError>>) -> Self {
        ScriptedAuthenticator {
            availability,
            script: Mutex::new(outcomes.into()),
            prompts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// How many times `authenticate` reached the script (requests that failed validation do not
    /// count).
    pub fn prompts(&self) -> usize {
        self.prompts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(feature = "memory-backend")]
impl OsAuthenticator for ScriptedAuthenticator {
    fn availability(&self) -> Availability {
        self.availability
    }

    fn authenticate(&self, request: &Request<'_>) -> Result<Verified, OsAuthError> {
        request.validate()?;
        self.prompts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let next = self
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or(Err(OsAuthError::Cancelled));
        next.map(|()| Verified::now())
    }
}
