# rightkit-shell

Shared Tauri desktop shell for Right Suite applications. It owns hardened webviews (`HardeningScript`, navigation and popup policy), window state, the native menu and tray, single-instance arbitration, deep links, autostart, rich clipboard, notifications, a watchdog, presence, and optional engine sidecar wiring. Its entry point is `Shell::builder(app_id).build().plugin()`, with lower-level modules public.

**Use it** in a Right Suite Tauri app's native host, so every app gets the same shell behavior.

**Do not use it** outside a Tauri 2 application, or as a replacement for an app's own invoke handlers and product logic. Choose one plugin entry point per app.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `engine` | on | `rightkit-jobs`, `rightkit-process` | Engine sidecar wiring over `rightkit-jobs` `EngineClient`. |
| `wv2-0_38` | on | `windows` 0.61, `webview2-com` 0.38 | Windows types for Tauri 2.11 (the default). Must match the app's Tauri. |
| `wv2-0_39` | off | `windows` 0.62, `webview2-com` 0.39 | Windows types for Tauri 2.12. With `default-features = false`, enable `engine` and `wv2-0_39`. Enabling both selects 0.39. |
| `devtools` | off | `tauri/devtools` | Lets `Devtools::On` and `DebugBuilds` open the inspector in release builds. Keep it off in production. |

Mismatched `wv2` features put two copies of `windows` or `webview2-com` beside Tauri's, and `cast()` then fails. With `default-features = false`, enable exactly the feature that matches the app's Tauri version.

## Platform support

- macOS: native notifications and presence through `objc2` and related crates; `cfg(target_os = "macos")` in `src/` (for example `notify.rs`, `presence.rs`, `frameless.rs`).
- Windows: WebView2 types through the `wv2` features; native toasts through `tauri-winrt-notification`; `cfg(windows)` in `src/`.
- Linux: this README does not describe Linux-specific behavior. Open-URL delivery on Linux depends on the `.desktop` MIME entry (see Standalone open-URL hook).

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`; the version sections below record additions by version.

## Example

Illustrative; not compiled as a doctest. The compiled references are `tests/policy.rs` (policy decisions) and the `example-app/` Tauri project in this crate.

## Windows

- Generation: `src/lib.rs` re-exports one `windows` and `webview2-com` pair as `crate::winapi`. `wv2-0_38` selects windows 0.61 and webview2-com 0.38 (default). `wv2-0_39` selects 0.62 and 0.39. Enabling both picks 0.39. With neither, the crate fails with `compile_error!`. Tauri 2.12 apps use `default-features = false, features = ["engine", "wv2-0_39"]`.
- The Cargo.toml comment ties 0.38 to Tauri 2.11 and 0.39 to Tauri 2.12. The Tauri lock versions are not checked here: the example app's `Cargo.lock` is stale and records rightkit-shell 0.1.0.
- WebView2 hardening (`src/harden.rs`, `windows_impl`): sets devtools, status bar, zoom control, pinch zoom, swipe navigation and browser accelerator keys (F5, Ctrl+R, Alt+Left, F12). It registers `ContextMenuRequested` through `ICoreWebView2_11`. That handler suppresses the menu unless the target is editable and `allow_editable_menu` is set.
- Idle (`src/idle.rs`): `ICoreWebView2_3::TrySuspend` and `SetIsVisible` on the controller.
- Clipboard (`src/clipboard.rs`): `OpenClipboard` with 10 retries at 20 ms. Reads `CF_UNICODETEXT` and `CF_HDROP`, with file lists read through `DragQueryFileW`.
- Notifications: WinRT toasts through `tauri-winrt-notification` 0.7, as described below.
- Registry and shell: autostart reads and writes `HKCU\...\CurrentVersion\Run` through `reg.exe`. Scheme registration writes under `HKCU\Software\Classes` through `reg.exe`. Both use `CREATE_NO_WINDOW`. `opener` launches `rundll32.exe url.dll,FileProtocolHandler`.
- Single instance uses the rightkit-service named pipe (`src/instance.rs`). Broker entropy comes from `SystemFunction036` in advapi32 (`src/broker.rs`).
- Dependencies: `windows_061`/`windows_062` and `webview2_com_038`/`webview2_com_039` are optional. They enable Win32 features `Win32_Foundation`, `Win32_System_Com`, `Win32_System_DataExchange`, `Win32_System_Memory`, `Win32_System_Ole` and `Win32_UI_Shell`. Consumers select only a `wv2-*` feature.
- Caveat: `harden.rs` and `clipboard.rs` both say they are not compiled on the macOS host. Run `rightkit cargo check -p rightkit-shell` on Windows before relying on them.

## Also (0.1.3)

### Single-instance arbitration only

```rust,ignore
tauri::Builder::default()
    .plugin(rightkit_shell::Shell::builder("viewright")
        .build()
        .single_instance_plugin(|app, argv, cwd| {
            // Handle forwarded files & focus using your app's own policy.
        }));
