//! Shared Chrome DevTools Protocol browser runtime for Right Suite products.
//!
//! * [`BrowserSession`] owns one Chrome process and any number of pages.
//! * [`ProfileSpec`] selects a temporary or named persistent profile.
//! * Pointer and keyboard actions use real `Input.dispatch*Event` commands, so
//!   `event.isTrusted`, `:hover`, focus, and default actions behave as for a user.
//! * [`AdmissionHook`] gates every effect and [`NetworkPolicy`] blocks private/loopback/file
//!   targets (redirects and subresources included) before they reach the network.
//! * [`BrowserPage::observe`] returns refs that are checked for staleness before use.
mod config;
mod error;
mod keys;
mod page;
mod policy;
mod proc;
mod proxy;
mod session;

pub use config::{find_chrome, LaunchOptions, ProfileSpec};
pub use error::{BrowserError, Result};
pub use page::{BrowserPage, ConsoleLine, Observation, ObservedNode, Rect, Target};
pub use policy::{
    classify_ip, ActionKind, AddressClass, AdmissionHook, AdmissionRequest, AllowCallback, BlockedRequest,
    BrowserEvent, Decision, EventSink, NetworkPolicy, NetworkRequest, Resolver,
};
pub use proxy::PinnedConnection;
pub use session::BrowserSession;
