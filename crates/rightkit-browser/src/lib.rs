//! Shared Chrome DevTools Protocol browser runtime for Right Suite products.
//!
//! * [`BrowserSession`] owns one Chrome process and any number of pages. Launch uses Chrome
//!   for Testing ([`chrome_for_testing_path`]); the system browser only with
//!   [`LaunchOptions::system_chrome`] or [`LaunchOptions::system_chrome_only`].
//!   Shutdown and drop close gracefully (CDP
//!   `Browser.close` / SIGTERM to the process group, bounded waits, SIGKILL last).
//! * [`ProfileSpec`] selects a temporary, named persistent, or caller-owned directory profile.
//! * Pointer and keyboard actions use real `Input.dispatch*Event` commands, so
//!   `event.isTrusted`, `:hover`, focus, and default actions behave as for a user.
//! * [`NetworkPolicy`] blocks loopback/link-local/private/reserved/`file:` targets
//!   (redirects, subresources, and every proxied connection included) before they
//!   reach the network; denials are typed ([`BrowserError::Blocked`]).
//! * [`AdmissionHook`] then gates every effect before it happens.
//! * [`BrowserEvent`] reports start/stop/crash/denied with session id, reason, and time.
//! * [`BrowserPage::set_request_interceptor`] lets the caller continue, rewrite, fulfil or fail
//!   every request after the network policy; [`BrowserPage::accessibility_tree`],
//!   [`BrowserPage::exceptions`], [`BrowserPage::set_page_scale`] and
//!   [`BrowserPage::set_download_behavior`] expose the matching CDP domains.
//! * Real-session mode (consent-gated): [`default_browser`], [`UserConsentReceipt`],
//!   [`ProfileSpec::ImportedFrom`] and [`BrowserSession::attach_user_browser`] reuse the user's
//!   own sign-ins in a private copy, confined to the consented domains. See [`ImportSpec`].
//! * [`cookies::parse_netscape`] reads a Netscape `cookies.txt` (bounded; values redacted in
//!   `Debug` and errors) and [`BrowserPage::import_cookies`] sets the result in the profile.
//!   With feature `cookie-export`, `CookieExport` goes the other way: it keeps the cookies of
//!   declared domains and publishes them as a private Netscape file through rightkit-fs; an
//!   empty or invalid export never replaces the previous file.
//! * With feature `profile-vault`, [`ProfileVault`] and [`LaunchOptions::encrypted_profile`] keep
//!   a persistent profile as one authenticated-encrypted file at rest: decrypted to a private
//!   working directory for the session, re-sealed and wiped at stop, recovered after a crash, and
//!   refused with a typed [`VaultError`] when the credential-store key is unavailable.
//! * [`capture`] loads one real URL and returns a typed [`CaptureReport`]: load result, PNG,
//!   metrics, console and (feature `axe`) axe-core output, each hashed and bound to the session,
//!   process, profile, page, frame, loader and context that produced it, with the terminal
//!   cleanup outcome ([`StopReport`]) of success, failure, timeout and cancellation alike.
//! * [`BrowserPage::observe`] returns refs that are checked for staleness before use.
//! * [`design_mode`] holds the element-picker primitives: an isolated-world binding bridge,
//!   a one-shot native-gesture submit gate, device-pixel capture geometry and the picker script builder.
// The source-only fixture browser is confined by macOS Seatbelt / the Windows sandbox; on other hosts its
// constructors refuse and the rest of the fixture machinery is unreachable.
#![cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
mod capture;
mod cdp_ext;
mod config;
#[cfg(feature = "cookie-export")]
pub mod cookie_export;
pub mod cookies;
pub mod design_mode;
mod error;
mod fixture;
mod fixture_protocol;
mod fixture_server;
#[cfg(feature = "install")]
pub mod install;
mod keys;
mod page;
#[cfg(windows)]
mod pipe_transport;
mod policy;
mod proc;
mod proxy;
mod real_session;
mod sandbox_access;
mod session;
#[cfg(feature = "profile-vault")]
mod vault;
mod visible;

pub use capture::{
    capture, AxeObservation, AxeSpec, AxeStatus, CaptureArtifact, CaptureBinding,
    CaptureCancellation, CaptureIdentity, CaptureReport, CaptureRequest, CaptureStatus,
    CleanupReport, ConsoleObservation, ConsoleRecord, LoadObservation, MetricsObservation,
    ScreenshotArtifact, CAPTURE_SCHEMA, MAX_CAPTURE_AXE_BYTES, MAX_CAPTURE_CONSOLE_RECORDS,
    MAX_CAPTURE_DEADLINE, MAX_CAPTURE_SCREENSHOT_BYTES, MAX_CAPTURE_VIEWPORT_EDGE,
};
#[cfg(feature = "axe")]
pub use capture::{AXE_SHA256, AXE_VERSION};
pub use cdp_ext::{
    AxNode, AxSummary, ContinueOverrides, DownloadBehavior, ExceptionFrame, InterceptDecision,
    InterceptFailure, InterceptStage, InterceptedRequest, PageException, RequestInterceptor,
    MAX_EXCEPTIONS,
};
pub use config::{
    chrome_for_testing_candidates, chrome_for_testing_path, find_chrome, LaunchOptions,
    ManagedRoot, ProfileSpec, CHROME_FOR_TESTING_ENV, CHROME_FOR_TESTING_INSTALL_HINT,
    CHROME_FOR_TESTING_ROOT_ENV,
};
#[cfg(feature = "cookie-export")]
pub use cookie_export::{ClearOutcome, CookieExport, CookieExportError, ExportReport};
pub use cookies::{
    parse_netscape, CookieFieldError, CookieParseError, NetscapeCookie, MAX_COOKIES,
    MAX_COOKIES_FILE_BYTES,
};
pub use error::{BrowserError, Result};
pub use fixture::{
    FixtureAttestation, FixtureCancellation, FixtureCleanupEvidence, FixtureConfinementBackend,
    FixtureOutcome, FixtureOutcomeStatus, FixtureRunFailure, FixtureSession,
};
pub use fixture_protocol::{
    actions_digest, FixtureAction, FixtureBundle, FixtureError, FixtureFile, FixtureIntent,
    FixtureKey, FIXTURE_SCHEMA,
};
pub use fixture_server::FixtureServer;
pub use page::{
    BrowserPage, ConsoleLine, Observation, ObservedNode, Rect, ScreenshotClip, ScreenshotFormat,
    ScreenshotOptions, Target, MAX_EXTRA_HEADERS, MAX_HEADER_VALUE_BYTES, MAX_USER_AGENT_BYTES,
    OBSERVE_AX_DEPTH,
};
pub use policy::{
    classify_ip, ActionKind, AddressClass, AdmissionHook, AdmissionRequest, AllowCallback,
    BlockReason, BlockedRequest, BrowserEvent, BrowserRequest, Decision, EventSink, LifecycleKind,
    NetworkPolicy, NetworkRequest, PolicyDecision, PolicyDenial, Resolver,
};
pub use proxy::PinnedConnection;
pub use real_session::{
    browser_executable, default_browser, list_profiles, purge_imported_profile, user_data_dir,
    BrowserKind, BrowserSource, ConsentRevocation, ImportReport, ImportSpec, SourceProfile,
    UserConsentReceipt, CONSENT_SCHEMA,
};
pub use session::{BrowserSession, PageSetup, StopReport};
#[cfg(feature = "profile-vault")]
pub use vault::{ProfileVault, VaultError};
pub use visible::{run_visible_until_exit, VisibleExit, VisibleLaunch};
