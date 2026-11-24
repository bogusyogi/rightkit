# rightkit-notifications

The neutral native notifications owner, usable without Tauri. `rightkit-tauri-shell`'s `notify.rs` posts through `UNUserNotificationCenter` and `tauri-winrt-notification` but only inside the Tauri crate, reports Linux as unsupported, and has no click or named actions, no replace or withdraw by key, no permission query, and badges only through Tauri `set_badge_count`. This crate owns all of that; the shell delegates to it.

Catalog capability: `notifications`. Not yet published (0.1.0, unreleased). Rightkit#958.

```rust
use rightkit_notifications::{Client, Notification};

let client = Client::new("com.orthiclabs.pulse");
client.on_activation(|a| println!("{:?}", a));            // clicks, dismissals, button presses
let handle = client.send(&Notification::new("Build finished").body("3 warnings").key("build").action("open", "Open"))?;
handle.update(&Notification::new("Build finished").body("0 warnings").key("build"))?;
handle.close()?;
client.set_badge(2)?;
```

## Entry points

`Client::{new, capabilities, permission, request_permission, on_activation, send, close, close_all, delivered, set_badge}`, `Notification` (builder: `body`, `key`, `thread`, `sound`, `action`), `Handle::{key, update, close}`, `Permission`, `Capabilities`, `Delivered`, `Activation` and `ActivationKind`, `Sound`, `Error`.

`permission` never prompts; `request_permission` prompts when not determined and blocks until answered, so call it from a worker thread. A send on macOS with permission not determined prompts too. Sending with a key that is still shown replaces it; `Client::close(key)` withdraws it.

## Phone push (APNs), transport-independent (RK-4)

`rightkit_notifications::push` is everything about an APNs phone alert that needs no network:

- `PhoneAlert::new(title, item_id)`: the only alert shape. A title (1 to 64 characters, no control characters, over-long is rejected) and an opaque item id (1 to 128 of `A-Za-z0-9._:-`). There is no body field, so message text cannot be sent.
- `build_provider_jwt` and `ProviderTokenCache`: the ES256 provider token (`kid`, `iss`, `iat`) signed by `rightkit_crypto::P256Signer`, reused until 50 minutes old (Apple rejects tokens over 60 minutes and throttles minting more often than every 20), `invalidate` after Apple rejects one.
- `build_request`: a pure `PushRequest` (host, `POST /3/device/<token>`, headers `authorization`, `apns-topic`, `apns-push-type: alert`, `apns-priority`, body). Its `Debug` hides the token and the authorization header.
- `PushTransport` (host trait), `classify`, `backoff_hint` and `send_alert`: 200 delivered; 410 or `BadDeviceToken` removes the device token; 429 and 5xx (and no answer) return `Retry { after }` with an exponential hint (the server `retry-after` wins); other statuses come back as `Rejected { status, reason }`.
- `DeviceTokenStore` (host trait, one token per paired device id) and `MemoryTokenStore`.

**The HTTP/2 transport is not here.** APNs requires HTTP/2, and `rightkit-http` has none yet; adding it (the `h2` dependency) is a pending dependency-allowlist decision. Until then the host implements `PushTransport` over its own HTTP/2 client. This crate depends on `rightkit-crypto` for signing and base64url.

## Platforms

| | macOS | Windows | Linux |
|---|---|---|---|
| backend | UserNotifications | WinRT toast (`windows` crate) | freedesktop D-Bus (`notify-rust`, zbus) |
| replace and close by key | yes (request identifier) | yes (toast tag and group) | yes (server id) |
| named actions, click callback | yes (categories union-registered, delegate) | no | no |
| permission | real status and prompt | always granted when the OS setting is enabled | not applicable |
| badge | dock badge, macOS 13+ (`setBadgeCount`, needs the Badge authorization) | Start and taskbar number badge of an AUMID app | none |

`capabilities()` reports exactly this, so an app never builds UI around a feature the platform lacks.

## Known gaps (documented, not hidden)

- Windows: a click does not reach `on_activation`. Activation after the app exited needs COM activation registered by the installer (an AUMID with an activator CLSID); a click while running is not wired. There is no taskbar overlay icon, only the numeric badge an AUMID app gets. Actions are not rendered.
- Linux: no action buttons, no badge, no per-app permission.
- macOS needs an app bundle with an identifier; a bare binary gets `Error::Unsupported`.

## Tests

No unit tests or fake backend (the suite's decision: end-to-end only). Acceptance is a real run per OS: send, replace, close, permission, badge and a click, from the installed app of the first adopter.

## Not verified here

The push module is not compiled or run either, and there is no live APNs exchange. Written against objc2-user-notifications 0.3.2, `windows` 0.61 and `notify-rust` 4 docs; not compiled or run on a device in this pass.
