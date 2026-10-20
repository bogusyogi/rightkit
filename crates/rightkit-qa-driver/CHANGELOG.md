# Changelog

## 0.7.0

BREAKING: re-exported automation 0.6.0 types. `rightkit-automation` is now a `0.6.0` dependency, so the paths this crate re-exports (`admission`, `events`, `lease`, `lease_store`, `json`, `keys`, `mac`, `offscreen`) carry the 0.6.0 types. `rightkit-qa` stays at `0.7.0` (no 0.8.0 bump in this tree). Callers that named automation 0.5.0 types through this crate must move to 0.6.0.

## 0.6.0 — unpublished staging

- Raises rightkit-automation to 0.5.0, rightkit-qa to 0.7.0, and rightkit-daemon to 0.3.0.

## 0.5.0

**Deprecated: this crate is now a shim.** Use `rightkit-qa` 0.6.0 (feature `control`) for the in-app control server and `rightkit-automation` 0.4.0 for background input. Needs `rightkit-qa` 0.6.0 published first. Existing code compiles unchanged after a version bump; the crate is yanked after one release.

- The embedded control server, with its admission, lease and protocol code and its unit tests, moved to `rightkit-qa` (`embedded`, feature `control`). `rightkit_qa_driver::embedded` is now `pub use rightkit_qa::embedded`, present only with `qa-control`.
- Features are unchanged and mapped: `qa-control` = `tauri-plugin` + `rightkit-qa/control`; `wv2-0_38` and `wv2-0_39` also enable `rightkit-qa/wv2-*` (when `qa-control` is on); `offscreen-private-spi` and `tauri-plugin` as before. The release marker `RIGHTKIT-QA-CONTROL-SERVER-PRESENT` is the same bytes, emitted from `rightkit-qa`, so `right-release` refuses these builds exactly as before.
- Removed dependencies: `rightkit-daemon`, `serde_json` (the server needed them). New optional dependency: `rightkit-qa` 0.6.0.
- Stays here: the WebDriver bridge, the macOS CLI and the Tauri offscreen helpers. The `control-test-app` example moved to `rightkit-qa`.
- README rewritten as a deprecation notice; the server documentation moved to `rightkit-qa/docs/CONTROL-SERVER.md`.

## 0.4.1

The paths below are unchanged, so `rightkit-qa`'s `0.4.0` requirement still resolves. Needs `rightkit-automation` 0.4.0 published first.

The generic input code moved to `rightkit-automation` 0.4.0 and this crate re-exports it under the old paths. No duplicate implementation remains here.

- `admission`, `events`, `lease`, `lease_store` are `rightkit_automation::control::*`. `mac` is `rightkit_automation::macos::background`. `offscreen` is `rightkit_automation::offscreen`, with `start_for_window` and `start_from_env` (the Tauri helpers) still defined in this crate. `json` is `rightkit_automation::json`. `keys` is `rightkit_automation::keys::appkit`. Every path a caller used keeps resolving.
- The WebDriver bridge, the CLI and the `qa-control` embedded server stay here. Their calls to the raw macOS primitives now go through `mac::unsafe_ungated` (inside executors the gate already admitted).
- New dependency: `rightkit-automation` 0.4.0. `offscreen-private-spi` now forwards to `rightkit-automation/offscreen-private-spi`. On Windows the crate now pulls the `windows` 0.62 crate through automation; macOS stays free of third-party crates.
- Behaviour that changed with the move (all stricter): the process-wide kill switch (`rightkit_automation::policy::kill`) now denies every effect admitted through an `EffectGate` (stage `Policy`, reason `kill switch engaged`) and `launch_offscreen` returns `OffscreenError::KillSwitch`; `mac::unsafe_ungated` does nothing while it is engaged. No caller in this repository engages the switch, so nothing changes until a host does.
- Removed: `impl Default for mac::Gated` (use `Gated::allow_all()` to opt out of an admission hook explicitly, or `Gated::new(gate)`). Added: `OffscreenError::KillSwitch`.
- The unit tests for admission, lease, lease store, events, offscreen geometry and the macOS reader moved with the code. `tests/control_plane.rs`, `tests/durable_lease.rs` and `tests/frontmost_macos.rs` now live in `rightkit-automation`; `tests/reexports.rs` here pins the old paths.
- **Hidden macOS launches never activate through the control server.** With `RIGHTKIT_QA_HIDDEN=1` the embedded `Control` plugin sets the app's activation policy to Accessory in plugin setup (before the app creates its windows) and again as each webview becomes ready, so a later `show()`, reopen or AppKit launch step cannot make the app frontmost. Visible and background launches are unchanged. Found by Ember's signed RC on macos-15 (run 38767131660): `hidden qualification must never activate app: states=[loginwindow, Ember, loginwindow]`.
- The launchers already suppress LaunchServices activation (`open -g -j -n`, in `mac::launch_hidden` and `rightkit-qa` `open_command`). What QA cannot enforce: an activation the app itself requests before plugin setup (tao/Tauri `activate_ignoring_other_apps`, default on) and a pre-built signed `.app`'s `LSUIElement` (rewriting its Info.plist breaks the signature). The app must turn `activate_ignoring_other_apps` off when `RIGHTKIT_QA_HIDDEN=1` (see the `rightkit-qa` README, hidden launches). Ember fixed its remaining activations by setting the Accessory policy on the built app before `run()`; `setup()` runs only after the app is already running in Tauri 2.11.
- New unit test for the hidden-flag decision (`hidden_requested`).

