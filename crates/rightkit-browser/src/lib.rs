//! Shared Chrome DevTools Protocol browser runtime for Right Suite products.
//!
//! * [`BrowserSession`] owns one Chrome process and any number of pages.
//! * [`ProfileSpec`] selects a temporary or named persistent profile.
//! * Pointer and keyboard actions use real `Input.dispatch*Event` commands, so
//!   `event.isTrusted`, `:hover`, focus, and default actions behave as for a user.
//! * [`BrowserPage::observe`] returns refs that are checked for staleness before use.
mod config;
mod error;
mod keys;
mod page;
mod session;

pub use config::{find_chrome, LaunchOptions, ProfileSpec};
pub use error::{BrowserError, Result};
pub use page::{BrowserPage, ConsoleLine, Observation, ObservedNode, Rect, Target};
pub use session::BrowserSession;
