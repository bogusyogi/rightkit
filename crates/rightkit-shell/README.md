# rightkit-shell

Shared Tauri desktop shell for Right Suite applications. It owns hardened webviews (`HardeningScript`, navigation and popup policy), window state, the native menu and tray, single-instance arbitration, deep links, autostart, rich clipboard, notifications, a watchdog, presence, and optional engine sidecar wiring. Its entry point is `Shell::builder(app_id).build().plugin()`, with lower-level modules public.

**Use it** in a Right Suite Tauri app's native host, so every app gets the same shell behavior.

**Do not use it** outside a Tauri 2 application, or as a replacement for an app's own invoke handlers and product logic. Choose one plugin entry point per app.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `engine` | off | `rightkit-jobs`, `rightkit-process` | Engine sidecar wiring over `rightkit-jobs` `EngineClient`. Opt in; it pulls `rightkit-jobs` (db, time, uuid). |
| `wv2-0_38` | on | `windows` 0.61, `webview2-com` 0.38 | Windows types for Tauri 2.11 (the default). Must match the app's Tauri. |
| `wv2-0_39` | off | `windows` 0.62, `webview2-com` 0.39 | Windows types for Tauri 2.12. With `default-features = false`, enable `wv2-0_39` (plus `engine` if used). Enabling both selects 0.39. |
| `content-devtools` | off | `tauri/devtools` | Required by `DevToolsPolicy::ContentOnly`. Only an app on the `right-release` allow-list (today ViewRight) may ship it; see "DevTools policy". Never a default feature. |
| `dev-inspect` | off | `tauri/devtools` | Development builds only: every webview is inspectable and the DevTools guards stand down; see "DevTools policy / Development builds". A build with it can never be released. Never a default feature, and not needed by ContentOnly. |

Mismatched `wv2` features put two copies of `windows` or `webview2-com` beside Tauri's, and `cast()` then fails. With `default-features = false`, enable exactly the feature that matches the app's Tauri version.

## Platform support

- macOS: native notifications and presence through `objc2` and related crates; `cfg(target_os = "macos")` in `src/` (for example `notify.rs`, `presence.rs`, `frameless.rs`).
- Windows: WebView2 types through the `wv2` features; native toasts through `tauri-winrt-notification`; `cfg(windows)` in `src/`.
- Linux: this README does not describe Linux-specific behavior. Open-URL delivery on Linux depends on the `.desktop` MIME entry (see Standalone open-URL hook).

Published version: see `INDEX.md` at the repository root. See `CHANGELOG.md`; the version sections below record additions by version.

## Example

Illustrative; not compiled as a doctest. The compiled references are `tests/policy.rs` (policy decisions) and the `example-app/` Tauri project in this crate.

## Windows