## 0.4.0

QA runs many times faster than real time. Every new parameter is optional and a request without them behaves as before, apart from the removed fixed pauses.

- **Input pacing is opt-in.** `click`, `drag`, `key` and `type` no longer sleep between native events (click was about 130 ms, drag about 300 ms, key 20 ms, type 8 ms per character). The optional `paceMs` (default 0, at most 5000) restores a gap; the old values were about click 40-50, drag 16, key 20, type 8. On macOS a request's events are dispatched in one main-thread hop with one viewport measurement, not one measurement and hop per event. On Windows each event is still one `Input.dispatch*` devtools call that completes when the event is handled; only the pauses are gone. `type` still sends real key down/up events per character, so keyboard listeners fire.
- **Events are settled, not slept.** WKWebView applies queued pointer and key events in later web-process round trips, so a read right after a call used to depend on the pauses. After dispatch the server now waits (bounded) for the page to count the matching DOM event (`keyup`, `mouseup`, `mousedown`, `mousemove`, `wheel`), so the effect is visible when the call returns.
- **New `wait` method** (effect kind Script): `wait{js,timeoutMs?,intervalMs?}` waits in the page in one round trip (MutationObserver, UI events, a short interval, rAF when visible) until the function body returns something truthy. Reply `{matched,value|last,error,elapsedMs}`. The predicate is inlined into the evaluated script, never `new Function`, so page CSP cannot block it. macOS uses `callAsyncJavaScript`, Windows `Runtime.evaluate` with `awaitPromise`.
- **New `batch` method**: `batch{steps:[{method,params}]}` runs up to 256 steps in one round trip and stops at the first failure. Each step is checked against the caller's allowlist and passes the effect gate and input lease on its own; `batch` cannot nest. `wait` and `batch` are in `METHODS`; apps that list methods per app (`allow_app`) must add them to use them.
- **Inline screenshots**: `screenshot{inline:true}` returns the PNG as base64 in the reply (`encoding:"base64"`, `data`) and writes no file; without `inline` the reply is unchanged (`path`). Masking is as strong as before: the secret elements are hidden, the page captured and the elements restored as one main-thread operation on macOS (a chain of completion handlers, no thread hop between hide, capture and restore); Windows keeps three strictly sequential devtools calls so the restore cannot overtake the capture. A page that cannot run the hide script is never captured and a restore failure is an error. The pending-animation finish for hidden pages runs inside the hide script (one evaluation, not a separate one).
- `set_viewport` returns at once when the page already has the requested size, ends its settle wait on the first reading equal to the request, and Windows' apply loop stops at once when the size already matches and polls every 5 ms instead of 20 ms.
- **Fixed: the macOS frontmost-application reader** (`mac::frontmost_pid`, `mac::frontmost_name`). It parsed `lsappinfo` text: on CI (macos-15) the output `"LSDisplayName"="Ember"` made the name the literal key `LSDisplayName` and no `pid =` was found, so the background proof (`owned_never_frontmost`) failed with `states=["LSDisplayName"]` and no pid (Ember CI run 38719584230). The pid now comes from the Process Manager `GetFrontProcess`/`GetProcessPID` (answered by LaunchServices on every call, no run loop) and the localized name from `NSRunningApplication` for that pid. `NSWorkspace.frontmostApplication` was not used: it is refreshed only by the main run loop, so a sampling thread in a test or CLI process would read a stale snapshot and could miss an owned app coming to the front. `lsappinfo` remains only as a fallback and `mac::parse_lsappinfo` takes the value after the `=` in every format (`pid = N`, `"pid"=N`, `"name"="X"`), never the key. Unit test with all formats; live test `tests/frontmost_macos.rs` asserts the pid and name are readable on the Mac.
- New `json::base64_encode`.

Needs `rightkit-daemon` 0.2.3 for the per-request latency fix described there.

## 0.3.1

- Fixed: `qa-control` now enables `rightkit-daemon/test-mode`. The control server resolves its suite root with `Suite::resolve()`, which honours `RIGHTKIT_SUITE_ROOT` only when the daemon compiles `test-mode` (since rightkit-daemon 0.2.0). Without it the server ignored the isolated root `rightkit-qa` gives it: on Windows it failed at plugin start (`no app-data root ... SHGetKnownFolderPath: 0x80070003`) under the QA profile, and on macOS it registered in the real per-user suite, so the harness never found it (`control service did not answer`). Found by the Pulse hub QA journey. `qa-control` never ships: `right-release` refuses release builds that enable it.

## 0.3.0

Renamed from rightkit-control; 0.1.11 remains published under the old name.

