//! rightkit-control: drive a real desktop app like a human, in the background.
//!
//! macOS: input is posted to the target pid with `CGEventPostToPid` (never the
//! global HID tap), so the frontmost app and the user's cursor are untouched.
//! State is read through the Accessibility API and window captures.
pub mod json;
pub mod keys;
pub mod webdriver;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(feature = "tauri-plugin")]
pub mod embedded;
