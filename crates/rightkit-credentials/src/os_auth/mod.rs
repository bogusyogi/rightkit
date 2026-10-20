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
pub trait BiometricBridge: Send + Sync + 'static {
    fn availability(&self) -> Availability;
    fn authenticate(&self, reason: &str, timeout: Duration) -> Result<(), OsAuthError>;
}

static BRIDGE: Mutex<Option<Arc<dyn BiometricBridge>>> = Mutex::new(None);

/// Wire the process-wide Android bridge. Call once at startup. A second call replaces the first.
/// Compiles on every platform; only an Android build consults it.
pub fn install_biometric_bridge(bridge: Arc<dyn BiometricBridge>) {
    *BRIDGE.lock().unwrap_or_else(PoisonError::into_inner) = Some(bridge);
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
fn installed_bridge() -> Option<Arc<dyn BiometricBridge>> {
    BRIDGE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
fn bridge_availability(bridge: Option<Arc<dyn BiometricBridge>>) -> Availability {
    bridge.map_or(Availability::Unavailable, |b| b.availability())
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
fn bridge_authenticate(
    bridge: Option<Arc<dyn BiometricBridge>>,
    request: &Request<'_>,
) -> Result<Verified, OsAuthError> {
    let bridge = bridge.ok_or(OsAuthError::Unavailable)?;
    bridge
        .authenticate(request.reason.trim(), request.timeout)
        .map(|()| Verified::now())
}

// ---- Pure OS-code mappings (tested on every host) -------------------------------------------

/// `LAError` codes (`LocalAuthentication/LAError.h`) in the `com.apple.LocalAuthentication`
/// domain.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
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
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
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
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
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

/// A scripted authenticator for coordinator tests. Compiled only with debug assertions or under
/// `cfg(test)`, like [`crate::MemoryBackend`]: a release build has no way to mint a [`Verified`]
/// without the OS.
#[cfg(any(test, debug_assertions))]
pub struct ScriptedAuthenticator {
    availability: Availability,
    script: Mutex<std::collections::VecDeque<Result<(), OsAuthError>>>,
    prompts: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, debug_assertions))]
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

#[cfg(any(test, debug_assertions))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_validation() {
        assert!(Request::new("Unlock Pulse").validate().is_ok());
        for bad in ["", "   ", "line\nbreak", "nul\0byte"] {
            assert!(
                matches!(Request::new(bad).validate(), Err(OsAuthError::Invalid(_))),
                "{bad:?}"
            );
        }
        let long = "x".repeat(Request::MAX_REASON_BYTES + 1);
        assert!(Request::new(&long).validate().is_err());
        let max = "x".repeat(Request::MAX_REASON_BYTES);
        assert!(Request::new(&max).validate().is_ok());
        let r = Request::new("ok");
        assert!(r
            .clone()
            .timeout(Duration::from_millis(999))
            .validate()
            .is_err());
        assert!(r
            .clone()
            .timeout(Duration::from_secs(601))
            .validate()
            .is_err());
        assert!(r.clone().timeout(Duration::from_secs(1)).validate().is_ok());
        assert!(r.clone().owner_window(0).validate().is_err());
        assert!(r.clone().owner_window(7).validate().is_ok());
        assert!(r.clone().guard_env("").validate().is_err());
        assert!(r.guard_env("A=B").validate().is_err());
    }

    #[test]
    fn invalid_request_is_rejected_before_the_guard_and_the_os() {
        let err = SystemAuthenticator.authenticate(&Request::new(""));
        assert!(matches!(err, Err(OsAuthError::Invalid(_))));
    }

    /// The guard and busy tests both go through `PROMPT`; they must not overlap.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn tripped_guard_never_reaches_the_os() {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        const VAR: &str = "RIGHTKIT_TEST_OS_AUTH_GUARD_TRIPPED";
        std::env::set_var(VAR, "1");
        let r = SystemAuthenticator.authenticate(&Request::new("Unlock").guard_env(VAR));
        std::env::remove_var(VAR);
        assert_eq!(r.unwrap_err(), OsAuthError::Guarded);
    }

    #[test]
    fn a_falsey_guard_value_does_not_trip_it() {
        const VAR: &str = "RIGHTKIT_TEST_OS_AUTH_GUARD_FALSEY";
        std::env::set_var(VAR, "0");
        let tripped = Request::new("Unlock").guard_env(VAR).guarded();
        std::env::remove_var(VAR);
        assert!(!tripped);
    }

    #[test]
    fn a_second_concurrent_prompt_is_busy() {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let _held = PromptLease::acquire().unwrap();
        let r = SystemAuthenticator.authenticate(&Request::new("Unlock"));
        assert_eq!(r.unwrap_err(), OsAuthError::Busy);
    }

    #[test]
    fn a_native_worker_keeps_the_gate_after_the_caller_times_out() {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let caller = PromptLease::acquire().unwrap();
        let worker = Arc::clone(&caller);
        drop(caller);
        assert!(matches!(PromptLease::acquire(), Err(OsAuthError::Busy)));
        drop(worker);
        assert!(PromptLease::acquire().is_ok());
    }

    #[cfg(not(any(target_os = "macos", windows, target_os = "android")))]
    #[test]
    fn unsupported_platforms_are_typed() {
        assert_eq!(platform::availability(), Availability::Unsupported);
        let r = platform::authenticate(&Request::new("Unlock"));
        assert_eq!(r.unwrap_err(), OsAuthError::Unsupported);
    }

    #[test]
    fn la_codes_map_and_unknown_codes_are_errors() {
        assert_eq!(map_la_code(-1), OsAuthError::Failed);
        assert_eq!(map_la_code(-2), OsAuthError::Cancelled);
        assert_eq!(map_la_code(-3), OsAuthError::Cancelled);
        assert_eq!(map_la_code(-4), OsAuthError::SystemCancelled);
        assert_eq!(map_la_code(-5), OsAuthError::NotEnrolled);
        assert_eq!(map_la_code(-6), OsAuthError::Unavailable);
        assert_eq!(map_la_code(-7), OsAuthError::NotEnrolled);
        assert_eq!(map_la_code(-8), OsAuthError::LockedOut);
        assert_eq!(map_la_code(-9), OsAuthError::SystemCancelled);
        assert_eq!(map_la_code(-1004), OsAuthError::Unavailable);
        let unknown = |code| OsAuthError::Os {
            op: "evaluatePolicy",
            code,
        };
        assert_eq!(map_la_code(-10), unknown(-10));
        assert_eq!(map_la_code(0), unknown(0));
    }

    #[test]
    fn hello_only_verified_is_success() {
        assert_eq!(map_hello_result(0), Ok(()));
        for v in [1, 2, 3, 4, 5, 6, 7, -1, 99] {
            assert!(map_hello_result(v).is_err(), "{v}");
        }
        assert_eq!(map_hello_result(6), Err(OsAuthError::Cancelled));
        assert_eq!(map_hello_result(5), Err(OsAuthError::LockedOut));
        assert_eq!(map_hello_result(2), Err(OsAuthError::NotEnrolled));
        assert_eq!(map_hello_availability(0), Availability::Available);
        assert_eq!(map_hello_availability(2), Availability::NotEnrolled);
        assert_eq!(map_hello_availability(77), Availability::Unavailable);
    }

    #[test]
    fn codes_are_stable_and_user_decisions_are_narrow() {
        assert_eq!(OsAuthError::LockedOut.code(), "locked-out");
        assert_eq!(OsAuthError::Os { op: "x", code: 1 }.code(), "os-error");
        assert_eq!(Availability::NotEnrolled.code(), "not-enrolled");
        assert!(OsAuthError::Failed.is_user_decision());
        assert!(OsAuthError::Cancelled.is_user_decision());
        assert!(OsAuthError::LockedOut.is_user_decision());
        assert!(!OsAuthError::SystemCancelled.is_user_decision());
        assert_eq!(OsAuthError::SystemCancelled.code(), "system-cancelled");
        assert!(!map_la_code(-4).is_user_decision());
        assert!(!map_la_code(-9).is_user_decision());
        assert!(map_la_code(-2).is_user_decision());
        assert!(!OsAuthError::Unavailable.is_user_decision());
        assert!(!OsAuthError::Timeout.is_user_decision());
        assert!(!Availability::Guarded.is_available());
        assert!(Availability::Available.is_available());
    }

    struct FakeBridge {
        outcome: Result<(), OsAuthError>,
        availability: Availability,
        seen: Mutex<Vec<(String, Duration)>>,
    }

    impl BiometricBridge for FakeBridge {
        fn availability(&self) -> Availability {
            self.availability
        }

        fn authenticate(&self, reason: &str, timeout: Duration) -> Result<(), OsAuthError> {
            self.seen.lock().unwrap().push((reason.to_owned(), timeout));
            self.outcome
        }
    }

    #[test]
    fn android_bridge_paths() {
        assert_eq!(bridge_availability(None), Availability::Unavailable);
        assert_eq!(
            bridge_authenticate(None, &Request::new("Unlock")).unwrap_err(),
            OsAuthError::Unavailable
        );
        let ok = Arc::new(FakeBridge {
            outcome: Ok(()),
            availability: Availability::Available,
            seen: Mutex::new(Vec::new()),
        });
        let bridge: Arc<dyn BiometricBridge> = ok.clone();
        assert_eq!(
            bridge_availability(Some(bridge.clone())),
            Availability::Available
        );
        let req = Request::new("  Unlock  ").timeout(Duration::from_secs(5));
        assert!(bridge_authenticate(Some(bridge), &req).is_ok());
        assert_eq!(
            ok.seen.lock().unwrap().as_slice(),
            &[("Unlock".to_owned(), Duration::from_secs(5))]
        );
        let denied: Arc<dyn BiometricBridge> = Arc::new(FakeBridge {
            outcome: Err(OsAuthError::Failed),
            availability: Availability::Available,
            seen: Mutex::new(Vec::new()),
        });
        assert_eq!(
            bridge_authenticate(Some(denied), &Request::new("Unlock")).unwrap_err(),
            OsAuthError::Failed
        );
    }

    #[test]
    fn scripted_authenticator_runs_the_script_and_validates() {
        let s = ScriptedAuthenticator::new(
            Availability::Available,
            vec![Err(OsAuthError::Failed), Ok(())],
        );
        assert!(s.availability().is_available());
        assert_eq!(
            s.authenticate(&Request::new("")).unwrap_err().code(),
            "invalid"
        );
        assert_eq!(s.prompts(), 0);
        assert_eq!(
            s.authenticate(&Request::new("Unlock")).unwrap_err(),
            OsAuthError::Failed
        );
        assert!(s.authenticate(&Request::new("Unlock")).is_ok());
        assert_eq!(
            s.authenticate(&Request::new("Unlock")).unwrap_err(),
            OsAuthError::Cancelled
        );
        assert_eq!(s.prompts(), 3);
    }

    /// A real prompt. Never runs in CI: it needs a person, a session and an enrolled method.
    /// Run it by hand with `--ignored os_auth_manual` and the `os-auth` feature.
    #[test]
    #[ignore = "shows a real OS authentication prompt"]
    fn os_auth_manual() {
        let a = SystemAuthenticator;
        eprintln!("availability: {}", a.availability().code());
        let r = a.authenticate(&Request::new("RightKit OS authentication manual test"));
        eprintln!("result: {r:?}");
    }
}
