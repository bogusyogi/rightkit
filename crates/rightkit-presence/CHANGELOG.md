# Changelog

## Unreleased

- Added `activity` (RK-6; additive, nothing existing changes): time since the last real user input (Windows `GetLastInputInfo` with a wrap-safe 32-bit tick difference; macOS `CGEventSourceSecondsSinceLastEventType` on the HID system state), a "typing now" flag derived only from input recency (`TYPING_WINDOW_SECS`, 5), and screen-locked, fullscreen, Do Not Disturb and in-meeting signals as `Yes`/`No`/`Unknown` (Windows through `SHQueryUserNotificationState`; macOS through the session dictionary and the existing window probe). `ActivityReport` carries `last_input_at` and a `RoutePresence` that is wire-identical to the attention crate's `PresenceState`. Only a timestamp is read: no key codes, no content, no hooks, no event taps. macOS Do Not Disturb/Focus, macOS window titles and every input-listening API are excluded (permission prompt or Full Disk Access). No new dependency (hand-declared FFI). Written, not compiled. No unit tests, per the 2026-11-03 rule.
- `RoutePresence` gains `Locked` (additive), matching the attention crate's new `PresenceState::Locked`; a locked screen now reports `Locked` instead of `DoNotDisturb` (locked wins over Do Not Disturb). Written, not compiled.

## 0.1.1

- Publishes the source changes made since 0.1.0 was published under the same version number (src/presence.rs).

## 0.1.0 (unreleased)

- First release (rightkit#956): split out of `rightkit-tauri-shell` 0.7.3 with no behaviour change. The code is the shell's `presence` module moved unchanged, so apps without Tauri (an egui host, a C-ABI companion, a menu-bar helper, an overlay app) can use it. `rightkit-tauri-shell` depends on this crate and re-exports it at the old path. Source-level move only: not compiled in this pass.
