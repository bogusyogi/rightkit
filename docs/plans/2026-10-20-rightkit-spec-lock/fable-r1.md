# RightKit locked package specification (draft, Fable, 2026-10-20)

Status: DRAFT for matching against Astra's independent draft. Nothing in it is enacted. When matched and approved by Adrian it is committed as docs/SPEC/ and the row ids below become the only vocabulary for gaps, exceptions and completion claims.

## 0. What this document locks

For every mandatory row (M1-M23) and its owner packages, this specification fixes:
1. The requirement: what an app must obtain from the owner, stated as behaviour.
2. The owner surface: the public API the app may use, at the locked version. Anything not listed is not a contract and may change.
3. App obligations: what the app must route through the owner, and what it must delete.
4. Permitted app-side code: data and composition the app may keep without an exception.
5. The exception rule: the only way other app code may remain.
6. Qualification evidence: what proves a release of the owner and an app's adoption.
7. Open owner work: agreed extensions with named deletion consumers (from the 2026-10-20 consensus), each with its acceptance.

## 1. Locking rules

L1. A row is locked when its requirement, surface and obligations are written here and the owner version named here is published with registry evidence (rightkit-versions.json registry section, not stagedCargo/stagedNpm).
L2. Surface changes are additive within a locked minor; a removal or behaviour change bumps the minor, updates this document in the same change, and names every app that must move.
L3. The required version in ownership-mandatory-set.json equals the latest published version of each owner. Apps pin exactly (`=x.y.z` / exact npm). There is no downgrade path; a wrong release is followed by a fixed release.
L4. Staged work (CHANGELOG "unpublished staging", RightKitSwift "held") is not part of any locked surface until published. Specifications may name it only under "Open owner work".
L5. M21 and M23 are rows with owners here; "ownerPending, never fails" is retired when this document is approved.
L6. Generated inventories (INDEX.md, CAPABILITY-MAP.md, rightkit-ownership.json) describe availability and fingerprints. This document is the behavioural contract; where they disagree, this document wins and the inventory is regenerated.

## 2. Exception rule (applies to every row)