```

Add this plugin first. It uses only `app_id` from the builder & always enables
arbitration. It retains the primary service guard until exit, forwards argv/cwd
to the primary callback & exits a successful secondary launch. Unavailable
arbitration reports its reason to stderr & allows startup, as `instance::arbitrate`
does. The callback owns all second-launch behavior, including URL delivery & focus.

This entry point installs no hardening script, native hardening, navigation
policy, window-state restoration, presence loop or `ShellState`. Use your own
invoke handler; `rightkit_shell::handler!` requires the full plugin. Choose one
plugin entry point per app. The lower-level `instance::arbitrate` remains public.

### Full-plugin script scope

```rust,ignore
use rightkit_shell::{HardeningScript, Shell};

let plugin = Shell::builder("viewright")
    .hardening_script(HardeningScript::TopFrameOnly)
    .build()
    .plugin();
```

`ShellBuilder::hardening_script` accepts `AllFrames` (unchanged default),
`TopFrameOnly` or `Off`. Top-frame mode requests native main-frame injection &
guards the script body with `window.top !== window.self`, so document iframes
remain untouched even on WebView2, which injects scripts into subframes.
`Off` registers no DOM hardening script. Native webview hardening, navigation
policy & remaining full-shell behavior still apply in both modes.

Tests inspect generated plugin scripts for all three scopes & the isolated
plugin, plus real service arbitration from a separate secondary process,
argv/cwd forwarding & guard release.

## Also (0.1.1)

### Windows notifications

`notify` now posts native toasts on Windows (`notify::platform::SUPPORTED` is true), through `tauri-winrt-notification`, the WinRT `ToastNotificationManager` wrapper that `tauri-plugin-notification` uses via `notify-rust`. The public API (`notify::post`, `status`, `NotificationRequest`, sound throttling, Do-Not-Disturb suppression) is unchanged, so callers are cross-platform.

- Toasts are attributed to an AppUserModelID. An installed app owns one through its Start-menu shortcut; Tauri's NSIS and MSI installers set it to the bundle `identifier`, and the shell uses `tauri.conf.json`'s `identifier` by default. Call `notify::set_app_id("com.example.app")` before the first notification to override it. The installer or shortcut must register that same AUMID, or Windows drops the toast.
- An unpackaged dev build has no AUMID, so toasts fall back to PowerShell's (`notify::FALLBACK_APP_ID`) and show its name and icon.
- `sound_name` accepts `Default`, `IM`, `Mail`, `Reminder`, `SMS`; any other name (for example the macOS `Glass`) plays the default toast sound.
- Windows has no per-app permission prompt and this backend cannot read delivered toasts: `authorization` reports `authorized`, `delivered` is empty, and `clear_delivered` is a no-op.

### Standalone open-URL hook

`open_url` delivers URLs the OS opens in the app, without the Shell plugin:

```rust,ignore
rightkit_shell::open_url::on_open_url(|urls| { /* mailto:, myapp://... */ });
let cold = rightkit_shell::open_url::current_urls(); // arrived before any handler existed
```

- macOS: call `open_url::handle_run_event(&event)` from `app.run(|_, event| ...)` (Apple Event URLs, `RunEvent::Opened`).
- Windows/Linux: `open_url::init_single_instance(app_id, version)` runs `instance::arbitrate`, captures this launch's command-line URLs and delivers a second launch's argv to the first instance (exit on `Arbitration::Secondary`). Without single instance, call `open_url::capture_launch_args()` once at startup.
- `on_open_url` handlers receive URLs that arrive after registration, in registration order; `current_urls()` keeps the cold-start ones. The same URL within `DEDUP_WINDOW` (2 s) is delivered once. Only `scheme:rest` entries with a scheme of two or more characters count, so flags and `C:\` paths are ignored.
- The full Shell plugin feeds the same hub.
- Scheme registration (`mailto:`, custom schemes) stays with the app's installer or bundle: `CFBundleURLTypes` on macOS, a protocol entry (NSIS/MSI or `deep_link::register_scheme`) on Windows, a `.desktop` MIME entry on Linux.

### Second-instance callback, dependency ranges

`Shell::builder(..).on_second_instance(|app, argv, cwd| ..)` replaces the default second-launch behaviour (event plus reveal of the main window); the callback owns focus. Arbitration is unchanged, and URLs on the forwarded argv still reach `open_url` handlers and `current_urls()` before the callback runs. Without a callback nothing changes.

`windows`/`webview2-com` are feature-selected so they match the app's Tauri: `wv2-0_38` (0.61/0.38, Tauri 2.11, **default since 0.1.2**) or `wv2-0_39` (0.62/0.39, Tauri 2.12). A mismatch puts two copies beside Tauri's and `cast()` fails (E0599, found by MailRight). With `default-features = false`, add one of them, e.g. `features = ["wv2-0_38"]` (0.1.1 defaulted to `wv2-0_39`, which broke Tauri 2.11 apps; found by CodeRight).
