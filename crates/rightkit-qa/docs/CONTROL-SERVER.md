# The embedded QA control server (`rightkit-qa` feature `control`)

The in-app Tauri control server, its admission gate, input lease and wire protocol. It moved here from `rightkit-qa-driver` (`qa-control`) in `rightkit-qa` 0.6.0, behind the dev-only `control` feature; `rightkit-qa-driver` 0.5.0 is a deprecated shim that re-exports it. Background native input, the lease store and the admission gate live in `rightkit-automation`. Every effectful request passes one admission gate, and input is fenced by an epoch-based lease.

Catalog capability: `desktop-control`. See the migration note at the top of [`../README.md`](../README.md).

## What it owns

| Item | Owns |
|---|---|
| `embedded` (`src/embedded/`) | The QA-only Tauri control server: `Control`, `QA_CONTROL_MARKER`, `METHODS`, `SECRET_SELECTORS`, `PlacementGate`, `ViewportCfg`. Never ships; see "QA control never ships". |
| `in_app` (`src/in_app.rs`) | `ServiceBridge`: `rightkit_automation::in_app::InApp` (tier 2) implemented as a client of the server over its `rightkit-daemon` service. |
| `control` | `Control`, the harness-side client that launches the app and drives it (existing, unchanged). |
| `guard` | The native-QA contract check for an app's `Cargo.toml` and sources; understands both `rightkit-qa` (`control`) and the legacy `rightkit-qa-driver` (`qa-control`). |

The admission gate, input lease, lease store, events, background macOS input, key tables and JSON helpers are in `rightkit-automation` (`control::*`, `macos::background`, `keys::appkit`, `json`). The WebDriver bridge and the macOS CLI stay in the deprecated `rightkit-qa-driver` shim.

## When to use it

- Use it to script a real desktop app or a WebView app in the background, with input that does not move the user's cursor or activate the app.
- Use it when effects need a host admission hook, input needs a lease, or events must go to a journal.

Do not use it for a web page that a headless browser can drive (see `rightkit-browser`). Do not use it for general cross-app automation with a four-tier ladder (see `rightkit-automation`).

## Cargo features

| Feature | Default | Pulls in | Enables |
|---|---|---|---|
| `control` | no | `tauri` 2, `rightkit-daemon/test-mode` | QA ONLY, NEVER SHIPPED. The embedded Tauri control server (`embedded`) and the `InApp` client (`in_app`). Same marker as the old `rightkit-qa-driver/qa-control`. |
| `wv2-0_38` | no | `windows` 0.61, `webview2-com` 0.38 (Windows only) | WebView2 types for Tauri 2.11 (wry 0.55). Use with `control`. |
| `wv2-0_39` | no | `windows` 0.62, `webview2-com` 0.39 (Windows only) | WebView2 types for Tauri 2.12 and later. Use with `control`. |
| `browser` | no | `rightkit-browser`, `tokio` | Chrome (CDP) targets. |

There are no default features. On Windows `control` requires one WebView2 generation (enable exactly one; if both are enabled `wv2-0_39` wins, `src/embedded/win_view.rs:18-25`): `features = ["control", "wv2-0_38"]` for Tauri 2.11, or `features = ["control", "wv2-0_39"]` for Tauri 2.12 and later (a Windows build with neither fails with a `compile_error!`). macOS and builds without `control` need neither.

## Usage

An app declares `rightkit-qa` as an optional dependency and names `control` only from a `qa-native` feature that is off by default, then registers the plugin inside `#[cfg(feature = "qa-native")]`:

```rust
#[cfg(feature = "qa-native")]
if let Some(p) = rightkit_qa::embedded::Control::<tauri::Wry>::new().build_if_enabled() { builder = builder.plugin(p); }
```

Rust integration goes through the gate. The default gate is allow-all, so existing callers keep their behaviour. A host supplies an `AdmissionHook` (trait or closure) to refuse effects. A denial, or a panicking hook, happens before any side effect.

## QA control never ships

