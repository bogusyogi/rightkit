# Changelog

## 0.1.1

- RK-4, transport-independent: new `push` module. `PhoneAlert` (title and item id only, no body; title capped at 64 characters), `build_provider_jwt` and `ProviderTokenCache` (ES256 via `rightkit_crypto::P256Signer`, refreshed at 50 minutes under Apple's 60-minute rule), `build_request` and `PushRequest` (pure, redacted `Debug`), `PushTransport` host trait with `classify`, `backoff_hint` and `send_alert` (200 ok, 410 or `BadDeviceToken` unregisters, 429 and 5xx retry hint, others reported), and `DeviceTokenStore` with `MemoryTokenStore`.
- New dependency: workspace crate `rightkit-crypto` 0.1.4 (carries `P256Signer`, so it is published first). No external crate added.
- The HTTP/2 transport waits on the rightkit-http HTTP/2 decision; the host supplies `PushTransport` meanwhile.

## 0.1.0 (unpublished)

- Fixed (Linux, Windows): `dispatch` is used only by the macOS backend and is allowed as dead code elsewhere; the Linux in-place update ignores its `Result` on purpose (the notification stays shown). Both failed the cross-target clippy rows.
Issue #958 (neutral notifications owner and badges).

- New crate: `Client`, `Notification` builder, `Handle::{update, close}`, `Permission`, `Capabilities`, activation callback, `set_badge`.
- macOS: UserNotifications with union-registered action categories, a delegate that forwards clicks, dismissals and button presses, replace and close by request identifier, permission status and prompt, dock badge through `setBadgeCount`.
- Windows: WinRT toasts through the `windows` crate with tag and group (replace and close by key), Start and taskbar number badge. Known gaps documented: no click callback (COM activation), no actions, no overlay icon.
- Linux: freedesktop notifications through `notify-rust`, replace and close by server id.
- `Client::delivered` (what the OS still shows for the app, as `Delivered { key, title, body, thread }`; macOS read-back, empty elsewhere) and `Client::close_all` (macOS delivered and pending, Windows toast history, Linux this process's notifications), plus `Capabilities::delivered`. `rightkit-tauri-shell` `notify.rs` now delegates to them and no longer carries its own UserNotifications read-back.
- Decision: no fake backend and no unit tests, per the suite's end-to-end-only rule; acceptance is a real run per OS.