- README "QA control never ships" now lists all four layers (feature, binary scan, source layer, environment) and the known limits. `right-release` also refuses wdio WebDriver plugin, remote-debugging and Tauri devtools strings in a release binary, scans MSIX/ZIP containers and staged payloads before packing, and refuses release builds that enable `devtools`, `qa-native`, `qa-control` or `dev-native`. No crate code changed.

Breaking (pre-1.0 minor). QA control can no longer ship by accident.

- **New feature `qa-control` gates `embedded::Control`.** `tauri-plugin` no longer compiles the control server; it keeps only `tauri` and the `offscreen::start_for_window` / `start_from_env` window helpers. `rightkit-daemon` and `serde_json` moved from `tauri-plugin` to `qa-control`. Migrate: `features = ["tauri-plugin", "wv2-0_39"]` becomes `features = ["qa-control", "wv2-0_39"]`, inside an app feature (`qa-native`) that is off by default with an optional dependency. With the feature off, `embedded` is not compiled.
- Every binary with `embedded` compiled in contains `RIGHTKIT-QA-CONTROL-SERVER-PRESENT` (`embedded::QA_CONTROL_MARKER`; `#[used]` static, also read by `Control::build`). `right-release` fails a release or update seal or upload that contains it.
- The server logs one stderr line, `RIGHTKIT QA CONTROL SERVER ACTIVE: ...`, when `RIGHTKIT_CONTROL_SERVICE` is set and it starts.
- README: new section "QA control never ships".

Carried from before the bump:

- `set_viewport` reports what it observed, not what was asked. After applying the request it reads `innerWidth`/`innerHeight` back from the page until two consecutive readings (50 ms apart) agree, within a bound (default 2 s; `Control::viewport_settle` or a per-request `settleMs`), and returns that size. Additive result fields: `requestedWidth`, `requestedHeight`, `settled`, `clamped` (settled at a size other than the request, for example clamped to the display's work area), `placementTimedOut`, and `reason` (`"clamped"` or `"unsettled"`). When the size is still changing at the bound, the last observed size is returned with `settled:false`. A size the system will not give is no longer a `viewport mismatch` / readback-timeout error; it is a result with `clamped:true`.
- `set_viewport` waits (bounded, `Control::viewport_placement_wait`, default 5 s) for the shell's initial window placement and state restore before applying, so a late restore can no longer undo the first request after launch. Driven by the `rk-shell:placement` app event `rightkit-shell` emits; without a shell that announces it, there is no wait. Nothing here shows, orders or focuses a window.
- Windows: a `devtools call timed out` error now says where the call stopped: never run by the UI thread (main thread blocked), or issued to WebView2 and then, after a `Browser.getVersion` probe, either "the browser process answers but the page did not complete it" (renderer busy or blocked, modal script dialog, paused debugger) or "the channel is unresponsive". The message still starts with `devtools call timed out`.
- Windows: opt-in tracing with `RIGHTKIT_CONTROL_TRACE=1` (stderr) or `=<file path>`: every devtools call with id, method, thread id and enqueue/dispatch/complete timing, and the webview's NavigationStarting, NavigationCompleted, ScriptDialogOpening, NewWindowRequested and ProcessFailed events. Off by default.
- Windows: stack capture on a devtools timeout (on with `RIGHTKIT_CONTROL_TRACE` or `RIGHTKIT_CONTROL_STACK_ON_TIMEOUT=1`). The V8 debugger is enabled when the webview is created, because `Debugger.enable` is not an interrupting command and would queue behind a busy script; on a timeout the harness sends `Debugger.pause` out of band, reads `Debugger.paused` through an event receiver, reports the top 15 frames (function, url, line, column) up to three times 300 ms apart in the timeout error and the trace, and resumes. No pause within 2 s is reported as renderer in native code or gone. Pauses nobody asked for (a `debugger;` statement) are logged and resumed at once. Frames are positions in the bundle the page loaded; a non-minified debug bundle keeps function names. macOS has no devtools protocol in WKWebView and is unchanged.

## 0.2.0

Minor bump: a default feature flipped (below). Consumers that relied on `wv2-0_38` being on by default must add it.

- **`wv2-0_38` is no longer a default feature.** `default` is now empty. Previously every Windows build pulled `windows` 0.61 and `webview2-com` 0.38 even without `tauri-plugin`, though only the `embedded` module (gated on `tauri-plugin`) uses them. The `wv2-0_38` and `wv2-0_39` features are unchanged.
- **`tauri-plugin` on Windows requires one `wv2-*` feature.** The existing `compile_error!` in `embedded/win_view.rs` enforces it and its message no longer calls `wv2-0_38` a default. Use `features = ["tauri-plugin", "wv2-0_38"]` (Tauri 2.11) or `["tauri-plugin", "wv2-0_39"]` (Tauri 2.12+). macOS builds are unaffected.
- The bundled `examples/control-test-app` now names `wv2-0_38` explicitly.
- Semver note: dropping a default is breaking for pre-1.0 users on Windows; the version stays 0.1.x only because `rightkit-qa` pins `rightkit-control = "0.1.8"`. Bump to 0.2.0 together with that requirement.
