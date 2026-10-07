//! Shared Chrome DevTools Protocol browser runtime for Right Suite products.
//!
//! * [`BrowserSession`] owns one Chrome process and any number of pages. Launch uses Chrome
//!   for Testing ([`chrome_for_testing_path`]); the system browser only with
//!   [`LaunchOptions::system_chrome`]. Shutdown and drop close gracefully (CDP
//!   `Browser.close` / SIGTERM to the process group, bounded waits, SIGKILL last).
//! * [`ProfileSpec`] selects a temporary or named persistent profile.
//! * Pointer and keyboard actions use real `Input.dispatch*Event` commands, so
//!   `event.isTrusted`, `:hover`, focus, and default actions behave as for a user.
//! * [`NetworkPolicy`] blocks loopback/link-local/private/reserved/`file:` targets
//!   (redirects, subresources, and every proxied connection included) before they
//!   reach the network; denials are typed ([`BrowserError::Blocked`]).
//! * [`AdmissionHook`] then gates every effect before it happens.
//! * [`BrowserEvent`] reports start/stop/crash/denied with session id, reason, and time.
//! * [`BrowserPage::observe`] returns refs that are checked for staleness before use.
mod config;
mod error;
mod keys;
mod page;
mod policy;
mod proc;
mod proxy;
mod session;

pub use config::{
    chrome_for_testing_candidates, chrome_for_testing_path, find_chrome, LaunchOptions,
    ProfileSpec, CHROME_FOR_TESTING_ENV, CHROME_FOR_TESTING_INSTALL_HINT,
};
pub use error::{BrowserError, Result};
pub use page::{BrowserPage, ConsoleLine, Observation, ObservedNode, Rect, Target};
pub use policy::{
    classify_ip, ActionKind, AddressClass, AdmissionHook, AdmissionRequest, AllowCallback,
    BlockReason, BlockedRequest, BrowserEvent, BrowserRequest, Decision, EventSink, LifecycleKind,
    NetworkPolicy, NetworkRequest, PolicyDecision, PolicyDenial, Resolver,
};
pub use proxy::PinnedConnection;
pub use session::BrowserSession;