- Generation: `src/lib.rs` re-exports one `windows` and `webview2-com` pair as `crate::winapi`. `wv2-0_38` selects windows 0.61 and webview2-com 0.38 (default). `wv2-0_39` selects 0.62 and 0.39. Enabling both picks 0.39. With neither, the crate fails with `compile_error!`. Tauri 2.12 apps use `default-features = false, features = ["wv2-0_39"]` (add `"engine"` for the sidecar).
- The Cargo.toml comment ties 0.38 to Tauri 2.11 and 0.39 to Tauri 2.12. The Tauri lock versions are not checked here: the example app's `Cargo.lock` is stale and records rightkit-shell 0.2.0 (`example-app/Cargo.lock:2692`; the crate is 0.3.0).
- WebView2 hardening (`src/harden.rs`, `windows_impl`): sets devtools, status bar, zoom control, pinch zoom, swipe navigation and browser accelerator keys (F5, Ctrl+R, Alt+Left, F12). It registers `ContextMenuRequested` through `ICoreWebView2_11`. That handler suppresses the menu unless the target is editable and `allow_editable_menu` is set.
- Idle (`src/idle.rs`): `ICoreWebView2_3::TrySuspend` and `SetIsVisible` on the controller.
- Clipboard (`src/clipboard.rs`): `OpenClipboard` with 10 retries at 20 ms. Reads `CF_UNICODETEXT`, `HTML Format`, `PNG`, `CF_HDROP` (file lists through `DragQueryFileW`) and `CF_DIB`. A write owns the clipboard through a message-only window, because `SetClipboardData` fails after `EmptyClipboard` with a NULL owner. `ClipboardOptions::image_preference` chooses `PNG` first (default) or `CF_DIB` first for the typed image.
- Notifications: WinRT toasts through `tauri-winrt-notification` 0.7, as described below.
- Registry and shell: autostart reads and writes `HKCU\...\CurrentVersion\Run` through `reg.exe`. Scheme registration writes under `HKCU\Software\Classes` through `reg.exe`. Both use `CREATE_NO_WINDOW`. `opener` launches `rundll32.exe url.dll,FileProtocolHandler`.
- Single instance uses the rightkit-service named pipe (`src/instance.rs`). Broker entropy comes from `SystemFunction036` in advapi32 (`src/broker.rs`).
- Dependencies: `windows_061`/`windows_062` and `webview2_com_038`/`webview2_com_039` are optional. They enable Win32 features `Win32_Foundation`, `Win32_System_Com`, `Win32_System_DataExchange`, `Win32_System_Memory`, `Win32_System_Ole`, `Win32_UI_Shell` and `Win32_UI_WindowsAndMessaging`. Consumers select only a `wv2-*` feature.
- Caveat: `harden.rs`, `clipboard.rs` and `idle.rs` are not compiled or exercised on the macOS host (`src/idle.rs:9`); the windows-2025 CI job compiles them. The clipboard backend has not been run against a live Windows clipboard, so verify copy and paste on a Windows machine before relying on it.

## DevTools policy

**The owner's fixed design, verbatim, never to be changed:** every Right app has absolutely NO access to DevTools: blocked at each and every level that is possible, for the app, its shell and its own UI. The single exception is ViewRight: it opens HTML documents, and DevTools must be available INSIDE that opened HTML content only (the content frame), never for ViewRight's own UI or shell.

Source: `src/devtools.rs` (`OWNER_DECISION`). Written, unverified: nothing here was compiled or run; the macOS and Windows calls are the uncertain part.

### API

- `DevToolsPolicy::Denied` (the default) and `DevToolsPolicy::ContentOnly` (exactly two values). `Shell::builder(id).devtools_policy(..)`.
- `ContentWebview::new(label, window_label).capability(json)...app_acl_manifest(true)` and `Shell::builder(id).content_webview(..)` register a webview that renders opened content. Registration is refused (typed `DevToolsError`, startup fails, loud stderr line) when: the policy is `Denied`; the `content-devtools` feature is not compiled; the label is the main webview (never allowed); a supplied capability document applies to the content webview by webview label or by the label of its parent window and grants permissions; a capability document cannot be parsed; the app did not attest an application ACL manifest.
- `devtools::guard_process_start()` runs when the plugin is built and may be called first in `main`. `devtools::open_content_devtools(app, label)` is the only path to an open inspector and refuses anything but a registered content webview.
- The old `Devtools` enum and `Hardening::devtools` are removed: there is no "on" and no "debug builds" any more.
- The policy is not a `Module`: opting out of `Module::JsHardening` does not opt out of it.

### What each layer blocks

| Layer | macOS (WKWebView) | Windows (WebView2) |
|---|---|---|
| Per webview, native | `isInspectable = false` (macOS 13.3+, guarded by `respondsToSelector`) and the private `developerExtrasEnabled` preference set to false explicitly, on every webview the shell sees, including debug builds. This removes Safari Develop attach, "Inspect Element" and WebKit's Cmd+Opt+I handler. `close_devtools` runs first where Tauri has one. | `AreDevToolsEnabled = false`; `AreBrowserAcceleratorKeysEnabled = false` (F12, Ctrl+Shift+I/J/C); a `ContextMenuRequested` handler removes every menu item whose name contains "inspect" from every menu, including the editable-field menu the hardening keeps. |
| Page script | F12, Ctrl+Shift+I/J/C, Cmd+Opt+I/J/C/U, Ctrl+Alt+I/J/C/U cancelled in every frame of every non-content webview (matched by `KeyboardEvent.code`, so Option-letter dead keys do not slip through). | Same. |
| Process start | Refuses a launch argument that enables remote debugging. | Same, plus: removes the WebView2 additional-browser-arguments and script-debugger-pipe environment variables from the process; refuses to start when the `AdditionalBrowserArguments` machine or user policy (HKLM in the 64-bit and the 32-bit registry view, HKCU) injects a debugging switch, or when one of those keys exists and cannot be read. The keys are read through the registry API; no program is started for it. Only a key that does not exist counts as "no policy". |
| After creation | none | A fresh `DevToolsActivePort` file in the app's local or roaming data folder (or its `EBWebView`) means a debugging port is open: the process logs and exits 1. |

