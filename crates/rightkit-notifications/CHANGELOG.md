# Changelog

## 0.1.0 (unpublished)

Issue #958 (neutral notifications owner and badges).

- New crate: `Client`, `Notification` builder, `Handle::{update, close}`, `Permission`, `Capabilities`, activation callback, `set_badge`.
- macOS: UserNotifications with union-registered action categories, a delegate that forwards clicks, dismissals and button presses, replace and close by request identifier, permission status and prompt, dock badge through `setBadgeCount`.
- Windows: WinRT toasts through the `windows` crate with tag and group (replace and close by key), Start and taskbar number badge. Known gaps documented: no click callback (COM activation), no actions, no overlay icon.
- Linux: freedesktop notifications through `notify-rust`, replace and close by server id.
- `Client::delivered` (what the OS still shows for the app, as `Delivered { key, title, body, thread }`; macOS read-back, empty elsewhere) and `Client::close_all` (macOS delivered and pending, Windows toast history, Linux this process's notifications), plus `Capabilities::delivered`. `rightkit-tauri-shell` `notify.rs` now delegates to them and no longer carries its own UserNotifications read-back.
- Decision: no fake backend and no unit tests, per the suite's end-to-end-only rule; acceptance is a real run per OS.
