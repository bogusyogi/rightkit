# rightkit-shell

Shared Tauri desktop shell behavior for Right Suite applications, including hardened webviews, window state, menus, tray, single instance, deep links, autostart, clipboard, watchdog, and engine wiring.

The `engine` feature enables engine sidecar wiring. The `devtools` feature enables release-build inspection support.

Publish through the RightKit release workflow.

## Also (0.1.1)

### Windows notifications

`notify` now posts native toasts on Windows (`notify::platform::SUPPORTED` is true), through `tauri-winrt-notification`, the WinRT `ToastNotificationManager` wrapper that `tauri-plugin-notification` uses via `notify-rust`. The public API (`notify::post`, `status`, `NotificationRequest`, sound throttling, Do-Not-Disturb suppression) is unchanged, so callers are cross-platform.

- Toasts are attributed to an AppUserModelID. An installed app owns one through its Start-menu shortcut; Tauri's NSIS and MSI installers set it to the bundle `identifier`, and the shell uses `tauri.conf.json`'s `identifier` by default. Call `notify::set_app_id("com.example.app")` before the first notification to override it. The installer or shortcut must register that same AUMID, or Windows drops the toast.
- An unpackaged dev build has no AUMID, so toasts fall back to PowerShell's (`notify::FALLBACK_APP_ID`) and show its name and icon.
- `sound_name` accepts `Default`, `IM`, `Mail`, `Reminder`, `SMS`; any other name (for example the macOS `Glass`) plays the default toast sound.
- Windows has no per-app permission prompt and this backend cannot read delivered toasts: `authorization` reports `authorized`, `delivered` is empty, and `clear_delivered` is a no-op.

### Standalone open-URL hook

`open_url` delivers URLs the OS opens in the app, without the Shell plugin:

```rust
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

`windows` (`0.61`) and `webview2-com` (`0.38`) match Tauri 2.11's wry, so an app resolves one copy shared with Tauri and can pass `windows`/`webview2-com` types across the boundary (a wider range let Cargo pick 0.62/0.39 beside Tauri's copy, and `cast()` failed: E0599, found by MailRight).
