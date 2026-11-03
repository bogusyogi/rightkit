//! Launch at login without Tauri (rightkit#956). `autostart` writes and removes the login entry
//! (LaunchAgent or `SMAppService` on macOS, HKCU Run on Windows, XDG `.desktop` on Linux) and migrates the
//! entry `tauri-plugin-autostart` left; `login_item` is the `SMAppService` status and settings link.

pub mod autostart;
pub mod login_item;