E1. The default is zero app code for a row beyond the permitted data and composition listed in that row.
E2. An exception is one entry in `.rightkit/ownership-exceptions.json` `{capability, package, reason, owner, date}` plus a record in `docs/rightkit-exceptions/M<n>.md` with: the function (file:line), its observable behaviour, why this app differs from every other app, why the owner's configuration or extension points cannot express it, and either a sunset condition (for compatibility code) or the evidence that no other app can need it (for product code).
E3. These reasons are never sufficient alone: platform-specific, only current consumer, historical format, thin adapter, performance without a measurement, "product policy" without a named behaviour that differs between apps.
E4. A reason that two apps share is not an exception; it is owner work (row 7 of that row's contract) and both apps carry a TEMPORARY entry with the owner issue as its sunset.
E5. The one settled product exception: ViewRight renders its DevTools inside the document iframe (D2 in rightkit#771). No other app has DevTools in any build.
E6. Exceptions are re-verified at every owner release of that row; a record whose cited lines no longer exist is void.

## 3. Qualification evidence (applies to every row)

Q1. Owner release: local gate green on both hosts (Windows, macOS) for every feature set in ci/gate.config.json; runtime budget measured where the row requires; Oracle source review of the exact diff; registry record written to rightkit-versions.json.
Q2. App adoption: exact pin at the locked version; owner symbols imported and executed on the production path (not a declaration); the deleted duplicate named by path in the app's CHANGELOG; the row's exception records re-verified; `right-release ownership-mandatory --enforce` passes.
Q3. Native security rows (M11, M12, M13, M21): additionally a negative test per refusal listed in the row (the owner refuses, the app does not fall back).
Q4. Completion is never a pin, a line count or a declaration.

## 4. Version policy at lock

Rows are locked at the latest published version on 2026-10-19 (registry readback), with the staged versions named only as open work:

| Owner | Locked (published) | Staged, not locked |
|---|---|---|
| rightkit-tauri-shell | 0.4.0 | 0.5.0 |
| rightkit-device | 0.3.1 | - |
| @rightkit/tauri | 0.2.3 | - |
| @rightkit/react-shell | 0.7.1 | 0.7.2 |
| @rightkit/platform-ui | 0.4.0 | - |
| @rightkit/theme | 0.3.1 | - |
| rightkit-job-queue | 0.4.0 | 0.4.1 |
| rightkit-child-process | 0.5.2 | 0.5.3 |
| rightkit-sidecar | 0.3.0 | - |
| rightkit-daemon | 0.3.0 | 0.3.1 (held) |
| rightkit-fs | 0.4.4 | 0.4.5 |
| rightkit-db | 0.5.0 | - |
| rightkit-app-settings | 0.2.4 | - |
| @rightkit/settings-ui | 0.6.1 | 0.7.0 |
| rightkit-credentials | 0.3.0 | 0.3.1 |
| rightkit-license | 0.2.3 | 0.2.4 |
| @rightkit/license | 0.2.1 | 0.3.0 |
| @rightkit/legal | 0.6.1 | - |
| @rightkit/legal-ui | 0.4.1 | - |
| @rightkit/onboarding | 0.6.2 | - |
| rightkit-logs | 0.3.3 | 0.3.4 |
| @rightkit/logs | 0.1.5 | 0.1.6 |
| rightkit-crash | 0.3.3 | - |
| rightkit-http | 0.3.6 | - |
| @rightkit/app-update | 0.4.0 | 0.4.1 |
| @rightkit/release | 0.4.0 | - |
| rightkit-qa | 0.7.1 | - |
| @rightkit/qa | 0.5.1 | - |
| @rightkit/qa-chrome | 0.1.8 | - |
| rightkit-agent-tools | 0.2.0 | - |
| RightKitSwift | 0.8.1 (tag) | 0.9.0 (held) |

The mandatory-set file raises device, qa, legal-ui and onboarding minimums to these values in the same change that approves this document.
## 5. Row contracts

Each row: (1) Requirement, (2) Owner surface locked at the published version, (3) App obligations, (4) Permitted app-side code, (5) Exception rule, (6) Qualification evidence, (7) Open owner work with named deletion consumers. "Locked" items are published (registry section). Items marked STAGED are open work, not contract (L4). Source references are to the working tree on 2026-10-20.

---

### M1 Window host

Owners (locked): rightkit-tauri-shell 0.4.0, rightkit-device 0.3.1, @rightkit/tauri 0.2.3, @rightkit/tauri-shell 0.1.1. Proposed addition when row 7 item (b) ships: rightkit-overlay (0.1.2 published) as the overlay state owner.

**1. Requirement.** One Tauri 2 desktop host per app, built from `Shell::builder(app_id)`, that owns: webview hardening (JS and native), navigation policy, external open, single instance with legacy arbitration, deep links, window state persist/restore, native menu, tray, autostart/login item, rich clipboard, notifications, presence and idle, watchdog, quit drain, path broker for file grants, preview/site/stream protocols, recovery store, frameless traffic lights. DevTools denied in every app build; the one content-only grant is ViewRight (E5). Every renderer-reachable host command is registered through the owner's `handler!` macro and is gated by the owner's caller rules.

**2. Owner surface (0.4.0).**
- Builder: `ShellBuilder` methods listed in surface-m1-m2 §rightkit-tauri-shell (app_name … engine_with_policy, build); `Shell::plugin()`, `lifecycle_plugin()`, `single_instance_plugin(cb)`; `ShellPlugin::builder().without(Module)`; `handler![..]`.
- Commands (0.4.0 set): rk_shell_open_external, ready, status, window, open_window, set_tray, set_menu_item_enabled, autostart, open_login_items_settings, clipboard_read/write/item, engine_call/_tagged/_status (feature `engine`), traffic_lights, quit, finish_quit, capabilities, set_theme, notify/_status/_clear, badge, set_dnd, presence/_prefs/_probe/_evaluate, idle_status/_note, path_require/_revoke/_grant_recent/_regrant_saved, stream_grant, site_grant/_revoke, recovery_save/_load/_clear.
- Events: `rightkit-shell://menu|tray|deep-link|second-instance|stall|engine|window|idle|presence|quit-requested`, `rk-shell:placement`.
- Modules (pub): autostart, b64, broker, clipboard, commands, deep_link, devtools, engine (feature), error, frameless, harden, idle, instance, lifecycle, login_item, menu (item/accel/separator), modules, nav, notify, opener, preview, quit, recovery, single_instance, site, startup_cache, stream, tray, watchdog, webview2_menu, window_state, windows (WindowSpec 7 fields).
- Features: `wv2-0_38` (default), `wv2-0_39`, `engine`; never default: `content-devtools`, `dev-overrides`, `qa-probes`.
- Not contract (STAGED 0.5.0): associations, caller_gate, logs_commands, open_paths, os_settings, print, webview2_env, menu Check/Submenu/overrides, WindowSpec extra fields, `rk_shell_set_menu_item_checked/_set_menu_state/_print/_open_paths_ready/_associations_*/_open_default_apps`.
- rightkit-device 0.3.1: `SecureStore<T>`, `MutationHook`, `SecureService`, `DeviceService{new, with_lock_file, get, get_or_create}`, `device_id_hash`, `AuthorizationService` (authorize, authorize_with_clock, authorize_any_device*), `guard_paid_command`, `LogHost/LogService`, `DiagnosticHost/DiagnosticService`; feature `credentials` -> `CredentialStore`.
- @rightkit/tauri 0.2.3: `createRightKitTauriClient`, per-app constructors, `standardRightKitTauriCommands`, `standardRightKitDeviceGetOrCreate`, `./dialog` pickFolder/pickFile/pickFiles. Not contract: 0.3.0 licence removal, dialog-qa.
- @rightkit/tauri-shell 0.1.1: `createShell`, `tauriTransport`, `EVENTS`, `bridge()`; the 40 commands it wraps.

**3. App obligations.**
- Build exactly one `Shell` with the app id (lowercase, ≤40 bytes); add the plugin first; register every command via `handler!`.
- Route through the owner and delete the app copy of: hardening scripts, navigation/window-open hooks, external opener, single-instance claim, deep-link parsing, window-state store, menu/tray build, autostart/login item, clipboard read/write, notification posting, idle suspend, watchdog, quit drain, path grants, recovery store, frameless traffic lights, webview context menu (`webview2_menu::install_context_menu`).
- `DevToolsPolicy::Denied`; no `tauri/devtools`; no `.devtools(true)` webview. ViewRight alone: `content-devtools`, `ContentOnly`, registered content webviews, ACL manifest, `devtools: content-only` in release config.
- Device identity: one `DeviceService` in managed state over `rightkit-device::credentials::CredentialStore` (M11); device id never set from the renderer; only `device_id_hash` leaves the machine.
- Renderer uses `@rightkit/tauri-shell` and `@rightkit/tauri`; no hand-written invoke wrappers for owner commands.
- Match `wv2-*` to the app's Tauri generation; call `shell.ready()` once; Windows toast AUMID set to the installer AUMID.

**4. Permitted app-side code.** Builder configuration values (app id, labels, scheme names, menu/tray specs, hardening flags, nav policy hosts, watchdog config), app command functions passed to `handler!`, menu/tray event handlers, the app's content ACL manifest (ViewRight), window specs. Nothing that opens a window, claims an instance, reads the clipboard, posts a notification or installs a WebView2 hook outside the owner.

**5. Exception rule.** §2. ViewRight iframe inspector is the only settled exception (E5) and stays app code until the owner's content-webview rule can express iframe inspection (README:82 says it cannot today).

**6. Qualification evidence.** Q1 on both hosts with `wv2-0_38` and `wv2-0_39` sets; Windows clipboard/hardening/idle executed on a Windows host (README:45 gap closes); devtools scanner negative test (release build with `content-devtools` refused by right-release); single-instance Forwarded/Failed exit paths tested; Q2 per app names the deleted files.

**7. Open owner work.**
- (a) Publish 0.5.0 as the first locked minor above 0.4.0 after Q1; moves associations/open_paths/print/os_settings/caller_gate/webview2_env into contract. Consumers: ViewRight (file associations, open-with), MailRight (mailto scheme stays installer), all apps (webview2_env apply). Acceptance: README feature table lists `logs`, `qa-probes`; dev-inspect text removed; `logs_commands` either in `handler!` or documented as app-registered with a caller gate test.
- (b) Overlay window host in tauri-shell composing rightkit-overlay: transparent non-activating window, level, live resize, work-area clamp, accessory start, destroy-on-close, Windows z-order walk for `covering_window`. Consumers: HeardRight pill, Pulse notch (Swift host via RightKitSwift), ViewRight floating panels. Acceptance: three app overlay hosts deleted; `OverlayKind` product names replaced by app-supplied kinds in a minor.
- (c) Host command registration (consensus R3): one caller-gated command set in tauri-shell for settings (M10), logs (M15), licence (M12), update (M18) over the existing stores; registered by `handler!`; credentials never get a renderer get/set. Consumers: all six apps delete their `rightkit_logs_*`, settings, licence and update command wrappers. Acceptance: @rightkit/settings-ui shell-adapter METHODS, @rightkit/license commands, @rightkit/app-update native commands all resolve to tauri-shell-registered handlers; negative test that a content-origin caller is refused.
- (d) @rightkit/tauri-shell: wrap the remaining Rust commands; `MenuEntry` Check/Submenu; `WindowSpec` full field set; README version line. Consumers: ViewRight (menu checks), MailRight (window specs).
- (e) rightkit-device: README feature/dep text; `AuthorizationDenial` `#[non_exhaustive]` at the next minor; name reconciliation with rightkit-license CHANGELOG (`authorize_any_device`).

---

### M2 App frame

Owners (locked): @rightkit/react-shell 0.7.1, @rightkit/platform-ui 0.4.0, @rightkit/theme 0.3.1.

**1. Requirement.** Every app webview renders inside the owner's frame: `ShellProvider` + `AppShell`/`PageShell` (or a slot-replaced region), owner shortcut registry, command palette, toasts, dialogs, context menus, UI primitives, theme store and boot script, platform detection and chord formatting, brand tokens generated from a brand file with contrast checks. Brand-specific values are data; layout and interaction code are the owner's.

**2. Owner surface.**
- react-shell 0.7.1: `.` nav/theme/match/geometry/toast/shortcuts/bridge/commands/hover; `./react` hooks, ui primitives (Button … Pill), ErrorBoundary, overlays (ModalShell, Dialog, ActionDialog, ConfirmDialog, PromptDialog), ContextMenu/useContextMenu, ToastViewport, CommandPalette/ShortcutHelp, Popover, ShortcutRecorder, ShellProvider + hooks, layout (Titlebar, AppFrame, Plane, AppearanceMenu, JumpTo, NavList, PageShell, AppShell); `./headless` useToggle/useSegmentedControl; stylesheets `shell.css`, `shell.layered.css`, `suite-aliases.css`. Keybinding defaults mod+k, ?, mod+\. Not contract: 0.7.2 `FuseCurves orientation`.
- platform-ui 0.4.0: `.` platform/chord/capabilities/window-control adapters; `./react` usePlatform, Kbd; `./headless` floating panel, toast bus, focus trap, dismiss, exit transition, list navigation, ranking; `./qa` (QA builds only).
- theme 0.3.1: colour/brand/css/check/runtime/tokens exports; `./node`; CLI `rightkit-theme generate|contrast|tokens`.

**3. App obligations.** Mount `ShellProvider` with the `AppShellBridge` from @rightkit/tauri-shell; import one stylesheet once; run `applyStoredThemeSync` or `themeBootScript` before first paint; generate tokens with `rightkit-theme generate --check` in the build; inject `window.__RIGHTKIT_PLATFORM__` from the host; delete app copies of: shortcut registries, palettes, toast systems, dialog shells, context menus, titlebars/caption buttons, theme stores, platform sniffing, primitive components that duplicate the ui set, hover guards.

**4. Permitted app-side code.** Brand file, nav item data, command definitions, page components, slot contents, keymap overrides, CSS that sets brand tokens, app-specific composite components built from the primitives.

**5. Exception rule.** §2. A visual difference is not an exception; a missing primitive is row 7 work with the app as consumer.

**6. Qualification evidence.** Q1 (npm gate, Chrome e2e; WebKit project is a named gap); Q2 names deleted component files per app; contrast report attached to the app release.

**7. Open owner work.** Consumers from rk-compliance-v2 item 13: theme presets + picker, Textarea/Table/Tooltip, prop pass-through, tab strip, sidebar rail, popover placement policies (ViewRight, MailRight, CodeRight named); MailRight deletes its three raw tables on Table landing. React 19 peer `<21` kept. README staleness (platform-ui QA exports, react-shell 0.6.9 heading, theme app-shell references) fixed at the next publish.

---

### M3 Engine supervision

Owners (locked): rightkit-job-queue 0.4.0, rightkit-child-process 0.5.2.

**1. Requirement.** An app that runs an engine process supervises it with `EngineSupervisor` (lifecycle Booting/Ready/Recovering/Down, restart policy, health check, hello/contract check, snapshot replay, bounded shutdown, stderr redaction) over `OwnedCommand`/`OwnedChild` tree ownership. No app implements its own restart loop, hello handshake, stall detector or kill-tree.

**2. Owner surface.**
- job-queue 0.4.0 feature `supervisor`: `EngineSupervisor` (new/start/start_background/call/call_with/set_snapshot/subscribe_lifecycle/subscribe_events/status/wait_ready/shutdown/shutdown_detailed), `SupervisorConfig` builder (with_exit_observer … with_stderr_sink), `EngineClient` (connect*/spawn*/hello/call*/subscribe/kill/reap/shutdown*), `Lifecycle`, `ExitReason`, `ShutdownReport`, `HealthCheck`, `ExitObserver`; `doctor` (`run_doctor`, `DoctorProbe`); `crash` feature `CrashExitObserver`. Not contract: 0.4.1 `with_handshake`/`HandshakeCall`, activity registry, `FifoScope::CrossProcess`.
- child-process 0.5.2: `OwnedCommand`/`OwnedChild`/`AdoptedProcess`, `ProcessHandle`, `RestartPolicy/RestartTracker`, `HealthPolicy/HealthTracker`, `StallWatchdog`, `HeartbeatWatch`, `NativeOutputCapture`, `StderrTail`, `terminate_all_owned`, `WindowsJob`, `PowerAssertion`. Not contract: 0.5.3 `run_command`, `run_streaming`, breakaway builder options.

**3. App obligations.** Spawn engines only through `SupervisorConfig::new(name, spawn_fn)` with an env-allowlisted `OwnedCommand`; use `require_current_contract_version`; route every engine call through `EngineSupervisor::call`; call `shutdown_detailed` before exit; delete app supervisors, hello/ready parsers, restart timers, stall watchdogs, stderr tailers, kill-tree helpers. Pulse and CodeRight adopt (currently neither); ScrapeRight and HeardRight keep theirs (already adopted); MailRight composes daemon `supervise_adopted` (M7) with job-queue dispatch.

**4. Permitted app-side code.** Spawn closure (binary path, args, env allowlist), `EnginePolicy` method allowlist, exit observer that maps to app UI, snapshot method/params.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1 with feature sets `supervisor`, `contract`, `service`, `crash`; child-process Windows suites executed on the Dell (README "Nothing here was run on Windows" closes); runtime budget measured (currently "not yet measured"); Q2 negative test: engine killed mid-call yields `Recovering` then `Ready`, no app-level retry code remains.

**7. Open owner work.**
- (a) child-process 0.5.3 publish after the capture-propagation fix is verified reachable: `run()` currently uses `command_mut()` which clears Windows auto config (native_capture/windows.rs:199,222 broken pipe → Ok(0)). Consumers: every app using `run_with_stdin`.
- (b) job-queue 0.4.1 publish (handshake payload, activity registry, cross-process FIFO). Consumers: HeardRight (handshake), ScrapeRight (activity list).
- (c) Engine host composition (consensus, not a new crate): a documented composition of `EngineSupervisor` + daemon `Service` + `Dispatcher` covering presence/refcount idle exit, restart generation latch, takeover, ordered shutdown, loopback transport option, Swift binding via `ffi`. Consumers: Pulse, CodeRight (adopt), MailRight (replace shell_mode supervision), HeardRight/ScrapeRight (delete remaining glue). Acceptance: one example in the job-queue README executed by a test on both hosts.

---

### M4 Window/CLI protocol

Owners (locked): rightkit-job-queue 0.4.0, rightkit-sidecar 0.3.0.

**1. Requirement.** Window↔engine and CLI↔engine traffic uses the owner wire: `wire::Frame` (Hello/Ready/Request/Event/Response/Bye), `CONTRACT_VERSION` 1, bounded JSON lines or length-prefixed frames with deadlines, `Dispatcher` with built-ins ping/doctor/shutdown, `serve`/`run_cli`, `JobCommands` for persisted jobs. Error envelopes are `JobError`/`WireError` codes. No app defines its own framing, envelope or CLI grammar.

**2. Owner surface.**
- sidecar 0.3.0: `FrameError`, `read_json_line*`, `write_json_line*`, `encode_json_line`, `ReadLimits/ReadOptions`, `Incoming/Oversized/peek_id`, `length_prefixed::*`, `wire::{Frame, WireError}`, `deadline::{read_frame_deadline, write_frame_deadline, DeadlineFramed}`, `UNCAPPED`.
- job-queue 0.4.0 features `protocol`+`contract`: `Dispatcher` (new/with_doctor/with_cli_target/with_alias/with_method_meta/with_frame_limit/call/handle_line), `serve`, `serve_report`, `run_cli`, `parse_flags`, `CommandSet`, `JobCommands` + `PayloadMapper`, wire code constants. Not contract: 0.4.1 `event_emitter`, `JobQuery`/`delete_terminal`.

**3. App obligations.** Engines implement `CommandSet` and call `serve`/`run_cli`; windows use `EngineSupervisor::call` (M3) or `EngineClient`; CLIs use `run_cli`; delete app framers, hello parsers, `--json` flag parsers, status-frame readers (GenRight legacy shape is a QA concern only).

**4. Permitted app-side code.** Method tables, request/response payload types, `PayloadMapper` impls, CLI target names.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1; contract conformance test per engine (`hello` carries `contract_version` 1, oversized frame → `TooLarge`, bad JSON → resync); Q2 names the deleted protocol files.

**7. Open owner work.** (a) README staleness (job-queue "published 0.2.7", sidecar "crates.io has 0.1.0", stale staging headings). (b) Typed dispatch from a method enum and typed events with replay-by-seq (consensus item 1, as job-queue extension, published only with HeardRight and Pulse as deletion consumers). (c) `JOB_METHODS` full list documented (12 names) with per-method CLI grammar decision.

---

### M5 Other child processes

Owner (locked): rightkit-child-process 0.5.2.

**1. Requirement.** Any process an app starts that is not the supervised engine (helpers, converters, ffmpeg, installers, updaters, shells) is spawned through `OwnedCommand` with tree ownership, bounded capture, identity-checked termination and no-window on Windows; adopted foreign processes go through `AdoptedIdentity`. `std::process::Command::new` appears in no app crate (clippy `disallowed-methods`).

**2. Owner surface.** As M3 item 2 plus: `run`, `run_with_stdin`, `RunOptions`, `OutputCapture`, `read_bounded`, `find_processes`, `sweep_matching`, `process_table`, `process_usage`, `tree_usage`, `observe_idle`, `open_url`, `launch_app`, `reveal::*`, `login_shell`, `PowerAssertion`, `with_windows_spawn_lock`, `terminate_all_owned`.

**3. App obligations.** Replace raw spawns (ViewRight lib.rs:133,152,165 named) with `OwnedCommand`; use `reveal::PathScope` for open/reveal; `open_url` for browser hand-off; call `terminate_all_owned` on quit; delete app kill-tree, process-table and reveal helpers.

**4. Permitted app-side code.** Argument construction, env allowlists, stdout parsers for the specific tool, timeouts.

**5. Exception rule.** §2; "the tool needs a console window" is a `windows_visible()` call, not an exception.

**6. Qualification evidence.** Q1; clippy disallowed-methods lint enabled in each app with zero findings; Windows suites executed on the Dell.

**7. Open owner work.** 0.5.3 (M3 7a). Document `kill_on_parent_exit` on Windows (no-op) in the row so apps stop writing parent-death watchdogs.

---

### M6 Data location

Owner (locked): rightkit-daemon 0.3.0 (Suite, AppRoot), composing rightkit-fs `app_dirs` (M8).

**1. Requirement.** Every app resolves its data, config, cache and shared-suite roots through `Suite::resolve` / `AppRoot` (`<Orthic Labs>/<App>`, shared under `Orthic Labs/shared`), creates roots private, migrates legacy folders with `AppRoot::migrate_from`, and never reads HOME/XDG/env selectors itself. Release builds ignore env overrides.

**2. Owner surface (0.3.0).** `Suite{resolve, per_user_default, resolve_ignoring_env_overrides, env_derived, at, per_run_temp, root, service_dir, private_service_subdir, data_dir, app_root}`, `SuiteError`, `refuse_build_tool_real_data`, `valid_service_name`, `ENV_SUITE_ROOT`, `SUITE_DIR_NAME`, `SHARED_DIR_NAME`; `AppRoot{new, name, root, path, data_dir, lock_writer, migrate_from}`, `MigrationOutcome`, `AppRootError`, `valid_app_name`, `MIGRATION_MARKER`. Not contract: 0.3.1 `migrate_from_with`/`MigrateOptions`/`MigrationReport` (held until empty-directory and optional-link semantics are fixed).
- rightkit-fs 0.4.4 `app_dirs`: `AppDirs{resolve, resolve_with_options, for_test, injected, migrate_dir, create_all}`, `AppId`, `Options`, `DEFAULT_ORG`.

**3. App obligations.** One `Suite` built at startup and passed to hosts and clients; `AppRoot::data_dir` for every app-owned folder; `migrate_from` for the legacy location once, marker respected; delete `dirs`/`directories` usage, hand-rolled migration (Pulse `migrated_app_root()` with zero callers deleted outright), env-based root overrides outside test mode (M21).

**4. Permitted app-side code.** App name constant, list of relative sub-paths, legacy source path for migration.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1; migration test on both hosts with files, symlinks and an empty directory; Q2 grep proves no `dirs::` call outside owner crates.

**7. Open owner work.** (a) daemon 0.3.1 held: fix dangling link for empty directories, Windows sockets/junction targets, then publish; consumers HeardRight and ScrapeRight (data-folder move completion). (b) rightkit-models `suite::paths` computes `default_suite_root()` via `dirs` in release builds on `Suite::resolve` error (paths.rs ~175-206) and `app_data_base()` calls `dirs` directly: models must delegate to the daemon owner and fail closed; consumers HeardRight, ScrapeRight, CutRight.

---

### M7 Local services

Owner (locked): rightkit-daemon 0.3.0.

**1. Requirement.** Any in-machine service an app hosts or calls (engine service, helper, control service, Pulse/MailRight agent surface transport) uses the owner: `service.json` discovery with rank and takeover, current-user-only Unix socket/named pipe, per-app allowlist via `MethodTable`, framed request/response/event, chunking opt-in, private per-run child with stdin secret, supervisor (`supervise`, `supervise_adopted`), stop tokens. Remote device access uses the `remote` feature (TLS daemon host, enrolment, pairing, signed requests, event cursors). No app opens a TCP listener, invents a discovery file or writes its own peer check.

**2. Owner surface (0.3.0).** Root items in surface-m3-m7 §rightkit-daemon (Client/ClientConfig/ClientPool, HostExpectation, host/Hosted/HostOptions/ServiceConfig/ServiceEvent, Service builder, ServiceError + codes, discovery, records, Suite, AppRoot, PrivateSecret/PrivateChild, peer, transport, FailLimiter); modules chunk, contract (Caller, MethodPolicy, MethodTable, Handler), mode, stop, ticker, supervisor, link/relay/remote (features). Not contract: 0.3.1 items.

**3. App obligations.** Host with `Service::builder().methods(table).handler(h).local(suite, cfg, opts)`; clients `connect_expecting` whenever a secret crosses; release builds `ignore_env_overrides(true)`; `MethodTable` is the only allowlist; handlers honour `handoff_requested`; delete app discovery files, socket/pipe code, token schemes, HTTP loopback servers, peer checks, supervisor loops (MailRight shell_mode keeps `supervise_adopted`, deletes the rest).

**4. Permitted app-side code.** Method ids and policies, handler bodies, service name and rank, role names.

**5. Exception rule.** §2. Pulse and MailRight "host an agent surface" (M22) on this transport; their approval policies are data (M22 row 7).

**6. Qualification evidence.** Q1 with `local`, `link`, `remote`, `testing`, `test-mode` sets; peer-check negative tests (other-user peer rejected) on both hosts; runtime budget measured (currently not); `ERROR_RATE_LIMITED` exported before lock of the next minor.

**7. Open owner work.** (a) 0.3.1 publish (M6 7a). (b) Approval workflow mechanics (consensus): pairing keys, PIN/seal, approval signatures, audit-before-success as daemon extensions with the action catalogue supplied by the app; consumers MailRight and Pulse delete their approval services. (c) Event cursor over HTTP (#306) decided before any app builds one. (d) README runtime-budget measurement.

---

### M8 Safe file writes

Owner (locked): rightkit-fs 0.4.4.

**1. Requirement.** Every file an app persists is written through the owner: atomic publish (`atomic_write*`, `AtomicWriter`, `publish_file*`), owner-only privacy, durable sync policy, state directories (`StateDir`), cross-process locks (`FileLock`), confined opens (`open_confined`, `Roots`), name sanitising, stale-temp sweep, directory publish, trash, verified copies and digests. No app calls `std::fs::write`/`rename` for state, no app implements a lock file, temp-and-rename, or path-containment check.

**2. Owner surface (0.4.4).** Always: `contain`, `names`, `plain`, `zip`. Feature `fs`: `atomic` (atomic_write, atomic_write_private, atomic_write_with(_options), atomic_write_reporting, AtomicWriter, AtomicOptions/AtomicWriteOptions, ReplaceMode, Privacy, RetryPolicy, publish_file(_with), publish_file_first_free, WriteReport, SyncLevel/SyncPolicy, durable_sync, sync_dir, create_dir_all_durable, sweep_stale_temps(_report), TEMP_MARKER), `append`, `state::StateDir`, `private`, `lease::Lease`, `migrate::migrate_dir`, `publish_dir::publish_dir`, `replaced`, `handle`, `file_key`, `clone`, `swap`, `netfs`, `space`, `tree::copy_tree`, `trash`, `error::FsError`, `verified`, `digest`. Feature `lock`: `FileLock`, `LockOptions`, `LockKind`, `with_lock`, `probe_owner`. Feature `json`: `LockOwner`, `identity::IdentityStore`. Feature `app-dirs`: `AppDirs` (M6). Not contract: 0.4.5 `*_with_access_denied_retry`, `publish_dir_with`/`DirPublishPolicy`, `AccessDeniedRetry`; features `testing`, `test-seams`, `test-mode` (M21).

**3. App obligations.** Name features explicitly; create the data root before the first owner-only write (or pass `Privacy::OwnerOnlyUnixInheritWindows`); one `LockKind` per lock file; `read_replaced` for files another process replaces on Windows; `Lease::renew` every ttl/3; delete app atomic-write helpers, lock files, temp sweepers, containment checks, trash code, hashing helpers.

**4. Permitted app-side code.** Paths relative to the app root, serialisation of app types, lock labels.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1 with every feature set; Windows write-through and owner-only DACL executed on the Dell; FAT/exFAT exclusive publish on a real volume (README:60 gap); Q2 grep proves no `std::fs::rename`/`write` on state paths in the app.

**7. Open owner work.**
- (a) 0.4.5 publish after `access_denied_retry_tests`, `migrate_dir`, `publish_dir`, trash tests run on both hosts; the lease working-tree fix (publish_guard via hard_link, lease.rs) ships complete-before-visible with no placeholder fallback (rename_exclusive's O_EXCL placeholder at atomic.rs:429-433 is not acceptable for the guard). Consumers: all apps on 0.4.4.
- (b) Transactional multi-file publish (consensus R2): journal, rollback, restart recovery over `publish_dir` and `AtomicWriter`; rightkit-db and rightkit-daemon compose database and root cutover on it; no atomic-visibility claim without a pointer switch or cooperating readers. Consumers: ViewRight document saves, MailRight store cutover, HeardRight model swaps.
- (c) rightkit-app-settings deletes its own `atomic_write`/`atomic_write_with` re-exports (src/atomic.rs) in its next minor.
- (d) README version line (says 0.3.3/0.4.0).

---

### M9 Database

Owner (locked): rightkit-db 0.5.0.

**1. Requirement.** Every SQLite/SQLCipher file an app opens goes through `DbOptions` (pragmas, key ordering, WAL with network fallback, read pool, checkpoint job, integrity checks, online backup/restore, versioned migrations, quarantine/recovery policy, rekey). Apps own schemas and queries only. No app calls `rusqlite::Connection::open` (ViewRight logs.rs:33 named), sets pragmas, implements backup, migration bookkeeping, or an in-memory fallback.

**2. Owner surface (0.5.0).** `DbOptions` and every `open_*` listed in surface-m8-m11 §rightkit-db; `write_txn`, `write_txn_unchecked`, `checkpoint_truncate/restart/if_wal_larger_than`, `optimize_*`, `ReadPool`, `online_backup_to`, `restore_from`, `replace_database(_locked)`, `StoreLock`, `migrate`/`Migration`/`MigrateOptions`/`VersionStorage`/`rollback_to`, `RecoveryPolicy` (with `allow_in_memory_fallback`), `quarantine_database*`, `rekey`, `restore_database(_locked)`, `recover_interrupted_restore`, `UnreadablePolicy`, `WalCheckpointJob`, `JOURNAL_ENV`, `per_host_path`. Features: `bundled`, `statement-cache`, `sqlcipher` xor `sqlcipher-system`, `hooks`.

**3. App obligations.** One `DbOptions` per store; `write_txn()` for every writing transaction; `recover_interrupted_restore` before open at startup; `WalCheckpointJob::tick` from the app loop; `StoreLock::shared` while connections are open; exactly one SQLCipher provider feature; delete app pragma setup, migration tables, backup copies, quarantine logic, raw `rusqlite` opens.

**4. Permitted app-side code.** Schema SQL and `Migration` lists, queries, `ConnInit` hooks for app functions, the DB path under the app root.

**5. Exception rule.** §2. FTS5/sqlite-vec extension loading (MailRight) is a `ConnInit` concern; whether the extension itself is app-only is an open decision for Adrian, not a row exception.

**6. Qualification evidence.** Q1 with `bundled`, `sqlcipher`, `sqlcipher-system`, `hooks`; the 0.4.3/0.5.0 "not compiled when written" items executed on both hosts; Windows replace-with-closed-connections test; Q2 negative test: corrupt file with `UnreadablePolicy::Fail` is never moved.

**7. Open owner work.** (a) CHANGELOG/README status reconciled (0.5.0 labelled staging while published). (b) Encrypted DB lifecycle orchestration over existing APIs (rk-compliance-v2 item 10): key rotation using `rekey` + credentials `get_or_create_key`, lost-key quarantine path, compaction (VACUUM INTO copy, verify, swap via M8 publish), legacy JSON import helper, size report. Consumers: MailRight, CodeRight, HeardRight delete their lifecycle code. (c) Runtime WAL refusal leaving an empty main file (CHANGELOG 0.5.0) fixed.

---

### M10 Settings

Owners (locked): rightkit-app-settings 0.2.4, @rightkit/settings-ui 0.6.1.

**1. Requirement.** App settings are one versioned JSON document per app (or `SqlSettings` table) owned by `Store<T>`/`Document`: typed defaults, migrations, atomic publish, corruption quarantine, revision compare-and-swap, external change polling, change events; secrets are referenced by `SecretRef` only. The settings UI is `SettingsPanel` with the standard sections (appearance, updates, licence, legal, diagnostics, models, API keys, about) over a `SettingsBackend` whose `patch` enforces `revision_conflict`. Secret values never pass through the webview: the API-key section's `set` must reach native code that writes to M11 without the value being exposed to any content frame, and the current `SecretsBackend.set(ref, value)` plaintext path is replaced by a native prompt or native-origin capture (row 7).

**2. Owner surface.**
- app-settings 0.2.4: `Store<T>` (open/open_path/open_read_only/with_backend/get/revision/poll_external/update/update_at/patch/patch_dry_run/remove_keys*/subscribe/refusal/writable/path), `read_with_defaults/options`, `Schema`, `Migration`, `Change`, `Refusal`, `Error`, `StoreOptions` builder, `ReadOptions`, enums (FileFormat, NullMeans, KeyOrder, OnRefused, OnCorrupt, ReplaceMode, QuarantineName, SharingRetry), `Document`/`DocumentState`, `SettingsBackend` trait + `FileBackend`, `SecretRef`/`Presence`/`SecretProbe`, feature `sql` `SqlSettings`.
- settings-ui 0.6.1: `/core` (SettingsPanel, SectionFrame, mergeSections, searchSections, revealSetting, SettingsGroup/SettingRow, useSettings, createMemorySettings/Secrets, RevisionConflict, search API, types), `/standard` (standardSections, section factories, useUpdates, useLicenseSection, useDoctor), `/headless`, `/shell-adapter` (createShellBackends, METHODS, EVENTS), `/appearance`, `settings.css`. Not contract: 0.7.0 `LicenseView` migration and Verify purchase.

**3. App obligations.** One `Store<T>` with the app's serde struct and `Schema` migrations; renderer gets `Document` get/put/patch through the owner's host commands (M1 7c) not app commands; `SettingsPanel` with `standardSections`; implement `SecretProbe` over M11; delete app settings files/formats, JSON read-modify-write code, settings UIs, revision handling, change broadcast.

**4. Permitted app-side code.** The settings struct and defaults, migration closures, app-specific sections (data only: labels, rows bound to keys), `ApiKeySlot` lists.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1 (both hosts; Android compile); Windows power-loss `MoveWriteThrough` evidence or the gap stays named; Q2 negative test: stale revision patch is refused by the host, not the renderer; negative test that no settings host command returns a secret value.

**7. Open owner work.**
- (a) Host commands: `rightkit.settings.get/patch`, `rightkit.settings.changed`, `rightkit.models.*`, `rightkit.secrets.presence/clear` registered by tauri-shell (M1 7c) over `Document` and M11 `Store::exists/delete`. `rightkit.secrets.set` is removed from the webview contract; replacement is native capture (the shell opens a native prompt or the host reads a one-shot paste) so the plaintext never originates in a content frame. Consumers: all six apps delete their settings and secret commands; settings-ui `/shell-adapter` targets the registered names.
- (b) Schema-driven codegen (Rust/TS/Swift/Kotlin) only for bindings demonstrated duplicated across at least two apps (consensus R3). Consumers named at that point.
- (c) 0.7.0 publish after @rightkit/license 0.3.0 (M12) is published.
- (d) `atomic_write` re-export removal (M8 7c); staging label reconciliation.

---

### M11 Secrets

Owner (locked): rightkit-credentials 0.3.0.

**1. Requirement.** Every secret an app holds (licence record, API keys, DB keys, OAuth tokens, device link keys) lives in the per-app OS store through `Store`: Keychain / Credential Manager / Secret Service / Android Keystore, passive `exists`, non-interactive default, `WriteIntent`, `add_if_absent`, DB-key provisioning, rotation with lock dir, plaintext-file migration (copy only), guard variables resolving to Deny in release. Secret bytes never enter a webview, a log, a settings file or an error. No app has a file, env or "plain" fallback (CodeRight token_store.rs:76,96 named).

**2. Owner surface (0.3.0).** `Store` (open/namespace/pinned/guard_active/diagnostics/with_rotation_lock_dir/with_namespace/interactive/get/exists/put/set/set_str/delete/delete_if_equals/add_if_absent/is_durable/generate_key/get_or_create_key/begin_rotation/pending_rotation/commit_rotation(_of)/abort_rotation(_of)/migrate_legacy_namespaces/resolve/status_source/import_env), `Config`, `GuardMode`, `WriteIntent`, `Diagnostics`, `UNIVERSAL_GUARD_ENV`, `Namespace`, `WindowsTarget`, `Secret`, `Backend`/`Interaction`, `system_backend`, `Error`, `migrate_between`, `migrate_plaintext_file`, per-OS backends, `apple_helper::AppleHelper` (new/timeout/args/status/read/exists/write/delete_everywhere/copy_synced_to_device_only, `resolve_bundled`), CLI. Feature `settings-probe`. Not contract: 0.3.1 `rotation_status`, `new_trusted`, `resolve_bundled_login_item`; `isolated-test-keychain` (CI only).

**3. App obligations.** Frozen `Namespace` per app; `Config::rotation_lock_dir` set; `get_or_create_key` for DB keys; `migrate_plaintext_file` then delete the legacy file as a separate verified step; `Interaction::Interactive` only on an explicit user action; delete app keyring wrappers, `keyring` crate usage, plaintext token files, env fallbacks, in-webview secret handling.

**4. Permitted app-side code.** Namespace/account constants, the calls, rotation orchestration order.

**5. Exception rule.** §2 plus Q3: no exception can introduce a non-OS store.

**6. Qualification evidence.** Q1 with `settings-probe` and (CI) `isolated-test-keychain`; Windows tests `windows_lock`/`windows_target`/`windows_keyring_compat` executed on the Dell; Linux by PR CI; Q3 negative tests: tripped guard in a release build denies; `exists` prompts nothing; a malformed stored value fails closed.

**7. Open owner work.** (a) 0.3.1 publish. (b) Typed `Secret<T>` and chunking for values over the Credential Manager limit, one redaction API shared with rightkit-logs `KnownSecrets` (rk-compliance-v2 item 11). Consumers: CodeRight (large OAuth payloads), MailRight. (c) App lock (Windows Hello, Touch ID, Android biometric) as a separate owner slice with HeardRight/MailRight as consumers. (d) rightkit-http `SecretStore` production implementation here (README drift resolved one way).

---

### M12 Licence/device

Owners (locked): rightkit-license 0.2.3, rightkit-device 0.3.1, @rightkit/license 0.2.1. Policy (2026-10-19): tokens never in the webview; the locked 0.2.1 renderer package still reads/writes tokens and is therefore locked only as "the surface being retired"; no app may adopt it anew.

**1. Requirement.** Native code owns licence storage (M11 record), verification (Ed25519 tokens, device binding, anti-rollback clock, revocation), lifecycle evaluation, refresh/activate/deactivate/trial transport, lease envelopes, device identity (M1 `DeviceService`). The renderer receives only `LicenseView` display projections and sends only transient key/email inputs through owner commands. Paid commands are guarded at the Rust boundary (`guard_paid_command` / `EngineEntitlements::require`).

**2. Owner surface.**
- rightkit-license 0.2.3: `Entitlement`, `LicenseRequirement`, `LicenseKind`, `EntitlementStage`, `VerifyError`, `verify_token*`, `authenticate_token*`, `verify_token_not_revoked`, `peek_unverified_claims`, `stored_signed_token`, `RevokedIds`, clock (`AntiRollbackClock`, `ClockStore`, `FileClockStore`, `ClosureClockStore`, `CrossProcessLock`, `reconcile_clock`, `plan_reanchor`), lifecycle (`Lifecycle`, `evaluate_lifecycle`, `LifecycleContext/Policy/Report/Messages`), refresh (`refresh_and_reanchor`, `judge_refresh_response`, `RefreshTransport`, outcomes), lease envelope fns, `EngineEntitlements` + config + token sources; features `subject-entitlement`, `engine-gate`. Not contract: 0.2.4 `client` (`LicenseClient`, `LicenseClientConfig`, `LicenseView`, `LicenseError`…), `update-auth`, `client-testing`.
- rightkit-device 0.3.1: as M1.
- @rightkit/license 0.2.1: retiring; no new adoption. 0.3.0 (`createLicenseClient({invoke, listen})`, `LicenseView`, `LICENSE_MESSAGES`, display helpers, `./testing` dev-only mock) is the target surface, not contract until published.

**3. App obligations.** Compile the public key; one `DeviceService`; store the record via M11; `authorize_with_clock` with the anti-rollback clock; every paid command through `guard_paid_command`; renderer uses `@rightkit/license` only; delete app token readers in TS (ViewRight, CodeRight named), app lifecycle classifiers, local grace logic, app licence commands once the owner registers them (M1 7c), legacy v1 verification once migrated.

**4. Permitted app-side code.** Public key constant, app id, licence requirement constants per command, `LegacySource` for one-time import.

**5. Exception rule.** §2 plus Q3.

**6. Qualification evidence.** Q1; test vectors (license-v2, lease-envelope-v1, dalek-parity-v1) pass on both hosts; Q3 negative tests: renderer-supplied device id refused; token > 16 KiB refused unread; rolled-back clock denies; `peek_unverified_claims` never grants; release bundle containing `@rightkit/license/testing` sentinel refused by right-release.

**7. Open owner work.**
- (a) Publish rightkit-license 0.2.4 (`client`, `update-auth`) and @rightkit/license 0.3.0 together with the tauri-shell licence command registration (M1 7c: `rightkit_license_status/activate/refresh/deactivate/request_trial`, event `rightkit://license-changed`), then @rightkit/tauri 0.3.0 and settings-ui 0.7.0. Consumers: all six apps delete their licence commands and renderer token code. Acceptance: no Rust crate outside rightkit-tauri-shell registers those names; repo grep in each app finds no stored-token type in TS.
- (b) Client extensions (consensus): Pro bridge client, lease transport (device lease over `/v1/pro/{app}/devices/lease`), account session, device management, link codes; HeardRight the named consumer now, others on adoption. Acceptance per slice: HeardRight deletes the matching module.
- (c) Signer freshness nonce (README:105 residual risk) tracked as a signer change; not a row blocker.
- (d) Raise mandatory minimums for rightkit-device to 0.3.1 (already published).

---

### M13 Pro entitlement

Owner (locked): rightkit-license 0.2.3 (`EngineEntitlements`, `LicenseRequirement`, `Entitlement::is_pro/tier_at_least/has_feature`, `subject-entitlement`, `engine-gate`).

**1. Requirement.** "Is this user entitled to feature X" is answered only by the owner from a verified token or subject entitlement: in the window through M12 authorization, in the engine through `EngineEntitlements` (token file or credential source, bounded cache, `invalidate` on a content-free hint) or `EngineGate` for subject-based gating. No app flag, env var, settings key or test mode decides entitlement in a release build (HeardRight `is_pro()` defaulting true under `HEARDRIGHT_ENGINE_TEST_MODE` without a debug gate is named as a violation to fix).

**2. Owner surface.** `EngineEntitlements<C>` (new/with_clock/with_wall_clock/invalidate/current/require/require_pro/require_requirement), `EngineEntitlementsConfig`, `EngineEntitlementError`, `TokenSource`, `FileTokenSource`, `CredentialTokenSource`, consts; `SubjectEntitlementSource` and friends; `EngineGate`, `GateOutcome`, `Verdict`, `Enforcement`, exit codes. Not contract: 0.2.4 `CredentialTokenSource::from_record_store`.

**3. App obligations.** Engines construct one `EngineEntitlements` with the app's public key and device source; every Pro method calls `require_requirement` first; the window sends only a content-free "licence changed" hint; delete app entitlement caches, Pro flags, env overrides; QA entitlement fixtures go through M21 test mode with a signed test token, never a flag.

**4. Permitted app-side code.** Requirement constants per method, the hint plumbing.

**5. Exception rule.** §2 plus Q3; "advisory enforcement" is `Enforcement::Advisory` chosen at release time (M23), not app code.

**6. Qualification evidence.** Q1; Q3 negative tests: no token → `NoLicense`; wrong device → denied; env/flag present in release build has no effect; `Enforcement::Enforce` exits 77 on denial.

**7. Open owner work.** Pro bridge client (M12 7b). HeardRight test-mode `is_pro` default fixed in the app on adoption (not owner work; recorded for Q2).

---

### M14 Legal/onboarding

Owners (locked): @rightkit/legal 0.6.1, @rightkit/legal-ui 0.4.1, @rightkit/onboarding 0.6.2.

**1. Requirement.** Legal documents are hash-bound snapshots generated from the RightKit masters by `legal-snapshot` and verified at release with `--final`; the first-run and re-acceptance gate is `LegalGate`; acceptance is a `LegalAcceptanceReceipt` built by the owner and persisted by the app through a `LegalReceiptPort` (native store, M8/M10); onboarding is the owner's `Onboarding` with built-in legal, permissions, models, licence and sign-in steps plus app steps. Apps carry no legal copy, company facts, acceptance logic or step runner of their own.

**2. Owner surface.**
- legal 0.6.1: `.` core (schema 2 types, `assertLegalManifest`, `createAcceptanceReceipt`, `receiptIsWellFormed`, `needsLegalAcceptance`, `materialDigestInput`, Node `materialAcceptanceDigest`), commerce (`buildCheckoutLegalReceipt`, `CHECKOUT_LEGAL_ROLES`), defaults (`RIGHT_SUITE_COMPANY_FACTS`, `SUITE_REFUND_POLICY`, `renderDefaultPolicies`, `defaultPolicyDescriptors`…); `./browser` (core + WebCrypto digests); `./tooling` (`syncLegalSnapshot`, `assertFinalLegalSnapshot`, `generateNpmLicenseAppendix`); bins `legal-snapshot`, `generate-npm-license-appendix`, `rightkit-legal` (`rust-licenses` BLOCKED).
- legal-ui 0.4.1: `LegalGate` (props incl. `layout`), `policyGateDocuments`, `LegalDocumentBody`, types.
- onboarding 0.6.2: `Onboarding` (props incl. `layout`, `legalReceipt`, `requiredDocumentsHash`), `isLegalAccepted`, `onboardingStateKey`, `permissionsStep`, `modelsStep`, `licenseStep`, `signInStep`, backend port types.

**3. App obligations.** `legal-snapshot.config.json` with all six roles (proprietary) and committed outputs; `legal-snapshot --final` in the release checks; `generate-npm-license-appendix` before sync; webviews import `@rightkit/legal/browser`; mount `Onboarding` with `legalMode="gate"`, `requiredVersion`, `requiredDocumentsHash` from the material digest, a `legalReceipt` port backed by native storage; delete app legal pages, acceptance flags, onboarding wizards, permission/model/licence step UIs (HeardRight moves its pages onto the built-in steps).

**4. Permitted app-side code.** `appName`, refund/support overrides where the app truly differs, document URLs, step adapters (`SettingsBackend`, `PermissionsBackend`, `ModelsBackend`, `LicenseBackend`, `SignInBackend`) as thin bindings to M10/M12/models owners, app-specific extra steps as data+render.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1 (npm gate); `--final` passes for each app at release; Q2: receipt persisted natively and re-read on second launch; re-acceptance shown when the material digest changes.

**7. Open owner work.** (a) Receipt built natively: today `Onboarding.tsx:224` builds the record in the renderer with `new Date()`; the owner adds a `LegalReceiptPort.create` path (native timestamp and digest) and marks the renderer-built record as legacy. Consumers: all apps. (b) `LicenseBackend` adapter over `@rightkit/license` 0.3.0 `LicenseView` shipped by onboarding (no app bridge). (c) Native receipt store, completion flag, reopen/reset, legacy receipt carry-forward (rk-compliance-v2 item 9). (d) `ctx.signIn` reachable via props or removed. (e) README peer table, `layout` prop documented; `rust-licenses` unblocked by choosing cargo-license-notices as its source (M19).

---

### M15 Logging

Owners (locked): rightkit-logs 0.3.3, @rightkit/logs 0.1.5.

**1. Requirement.** Apps log through `JsonlStore`/`AsyncWriter` (content-free `EventRecord`), rotate text logs with `RotatingTextWriter`, capture engine stderr with `capture_stderr`, scrub with the owner's redaction (built-in keys cannot be removed), collect diagnostics with `collect_diagnostics`, and upload only through the consented `Outbox`/`Uploader`. The webview uses `createLogsClient` (closed `allowedFields`) and `installWindowCrashCapture` against the three logs commands and the crash command registered by the host owner (M1 7c). No app has its own JSONL writer, rotation, scrubber, consent store or uploader.

**2. Owner surface.**
- rightkit-logs 0.3.3: `JsonlStore`, `EventRecord`, `Level`, `AsyncWriter`, `FailureReporting`, `RotatingTextWriter`, `StderrLine`, `capture_stderr`, scrub fns (`scrub_text`, `scrub_value`, `redact_quoted`, `redact_url_text`, `ScrubOptions`), `collect_diagnostics`, `CollectionPolicy`, `DiagnosticsBundle`, `RedactionPolicy`, `Consent`, `ConsentStore`, `Outbox`, `Uploader`, `UploadConfig`, `Telemetry`, `Transport`, `Scheduler`, `validate_endpoint`, `identity::{random_uuid, install_id}`, `system::*`, `android::*`; features `http` (default), `tracing` (`install_tracing`, `TracingFileConfig`, `TracingGuard`, `stderr_layer`), `log-bridge`, `testing`. Not contract: 0.3.4 `DetachedLog`, public `KnownSecrets`.
- @rightkit/logs 0.1.5: command constants, `createLogsClient`, `scrubLogText`, `installWindowCrashCapture`, `scrubWindowCrashText`. Not contract: 0.1.6 `createDiagnosticsRecorder`.

**3. App obligations.** One `JsonlStore` at `<app root>/logs/events.jsonl`; `install_tracing` with the owner config where tracing is used; engine stderr through `capture_stderr`; `KnownSecrets`/`redact_values` fed from M11 accounts (values never logged); consent UI calls `ConsentStore::grant/revoke`; delete app log writers, rotation, scrub regexes, consent files, upload code, diagnostics bundlers, renderer log shims.

**4. Permitted app-side code.** Event names and allowed-field lists, log paths under the app root, consent UI, support action that serialises the bundle.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1; runtime budget measured (currently not); parity vectors for `scrubLogText` vs `scrub_text` run in both suites; Q2 negative test: an event with a `password` field is dropped before the IPC call.

**7. Open owner work.** (a) 0.3.4 / 0.1.6 publish (DetachedLog, KnownSecrets, recorder). (b) Logs commands registered by tauri-shell (M1 7c) instead of per app; `logs_commands` feature then joins `handler!`. (c) Logging config shape (stems, retention incl. count-based, bundle sources, consent cache) and crash hook composition (rk-compliance-v2 item 14); consumers all six apps. (d) Feature table lists `log-bridge`; README dependency sentence corrected.

---

### M16 Crash reporting

Owner (locked): rightkit-crash 0.3.3.

**1. Requirement.** Every process (window, engine, helper) installs the owner panic hook with a `CrashConfig` naming app, version, crash dir and redaction values; native capture (macOS sample, Windows minidump via the shipped dump helper) is opt-in `Auto`; helper exits are recorded supervisor-side; pending reports enter the consented outbox (M15) at launch; support bundles are built by `build_support_bundle`; renderer errors arrive only through `record_window_error` behind the owner's gate. No app writes its own panic handler, crash file format, minidump invocation or support zip.

**2. Owner surface (0.3.3).** `CrashConfig` (new, redact_values, chain_default_hook, process_kind, with_sink, write_report_file, capture_backtrace, native_capture), `ProcessKind`, `install_panic_hook`, `install_process_hook`, `record_report`, `record_helper_exit`, `set_recording_enabled`, `fallback_crash_dir`, `capture_native(_for_pid)`, `NativeCapture`, `set_dump_helper`, `dump_helper::*`, `list_pending`, `enqueue_pending`, `Retention`, `prune`, `record_android_exits`, `CrashReport`, `CRASH_SCHEMA_VERSION`; feature `support-bundle` (default): `build_support_bundle`, `SupportBundleOptions`, `BundleManifest`, `create_zip`; feature `window-errors`: `record_window_error`, `WindowErrorGate`, `WindowErrorReport`. Binary `rightkit-dump-helper`. Not contract: 0.3.4 `KnownSecrets`-backed redaction.

**3. App obligations.** `install_panic_hook` first thing in `main` of every process; ship `rightkit-dump-helper` beside the executable and call `set_dump_helper`; `enqueue_pending` at launch; `Retention::prune` on a schedule; register `rightkit_crash_window_error` (via M1 7c) calling `record_window_error`; include native dumps in a bundle only on explicit user opt-in; delete app panic hooks, crash writers, zip builders.

**4. Permitted app-side code.** Crash dir under the app root, consent UI, bundle sources list.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1; Windows 11 minidump run recorded (README:46 gap); Q2 negative test: scrub failure withholds the sample; a renderer error triggers no native capture.

**7. Open owner work.** 0.3.4 publish with the rightkit-logs floor stated correctly (CHANGELOG says 0.3.3, manifest 0.3.4); README lists `window-errors` and the process-kind surface; Android exit-history path run on a device before Android adoption.

---

### M17 HTTP

Owner (locked): rightkit-http 0.3.6.

**1. Requirement.** Every outbound HTTP request from app code (licence, updates manifest, telemetry, model download, provider APIs, OAuth) goes through `Client`/`AsyncClient` with the owner's address policy (SSRF denial that no hook can relax for metadata/link-local), timeouts and deadlines, retry budgets with Retry-After, redirect policy with credential stripping, secret redaction, SSE, multipart, platform trust roots, proxy modes, quota schedule, untrusted web-text wrapping. No app depends on reqwest/ureq/hyper directly and no app re-implements retries, redirects or URL validation.

**2. Owner surface (0.3.6).** Items in surface-m17-m23 §rightkit-http: `Client`, `ClientConfig`, `Request`, `Response`, `StreamResponse`, `HttpError`/`ErrorClass`/`Phase`, `netpolicy` (`AddressPolicy`, `CredentialScope`, `validate_public_https_url`, `resolve_vetted`), `RetryPolicy`, `parse_retry_after`, `SseDecoder/SseReader`, `Multipart`, `CancelToken`, `QuotaSchedule`, `Secret`/`SecretStore`, `ProxyMode`, `TrustRoots`; features `gzip`, `untrusted`, `async`, `testing`. Not contract: 0.3.7 fractional reset precision.

**3. App obligations.** One `Client` per target host with `first_party` where applicable; `Request::secret_header`/`bearer` for credentials; `credential_scope` set; `QuotaSchedule` for provider quotas; `resolve_vetted` for WebSocket/raw TCP; delete app HTTP wrappers, retry loops, redirect handling, URL guards, SSE parsers, multipart builders.

**4. Permitted app-side code.** Endpoint URLs, request/response types, per-provider header names, quota keys.

**5. Exception rule.** §2; a provider needing HTTP/2 or WebSocket is row 7 work, not an exception.

**6. Qualification evidence.** Q1 with every feature set; WinHTTP PAC path tested against a real proxy once (CHANGELOG 0.3.2 gap); Q2 negative tests: loopback denied under default policy; HTTPS→HTTP redirect refused; cross-origin redirect drops Authorization.

**7. Open owner work.** (a) Redirect URL-shape check, typed redirect error, string-level URL guard (rk-compliance-v2 item 15); consumers CodeRight, MailRight. (b) `download_to_path` with verified digest composed over M8 `copy_verified` (named absent in README:23); consumers HeardRight/ScrapeRight model downloads via rightkit-models. (c) README drift (0.2.5/0.3.0 text, reqwest version, socks5) and the `SecretStore` ownership statement resolved with M11 7d.

---

### M18 Updates

Owner (locked): @rightkit/app-update 0.4.0 (renderer policy); native half pending (row 7).

**1. Requirement.** Update checks, patch/update classification, eligibility (patch-only by default; Pro-gated updates via M12 `update-auth`), download/install consent (separate approvals), boot probation with reinstall guidance, persisted check cache, single-instance package check, and the overlay/consent UI come from the owner. Signature verification is the Tauri updater's against the suite key 29395F9FF466261D. No app writes its own check loop, version compare, consent state, probation record or update UI, and no URL, header or token crosses IPC once the native bridge lands.

**2. Owner surface (0.4.0).** `createAutoUpdateClient` + config hooks, `fetchReleaseStatus`, `updateTierFromManifest`, `compareVersions`/`isNewerVersion`/`parseVersion`, channels (`validateChannels`, `resolveChannel`), `createCheckGate`, `createBootProbation`/`previousInstallPath`, `registerUpdatesInstance`/`assertSingleUpdatesVersion`, overlay/consent (`deriveUpdateOverlay`, `createUpdateConsent`, `createUpdateConsentRunner`), `./react` (`UpdateOverlay`, `UpdateConsentCard`), `./contract-vectors`. Not contract: 0.4.1 `createPersistedCheckCache`, `./native` (`createNativeUpdateClient`, commands `rightkit_update_check/status/download/install/cancel`, event `rightkit://update-progress`).

**3. App obligations.** Inject `check` (Tauri updater), explicit `getInstallHeaders`, `canInstall`, `isEligible`, `onStatus`; `createUpdateConsentRunner` as the once-allow runner; `tauri.conf.json` updater pubkey = suite key; `distribution.updaterKeyId` in release config; host storage for probation and check cache; delete app updaters, version comparers, consent flags, overlay UIs.

**4. Permitted app-side code.** Endpoint/channel config, storage adapters, where the overlay mounts.

**5. Exception rule.** §2.

**6. Qualification evidence.** Q1 (contract vectors); Q2: `assertSingleUpdatesVersion` passes; probation record produced on a simulated failed boot; negative test: `getInstallHeaders` absent yields no headers on a private artifact.

**7. Open owner work.** (a) Native update bridge: tauri-shell `update` feature registering the five commands over rightkit-license `update-auth` + the Tauri updater, @rightkit/app-update 0.4.1 `./native` and persisted cache published together (M1 7c). Consumers: five apps delete native update code (rk-compliance-v2 item 7). Acceptance: no URL/header/token in IPC payloads (closed DTOs), `UPDATE_MESSAGES` de-licence-flavoured. (b) README drift: `.previous` restore text replaced by reinstall_required; `getInstallHeaders` strict default; `./native` listed; `dist/` rebuilt before publish.

---

### M19 Release tooling

Owner (locked): @rightkit/release 0.4.0 (dev dependency allowed).

**1. Requirement.** Every app build, sign, package, publish, install-for-dev, dependency/ownership check, hardening scan, QA receipt and release-switch scan runs through `right-release`. Public RightKit releases build on CI; private app builds run on the native host. The tool refuses: test/debug surfaces in release builds (feature closure, payload markers, MSIX scan, sealed-file verify), DevTools outside ViewRight content-only, upload off the primary checkout or with a dirty tree, apps without a QA lane, app copies of RightKit-owned capabilities (`ownership`), declarations below the mandatory minimums (`ownership-mandatory --enforce`).

**2. Owner surface (0.4.0, CLI).** Commands listed in surface-m17-m23 §@rightkit/release: build/package family, `dev`, signing family, publish family (`publish cargo|npm|swift`, `github`, `publish-update`), gates (`doctor`, `deps`, `hardening`, `bundle-scan`, `dist-scan`, `identity-scan`, `check-release-dependencies`, `qa`, `inventory check`, `ownership`, `ownership-mandatory`, `license-notices`, `model promote`, `addon`), `cache`, `disk`, `engine-locate`. Internal modules (release-switch-scan, qa-build-features, qa-control-scan, devtools-policy, publication-gate, runtime-budget) are contract through their CLI behaviour, not as imports. Not contract: 0.4.1 staging (production-mode frontend builds, `development` resolve-condition refusal, licence-testing bundle markers, Windows `publish cargo --local`, iOS recipe, #778 updater signing).

**3. App obligations.** Release config with `distribution.provider/repository`, `updaterKeyId`, `legal.npmNotices`, `releaseSwitches` inventory (`switches.json`) and manifest, QA lane, runtime-budget declarations in each crate; `rightkit-mandatory.json` when a row is not applicable; pin `@rightkit/release` at the locked version; delete app build scripts, signing scripts, notarize steps, installer recipes, dependency audits that duplicate the CLI.

**4. Permitted app-side code.** Config files, app-specific `checks` entries that call the CLI with app inputs, icons/assets.

**5. Exception rule.** §2; a packaging need not covered is a recipe request (row 7), never a parallel script.

**6. Qualification evidence.** `pnpm test` incl. registry parity and signer tests on both hosts; Q2: `right-release release` for the app runs the switch scan with a manifest and inventory present (no `release-switch-manifest-missing`), `ownership --audit-json` clean, `ownership-mandatory --enforce` passes.

**7. Open owner work.** (a) 0.4.1 publish. (b) Recipes (rk-compliance-v2 item 8): nested helper signing + provisioning, raw EXE Authenticode before NSIS, multi-binary payload, runtime/sidecar staging declaration, artifact verification, dev launcher with sidecar staging, pinned-digest asset fetch, cargo licence notices CLI route. Consumers: six apps delete the named scripts. (c) README documents `ownership-mandatory`, `ownership` subcommands, D4 wording aligned with the mandatory set (M21/M23). (d) smol-toml pin text.

---

### M20 QA

Owners (locked): rightkit-qa 0.7.1, @rightkit/qa 0.5.1, @rightkit/qa-chrome 0.1.8 (dev dependencies allowed).

**1. Requirement.** Every app has a QA lane on the owners: engine black-box scenarios (`.scenario.toml`, `EngineTarget`, fake CLI, mocks, media validators), hidden native UI driving through the dev-only `control` feature (embedded Tauri control plugin, never in a release build, marker-scanned by M19), managed run storage with private bounded homes and the `.rightkit-app.json` binding (M21 input), user-data guard with no opt-out, evidence bundles with hashed artifacts, browser QA through `@rightkit/qa` + Chrome for Testing. No app has its own test harness, process cleanup, evidence format, mock server, or loopback control server.

**2. Owner surface.**
- rightkit-qa 0.7.1: modules scenario, engine, workspace, storage, userdata, guard, harness, suite, control (client), embedded (`control` feature, debug only), in_app, binary, installed, config/settings, check/evidence, lock, process, retention, inplace, mock, http, page/a11y/dialog, audio, media, loudness, frametrace, tart, fake_cli, interactive, android (feature); CLI `rightkit-qa` (doctor, list, run, guard, sweep, prune-runs, verify), `rightkit-qa-interactive`, `rightkit-fake-cli`. Features `browser`, `control`, `android`, `wv2-0_38|0_39`.
- @rightkit/qa 0.5.1: a11y, evidence, devserver, frames, guard, installed, loopback, ownership, paint, panes, process, workspace, storage, userdata, page (`QaBrowserSession`, `QaPage`, CDP), `./chrome`, `./ts-register`.
- qa-chrome 0.1.8: resolve/launch/close Chrome for Testing, managed install with pinned digest, Windows sandbox grant, macOS code-sign clone helpers; CLI `rightkit-qa-chrome install|path|list`. Not contract: 0.1.9 close budget.

**3. App obligations.** `qa-native = ["dep:rightkit-qa", "rightkit-qa/control", <wv2>]` off by default and unreachable from `default`; plugin registered only under that feature; hidden launches never take focus; isolated data dir for engines; settings file over env under `rightkit cargo`; QA scripts via `@rightkit/qa` functions; git-ignore the QA pointer/stash files; delete app harnesses, wdio shims, process killers, evidence writers, mock servers, Tauri invoke mocks (until 7a lands, the mock is the one tolerated TEMPORARY exception with 7a as sunset).

**4. Permitted app-side code.** Scenario files, fixtures, per-command QA step handlers, app-registered Control commands (data), tier lists.

**5. Exception rule.** §2 with the TEMPORARY entry above.

**6. Qualification evidence.** Q1 with `browser`, `control`+`wv2-*`, `android`; Windows runtime budget measured for rightkit-qa (README:293, #808) before the next publish; Q2 negative test: release build with the control marker refused by right-release; UI launch outside a QA home refused.

**7. Open owner work.** (a) QA harness glue (rk-compliance-v2 item 6): Tauri invoke/listen mock with per-command fixtures, browser IPC seam, legal-gate helper, journey runner, capture and visual diff, app-registered Control endpoint registry, process-tree stop probe; consumers all six apps. (b) Windows installed-package lane (gap row 56). (c) Permission-dialog detector implemented or removed from the API. (d) DOC DRIFT: README/CONTROL-SERVER references to `rightkit-qa-driver`; @rightkit/qa README pin.

---

### M21 Test mode (owner assigned by this document, L5)

Owners (locked at publish of the named versions): rightkit-fs 0.4.4 `test_mode` + `app_dirs` (feature `test-mode`), rightkit-daemon 0.3.0 `mode` (feature `test-mode`), rightkit-qa 0.7.1 (binding writer). Mandatory-set entry changes from `owners: []` to these three; `ownerPending` removed; enforcement starts with the next mandatory-set minor.

**1. Requirement.** A test-capable build of an app enters test mode only when the QA runner supplies activation and a validated root together (`rightkit_daemon::mode::decide` → `rightkit_fs::test_mode::decide`): root absolute, existing, non-symlink, owner-only, disjoint from protected real paths and cloud folders, bound to the app by `.rightkit-app.json` written by rightkit-qa. In test mode every data root, suite root, model root, settings file, credential namespace guard and licence source resolves under that root. A release build compiles none of this (`Mode::Release` is the only value; no env read; marker `RIGHTKIT-TEST-MODE-COMPILED` absent, scanned by M19/M23). No app has its own `*_TEST_MODE` env var, debug flag or alternate data path (HeardRight `HEARDRIGHT_ENGINE_TEST_MODE` named).

**2. Owner surface.** rightkit-fs `test_mode::{decide, validate_root, Request, Decision, ValidatedRoot, Refusal (+rule ids), BINDING_FILE}`, `app_dirs` QA vars (`RIGHTKIT_QA_DATA_DIR`, `RIGHTKIT_QA_HOME`, `RIGHTKIT_UNDER_TEST`) read only with feature `test-mode`; rightkit-daemon `mode::{AppId, Mode, Refusal, TestRoot, ModeInput, ProcessInput, AppRoots, decide, init_or_exit, roots, roots_with, default_protected_paths, TEST_CAPABILITY, TEST_MODE_MARKER}`; rightkit-qa `QaWorkspace::scenario_dir` binding writer; rightkit-credentials guard vars (`RIGHTKIT_NO_KEYCHAIN`, per-app) as the M11 half; rightkit-models `suite::paths` overrides under its `test-mode` feature.

**3. App obligations.** One app-level feature `test-mode = ["rightkit-daemon/test-mode", ...]` reachable only from `qa-native`/`dev-native`; call `mode::init_or_exit` first in every process and derive every root from `AppRoots`; `switches.json` lists the activation selectors with `compiledOut: feature:test-mode`; delete app test-mode env vars, debug-only data paths, entitlement defaults under test (M13).

**4. Permitted app-side code.** The feature declaration, protected-path additions, the `switches.json` entry.

**5. Exception rule.** §2 plus Q3; no exception may read an activation selector outside the owner.

**6. Qualification evidence.** Q1 with and without `test-mode`; Q3 negative tests: release build ignores every selector; activation without root refused; root overlapping user data refused; binding for another app refused; marker absent from release artifacts (M19 scan receipt).

**7. Open owner work.** (a) Ratify: `test_mode.rs:5-7` "code placement only, not ratified (D06)" and D4 wording resolved by Adrian's approval of this document; mandatory-set row updated. (b) Unify the three resolution paths: rightkit-models `suite::paths` must take its root from `AppRoots` (no own env read, no panic path, no `dirs` fallback); credentials guard documented as the M11 half; rightkit-db `JOURNAL_ENV` recorded in `switches.json` class S4 or removed. (c) rightkit-qa 0.7.1 binding semantics ("bookkeeping, not proof of runner") upgraded or stated in the row as accepted.

---

### M22 Agent surface

Owner (locked): rightkit-agent-tools 0.2.0 (with rightkit-automation 0.5.0 and rightkit-qa `control` as backends, rightkit-daemon as transport).

**1. Requirement.** Two shapes, both owned. (i) Client toolbox: any agent-facing tool over an app goes through `Toolbox` (hard exposure rules: no pasteboard/paste tools anywhere, dictation names refused for dictation apps, sensitive apps lose generic read channels; per-caller allowlist; schema validation; admission hook; journal) and the `right-ctl` CLI; `<app>.*` tools exist only in dev/QA builds over the rightkit-qa control service. (ii) Hosted agent surface (MailRight, Pulse): the in-app service that exposes app actions to an agent is a daemon `Service` (M7) with a `MethodTable`, the approval mechanics from M7 row 7b, and the app's action catalogue as data. No app ships its own tool gate, exposure policy, CLI or approval transport.

**2. Owner surface (0.2.0).** `Toolbox` (new/protect_dictation/register(_all)/allow/set_admission/add_journal/metrics/list_for/list_all/call/call_cancellable), `spec::*` (Effect, ToolSpec, ToolError codes, Caller, CancelToken, CallCtx, Tool, validate_args(_strict)), `exposure` rules, `backends::control` (`AppProfile`, `CommandPolicy`, `control_tools`, `CONTROL_METHODS`), `backends::desktop` (feature automation), `backends::web` (feature browser), `backends::speech` (feature speech), `peer::CallContextExt`, `JsonlJournal`, `control_toolbox`, `add_app`, `shared`, `CTL_CALLER`; bin `right-ctl`. Feature `demo` never in release. Not contract: 0.2.1 camelCase exposure hardening.

**3. App obligations.** Register an `AppProfile` (sensitive + dictation rules for HeardRight); expose app actions as control `command`s in the dev build; hosts of an agent surface supply the admission hook and journal and build the service on M7; delete app tool gates, exposure lists, CLIs, approval transports.

**4. Permitted app-side code.** Action catalogue (names, schemas, effect kinds), admission policy decisions (approve/deny per action class), journal sink location.

**5. Exception rule.** §2. Trust policies differ per app by design; that is data in the admission hook, not an exception.

**6. Qualification evidence.** Q1 with default features and `demo`; `schema_control_conformance` on both hosts; a `desktop.*` test exists (README:96 gap); Q2 negative tests: a `paste` tool cannot be registered; `max_tier=foreground` refused; HeardRight `eval` refused.

**7. Open owner work.** (a) 0.2.1 publish; Cargo description drops `rightkit-qa-driver`. (b) Approval service mechanics in M7 7b with MailRight and Pulse as consumers. (c) CodeRight `desktop.*` authority is Adrian's decision (recorded in rightkit#771 when taken), not row work.

---

### M23 Release-time switches (owner assigned by this document, L5)

Owners (locked at publish): @rightkit/release 0.4.0 (`release-switch-scan.mjs`, `release-switch-registry.json`, `qa-build-features.mjs`, `qa-control-scan.mjs`, `devtools-policy`), rightkit-daemon 0.3.0 `mode` (runtime half). Mandatory-set entry changes from `owners: []` to these; `ownerPending` removed.

**1. Requirement.** Every behaviour switch that must not reach users is compiled out of release builds and proven absent at release: forbidden Cargo features (test-mode, qa-native, dev-native, qa-control, dev-inspect, testing, test-seams, isolated-test-keychain, demo, dev-overrides, qa-probes, dev-worker-override, qa-shot, qa-tools, dev-control, bench-native, rightkit-qa/control) by feature-closure scan, inventory completeness (`switches.json`, classes S1-S6, each S1-S4 switch `compiledOut`), binary marker and deny-name scan with a receipt. Runtime switches that remain (S5 user-content location, S6 diagnostics) are inventoried with their conditions. Release-time policy choices (M13 `Enforcement`, update channel, devtools content-only for ViewRight) are config values read by the owner's manifest, never code branches on env.

**2. Owner surface.** Behaviour of `right-release build|release` three-layer scan (findings `forbidden-feature`, `release-switch-manifest-missing`, `unregistered-read`, `dynamic-read`, `not-compiled-out`, `inventory-invalid`, `test-mode-capability`, `release-switch-names`), `release-switch-registry.json` (forbiddenFeatures, marker, denyNames with classes, content-only devtools allowance), `switches.json` schema 1, `releaseSwitches` config keys; rightkit-daemon `mode` (M21). Functions in `release-switch-scan.mjs` are contract via the CLI only.

**3. App obligations.** Maintain `switches.json` (every env/argv/settings/preference/frontend switch, class, channel, `compiledOut`, `userGesture`, `valueType`); `releaseSwitches.manifest/features/forbid` in release config; never `--all-features` for release; delete app-level env switches that are not inventoried, and any that are S1-S4 and not compiled out.

**4. Permitted app-side code.** The inventory and config files; `cfg(feature = ...)` gates that match the inventory.

**5. Exception rule.** §2 plus Q3; `releaseSwitches: { rust: false }` is permitted only for an app with no Rust target.

**6. Qualification evidence.** Q1 (`pnpm test` fixtures under `fixtures/release-switches/`); Q2: the app's release receipt lists every scanned file with sha256 and zero findings; a seeded forbidden feature fails the build (negative test in the app's checks).

**7. Open owner work.** (a) Ratify (registry says "D4, ratified", mandatory set says "unratified"): mandatory-set row updated on approval. (b) Pack-time coverage limits (NSIS LZMA payload, compressed DMG, symlinks) stated in the row as accepted or closed with an unpacked re-scan. (c) `remoteServiceFindings` kept here or moved to M7's gate; decision recorded. (d) rightkit-db `JOURNAL_ENV` and rightkit-credentials guard vars classified (S4/S1) in the registry so every app inventory is checked the same way.

---

## 6. Self-check against ownership-mandatory-set.json (2026-10-20)

| Row | Set owners (min) | This spec | Delta |
|---|---|---|---|
| M1 | tauri-shell 0.4.0, device 0.3.0, @rightkit/tauri 0.2.3 | same, device raised to 0.3.1; @rightkit/tauri-shell added | add @rightkit/tauri-shell 0.1.1; device min 0.3.1; overlay pending 7b |
| M2 | react-shell 0.7.1, platform-ui 0.4.0, theme 0.3.1 | same | none |
| M3 | job-queue 0.4.0, child-process 0.5.2 | same | none |
| M4 | job-queue 0.4.0, sidecar 0.3.0 | same | none |
| M5 | child-process 0.5.2 | same | none |
| M6 | daemon 0.3.0 | same + fs app_dirs (via M8) | none |
| M7 | daemon 0.3.0 | same | none |
| M8 | fs 0.4.4 | same | none |
| M9 | db 0.5.0 | same | none |
| M10 | app-settings 0.2.4, settings-ui 0.6.1 | same | none |
| M11 | credentials 0.3.0 | same | none |
| M12 | license 0.2.3, device 0.3.0, @rightkit/license 0.2.1 | same, device 0.3.1 | device min 0.3.1 |
| M13 | license 0.2.3 | same | none |
| M14 | legal 0.6.1, legal-ui 0.4.0, onboarding 0.6.1 | legal-ui 0.4.1, onboarding 0.6.2 | raise both |
| M15 | logs 0.3.3, @rightkit/logs 0.1.5 | same | none |
| M16 | crash 0.3.3 | same | none |
| M17 | http 0.3.6 | same | none |
| M18 | app-update 0.4.0 | same | none |
| M19 | release 0.4.0 allowDev | same | none |
| M20 | qa 0.7.0, @rightkit/qa 0.5.1, qa-chrome 0.1.8 allowDev | qa 0.7.1 | raise qa |
| M21 | none, never fails | fs 0.4.4 + daemon 0.3.0 + qa 0.7.1 | assign owners |
| M22 | agent-tools 0.2.0 | same | none |
| M23 | none, never fails | release 0.4.0 + daemon 0.3.0 | assign owners |

Registry rows not in the set and used by rows here: rightkit-overlay 0.1.2 (M1 7b), rightkit-models 0.4.4 (M6 7b, M21 7b), rightkit-automation 0.5.0 (M22 backend), rightkit-sidecar listed, RightKitSwift 0.8.1 (Apple profile; `apple` profile rows are not written in this draft and follow the desktop contracts with the Swift targets named in rk-compliance-v2 item 16).

## 7. Cross-row defects this document makes visible (owner work before the row can be called locked in practice)

1. Renderer-registered owner commands: `rightkit_logs_*`, `rightkit_license_*`, `rightkit.settings/models/secrets.*`, `rightkit_update_*`, `rightkit_crash_window_error` are invoked by owner packages and registered by no owner crate. M1 7c closes all of them in one tauri-shell minor.
2. `SecretsBackend.set` carries plaintext from the webview (settings-ui types.ts:61-66). M10 7a.
3. Test mode is implemented three ways (fs test_mode, fs app_dirs env vars, models suite paths) plus two guard mechanisms (credentials, db journal env). M21 7b.
4. "Unpublished staging" headings on published versions in nine packages and stale README version lines in eight; L6 makes this document win, but every next publish must correct its own CHANGELOG.
5. Runtime budget "not yet measured" for job-queue, daemon, logs, rightkit-qa (Windows); Q1 requires the measurement before the next publish of each.
6. D4 is "ratified" in the release registry and "unratified" in the mandatory set; approval of this document settles it (M21/M23 7a).
