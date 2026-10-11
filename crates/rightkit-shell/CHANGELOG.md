# Changelog

All notable changes to `rightkit-shell`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## Unreleased

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
`Unavailable("invalid service name ..")` because `rightkit-service` names allow only `[a-z0-9-]`, so a
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

## 0.3.0

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
  `rightkit-service` dev-dependency and duplicate `rightkit-control` dev entry.