The QA harness (`rightkit-qa`, `@rightkit/qa`) is an internal development tool. A shipped app must not be operable headlessly by anyone's harness: the only supported automation surface for users is the app's own command line or plugin. The app-side half of the harness is `embedded::Control`, an in-process server that accepts input injection, DOM and accessibility reads, screenshots and JavaScript evaluation. Its caller identity is a label the client sends in the hello frame (`right-qa`, `right-ctl`); the only OS check is that the peer is the same user (`rightkit-daemon/src/peer.rs`). Anything of that user can therefore drive a build that contains it, so a shipped build must not contain it. Four layers enforce that:

1. **Feature.** `embedded` is compiled only with `control`. An app names it only from a `qa-native` feature that is off by default, with `rightkit-qa` declared `optional = true`: `qa-native = ["dep:rightkit-qa", "rightkit-qa/control", ...]`. (The deprecated shim form `qa-native = ["dep:rightkit-qa-driver", "rightkit-qa-driver/qa-control", ...]` still works and compiles the same code.) The registration sits inside `#[cfg(feature = "qa-native")]`. Dev lane builds (profile `release-iterate`, `packages/release/development.mjs:15`) enable it with `--features qa-native`, as the rightkit-qa app pattern documents (`crates/rightkit-qa/README.md`); `development.mjs` does not check that flag. Release and update lanes must not enable it, and the source layer below refuses it.
2. **Release scan (binary layer).** Every binary that compiles `embedded` contains the fixed string `RIGHTKIT-QA-CONTROL-SERVER-PRESENT` (`embedded::QA_CONTROL_MARKER`, a `#[used]` static that `Control::build` also reads, so linker dead-stripping, `/OPT:REF`, LTO and release optimisation keep it for as long as the server is kept). `right-release` scans Mach-O, PE and ELF files (`packages/release/qa-control-scan.mjs`) and fails the build, seal or upload when it finds the marker. The same scan also applies four more independent rules, so no test-automation surface of any kind ships: `shell-dev-inspect` (rightkit-tauri-shell `dev-inspect`, marker `RIGHTKIT-SHELL-DEV-INSPECT-COMPILED`), `wdio-webdriver` (the `tauri-plugin-wdio-webdriver` test driver behind the apps' `qa-native`/`dev-native`; needs 2 of 3 signatures: `tauri-plugin-wdio-webdriver`, `tauri_plugin_wdio_webdriver`, `wdio-webdriver:default`), `remote-debugging` (`--remote-debugging-port`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`) and `tauri-devtools` (`plugin:webview|internal_toggle_devtools`, `toggleDevtoolsHotkey`). A `tauri-devtools` hit is accepted only when the release's devtools allowance is content-only (an app on the allow-list in `packages/release/devtools-policy.mjs`) and the binary carries `RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED`. Each signature is matched as bytes and as UTF-16LE. The scan runs on: the raw EXE or signing set before signing, the staged MSIX payload directory before the package is packed, the finished MSIX read through its ZIP central directory (stored and deflate entries, nested containers up to depth 3, bounded sizes), the `.app` staged for a DMG (`notarize.app`, and the DMG staging directory inside `dmg build`), `target.qaControlScan` entries, the hardening targets, and every sealed file. A scan that finds no native binary at all (missing or empty staging input) fails: it never reads as clean. The scan is skipped for the development lane.
3. **Source layer.** Before it builds anything, `right-release` refuses a release or update tier whose build command (`prePackage`, `package`, the native package hook or finalizer, `postSign`, and the package.json scripts they run) enables `devtools`, `content-devtools`, `qa-native`, `qa-control`, `dev-native` or `dev-inspect` (`--features`, `--features=`, `--feature`, `-F`, `-f`, `--all-features`), or whose `src-tauri/Cargo.toml` or `Cargo.toml` lists `devtools` on `tauri`, `qa-control` on `rightkit-qa-driver`, `control` on a non-optional `rightkit-qa` (and the qualified item `rightkit-qa/control`), or a non-optional `tauri-plugin-wdio-webdriver`, or whose default feature set reaches any of them (`packages/release/qa-build-features.mjs`). `devtools` and `content-devtools` are accepted only for an app on the devtools allow-list (`packages/release/devtools-policy.mjs`). This holds in `--dry-run` too.
4. **Environment, and a loud line.** A compiled-in server still serves nothing unless the launcher sets `RIGHTKIT_CONTROL_SERVICE=<name>`, and then writes `RIGHTKIT QA CONTROL SERVER ACTIVE: ...` to stderr before the socket exists.

### What the scans cannot see

- **Compressed installers.** An NSIS setup (LZMA payload) and a compressed DMG (UDZO) hide their contents. They are never scanned after packing; `right-release` instead requires a pre-pack scan of the raw EXE, the staged payload or the `.app` that found at least one native binary, and fails the release without one (fail closed). Anything the installer packs that was not in the scanned set (NSIS resources not listed in `sign.prePackageFiles` or `target.qaControlScan`) is not covered.
- **Not ZIP-readable.** Encrypted entries, compression methods other than stored and deflate, and entries larger than 1 GiB inflated (or a container past 8 GiB inflated) are reported as findings, not skipped.
- **`wdio-webdriver` is weaker than the marker.** The plugin source was not available when its signatures were chosen; they follow Rust's path, module and permission naming. A binary built with path remapping and stripped module names and with the capability JSON absent can show fewer than two signatures; the source layer is the backstop.
- **Frontend assets.** Tauri embeds the web assets compressed; JavaScript test hooks in the bundle are invisible to the scan.
- **Devtools in a build that is not a Tauri build, and unknown drivers.** Only the named surfaces are detected. A new automation plugin needs a new rule in `QA_RULES`.
- **Debug builds.** A debug build contains `tauri-devtools` strings by design and is refused as a release input.

## Hidden macOS pointer input

Embedded Tauri targets receive native `NSEvent` down, drag and up calls on the WKWebView responder, on AppKit's main thread. This bypasses window first-mouse handling without making a window key, showing it, or activating its app. WebKit reads `+[NSEvent pressedMouseButtons]` to build Pointer Events, so the embedded path supplies a thread-local button mask during dispatch. A second scoped hook bypasses WKWebView's asynchronous pointer text-input interception. Both Objective-C hooks forward to their original implementations outside that dispatch. No DOM input events or global HID events are synthesized.

`pointer {phase:"down"|"drag"|"up"|"move",x,y,button?,modifiers?}` supports scrubs split across requests. `click`, `move` and composite `drag` share native delivery; x and y are viewport CSS pixels. Pointer calls keep effect admission, allowlists and optional input-lease fencing. `rightkit-qa::Control::pointer` uses this service for hidden launches. Non-embedded callers use the per-pid CGEvent fallback, which cannot guarantee first-mouse delivery to inactive views.

`examples/control-test-app/proof.sh` proves trusted Pointer Events, capture, scrub value changes, modifier clicks, hidden and non-key readback, and foreground stability throughout the run. `TARGET` names the workspace managed debug directory. `APP_TARGET` names the standalone fixture's managed debug directory.

## Native viewport sizing

### Speed: pacing, waits, batches, inline screenshots (0.4.0)

QA latency is dominated by round trips and fixed sleeps, so the in-app server avoids both. All parameters below are optional; a request without them behaves as before apart from the removed pauses.

- `paceMs` (default 0, at most 5000) on `click`, `drag`, `key`, `type`: the gap between native events. Without it the events of a request go back to back, in one native hop on macOS (Windows: one devtools call per event, no pause). The former fixed pauses were about click 40-50 ms, drag 16, key 20 and type 8 per character; pass those values for a page that needs real time between events. `type` still sends real key down/up events per character, so keydown, keypress and keyup listeners fire.
- On macOS WKWebView queues pointer and key events and applies each in a web-process round trip. After the events are dispatched, the server waits (bounded) for the page to count the matching DOM event (`keyup`, `mouseup`, `mousedown`, `mousemove`, `wheel`), so a read right after the call sees the effect. Windows' `Input.dispatch*` calls complete when the event is handled.
- `wait {js,timeoutMs?,intervalMs?}` waits in the page in ONE round trip until the function body `js` returns something truthy, re-checking on DOM mutations, common UI events, a short interval (default 20 ms) and every frame when the page is visible. Reply `{matched,value|last,error,elapsedMs}`; a timeout is `matched:false`, not an error. The predicate text is inlined into the evaluated script (never `new Function`), so page CSP cannot block it. It is an effect of kind Script, gated like `eval`.
- `batch {steps:[{method,params}]}` runs up to 256 steps in one round trip and stops at the first failure: `{ok:true,completed,results,stepMs}` or `{ok:false,failedStep,error,errorCode,results}`. Every step is checked against the caller's allowlist and passes the effect gate and the input lease on its own, exactly as if it were sent alone; `batch` cannot nest. A `screenshot` step defaults to `inline:true`.
- `screenshot {inline:true}` returns `{bytes,masked,encoding:"base64",data}` and writes no file. Masking is unchanged and as strong: the secret elements are hidden, the page captured and the elements restored as one operation (on macOS one chain of completion handlers on the main thread; on Windows three strictly sequential devtools calls, so the restore can never overtake the capture). A page that cannot run the hide script is never captured, and a restore failure is an error.
- `set_viewport` confirms two consecutive page measurements 500 ms apart, even when initial dimensions match.

`set_viewport {width:1440,height:1000,settleMs?}` accepts integer CSS dimensions in `1..=16384`. After bounded shell placement wait (default 5 s), it applies dimensions & reads `innerWidth`/`innerHeight` until two consecutive non-empty measurements agree, 500 ms apart. Invalid readings & evaluation errors break confirmation. `settleMs` bounds readback (default 2000, accepted `1..=10000`); budgets too short for confirmation cannot succeed.

Reply fields: `innerWidth`, `innerHeight`, `requestedWidth`, `requestedHeight`, `settled`, `clamped`, `placementTimedOut`, optional `reason` & `error`. `ok:true` requires settled readback & completed shell placement. Unsettled readback or placement timeout returns `ok:false` with diagnostics. A permitted OS size limit returns confirmed actual dimensions with `clamped:true`.

- macOS: QA-sized hidden windows keep their screen-constraint exemption through deferred AppKit layout. Window-owned Objective-C association scopes this exemption to that NSWindow's lifetime; showing it restores normal constraints. No class swap, activation, ordering or key-window change. Hidden mismatches trigger bounded reapplication, so late shell placement cannot turn into clamped success. Windows that cannot reach requested dimensions fail. [AppKit constraint API](https://developer.apple.com/documentation/appkit/nswindow/constrainframerect(_:to:)), [Objective-C association API](https://developer.apple.com/documentation/objectivec/objc_setassociatedobject(_:_:_:_:)).
- macOS explicitly sizes hidden WKWebView & completes native layout before polling page dimensions off AppKit's main thread. Zoom & titlebar insets remain measured.
- Windows uses `SetWindowPos` without move, z-order change or activation; visible-window constraints may yield confirmed smaller dimensions.
- Other platforms return `unsupported`.

`rightkit_qa::control::Control::set_viewport(width, height)` returns confirmed CSS dimensions or an error. It validates server settling & placement metadata, including responses from older servers. Effects are never replayed by RPC; one server request can reapply hidden native geometry within its settling bound.

For custom adapters, `rightkit_qa::embedded::set_viewport(&window, &gate, label, width, height, &cfg)` uses `ViewportWindow::{apply,read,requires_exact_size}`. Default `requires_exact_size` is `false`; use `true` for windows that must reach requested dimensions. `ViewportOutcome::to_json` applies wire success rules. All embedded APIs require feature `control`.

Tests: `rightkit-qa/tests/viewport.rs` covers public adapters & late placement; viewport/client unit tests cover confirmation & failure. `rightkit-qa-driver/tests/offscreen_macos.rs` has ignored native qualification for never-shown windows larger than current display, delayed page readback & focus preservation. Run with `--features qa-control --test offscreen_macos hidden_press -- --ignored --test-threads=1`; provide `RK_CONTROL_TEST_APP` built with patched `rightkit-qa`, or let test build its standalone fixture.

## Control-plane guarantees

- **Admission (EFF-001).** `admission::EffectGate` validates each effectful request, asks the host `AdmissionHook` for approval, executes it, and settles it. `webdriver::Server::with_gate` (click, send keys, screenshot), the `qa-control` `Control::gate`, `mac::Gated` and the CLI all route effects through it.
- **Input lease (PTY-002).** `lease::InputLease` acquire bumps the epoch and fences the previous holder. Stale epochs are rejected. Duplicate acknowledged input is deduplicated without re-execution. Gaps are rejected. Ambiguous (unacknowledged) input must be explicitly reconciled before new input. `Control::input_lease` makes the plugin's input methods require `leaseEpoch` and `inputSequence` and adds `lease_*` methods.
- **Durable lease state.** `InputLease::open` with a `LeaseStore`. The default is in-memory. `FileLeaseStore` writes atomically (temp file, fsync, rename, directory fsync, checksum). An epoch is granted only once durable, and an input sequence is reserved durably before its executor runs. Each lease has an `ExecutionIdentity` and an `ExecutionState` (`Running`, `Exited`, `Unknown`). On restart the epoch is fenced forward, the lease is detached, `Running` comes back `Unknown`, and unacknowledged input comes back as `Unknown` input. It must be resolved with `acknowledge_input(epoch, seq)`, `reconcile` or `acquire_reconciling`. A corrupt or foreign record fails closed rather than resetting.
- **Events.** `events::ControlEvent` content-free lifecycle events through an `EventSink`.

## CLI policy

Without a policy, the gate is allow-all and output is unchanged. With `--policy FILE` or `RIGHTKIT_CONTROL_POLICY`, the gate applies `cli::Policy`: `default allow|deny`, then first-match `allow|deny <kind|*> [method|*]` lines. An unreadable or invalid policy exits 2. A denial exits 3 and does nothing. `--events` or `RIGHTKIT_CONTROL_EVENTS=stderr` prints content-free lifecycle events to stderr.

## Known gaps

- `FileLeaseStore` assumes one owning process per file. It has no cross-process lock.
- If the post-execute acknowledgement save fails, the durable record stays conservative: that input recovers as `Unknown`. `InputLease::store_degraded()` reports it.
- `acquire_reconciling` panics if a durable store refuses the save. Durable callers should use `try_acquire_reconciling`.
- The holder is identified by its epoch only. No holder label is stored.
- Recovery runs in `InputLease::open` before an event sink is attached, so no recovery event is emitted. Inspect `record()` after opening.
- `LeaseError` gained variants (`ExecutionNotRunning`, `UnsentSequence`, `NonMonotonicAck`, `InvalidExecutionTransition`, `Store`). Exhaustive external matches need a new arm.
- The `control` lease methods do not yet expose durable state or `acknowledge_input` over the wire.
- Breaking for direct `mac` callers: `mac::click`, `type_text`, `key`, `ax_press`, `click_node`, `launch_hidden`, `capture_window`, `ax_make_key`, `key_without_raise`, `hover`, `drag` and `scroll` moved to `mac::Gated` (or `mac::unsafe_ungated`). Reads (`windows`, `main_window`, `frontmost_*`, `ax_dump`, `ax_find`, `ax_text`, `accessibility_trusted`) are unchanged.
- `webdriver::macos::AxBackend` is an adapter whose `Backend` methods act directly, and `Server::backend` is a public field. Only calls through `Server::handle` or `serve` are admitted.

## Platform support

- macOS: accessibility reads, per-pid events (`CGEventPostToPid`, never the global HID tap), the embedded WKWebView path, and captures.
- Windows: WebView2 through the `wv2-*` features, and `SetWindowPos` for viewport sizing.
- Other platforms: typed `unsupported` where the embedded path has no implementation.

## Tests and examples

- Server unit tests live in `src/embedded/` (marker, viewport core, key and mouse mapping) and `src/in_app.rs` (bridge over a real daemon host). The shim keeps `tests/gated_paths.rs`, `tests/serve_auth.rs`, `tests/offscreen_*.rs` and `tests/reexports.rs`.
- `../examples/control-test-app/` with `proof.sh` (see above) is the standalone UI proof target.

## Version and changes

Published version: see [INDEX.md](../../../INDEX.md). Changes are listed in [CHANGELOG.md](../CHANGELOG.md).
