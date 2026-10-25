# Changelog

## Unreleased

- **New: `ShellPluginBuilder::host_windows()`, `Shell::host_windows_plugin()` and `ModuleSet::host_windows()`.** The profile for an app that owns its windows: it turns off `WindowState`, `ShowOnReady`, `Presence` and `Watchdog` (`Module::HOST_WINDOWS_OFF`) and keeps `JsHardening`, `NavigationPolicy`, `RevealOnReopen` and the state modules. Like `lifecycle_only`, it leaves `ShowOnReady` and the cache repair off by default, but it keeps the webview policy that `lifecycle_only` removes.
- **New: `ShellPluginBuilder::with(Module)` and `ShellPluginBuilder::modules()`.** `with` re-enables one module after a profile or `without`; `modules()` returns the set `build()` would install.
- **Docs: `lifecycle_only`, `Shell::lifecycle_plugin` and `ModuleSet::lifecycle_only` now say to prefer `host_windows`,** because `lifecycle_only` also removes JS hardening and the navigation policy. Behaviour is unchanged.
- **BREAKING (source): `LogsRuntime::clear` now returns `ClearReport` instead of `io::Result<()>`.** A Rust caller that wrote `runtime.clear()?` must read the report (`is_complete()`, `cleared`, `failed`). The command `rightkit_logs_clear` answers the report when complete.
- **Fixed: `rightkit_logs_clear` cleared only the renderer log.** It now also clears every `LogsConfig::clear_sources` entry (`ClearSource::new`, `ClearSource::jsonl`, a `tracing` family through its guard), attempts all of them even after a failure, and rejects with `clear failed for: <source> (<error>), ...` when any failed, so the client never reads a partial clear as success. Error text goes through `scrub_text` and `redact_url_text`.
- **New: `LogsConfig::settings` (`SettingsSnapshot`).** Called on each collect; its value fills the bundle's `metadata.settings` after key redaction and scrubbing. A panicking closure yields `{"error":"settings snapshot unavailable"}`. Unset keeps `{}`.
- **New: `crash_commands` (feature `crash`).** `rightkit_crash_window_error` no longer needs `host-services`: `crash_commands::install(app, CrashConfig, WindowErrorGate)` manages a `CrashSink`, and `handler!` lists the command whenever `crash` is on. The caller must be an app webview. With `host-services` and a registered `CrashHostService` the host path (central caller policy, then the owner's gate) is taken unchanged and wins over a sink. With neither, the command answers `unsupported`. `host::crash::CrashHostService` keeps its public items, but the command moved from `host::crash` to `crash_commands` and now rejects with `ShellError` (same `{ code, message }` wire shape). An app that enables `crash` without `host-services`, uses `handler!` and also lists its own `rightkit_crash_window_error` now has two commands of that name; drop its own and call `crash_commands::install`, or the shell's answers `unsupported` and renderer crash capture is lost.

## 0.6.0 (2026-10-23)

- **BREAKING (source): `UpdatePolicy` gains the public field `release_status_url: Option<Url>`.** Apps building it with `UpdatePolicy::new(hosts)` (all current apps) are unaffected; a full struct literal must add the field.
- **New (`update-license`): `UpdateHostPolicy::right_suite(app)`, `UpdatePolicy::right_suite(app)`, `TauriUpdateConfig::right_suite(app, public_key, timeout)`** build hosts, the updater endpoint and the release-status URL from `rightkit_license::suite_api` (rightkit#940); apps drop their own host literals and the `plugins.updater.endpoints` copy in `tauri.conf.json`.
- **New: native release status.** `UpdateHost::release_status()` and the command `rightkit_update_release_status` (authorization action `release_status`): https only, no redirect, 200 only, 16 KiB body cap, strict `{schema: 1, appKey, platform, patchVersion, updateVersion}` shape. `UpdateTransport::release_status` has a default, so custom transports keep compiling. `rustls` (ring) becomes an optional dependency of `update`.

## 0.5.1 (unpublished staging)

- **Fixed: features `update` and `update-license` were unusable in a Tauri 2.12 app.** `tauri-plugin-updater` was pinned `=2.10.1`, while Tauri 2.12 apps resolve 2.14, so Cargo could not select both (ViewRight, 2026-10-23: blocks its native licence cutover). The requirement is now `>=2.10.1, <3`. Checked with a consumer-shaped crate: tauri 2.12.5 + tauri-plugin-updater 2.14.0 + features `update,update-license`, `cargo check` green on macOS through the broker. The RightKit workspace lock stays on 2.10.1 until the Tauri 2.12 move; runtime behaviour on 2.14 is the app's qualification.

## 0.5.0 — 2026-10-23

Status: nothing in this section has been compiled, tested or run, and CI has not run on it. The entries are read from source (`crates/rightkit-tauri-shell/src/**`, `tests/**`, `Cargo.toml`); none is verified by a build. Qualification still owed: `rustfmt --check` on the changed Rust files, `cargo test` and `cargo check` with the `wv2-0_38` and `wv2-0_39` feature sets, and a signed-fixture updater test on macOS (cancel, signature, install).

**Windows binding default.** `default` now selects `wv2-0_39` for fresh Tauri 2.12 registry consumers. Apps locked to Tauri 2.11 must use `default-features = false, features = ["wv2-0_38"]` & retain any other selected shell features. Explicit 0.38 support remains; enabling both generations still selects 0.39. Workspace gate rows select 0.38 explicitly against their existing 2.11 lock, while PK08 keeps bare defaults to qualify fresh registry resolution.

**Feature flags.** All new service flags are off in `default`, and each new service dependency is optional (`Cargo.toml`).

- `host-services`: adds `rightkit-app-settings` 0.3.0 (default features off) and the `host` module (`src/host/`): settings bridge, write-only API-key slots, legal receipt, and the caller policy `HostCallerPolicy`.
- `logs`: adds `rightkit-logs` 0.3.4 and `src/logs_commands.rs`.
- `crash`: adds `rightkit-crash` 0.3.4 with its `window-errors` feature (renderer error capture, enabled by the shell's dependency line in `Cargo.toml`), and `watchdog::crash_capture` with `CrashCaptureOptions` (`src/watchdog.rs:1085-1104`). With `host-services` it also registers `rightkit_crash_window_error`, the renderer error sink (`src/host/crash.rs:35`; `HostServicesBuilder::crash`, `src/host/mod.rs:314-316`).
- `models`: implies `host-services`; adds `rightkit-models` 0.5.0 (default features off) and the native model store commands in `src/host/models.rs` (`HostServicesBuilder::models`, `src/host/mod.rs:302-304`).
- `license`: implies `host-services`; adds `rightkit-license` 0.2.4 with feature `update-auth`.
- `update`: implies `host-services`; adds `tauri-plugin-updater` =2.10.1, `reqwest` 0.13.5 (`rustls-no-provider`), `sha2` 0.10 and `tokio` 1 (`macros`, `time`).
- Workspace lock: dev-dependency `tauri-utils >=2.9.3, <2.10` keeps the shared `Cargo.lock` on tauri-utils 2.9.x so tauri 2.11 `test` builds (tauri-plugin 2.7+ pulled tauri-utils 2.10.2 into the lock, E0063 `app_directories_override`). Dev-only: published requirements are unchanged and apps on Tauri 2.12 are unaffected.
- `update-license`: implies `update` and `license`; adds `url` 2 and the licence authority `LicenseAuthority` (`src/update.rs:1317`).
- `overlay`: adds `rightkit-overlay` 0.2.0 and `src/overlay_host.rs`.
- Documentation: README feature table now lists `qa-probes` and `app-lock`; the stale `dev-inspect` feature text is removed (the feature does not exist in `Cargo.toml`; `DEV_INSPECT_COMPILED` is always `false`, `src/devtools.rs:60-62`).
- `pdf-host`: implies `host-services`; adds `rightkit-pdf` 0.3.3 with its `host` feature (`Cargo.toml`) and `src/pdf_host.rs` (`src/lib.rs:51`). PDFKit drives the view on macOS; every other platform answers `unsupported` (`src/pdf_host.rs:41`).

**New modules.** Compiled on every feature set: `associations`, `caller_gate`, `open_paths`, `os_settings`, `preview`, `print`, `surface`, `webview2_env`, `webview2_menu` (`src/lib.rs`). Behind features: `host` (`host-services`), `logs_commands` (`logs`), `overlay_host` (`overlay`), `update` (`update`), `pdf_host` (`pdf-host`).

**Commands.** Registered by `handler!` in `src/lib.rs` (`__rk_handler_*` macros for the feature-gated families):

- `rightkit_settings_describe|get|patch|reset|subscribe|unsubscribe`, `rightkit_secrets_presence|set|clear`, `rightkit_legal_receipt_read|create|reset`, `rightkit_preview_register|revoke|revoke_all` (`host-services`). `patch` takes `expectedRevision` (compare-and-set; `src/host/settings.rs`). No secrets command returns a secret (`src/host/secrets.rs`). The legal service records what the renderer declares it showed (`src/host/legal.rs`).
- `rightkit_license_status|activate|refresh|deactivate|request_trial` (`license`). The views `LicenseView`, `RefreshView` and `TrialView` carry no token, key or device id (`src/host/license.rs:16-18`).
- `rightkit_update_check|status|download|install|cancel` (`update`; `src/update.rs:1425-1471`).
- `rightkit_logs_write|collect|clear` (`logs`).
- `rk_shell_associations_status|set`, `rk_shell_open_default_apps` (`src/associations.rs`); `rk_shell_open_paths_ready` (`src/open_paths.rs`); `rk_shell_set_surface` (`src/surface.rs`); `rk_shell_print` (`src/print.rs`); `rk_shell_set_menu_item_checked`, `rk_shell_set_menu_state` (`src/commands.rs`).
- `rightkit_pdf_open|update|state|find|print|suspend|close` (`pdf-host`): registered in `src/lib.rs:1564-1581`. Each command authorizes the caller through the installed `PdfHostService` under `PDF_SERVICE`, registered by `PdfServiceExt::pdf` (`src/pdf_host.rs:521-528`).
- `rightkit_models_list|install|cancel|remove` (`models`): registered in `src/lib.rs:1523-1531`. `install`, `cancel` and `remove` take a declared model id only (`src/host/models.rs:296-329`).
- `rightkit_crash_window_error` (`host-services` + `crash`): registered in `src/lib.rs:1545-1553`.
- Caller guard: every `rk_shell_*` command takes the invoking `Webview` and calls `guard_command` first (`src/commands.rs:30-41`). With `host-services` the call is checked against the host caller policy, but only once the app enables `HostServicesBuilder::guard_shell_commands`. Without `host-services` nothing is checked and the call is allowed. This is not a default deny.

**Native updater (`update`).**

- Signature: the plugin's minisign verification runs before the bytes are returned (`src/update/tauri_transport.rs:208-209`).
- HTTPS only: the transport client sets `https_only(true)` (`tauri_transport.rs:74`). `UpdateHostPolicy` allows only https, no userinfo and exact hosts, and checks that before the authority is asked (`src/update.rs:524-527`).
- No redirects: `redirect::Policy::none()` (`tauri_transport.rs:81-82`); every redirect hop fails closed.
- Downgrade refusal: the plugin's `version_comparator` accepts only `release.version > current` (`tauri_transport.rs:143`).
- Authorization: the licence authority supplies headers for each request, redirect hops included (`src/update.rs:541-554`). Headers are removed from the cached native update object (`tauri_transport.rs:4-5`).
- Licence-bound install: the binding is captured once at check (`src/update.rs:35-38`), rechecked when the download completes (`:1127`), and rechecked at install admission (`:1194`). A licence change since the check refuses the update.
- Staging: the SHA-256 and length of the staged bytes are recorded (`tauri_transport.rs:87`) and checked again at install (`src/update.rs:42`).
- Progress and cancel: event `rightkit://update-progress` (`src/update.rs:74`); `rightkit_update_cancel` returns `{ cancelled }` (`:368-371`); at most 4 handles, each with a 1-hour TTL (`:78-80`).

**Associations, open paths, OS settings.**

- `associations`: `AssociationSpec` carries the app's bundle id and type list. `rk_shell_associations_set` asks macOS, which may show a consent sheet; on Windows it opens Default apps and reports `userActionRequired`. The timeout is clamped to 1 to 120 s (`src/associations.rs`).
- `open_paths`: paths from launch arguments, a second launch and macOS `RunEvent::Opened` are queued until the renderer calls `rk_shell_open_paths_ready`. Only existing regular files survive (`src/open_paths.rs:87-89`), each is granted through `PathBroker`, and the renderer never receives a raw path. They are delivered as `rightkit-shell://open-paths` to the main webview (`:39`).
- `os_settings`: `SettingsPane` is a closed enum mapped to fixed URIs per OS (`src/os_settings.rs`). It is a Rust API only; there is no command.

**Watchdog and engine.**

- `WatchdogConfig` gains `observer_gap` (default 1 s: a late wake suppresses the stall test until a fresh beat), `capture`, `on_event` and `shutdown_bound` (default 2 s) (`src/watchdog.rs:182-232`). One heartbeat ping is outstanding at a time (`:185`). New public items: `status()`, `shutdown()` returning `StopReport`, `WatchdogStatus`, `ShellRuntimeStatus`, `CaptureOutcome`, `WatchdogEvent`, and `DEFAULT_CAPTURE_COOLDOWN` (60 s, `:78`).
- `engine` (feature): `EnginePolicy` gains `presence` (`PresencePolicy`, with `idle_exit`), `snapshots` (`SnapshotSlot`: `key`, `method`, `required`) and `shutdown_drain` (`src/engine.rs:133-153`). `EngineHost` holds presence through `EngineHold` with a refcount, and latches the engine generation: a reply from an earlier generation is not delivered (`src/engine.rs:22-24`). `EngineHost::shutdown_ordered()` returns `EngineShutdownReport` (`first`, `drained`, `abandoned_calls`, `holders_left`, `engine`; `:481`).

**Menu, hardening, WebView2, print, preview, surface.**

- Menu: `MenuEntry` gains `Check` and `Submenu` (nested up to `MAX_SUBMENU_DEPTH` = 4), built with `MenuEntry::check` and `MenuEntry::submenu` (`src/menu.rs:34-47`). `MenuOverride`, `OverrideRole` and `MenuItemState` let an app override built-in roles and set item state (`src/menu.rs`).
- Hardening: `Hardening::disable_browser_accelerators` turns WebView2's accelerator keys off for that webview, all or nothing (`src/harden.rs:55-63`).
- `webview2_env`: the WebView2 user-data folder and browser arguments; applied at startup, before the first window (`src/webview2_env.rs`). `webview2_menu`: one native context-menu handler that composes the DevTools, hardening and app stages in a fixed order (`src/webview2_menu.rs`).
- `print`: WebView2's in-app print preview on Windows, the system print operation on macOS, `Unsupported` elsewhere (`src/print.rs`).
- `preview`: in-memory documents served from their own custom scheme with their own CSP; `ShellBuilder::preview_protocol` (`src/preview.rs`, `src/lib.rs`).
- `surface`: named window shapes through `SurfaceSpec`, `ShellBuilder::surface` and `surface_fallback` (`src/surface.rs`, `src/lib.rs`).
- `overlay_host`: the native host for an always-on-top, no-activate panel, around `rightkit-overlay` (`src/overlay_host.rs`).

**BREAKING.**

- `MenuEntry` is `#[non_exhaustive]` (`src/menu.rs:22`). Matches outside the crate need a `_` arm. Build rows with the constructors `MenuEntry::item`, `check` or `submenu`.
- `Hardening` gains `disable_browser_accelerators` (`src/harden.rs:63`, default `false`). Struct literals must end with `..Hardening::default()`.
- `WatchdogConfig` gains `observer_gap`, `capture`, `on_event` and `shutdown_bound` (`src/watchdog.rs:204-215`). Struct literals must end with `..WatchdogConfig::default()`.
- `EnginePolicy` gains `presence`, `snapshots` and `shutdown_drain` (`src/engine.rs:133`). Struct literals must end with `..EnginePolicy::default()`.
- Rust callers of these `rk_shell_*` functions must pass the invoking `Webview<Wry>` as the first argument: `rk_shell_open_external`, `rk_shell_open_window`, `rk_shell_open_login_items_settings`, `rk_shell_finish_quit`, `rk_shell_presence`, `rk_shell_capabilities` (`src/commands.rs`). `rk_shell_finish_quit`, `rk_shell_presence` and `rk_shell_capabilities` now return `Result<_, ShellError>`. JavaScript `invoke` arguments are unchanged, because Tauri injects `Webview`.

- `app-lock` feature (off by default; `host-services` + `rightkit-credentials` 0.4.0 `os-auth`): `NativeAppLock` (`src/app_lock.rs`) takes an `Arc<dyn EngineLock>` and an explicit lifecycle policy chosen natively (`InvalidateOnly` or `Lock`; the webview cannot change it). The `rightkit_app_lock_*` commands (`src/host/app_lock.rs`) take no arguments beyond the caller webview and go through the caller gate; unlock runs the OS prompt (Windows Hello / macOS LocalAuthentication) outside every lock, and a document generation check refuses a stale authorization after navigation or invalidation. Fails closed: any auth, credential or state error leaves the app locked. Navigation, window destruction and resume invalidate; there is no background or idle-timeout relock in this version. Local unlock never grants remote approval, PIN or signing authority.

## 0.4.0 (published 2026-10-19)

- Runtime budget: the wake-up gate is the watchdog's cost over a bare Wry loop measured in the same bench run (`ceilingBasis` delta-over-bare, 4/s), not the whole-process rate, which host and WindowServer activity move by 2x within a day (#789). The bench runs bare and watchdog in separate, interleaved child processes and reports the median pair; the whole-process figure stays as recorded evidence.
- Engine errors are structured, window-state saves preserve generation order, and QA controls stay excluded from release builds.
- BREAKING: `EnginePolicy::default()` now denies every engine method (it allowed all). List methods with `allow_methods`, or opt in with `allow_any_method()`. `ShellBuilder::engine(spec)` is removed; use `engine_with_policy(spec, policy)`.

All notable changes to `rightkit-shell`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## 0.3.3 (2026-10-18)

- Fixed: `build.rs` emitted `cargo:rustc-link-arg-benches` on windows-msvc, but the published package ships no `benches/`, so Cargo refused to build the crate for every Windows consumer (#774). Each link-arg kind is now emitted only when its directory exists.

## 0.3.2 (2026-10-17)

- Changed: `frameless::button_frames`, `ButtonFrame` and `ButtonFrames` are compiled only with the `qa-probes` feature and `clipboard::qa` are compiled only with `qa-probes` in a debug-assertions build (QA probe, never in a release build): the gate is `all(feature = "qa-probes", debug_assertions)` and a release build that enables `qa-probes` fails with `compile_error!`. The 0.3.2 note already said so; the code did not.
- Added: `frameless::TrafficLights::set_inset(&window, x, y)` places the buttons at an absolute inset (macOS).
- Added: `tray::LeftClick::Show` (left click reveals the window and never hides it).
- Changed: the app-menu quit item (macOS "Quit <app>", Windows/Linux "Exit <app>") is a shell item that routes to the ordered quit, not the predefined quit, so close-to-tray no longer swallows Cmd+Q.
- Added: `OAUTH_POPUP_ORIGINS` gains `mcp.atlassian.com`, `auth.atlassian.com`, `login.salesforce.com`, `test.salesforce.com`. `nav::strip_coop_headers`, `nav::popup_origins_from_json`, `NavPolicy::decide_window_open_pinned` (opener must equal a pinned origin) and `NavPolicy::protocol_consent_scheme` (the scheme of a non-web `window.open` target an app may confirm; `decide_window_open` still returns `Deny` for it). `NavPolicy` and `WindowOpenDecision` keep their previous public shape.
- Changed: the shell's `on_new_window` hook passes the opening window's current URL (`WebviewWindow::url`) to the popup decision, so an opener not served in place is external. Partial for #638: the shell does not yet carry a pinned server origin (no `ShellConfig` setting), so pinned-origin popups need the app to call `decide_window_open_pinned`; the hook uses the unpinned rule.
- Note (unchanged, pre-existing policy): DevTools `AllowDev` (dev-inspect) and `AllowContent` (ContentOnly) decisions remain.
- Added: engine policy and tray options from the CutRight gap patch G15-G18 (see `engine::EnginePolicy`, `tray::TrayOptions`). Not built or run locally.

## 0.3.2

- Added (Orthic-Labs/rightkit#309; not built or run locally):
  - `PathBroker::revoke_path` matches display paths case-insensitively on macOS as well as Windows (those volumes are case-insensitive), so a differently-cased path for a deleted file is revoked. Other Unix stays exact.
  - `PathBroker::with_app_name(&str)`: denial text reads "through {app} first" instead of "through the app first". The shell builds its broker from `ShellConfig::app_name`; `PathBroker::new()` and `default()` keep "the app".
  - `site::validate_chosen_site_root(&Path) -> Result<PathBuf, ShellError>` is public: the folder must exist, be a directory and not be a filesystem root or the home directory.
  - `site::infer_site_root_report` returns `InferredSiteRoot { root, marker_found }` so an app can tell a marker hit from the document-folder fallback; `infer_site_root` is unchanged.
  - Release note: 0.3.2 carries `clipboard::qa` and `frameless::button_frames` behind `qa-probes` (entries below), so apps can delete their local QA probe copies.

- Added (Orthic-Labs/rightkit#247; additive and opt-in, not built or run locally): macOS 13+ launch at login through `SMAppService.mainApp`, so the app is a named item in System Settings > General > Login Items instead of a legacy LaunchAgent.
  - `AutostartOptions::macos_backend: MacosBackend { LaunchAgent (default, unchanged), ServiceManagement }`. With `ServiceManagement`, `autostart::set_enabled` registers or unregisters `SMAppService.mainApp` and removes the shell's old LaunchAgent plist; `autostart::migrate_launch_agent` (run at startup by the shell when the backend is on) registers and removes an existing plist; `migrate_plugin_entry` registers instead of writing a plist. It falls back to the LaunchAgent on macOS 12 and older, for an executable outside a `.app` bundle, and whenever `RIGHTKIT_SHELL_HOME` or `config_home` redirects the entry (so a sandboxed run never registers the real user's login item).
  - New `login_item` module: `LoginItemStatus { NotRegistered, Enabled, RequiresApproval, NotFound, Unknown(i64) }` with `login_item_status_from_raw`, `is_enabled`, `needs_approval`; `register`, `unregister`, `status`, `available`; `open_login_items_settings` (`SMAppService.openSystemSettingsLoginItems`); `set_background_io(bool)` (`setiopolicy_np` `IOPOL_THROTTLE`, the process-level equivalent of the plist's `LowPriorityIO`). `AutostartStatus` gains `login_item: Option<LoginItemStatus>` (omitted from JSON unless the backend is in use; `enabled` is true only for `Enabled`). New command `rk_shell_open_login_items_settings`, and `@rightkit/tauri-shell` types `LoginItemStatus` and `AutostartStatus.loginItem`.
  - Behaviour differences the app must handle: `SMAppService` carries no launch arguments, so `AutostartOptions::args` (`--background`) and `low_priority_io` are ignored and the app detects a login launch itself; a signed `.app` bundle is required; `requiresApproval` means registered but not yet allowed by the user. No new dependency or Cargo feature: the `SMAppService` calls use the existing `objc2` runtime and link the ServiceManagement framework, so `Cargo.lock` is unchanged.
  - Tests: status mapping, unknown raw values, serialisation, bundle detection, backend selection, register and unregister outcome rules (`login_item::tests`), and the sandbox fallback (`tests/autostart.rs`). The `SMAppService` calls need a signed app on a Mac (`mainApp` register, `requiresApproval`, unregister, Settings deep link).

- Security (behaviour change; Orthic-Labs/rightkit#109): dev-control no longer follows `cfg(debug_assertions)`, which is on in the dev profile used by installed dev builds. `NavPolicy::default().allow_loopback` is now `false` unless the new non-default `dev-overrides` feature is on. The `devtoolsOpen` readback in the QA state command (`commands.rs`) is compiled only with `content-devtools` or `dev-inspect`; otherwise it reports `false`. The inspector-closing call in `devtools::apply_native` keeps `debug_assertions` in its cfg because it only closes an inspector on a denied webview. App migration: an app that relied on loopback navigation in dev builds enables `rightkit-tauri-shell/dev-overrides` from its QA/dev feature (apps may also set `allow_loopback: true` explicitly in `NavPolicy`); an app that relied on the devtools readback in dev builds enables `dev-inspect`. Not built or run locally.

- Added (Orthic-Labs/rightkit#232; additive, moved from ViewRight so it can delete its copies; not built or run locally):
  - `frameless::button_frames(&WebviewWindow) -> Result<ButtonFrames, ShellError>`: read-only macOS probe of the close, minimise and zoom buttons (`standardWindowButton` frame, hidden flag, plus `window_visible`), run on the main thread (through the shell's dispatch when the plugin is installed, otherwise inline or queued with a 5 s wait). Typed `ButtonFrame { x, y, width, height, hidden }` and `ButtonFrames { close, miniaturize, zoom, window_visible }`, with `ButtonFrames::to_json()` in ViewRight's control-command shape. Returns `ShellError::Unsupported` off macOS. It never moves a button, orders the window in or activates the app.
  - `clipboard::named_board_refusal(name)` is now public and available on every platform (it was private, macOS and test only): the pure guard that refuses empty names and the `Apple CFPasteboard ...` system board names.
  - New non-default feature `qa-probes` (development/QA builds only) compiles `clipboard::qa`: `paste_from_board(&PathBroker, app_id, raw)` (typed read of a private named pasteboard, as JSON; refuses system names before touching a pasteboard), `stage_board(raw, prefix)` / `plan_stage` (put a file URL and/or a generated 1x1 PNG on a board whose name starts with the app's QA prefix), `control_args` and `test_png`. Apps register them as `rightkit-qa` control commands. The paste result uses the shell's `kind` of `none` where ViewRight's own type said `empty`.
  - Example app: control commands `traffic_lights`, `stage_board`, `paste_from_board`, `refuse_system_boards`, a `rightkit-qa.toml`, and the macOS-only scenarios `scenarios/clipboard-named-board-mac.scenario.toml` and `scenarios/traffic-lights-mac.scenario.toml`.
  - Tests: system-name refusal (existing `clipboard` tests plus `clipboard::qa` tests that the refusal runs before any pasteboard or temp directory is touched), staging prefix rules, test PNG shape, `ButtonFrame` mapping and the JSON shape. The macOS pasteboard read and the AppKit button read need a Mac run.
- Added (Orthic-Labs/rightkit#230; additive, not built or run locally): minimise-only idle for an app with a single web view (Pulse's hub, `main`).
  - `IdleTrigger { HiddenOrMinimised (default), Minimised }`, `IdleOptions::trigger(..)` and `IdleOptions::on_minimise(true)` (shorthand for `Minimised`), `IdleOptions::minimise_enabled()`, plus the pure `source_starts_idle(trigger, IdleSource)`. With `Minimised` a window minimise always feeds the idle machine (no `start_on_minimize` needed) and a tray hide, close-to-tray or `note_hidden` does not. Whole-app: it acts on the webview of the window that was minimised (the main window); no per-label idle.
  - Windows: minimise is `SetIsVisible(false)` at once (or on the timer with `ControllerHide::OnTimer`) then `TrySuspend` after `suspend_after`; nothing is closed. Restore is `Resume` when suspended, then `SetIsVisible(true)`; a restore before the timer only shows. macOS and Linux: the machine and the `rightkit-shell://idle` event run, nothing is suspended (documented no-op; the app pauses its own timers).
  - Tests: trigger gating and the minimise, restore, double minimise and restore-while-not-suspended sequences, on the pure state machine and a fake controller (`tests/idle_options.rs`, `idle::tests`). The WebView2 calls need a real Windows app.

## 0.3.1

- **`dev-inspect` now enables `tauri/unstable`** (Orthic-Labs/rightkit#103). `devtools.rs` calls `AppHandle::get_webview`,
  which Tauri 2 exposes only with `unstable`, so apps no longer add it to their own dev-only feature. No source change;
  default features unchanged.

## 0.3.0

Renamed from rightkit-shell; 0.2.0 remains published under the old name.

- **`WindowSpec` per-window options (owner gap M1-G1), all additive and optional.** New fields `minWidth` / `minHeight`
  (default stays 320 x 240; invalid values fall back; the initial size is raised to the minimum), `decorations`
  (override after the frameless policy), `center` (wins over `x` / `y`), `fullscreen`, `skipTaskbar`, and `x` / `y`
  (initial logical position, corrected onto a visible monitor when it would be off-screen). New pure helpers
  `windows::spec_min_size` and `windows::correct_offscreen`. Without the new fields `windows::open` builds the same window as
  before. Tests: `tests/window_spec.rs`.

- **Window state: `WindowStateStore::import_legacy` migrates an app's own geometry store once (owner gap M1-G5).**
  Reads a legacy flat `label -> state` JSON file (CodeRight `window_state/mod.rs:92,110-118` shape; `version` 0 means
  physical pixels), runs the same v0 -> v1 migration the store applies on load, maps labels with `(legacy, owner)` pairs
  (empty map keeps all), never overwrites an existing owner entry, and writes a `window-state.imported-<source_id>`
  marker so later calls return `LegacyImport::AlreadyImported`. A missing file is not marked. Additive; the store file
  format is unchanged. Tests: `tests/window_state_store.rs`.

- Tests: the two Windows tests that write under `HKEY_CURRENT_USER` (`devtools_policy.rs` real key, all of
  `autostart_registry_windows.rs`) run only on CI (`GITHUB_ACTIONS=true`) or with `RIGHTKIT_TEST_REAL_USER_STORES=1`, and otherwise print why and pass;
  each asserts its key starts with its test prefix, and the DevTools key is removed however the test ends. The HTTP
  cache directory test uses a temporary root instead of creating a folder under the user's cache root. Shared helper:
  `tests/common/mod.rs`. No product change.
- **Clipboard: `take_item_from_with(&Board, ..)` reads a private pasteboard without touching the user's clipboard.**
  New `clipboard::take_item_from_with(board, broker, temp_dir, policy, options)`; `take_item_with` is now its
  `Board::General` wrapper and behaves as before. For `Board::Named` on macOS the whole read stays on that pasteboard:
  the two AppleScript fallbacks (`public.file-url` without a path, PNG coerced from TIFF or an image type) read the
  general clipboard, so they are skipped for a named board (decided by the pure `fallback_allowed`,
  `needs_file_fallback`, `needs_png_fallback`, and each osascript helper now takes the board and does nothing for a
  named one). `platform_snapshot` takes the board; it no longer reads `Board::General` unconditionally. Windows and other
  platforms: `Board::Named` returns the reader's "named boards are macOS-only" error; `Board::General` is unchanged.
  Also: a `Board::Named` whose name is empty or in the `Apple CFPasteboard ...` family (the system boards, including
  the general pasteboard, which `pasteboardWithName` would otherwise return) is now refused by `read`, `write` and
  `formats` on macOS, and Windows `formats(Named)` now returns the same error as `read` instead of listing the user's
  clipboard formats. Tests: pure fallback-decision tests on every platform, and macOS tests on uniquely named
  pasteboards (`rightkit-shell-test-<pid>-<n>`) that are released afterwards.
- **Windows: a policy that substitutes the browser runtime is refused, and the port-file tripwire looks where the browser
  really writes (ledger 180, findings 7 and 8).** `guard_process_start` now refuses the WebView2 `BrowserExecutableFolder`
  policy when a value applies to this program (`DevToolsError::BrowserRuntimeRedirected { source, folder }`, code
  `browser_runtime_redirected`); only `*` and this program's executable file name match, without regard to case. The
  environment variable of the same purpose is refused the same way when set at start. The folders a `UserDataFolder`
  policy and the WebView2 user-data environment variable name are added to the tripwire's folders, and an app that gives
  a webview its own `data_directory` registers it with the new `devtools::register_user_data_dir`. The tripwire runs once
  more when the first page has finished loading (`verify_after_first_navigation`), because the browser can write its port
  file after the webview is ready. An unreadable policy key refuses the start, as before. New: `policy_value_applies`,
  `runtime_folder_violations`, `runtime_folder_env_violation`, `runtime_folder_env_name`, `policy_user_data_dirs`,
  `expand_env_refs`. README: the pipe form of remote debugging leaves no port file, and which WebView2 environment
  variables the guard inspects.
- **Windows: a denied webview whose DevTools cannot be disabled is refused (ledger 180, findings 3 to 6).**
  `apply_native` ignored the result of `SetAreDevToolsEnabled(false)`. It now sets the value, reads it back, and when the
  webview has no settings object, the call fails or the value is still on, it closes the webview and reports the new
  `DevToolsError::NativePolicyNotApplied { label, detail }` (code `native_policy_not_applied`); the shell sets nothing
  else up for that webview. **API change:** `devtools::apply_native` returns `Result<(), DevToolsError>` (always `Ok`
  off Windows). When Tauri runs the native step later instead of at once, the function has already returned `Ok` and
  the refusal is the printed error and the closed webview. A webview that may be inspected is never refused.
  New: `DevToolsSwitch`, `enforce_switch`, `is_blocked_accelerator`.
- **Windows: browser keys are stopped one by one, never all at once (ledger 180; MailRight lost Ctrl+P and keyboard zoom).**
  The DevTools layer turned every WebView2 browser accelerator key off in a denied webview, and the hardening did the
  same for `block_reload` or `block_history_nav`. Neither calls `SetAreBrowserAcceleratorKeysEnabled(false)` any more.
  Each webview has one `AcceleratorKeyPressed` handler that stops exactly the keys its active rules name: F12,
  Ctrl+Shift+I / J / C and Ctrl+U in a denied webview; F5, Ctrl+R, Ctrl+Shift+R and the refresh key only with
  `block_reload`; Alt+Left / Right and the back and forward keys only with `block_history_nav`; Ctrl with plus, minus
  or zero only with `block_zoom` (Ctrl+wheel stays with `IsZoomControlEnabled`). Ctrl+P, Ctrl+F and every other key
  pass. **Behaviour change:** a denied webview without `block_reload` now reloads on F5, and without
  `block_history_nav` goes back on Alt+Left. New: `KeyRules`, `KeyModifiers`.
- Windows: the context-menu item name is freed after it is read, and a webview that becomes ready again has its previous
  context-menu and key handlers removed before new ones are added. The hardening layer's own context-menu handler is
  unchanged.
- **Windows: the WebView2 policy check no longer fails open (ledger 180, findings 1, 2, 8).** The guard read the
  `AdditionalBrowserArguments` policy keys by running `reg.exe query` and treated exit code 1 as "no policy". `reg.exe`
  exits 1 for a missing key and for a key it could not read alike, and a program started by name is looked up in the
  application's directory and the working directory before the system's. The keys are now read through the registry API
  (`RegOpenKeyExW` with `KEY_READ` and an explicit 64-bit or 32-bit view, `RegEnumValueW`): `ERROR_FILE_NOT_FOUND` is the
  only status that means no policy, and any other failure to open or read a key is `DevToolsError::RegistryUnreadable`,
  so the guard refuses to start. No process is started. `REG_SZ`, `REG_EXPAND_SZ` and every string of a `REG_MULTI_SZ`
  are checked. API: `parse_reg_query` is removed; new `decode_reg_text`, `PolicyKeyOpen`.
- **`dev-inspect` feature (development builds only).** Off by default, never a default feature, independent of
  `content-devtools`. Only when compiled in: every webview is inspectable (`WebviewDecision::AllowDev`,
  `DevToolsGate::is_dev_inspect`, new `build_gate_with`), the process guard no longer refuses debugging
  arguments or a registry policy and no longer scrubs the WebView2 override variables, the `DevToolsActivePort`
  tripwire is off (a remote-debugging port, e.g. the wdio driver, works), the marker
  `RIGHTKIT-SHELL-DEV-INSPECT-COMPILED` (`DEV_INSPECT_MARKER`, `#[used]` and read from startup code) is
  embedded, and one stderr line announces that developer inspection is active. Without the feature the policy is
  unchanged. `right-release` refuses the feature and the marker in release and update builds and `rightkit-qa`
  rejects it in default features. Written, unverified: nothing was compiled or run.
- **DevTools policy (owner's fixed design).** Every Right app has no access to DevTools; ViewRight alone may
  inspect the HTML content it opens, never its own UI or shell. New `devtools` module: `DevToolsPolicy`
  (`Denied` default, `ContentOnly`), `ContentWebview`, `DevToolsGate`, `DevToolsError`, `guard_process_start`,
  `open_content_devtools`, plus `ShellBuilder::devtools_policy` / `content_webview`. Every webview is made
  non-inspectable (macOS `isInspectable` and `developerExtrasEnabled` false; Windows `AreDevToolsEnabled`
  false, accelerator keys off, Inspect removed from context menus) whatever modules are installed; the process
  refuses a remote-debugging launch argument, removes the WebView2 override environment variables and refuses a
  debugging `AdditionalBrowserArguments` policy. The hardening script now also cancels Ctrl+Shift+J/C,
  Cmd+Opt+C/J/U (by key code). **Breaking:** the `Devtools` enum and `Hardening::devtools` are removed (no
  "on", no "debug builds"); the `devtools` cargo feature is now `content-devtools`. Written, unverified (not
  compiled, tests not run; Objective-C and WebView2 calls are the uncertain part).
- **Per-hide idle delay provider.** `IdleOptions::default().suspend_after_with(|| Some(d))` (or on a
  literal, chained) reads the suspend delay at each hide transition instead of fixing it at build time.
  The provider runs once per hide (a window hide, or the start of a minimise with `start_on_minimize`),
  on both platforms (the timer code is shared). `None` uses the fixed `suspend_after`; a panic in the
  provider is caught and also uses it; an explicit per-hide delay (`note_hidden_after`, `note_minimized_after`,
  `rk_shell_idle_note`) still wins. Values are clamped to `MAX_SUSPEND_AFTER`.
  **Breaking for struct literals and copies:** `IdleOptions` is no longer `Copy` (it may hold the provider),
  and its new private field means a literal outside the crate needs `..IdleOptions::default()`. The one
  literal in this repository is `lib.rs`, updated to set `suspend_after` on the options. `IdleStatus.suspend_after_ms`
  still reports the fixed value.

Single-instance naming defect (found by MailRight): `instance::arbitrate("app.mailright", ..)` returned
`Unavailable("invalid service name ..")` because `rightkit-daemon` names allow only `[a-z0-9-]`, so a
dotted bundle identifier ran with single-instance protection off.

- **Service names are derived from the app id by a documented normalisation**: ASCII lowercase, every
  character outside `[a-z0-9-]` becomes `-` (`app.mailright` -> `app-mailright-shell`). An already valid id
  gives exactly the same name as before. The caller identity used for the allowlist and the connect is the
  same normalised id, so differently spelled equivalents agree.
- **New `Arbitration::InvalidAppId(InvalidAppId)`** for an id that cannot be named (empty, or a name over
  48 bytes). `Arbitration` is not `#[non_exhaustive]`, so an exhaustive `match` in an app now fails to
  compile: that is intended, handle it by refusing to start. `arbitrate` also prints a line to stderr. New
  `instance::{try_service_name, InvalidAppId, InvalidAppIdReason}`; `instance::service_name` now normalises.
- **`ClaimFailure::InvalidId(String)`** for `SingleInstance::claim*` (name `<id>-instance`, so ids over 39
  bytes are refused). Same normalisation; it was previously a generic `ClaimFailure::Host`.
- The plugin entry points (`single_instance_plugin`, the full plugin) fail startup with a message naming
  the id on `InvalidAppId`; OS-level `Unavailable` behaves as before.

Gaps ViewRight reported, closed additively (CutRight, PostRight, GenRight and ScrapeRight need no change):

- **`TrafficLights` works without `ShellState`.** `set_offset` and `reapply` now fall back, when the shell
  plugin is not installed, to running inline on the macOS main thread (`pthread_main_np`) or queueing to it
  (fire and forget). New: `Clone`, `Debug`, `new()` and `handle_window_event(window, event)` (re-applies on
  resize, focus gain and scale change). With the plugin installed the old dispatch is unchanged.
- **`StreamPolicy::observer(|event| ..)`.** Typed `StreamEvent { mime, kind: StreamEventKind }`: `Resolved`
  (size), `Head`, `RangeServed`, `FullServed`, `RangeRejected`, `BytesRead { bytes, ranged }`. Zero cost
  when unset; an observer panic is caught and dropped. `StreamPolicy` gains the public field `observer`
  (full struct literals add `observer: None`, or use `..Default::default()`).
- **`startup_cache::ensure_http_cache_dir_with(identifier, root_resolver)`**: repair under a caller-supplied
  cache root (for example `dirs::cache_dir`). `ensure_http_cache_dir` is unchanged and delegates with
  `user_cache_root`.
- **Clipboard.**
  - macOS text read falls back to `NSStringPboardType`, `public.utf16-external-plain-text`,
    `public.plain-text` and `public.text`, asking the board first (AppKit coercion, as `pbpaste -Prefer txt`).
  - **Behaviour change:** `clipboard::read` (and `rk_shell_clipboard_read`) no longer copies bytes of
    unrecognised pasteboard types into `custom`. Opt in with `ClipboardOptions::custom_max_bytes`
    (per-type cap) through `clipboard::read_with` or `ShellBuilder::clipboard_options`. Windows never read
    unknown formats.
  - `ClipboardOptions { image_preference: ImagePreference::{PngFirst, DibFirst}, custom_max_bytes }`,
    `read_with`, `take_item_with`, `ShellBuilder::clipboard_options`, `ShellState::clipboard_options`.
    `DibFirst` (Windows) takes `CF_DIB` as BMP and falls back to `PNG`.
  - Windows review: a write now owns the clipboard through a message-only window (a NULL owner makes
    `SetClipboardData` fail after `EmptyClipboard`); new Cargo feature `Win32_UI_WindowsAndMessaging` on the
    `windows` crates. Not run against a live Windows clipboard.
  - `dib_to_bmp`: `BI_BITFIELDS`/`BI_ALPHABITFIELDS` masks are separate only after a 40-byte header; V4/V5
    headers carry them inside, so the pixel offset was too large and such DIBs were refused.
  - `ClipboardItem::None` still serialises as `"none"` (unchanged). ViewRight's own type said `"empty"`; map it
    in the app, because serde cannot alias a serialised name.

- **Webview idle matches ScrapeRight's WebView2 ordering.** Suspend is one operation on the WebView2
  thread, `SetIsVisible(false)` then `TrySuspend`; a failed hide means no suspend, the webview stays
  visible and running, the failure goes to `webview_idle_on_error`, and the next hide starts a fresh timer.
  Resume is one operation, `Resume` then `SetIsVisible(true)`; a failed resume means no show, and a failed
  show is no longer swallowed: either is reported and retried by the next resume request. Resume makes no
  controller call unless this shell hid or suspended the webview. The idle timer is decided on the
  WebView2 thread, so a show that races it wins. Counters now count successful transitions.
- **`ShellBuilder::webview_idle_on_transition(|t| ..)`**: `IdleTransition { kind: Suspended | Resumed,
  window, elapsed }`, once per real transition (Suspended: time hidden; Resumed: time suspended). Unset: no
  change.
- **`idle::note_minimized_after(app, minimized, Option<Duration>)`** (and `minimize_action`): the minimise
  trigger takes the per-hide delay `note_hidden_after` takes. `note_minimized` is unchanged (`None`).
- **Behaviour change, `never_show_env`.** Only `1`, `true`, `yes`, `on` (ASCII case-insensitive, exact, no
  trimming) mean never-show; every other value, including `native` or padded text, is now off (previously
  any value but empty/`0`/`false`/`no`/`off`). New `ShellBuilder::never_show_env_value(name, expected)`
  matches only that exact value. Empty names panic at startup as before.

Additive otherwise. The one behaviour change is `lifecycle_only` only, and is called out below.

- **Placement announced.** When a webview becomes ready the shell emits the app event `rk-shell:placement` `{label, phase:"begin"}`, runs window-state restore, then emits `{label, phase:"done"}` queued on the main thread behind the restore's size and position calls (`windows::with_placement_announced`, `windows::EVENT_PLACEMENT`). `rightkit-control` holds `set_viewport` until `done`, which fixes the first viewport request after a hidden launch being overridden by the late restore (1440x900 requested, 1440x789 settled). Announcing never shows, orders or focuses a window and runs under never-show too.
- **One visibility guard.** Every path that shows, unminimises or focuses a window asks
  `lifecycle::reveal_mode(app, RevealSource)` (decision: `ShowPolicy::mode`): Dock reopen, opened URL,
  second instance, show-on-ready, tray Show and toggle, the `rk_shell_window` `show` command,
  `windows::open`, `windows::reveal` and `WindowStateStore::restore_with`. Answers: `Forbidden` (no
  window call at all), `Background` (`RIGHTKIT_QA_BACKGROUND`: native order-back only) or `Foreground`.
  `windows::apply_reveal` + `RevealTarget` is the seam the tests record through.
- **`ShellBuilder::never_show(bool)` and `never_show_env(name)`.** Absolute host-declared never-show: no
  show, unminimise or focus under any event, windows built through `windows::builder` are unfocused, hide
  and reveal never change the macOS activation policy, and on macOS the policy is set to Prohibited at
  plugin setup (and again when each webview is ready). `never_show_env` registers extra environment
  variables that mean never-show when set to anything but empty, `0`, `false`, `no` or `off`, read at each
  reveal (ScrapeRight: `RIGHTKIT_QA_NATIVE_REAL`). The `show` command answers `Ok` and does nothing.
- **`ShellBuilder::reveal_on_reopen(bool)` and `Module::RevealOnReopen`.** Default on. Off, a Dock reopen
  and an OS-delivered URL or file deliver their links and reveal nothing. `Module::ALL` is now 12;
  `state_only` keeps the module (its behaviour is unchanged). `show_on_ready_decision` is unchanged.
- **Webview idle.** `ShellBuilder::webview_idle_options(IdleOptions { suspend_after, start_on_minimize,
  controller_hide, events })`. `start_on_minimize` starts the timer when the main window is minimised
  (and a restore or focus ends it). `ControllerHide::Immediate` (default, the donor behaviour: the
  WebView2 controller is hidden as the window hides) or `OnTimer` (hidden when the timer fires, then
  suspended). `IdleEvents::NativeSuspendOnly` stops `rightkit-shell://idle` events where nothing is
  suspended (macOS, Linux); default `Always`. `suspend_after` defaults to 30 s
  (`DEFAULT_SUSPEND_AFTER`); `webview_idle(d)` still works. Per-hide override: `idle::note_hidden_after`
  and `rk_shell_idle_note({hidden, suspendAfterMs})` (optional). Delays clamp to 24 h.
- **Behaviour change, `lifecycle_only` only.** `ModuleSet::lifecycle_only()` / `Shell::lifecycle_plugin()`
  now also leave `ShowOnReady` off, and the startup cache repair defaults to off. The host turns them on
  with `show_on_ready(true)` and `repair_cache(true)`. `Shell::plugin()`, `ShellPlugin::builder()` sets and
  `install_state_only` are unchanged.
- **Legacy macOS socket: a listener nobody serves is no longer taken for a live holder.** `connect`
  succeeding to `/tmp/<id>_si.sock` only proves that some process still has the listening descriptor open;
  a legacy holder's descriptor can leak into a sidecar it spawned and keep the socket accepting after the
  app exited, so a new launch "forwarded" its arguments to nobody and exited (the user double-clicks the
  app and nothing opens). This is also what made the stale-socket test flaky on macOS CI: `socket()` cannot
  set close-on-exec atomically there, so a fork+exec on another test thread inherited the listener of a
  dropped `UnixListener`. The plugin never replies (`tauri-plugin-single-instance` 2.4.2 and 2.4.3,
  `macos.rs`: the holder reads to end-of-stream, runs the callback, drops the connection), so the forward
  now shuts its write side and waits up to 2 s for the holder to close (one kernel receive timeout, no
  sleep loop), and reads the socket's owner with `getsockopt(SOL_LOCAL, LOCAL_PEERPID)`: an owner pid that
  no longer runs is stale and nothing is written to it; an owner that runs a different program (not this
  executable, not this `.app` bundle) and stays silent is stale; a close confirms the launch. A silent
  socket owned by this app's own process, or by an owner that could not be identified, keeps today's
  behaviour (`ClaimOutcome::Forwarded` / `Arbitration::Secondary`) but is logged on stderr as "forwarded
  but NOT confirmed". A torn-down connection to a possibly live holder fails closed
  (`ClaimFailure::Legacy`). A stale verdict removes nothing; the existing stale-file removal in the legacy
  hold is unchanged. Worst-case `claim` time on macOS with a legacy holder grows by 2 s (documented on
  `ClaimFailure`). No public type changed. Windows and Linux unchanged. Written, unverified: nothing was
  compiled or run; the `LOCAL_PEERPID` answer for a connection the listener has not accepted yet is the
  uncertain part.

Additive except where marked **Breaking**. Each item has a test in `tests/`.

- **Quit drain (opt-in).** `ShellBuilder::quit_drain(true)` / `quit_drain_bound(Duration)`. A quit request
  (tray Quit, `ExitRequested` from Cmd+Q or the last window) then emits `rightkit-shell://quit-requested`
  (`{source, boundMs}`) and prevents the exit; `rk_shell_finish_quit` sets `exiting`, then exits, and the
  resulting `ExitRequested` does not prompt again. The host exits on its own after the bound: 15 s by
  default (`quit::DEFAULT_QUIT_DRAIN_BOUND`, above the npm package's 10 s handler budget), clamped to 120 s.
  With no webview window left there is nobody to drain and the quit is immediate. `rk_shell_quit` stays
  immediate. Off by default: Quit is unchanged. New commands `rk_shell_finish_quit` and
  `rk_shell_capabilities` (`{quitDrain, notifyBypassDnd}`) are in `handler!`. `quit::QuitGate` is the whole
  decision, tested without an event loop.
- **Notification DND bypass.** `NotificationRequest::bypass_do_not_disturb` (`bypassDoNotDisturb`, serde
  default `false`) skips only the Do-Not-Disturb check; `notify::suppression` is the pure decision.
- **Close behaviour and visibility hooks.** `ShellBuilder::close_behavior(CloseBehavior)`; `HideToBackground`
  hides on close like `close_to_tray` and, on macOS, sets Accessory when the main window hides (and no other
  window is visible) and Regular before it is revealed. `ShellBuilder::on_visibility(Fn(&AppHandle, Visibility))`
  runs after each main-window hide or reveal for a host that drives its own policy. Decision logic
  (`lifecycle::activation_for`, `ActivationTracker`) is tested on every host; the `set_activation_policy`
  call is not exercised against a real NSApplication.
- **Single instance, host-decided.** `qa_instance_exempt(true)`: a `RIGHTKIT_QA_HIDDEN=1` launch neither
  claims the primary nor forwards (`lifecycle::instance_mode`); `second_instance_reveal(Fn -> bool)` keeps the
  default event and deep links but lets the host decide whether to reveal and focus. `show_on_ready(..)` is now
  an explicit visibility decision that `RIGHTKIT_QA_HIDDEN` no longer overrides (`--background` still does);
  when never called the behaviour is unchanged. `single_instance(false)` and `on_second_instance` already existed.
- **Lifecycle-only entry.** `Shell::lifecycle_plugin()` / `ShellPluginBuilder::lifecycle_only()` /
  `ModuleSet::lifecycle_only()`: the full plugin minus `JsHardening`, `NavigationPolicy`, `Presence`,
  `Watchdog` and `WindowState`. `Shell::plugin()` is unchanged.
- **Extra internal schemes.** `NavPolicy::internal_schemes`, `NavPolicy::with_internal_scheme`,
  `ShellBuilder::internal_scheme`: a host's own custom protocol loads in place. Web, script and `file`
  schemes are refused. The built-in set is unchanged.
- **Window state, standalone.** `WindowStateStore` works without the builder (explicit directory, nothing
  restored unless called): new `saved`, `save_state`, `restore_with(RestoreOptions { unminimize, show, focus,
  restore_minimized })`, and the `RestoreTarget` seam. Minimised windows are saved (`WindowState::minimized`,
  last normal geometry kept; old files load). `restore` is unchanged.
- **Autostart options.** `AutostartOptions::startup_wm_class` (Linux `StartupWMClass`),
  `config_home` (injected config home, no environment), `macos_unsupported` (report unsupported, write no
  LaunchAgent); `linux_desktop_entry_for`, `reports_unsupported`. Defaults write the same files as before.
- **Breaking (struct literals only).** `NavPolicy`, `WindowState`, `AutostartOptions` and
  `NotificationRequest` each gain fields; build them with `..Default::default()` (`NotificationRequest` from
  JSON). `ShellState` gains fields.
- **Reverse upgrade order: hold the legacy claim.** `ClaimOptions::with_legacy(id).holding_legacy()` makes
  the primary also hold and service `tauri-plugin-single-instance`'s claim (Windows: mutex
  `<id>-sim` plus a hidden `<id>-sic`/`<id>-siw` window on a dedicated message thread taking
  `WM_COPYDATA` 1542; macOS: the `/tmp/<id>_si.sock` listener, mode 0600; Linux: no-op, the D-Bus
  name is not implemented). An old build launched later forwards its cwd and argv to `on_message` and
  exits. The primary endpoint is hosted first and only its winner takes the legacy claim; a live old
  holder instead receives this launch. Payloads are bounded (64 KiB, 4096 args) and parsed
  defensively. `destroy()` and guard drop stop and join the thread and free the names. Legacy
  forwards no longer block on a hung holder (Windows `SendMessageTimeoutW`, abort-if-hung, 5 s; macOS
  write timeout 5 s), claim hosting waits at most 5 s, and `ClaimFailure` documents the bounds and
  what an app should do per variant.
- **Watchdog.** A failed `rightkit-shell://stall` emit no longer ends the loop: it is counted
  (`WatchdogStatus::emit_failures`) and the watchdog keeps re-arming. `WatchdogConfig::emit_event = false`
  is a callback-only mode in which the poll thread is never given an `AppHandle`.
- **Watchdog idle wake-ups (runtime budget).** The watchdog no longer polls. It used to wake every
  `poll_interval` (250 ms) and sleep a fixed heartbeat period on a second thread, 9 to 13 wake-ups a
  second measured on an idle event loop. Now the heartbeat thread sleeps one `heartbeat_interval` on a
  condition variable and pings, and the watchdog thread sleeps until the last beat plus the threshold
  (about every 4 to 5 s with the defaults), looks at the beat then, and sleeps to the next such moment:
  about 1.2 wake-ups a second from the two threads. After a report the watchdog thread waits for the
  next beat, which wakes it, or for the stop, which also does; a healthy watchdog is never woken by a
  beat. Hang detection is not later: a stall is reported at the last beat plus the threshold (one
  millisecond past it), and never more than `poll_interval` after that. `WatchdogConfig::poll_interval`
  keeps its field, default and 10 ms floor, now meaning that lateness bound instead of a polling
  period; `heartbeat_interval` gains the same 10 ms floor (zero used to spin). A beat newer than the
  one a stall was reported against now re-arms the detector even if it has already gone stale
  again (the 250 ms poll could miss that). The two threads stay separate: the heartbeat thread holds
  the `AppHandle`, the watchdog thread still never does in callback-only mode. Public API unchanged
  (`install`, `status`, `WatchdogConfig`, `StallDetector`, `should_fire`, `DEFAULT_POLL_INTERVAL`).
  Tests in `src/watchdog.rs`: stop ends both threads out of hour-long waits, a beat wakes a stalled
  watchdog and does not wake a healthy one, a silent beat is reported within the threshold plus the
  poll period, a failed emit still re-arms, and at most three wake-ups in a quiet two seconds.
  Not yet re-measured: the recorded budget evidence predates this change.
- **Autostart migration carries `StartupApproved`.** On Windows the plugin's Task Manager flag is read; a
  disabled flag is copied to the shell's value name (the user stays off), and the legacy flag is removed
  with the legacy command. `autostart::migrate_plugin_entry_in(&RegistryKeys, ..)` runs it against
  scratch keys; `startup_approved_is_disabled` is `auto-launch`'s rule.
- **Test binaries load on Windows.** A `build.rs` embeds the Common Controls v6 manifest dependency in
  test binaries (`STATUS_ENTRYPOINT_NOT_FOUND` before `main` otherwise).
- The legacy names were confirmed by HeardRight against its locked `tauri-plugin-single-instance` 2.4.5.
- **Relaunch for update.** `SingleInstance::destroy()` is documented and tested as idempotent, and
  now also runs releases registered with `SingleInstance::on_destroy` (once each, a panicking one
  does not stop the rest), so an app that still registers `tauri-plugin-single-instance` releases
  both claims with one call. `ClaimOptions::legacy_identifier` only probes; this type never owns a
  legacy claim.
- **Legacy names as data.** `LegacySingleInstanceNames` (mutex, window class and title, socket,
  D-Bus name) and `LegacyAutostartNames` (Run value, LaunchAgent label and plist, desktop entry), pinned
  with HeardRight's values in `tests/legacy_names.rs`. The Linux legacy autostart entry is now found
  under `~/.config/autostart` as the plugin writes it, even when `XDG_CONFIG_HOME` differs.

- **Site-root scheme** (`site` module, `ShellBuilder::site_protocol`). A second custom scheme,
  `<app>-site://`, serves allowlisted subresources beneath a site root granted for a document the app
  already holds (`rk_shell_site_grant`; ended with `rk_shell_site_revoke`). Root inference looks for
  project markers, never a filesystem root or the home directory, and falls back to the document's
  folder. Denied names default to the dotenv family and the npm and netrc credential files; a
  `SitePolicy` can add more (exact name, or `prefix*`). Only the MIME allowlist (`site_mime`) is
  served, with percent-decoding, containment checked after canonicalization, and `nosniff`.
- **Multiple stream schemes.** `stream_protocol` may be called repeatedly; each call adds a scheme,
  and a repeated scheme replaces its earlier entry. `rk_shell_stream_grant` takes an optional
  `scheme`. `ShellState` gains `stream_schemes`.
- **Per-type stream caps.** `StreamPolicy::max_bytes_by_kind` (keys: an extension, a full MIME type,
  or a MIME family such as `video`), with `StreamPolicy::with_kind_cap` and `max_bytes_for`. Lookup
  is extension, then full MIME, then family, then `max_bytes`. `.ico` maps to `image/x-icon`.
- **Modular install** (`modules` module). `ShellPlugin::builder().shell(..).without(Module::X).build()`
  opts out of `JsHardening`, `NavigationPolicy`, `WindowState`, `ShowOnReady`, `Watchdog`, `Presence`,
  `Broker`, `Stream` (which also covers the site scheme), `Recovery`, `Frameless` and `Clipboard`. An
  opted-out command family answers `unsupported`. `install_state_only()` keeps the broker, stream,
  recovery, frameless and clipboard state and drops the rest. `Shell::plugin()` is unchanged.
  `PathBroker` is a plain library type that needs no plugin.
- **Recovery.** `RecoverySpec::at_path` pins the snapshot file. `RecoveryStore::migrate_legacy` does a
  one-time, validated copy from a caller-supplied legacy path and returns a `MigrationOutcome`.
  Cache-directory repair moved to `startup_cache`; `recovery` re-exports it.
- **Typed clipboard.** `take_item` and `item_from_snapshot` return `ClipboardItem` (`files`, `image`,
  `text`, `none`), exposed as `rk_shell_clipboard_item`. Priority is files, then a bitmap, then text.
  A bitmap goes to a granted temp file: PNG when the clipboard offered it, otherwise Windows `CF_DIB`
  converted to BMP. macOS calls AppleScript only where AppKit cannot supply the data (a file URL it
  cannot turn into a path, or a TIFF coerced to PNG). Temp files are purged after
  `ShellBuilder::clipboard_purge_after` (default one hour). `dib_to_bmp` is public.
- **Startup cache.** `startup_cache::ensure_http_cache_dir(identifier)` repairs
  `<user cache root>/<identifier>` without a Tauri handle. The identifier must be one path component.
- **Idle failures.** `ShellBuilder::webview_idle_on_error` receives a typed `IdleFailure`
  (`phase`: suspend or resume, `window`, `error`) for each platform suspend or resume that fails.
  Windows previously dropped these silently.
- **Standalone single instance** (`single_instance` module). `SingleInstance::claim` and `claim_with`
  return `ClaimOutcome::{Primary, Forwarded, Failed(ClaimFailure)}`. A second launch forwards an
  `InstanceMessage` (argv, cwd, optional action, JSON argument map). Undecidable claims fail closed.
  Only the connect step is retried, so the callback never runs twice for one launch. Malformed
  payloads are refused.
- **Breaking (struct literals only).** `StreamPolicy`, `RecoverySpec` and `IdleHost` each gain a
  field (`max_bytes_by_kind`, `exact_path`, `on_error`). Code that builds them with full struct
  literals must add the field, or use `..Default::default()` (`StreamPolicy`) or the builder methods
  (`RecoverySpec::at_path`, `IdleHost::with_on_error`). `ShellState` gains fields; read it, do not
  construct it.
- Authority tests ported from ViewRight (`tests/authority.rs`): known-token exchange,
  canonicalization, stream-token revocation not revived by reopen, site-root tokens and revocation,
  site-root grant is not a file grant, independent per-tab tokens, symlink escape, and revoked-root
  serving. `tests/site_scheme.rs` covers denied names, symlink escape, percent-decoding, the MIME
  allowlist and root inference. `tests/transport.rs` covers state and transport.
  `tests/single_instance.rs` runs the two-process single-instance tests.

## 0.2.0

- Breaking: the `engine` feature (engine sidecar wiring over `rightkit-jobs` and
  `rightkit-process`) is no longer a default feature. It pulled `rightkit-jobs`
  and, through it, `db`, `time`, and `uuid` into every consumer. Apps that use
  `Shell::builder(..).engine(..)`, `shell::engine`, or `Shell::engine` now enable
  it: `features = ["engine"]`. `default` is now `["wv2-0_38"]`.
- The example app opts in to `engine` explicitly, and drops its unused
  `rightkit-daemon` dev-dependency and duplicate `rightkit-control` dev entry.