### What cannot be blocked

- **Windows machine policy.** WebView2 appends policy and environment arguments to whatever the app passes (`learn.microsoft.com ... webview2-idl`, CreateCoreWebView2EnvironmentWithOptions), so an explicit argument string cannot override them. The environment variable is process-local and is removed. The registry policy is not ours: the shell detects it at startup and refuses to run. An administrator can still write the key after the check; the port-file tripwire is the second line. Elevated host apps ignore the environment variable (Microsoft's security page, via the browser-flags page), so this matters most for non-elevated apps.
- **A debugger attached to the process** (lldb/Visual Studio, `get-task-allow`, Time Travel Debugging) and **UI automation / accessibility** are outside "DevTools" and outside this crate. The hardened runtime and signing checks in `right-release` are the control for the macOS entitlement.
- **macOS before 13.3** has no `isInspectable`; only the `developerExtrasEnabled` preference and the absence of `get-task-allow` apply.
- **The user's own `WebKitDeveloperExtras` default** is overridden by the explicit per-webview preference; this ordering is unverified.
- Linux is not covered by native settings (Tauri on Linux is not a supported Right target).

### ContentOnly

`ContentOnly` makes only registered content webviews inspectable: `isInspectable = true` + `developerExtrasEnabled = true` (macOS); `AreDevToolsEnabled = true` with a context menu trimmed to the Inspect item, accelerator keys still off, so the inspector is opened from that item or from `open_content_devtools` (Windows). The shell's DOM, Console and Sources are never reachable because the shell is a different WKWebView / CoreWebView2 whose own setting stays off.

Iframes: inspection of an iframe alone cannot be granted. On both platforms the inspector attaches to a whole webview, so a page whose content is an iframe in the app UI webview would make the UI inspectable. Content must live in its own webview. ViewRight renders HTML previews as iframes today and does not meet this rule; see `docs/plans/2026-10-11-viewright-content-devtools-design.md`.

### Adoption

- Every app: nothing to call. Do not enable Tauri's `devtools` feature, do not use `Devtools::*` (removed), do not create webviews with `.devtools(true)`.
- An app that created webviews itself: they pass through `on_webview_ready` like any other and are denied.
- ViewRight only: enable `content-devtools`, `devtools_policy(ContentOnly)`, register each content webview, declare an application ACL manifest in `build.rs`, scope its capabilities with `webviews` (not `windows`) and set `"devtools": "content-only"` in its `right-release` config.
- `right-release` refuses `devtools` in a release or update build for any app that is not allow-listed (`packages/release/devtools-policy.json`).

### Development builds

The owner's rule for shipped apps is unchanged: no DevTools, ViewRight's content frame excepted. Developers and the QA tooling still need to inspect a development build, so exactly one switch exists, the cargo feature `dev-inspect` (off by default, never reached from `default`, and not needed by `ContentOnly`; a `content-devtools` release build never compiles it).

- **Only under `dev-inspect`:** the policy for the app's webviews becomes "developer inspection allowed". Every webview, the app UI included, is inspectable (`WebviewDecision::AllowDev`; macOS `isInspectable` and `developerExtrasEnabled` true, Windows `AreDevToolsEnabled` true, menus and accelerator keys left as the hardening set them); `guard_process_start` stops scrubbing `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, stops refusing a debugging launch argument or registry policy, and the `DevToolsActivePort` tripwire does not run, so a remote-debugging port (the wdio WebDriver driver of `qa-native` builds) keeps working. The build embeds the marker `RIGHTKIT-SHELL-DEV-INSPECT-COMPILED` (`DEV_INSPECT_MARKER`, a `#[used]` static read from startup code, the technique `RIGHTKIT-QA-CONTROL-SERVER-PRESENT` uses) and prints one stderr line at startup beginning `RIGHTKIT SHELL DEVELOPER INSPECTION ACTIVE`.
- **Without the feature nothing changes:** `Denied` / `ContentOnly` exactly as above, in every build profile (debug assertions do not matter, because installed dev builds are release-profile builds).
- **Selecting it:** an app adds an optional feature that its default features never reach, `dev-native = ["rightkit-shell/dev-inspect"]` (beside `qa-native`), and builds its development lane with `--features dev-native`.
- **Such a build can never be released, three layers:** (1) `right-release` refuses `dev-inspect` (and `dev-native`) in a release or update build command, package script, or Cargo manifest, directly, through another feature, through `default`, or via `--all-features` (`qa-build-features.mjs`); (2) the binary scan refuses the marker in every release and update artifact, including inside MSIX/ZIP containers, and no `devtools: "content-only"` allowance lifts it (`qa-control-scan.mjs`, rule `shell-dev-inspect`); (3) the `rightkit-qa` native QA guard rejects `dev-inspect` / `dev-native` reachable from default features, a non-optional `rightkit-shell` that lists it, and release-named package scripts that enable it. The development lane (`right-release dev`) never runs layers 1 and 2.

## Hidden hosts: never_show, reopen opt-out, idle options

- `Shell::builder(..).never_show(true)` and `.never_show_env("RIGHTKIT_QA_NATIVE_REAL")`: the shell shows,
  unminimises and focuses nothing under any event, and macOS runs Prohibited from plugin setup. Every
  reveal path goes through `lifecycle::reveal_mode`; the host's own direct window calls are outside it.
- `.reveal_on_reopen(false)` (or `ShellPlugin::builder().without(Module::RevealOnReopen)`): Dock reopen and
  opened URLs no longer reveal; links still deliver.
- `never_show_env(name)` counts only `1`, `true`, `yes`, `on` (case-insensitive, exact); other values are off.
  `never_show_env_value(name, "token")` matches that exact value only.
- Idle ordering (Windows): suspend = `SetIsVisible(false)` then `TrySuspend`, skipped if the hide failed;
  resume = `Resume` then `SetIsVisible(true)`, skipped if the resume failed; failures reach
  `webview_idle_on_error` and are retried on the next resume; resume does nothing if this shell did not hide
  or suspend. `webview_idle_on_transition(|t| ..)` logs real suspends and resumes with elapsed time;
  `idle::note_minimized_after` takes a per-minimise delay.
- `.webview_idle_options(IdleOptions { start_on_minimize: true, controller_hide: ControllerHide::OnTimer,
  events: IdleEvents::NativeSuspendOnly, ..Default::default() })`. Defaults match `webview_idle(30 s)`.
- `IdleOptions::default().suspend_after_with(|| Some(read_delay()))` reads the suspend delay at each hide (`None` or a panic uses the fixed `suspend_after`).
- `lifecycle_only` leaves `ShowOnReady` and the cache repair off unless the host calls `show_on_ready(true)` /
  `repair_cache(true)`.

## Also (0.3.0, host-decided lifecycle)

- Quit drain: `quit_drain(true)` / `quit_drain_bound(d)`; the renderer (`@rightkit/shell` 0.1.1) answers
  `rightkit-shell://quit-requested` and calls `rk_shell_finish_quit`. Default bound 15 s, max 120 s; off by default.
- `close_behavior(CloseBehavior::HideToBackground)`, `on_visibility`, `qa_instance_exempt`,
  `second_instance_reveal`, `show_on_ready` (explicit beats `RIGHTKIT_QA_HIDDEN`), `internal_scheme`.
- `Shell::lifecycle_plugin()`: the lifecycle without hardening, navigation policy, presence, watchdog or window state.
- `WindowStateStore` standalone with `restore_with`; `AutostartOptions::{startup_wm_class, config_home, macos_unsupported}`.
- `NotificationRequest.bypassDoNotDisturb`; `rk_shell_capabilities`.

## Also (0.3.0)

### Modular install and the broker as a library type

```rust,ignore
use rightkit_shell::{Module, Shell, ShellPlugin};

// Everything except presence and the watchdog:
let plugin = ShellPlugin::builder()
    .shell(Shell::builder("viewright").build())
    .without(Module::Presence)
    .without(Module::Watchdog)
    .build();

// Only broker, stream, recovery, frameless and clipboard state:
let plugin = ShellPlugin::builder()
    .shell(Shell::builder("viewright").build())
    .install_state_only();
```

An opted-out command family answers `unsupported`. `PathBroker` needs no plugin: create one
in your own state and call its grant and `require_*` methods from your own commands.

### Site-root scheme

`ShellBuilder::site_protocol("viewright-site", SitePolicy::default())` serves allowlisted
subresources beneath a site root. The app grants it for a document it already holds with
`rk_shell_site_grant` (which returns `{token, root, baseUrl}`) and ends it with
`rk_shell_site_revoke`. Denied names and the MIME allowlist are part of `SitePolicy`.

### Typed clipboard, recovery, idle and single instance

- `rk_shell_clipboard_item` returns `{"kind": "files" | "image" | "text" | "none", ...}`. Images are
  written to a granted temp file that is purged after `clipboard_purge_after` (default one hour).
- `RecoverySpec::documents().at_path(p)` pins the snapshot file. `RecoveryStore::migrate_legacy`
  does a one-time, validated migration from an older path.
- `webview_idle_on_error(|failure| ..)` reports failed main-webview suspend or resume calls.
- `SingleInstance::claim(id, on_message)` returns `ClaimOutcome::{Primary, Forwarded, Failed}`.
  Treat `Forwarded` and `Failed` as exit paths. `Failed` never becomes a second primary.
- `startup_cache::ensure_http_cache_dir(identifier)` repairs the user cache directory for the app.
  `ensure_http_cache_dir_with(identifier, || dirs::cache_dir())` does the same under the root your
  Tauri resolves (`dirs::cache_dir`, or the path resolver once an `AppHandle` exists).
- Clipboard reads: `clipboard::read` no longer copies bytes of unrecognised pasteboard types. Opt in with
  `ClipboardOptions { custom_max_bytes: Some(n), .. }` through `clipboard::read_with` or
  `ShellBuilder::clipboard_options`. On macOS text also falls back to the other plain-text types
  (`NSPasteboardTypeString` coercion, `NSStringPboardType`, UTF-16 and generic plain text), as `pbpaste` does.
  `ClipboardItem::None` still serialises as `"none"`; an app whose front end expects `"empty"` maps it.
- `StreamPolicy::default().observer(|event| ..)` receives `StreamEvent { mime, kind }` with
  `StreamEventKind::{Resolved, Head, RangeServed, FullServed, RangeRejected, BytesRead}`. Unset costs one
  branch per request; a panicking observer is caught and never reaches the response.
- `frameless::TrafficLights` is a plain value (`Default`, `Clone`). An app without the shell plugin can
  `app.manage(TrafficLights::default())`, call `set_offset` from its own command and forward window events
  to `handle_window_event`; no `ShellState`, lifecycle hook or single-instance claim is needed.

### Relaunch for update, and the legacy plugins' names

`SingleInstance::destroy()` releases every claim this process holds through the shell (the
standalone claim and the plugin's primary), then runs each release registered with
`SingleInstance::on_destroy`, once. It is idempotent. It never touches a `tauri-plugin-single-instance`
claim by itself: `ClaimOptions::legacy_identifier` only probes for a legacy holder, and the plugin's
mutex, window, socket or D-Bus name belongs to the plugin's own state in the app. An app that still
registers the plugin makes one call release both:

```rust,ignore
let handle = app.clone();
SingleInstance::on_destroy(move || tauri_plugin_single_instance::destroy(&handle));
// relaunch-for-update, after spawning the helper:
SingleInstance::destroy();
app.exit(0);
```

Without the hook, call `tauri_plugin_single_instance::destroy(&app)` and `SingleInstance::destroy()`,
both, before exiting. On Windows the plugin closes its mutex (which frees the name) but its
`DestroyWindow` only works on the thread that created the window, so the hidden window can outlive
the call until exit; the relaunch is not blocked, since arbitration is the mutex.

Exact legacy names (`LegacySingleInstanceNames`, `LegacyAutostartNames`), read from
`tauri-plugin-single-instance` 2.4.2 and 2.4.3, `tauri-plugin-autostart` 2.5.1, `auto-launch` 0.5.0.
HeardRight (`identifier` `app.heardright.next`, `productName` `HeardRight`):

| Item | Derivation | HeardRight |
|---|---|---|
| Windows mutex / window class / title | `<identifier>-sim` / `-sic` / `-siw` | `app.heardright.next-sim` / `-sic` / `-siw` |
| macOS socket | `/tmp/<identifier with . and - as _>_si.sock` | `/tmp/app_heardright_next_si.sock` |
| Linux D-Bus name (not probed) | `<identifier>.SingleInstance` | `app.heardright.next.SingleInstance` |
| Windows Run value | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, value `<entry name>` | `HeardRight` |
| macOS LaunchAgent | label `<entry name>`, `~/Library/LaunchAgents/<entry name>.plist` | `HeardRight`, `~/Library/LaunchAgents/HeardRight.plist` |
| Linux autostart | `~/.config/autostart/<entry name>.desktop` | `HeardRight.desktop` |

The entry name is the plugin builder's `app_name`, else `PackageInfo::name`, which is `productName`
when set and the Cargo package name otherwise. The names assume the plugins' `semver` feature is off
(HeardRight does not enable it). The plugin also writes a same-named value under
`...\Explorer\StartupApproved\Run`; the migration leaves it. HeardRight's lockfile pins
`tauri-plugin-single-instance` 2.4.5, whose source was not available to read; 2.4.2 to 2.4.3 changed
only the macOS listener (tokio), not the names.

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
arbitration fails startup (the plugin setup returns an error): the app never runs as a second
primary. `instance::arbitrate` returns `Arbitration::Unavailable` for OS-level failures and
`Arbitration::InvalidAppId` for an unusable id; the caller must treat neither as "single instance off". The callback owns all second-launch behavior, including URL delivery & focus.

This entry point installs no hardening script, native hardening, navigation
policy, window-state restoration, presence loop or `ShellState`. Use your own
invoke handler; `rightkit_shell::handler!` requires the full plugin. Choose one
plugin entry point per app. The lower-level `instance::arbitrate` remains public.

### Single-instance naming

The single-instance service is named `<id>-shell` (`instance::arbitrate`, the plugins) or
`<id>-instance` (`SingleInstance::claim`), after the id is normalised: ASCII-lowercased, then every
character outside `[a-z0-9-]` becomes `-`, one for one, nothing collapsed or trimmed. A dotted bundle
identifier works: `app.mailright` becomes `app-mailright`, `Mail_Right` becomes `mail-right`. An id that
is already `[a-z0-9-]` is unchanged, so running instances are still found across an upgrade. Distinct
ids can normalise to one name (`a.b` and `a-b`), which makes them the same app for this purpose.
`rightkit-service` allows 1 to 48 bytes of `[a-z0-9-]`, so an empty id, or one longer than 42 bytes
(`-shell`) or 39 bytes (`-instance`), is refused with `Arbitration::InvalidAppId(InvalidAppId)` or
`ClaimFailure::InvalidId`, never with `Unavailable` or `Host`. `instance::try_service_name(id)` returns the
name or the typed error. `ShellBuilder::build` still panics on any id outside `[a-z0-9-]` and 40 bytes.

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

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 119.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: undeclared.
- Disk operations ceiling: undeclared.
- Required scale: 1 watchdogs or more.
- Required hosts: macOS and Windows.
- Status exposure: `status()` in `src/watchdog.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
