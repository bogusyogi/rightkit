//! rightkit-computer: observe and act on any desktop app without taking the
//! user's focus. See the design report for the tier ladder.
//!
//! Observe: window list, accessibility tree, per-window screenshot.
//! Act: AX action / AX set-value first (no input events at all), then
//! pid-addressed key events, with the foreground app verified unchanged.

pub mod model;
pub mod safety;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub use macos::*;
