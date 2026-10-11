# rightkit-control

Shared native desktop control for Right Suite applications: drive a real desktop app like a person would, in the background, without taking focus. It provides background native input, accessibility reads, a WebDriver bridge, an embedded Tauri control server (behind a feature), and a macOS-only `rightkit-control` CLI (`src/bin/rightkit-control.rs:5-8` exits 2 on other targets). Every effectful request passes one admission gate, and input is fenced by an epoch-based lease.

Catalog capability: `desktop-control`. The latest crates.io release is 0.1.11; source 0.3.0 is staged and not yet published (see [INDEX.md](../../INDEX.md)).

## What it owns

| Module | Owns |
|---|---|
| `admission` | `EffectGate`, `AdmissionHook` (trait or closure), `AllowAll`, `Effect` and `Settlement`. Every effect is validated, admitted, executed and settled as `ok`, `denied`, `failed` or `cancelled`. |
| `lease`, `lease_store` | `InputLease` and `ControlSession`: input carries `(epoch, sequence)`. Optional durable state through `FileLeaseStore`. |
| `events` | `ControlEvent` lifecycle events (start, stop, crash, denied, lease acquired, fenced, released, rejected) via an `EventSink`. Content-free. |
| `mac` | macOS primitives. Effectful ones are reachable only through `mac::Gated`; `mac::unsafe_ungated` is the explicit opt-in. |
| `webdriver` | `webdriver::Server` with `with_gate`, plus `webdriver::macos::AxBackend`. |
| `embedded` | The QA-only Tauri control server behind the `qa-control` feature. Never ships; see "QA control never ships". |
| `cli` | `rightkit-control` command parsing and the `Policy` file. |
| `keys`, `json` | Key chord mapping and small JSON helpers. |

## When to use it

- Use it to script a real desktop app or a WebView app in the background, with input that does not move the user's cursor or activate the app.
- Use it when effects need a host admission hook, input needs a lease, or events must go to a journal.

Do not use it for a web page that a headless browser can drive (see `rightkit-browser`). Do not use it for general cross-app automation with a four-tier ladder (see `rightkit-automation`).

## Cargo features

| Feature | Default | Pulls in | Enables |
|---|---|---|---|
| `wv2-0_38` | no | `windows` 0.61, `webview2-com` 0.38 (Windows only) | WebView2 types for Tauri 2.11 (wry 0.55). |
| `wv2-0_39` | no | `windows` 0.62, `webview2-com` 0.39 (Windows only) | WebView2 types for Tauri 2.12 and later. |
| `tauri-plugin` | no | `tauri` 2 | Tauri window helpers only (`offscreen::start_for_window`, `offscreen::start_from_env`). Does not compile the control server. |
| `qa-control` | no | `tauri-plugin`, `rightkit-service`, `serde_json` | QA ONLY, NEVER SHIPPED. The embedded Tauri control server (`embedded` module). Since 0.3.0 this is the only way to get `embedded::Control`. |
| `offscreen-private-spi` | no | none | macOS only: WKWebView `_setWindowOcclusionDetectionEnabled:` private SPI for `offscreen` launch; without it `offscreen` fails with `OcclusionSpiNotCompiled` (`Cargo.toml:29-32`). |

There are no default features. On Windows the `qa-control` feature requires one WebView2 generation (enable exactly one; if both are enabled `wv2-0_39` wins, `src/embedded/win_view.rs:18-25`): `features = ["qa-control", "wv2-0_38"]` for Tauri 2.11, or `features = ["qa-control", "wv2-0_39"]` for Tauri 2.12 and later (a Windows build with neither fails with a `compile_error!`). macOS and builds without `qa-control` need neither.

## Usage

CLI syntax from the crate docs:

```text
rightkit-control [--policy FILE] [--events] <command> ...
```

`<command>` is one of `trusted frontmost windows ax find` (read-only, so not effects and not admitted) or `launch click hover drag scroll click-text press keywin keyraw type key shot`, and `serve` handles click, send keys and screenshot.

Rust integration goes through the gate. The default gate is allow-all, so existing callers keep their behaviour. A host supplies an `AdmissionHook` (trait or closure) to refuse effects. A denial, or a panicking hook, happens before any side effect.

## QA control never ships

The QA harness (`rightkit-qa`, `@rightkit/qa`) is an internal development tool. A shipped app must not be operable headlessly by anyone's harness: the only supported automation surface for users is the app's own command line or plugin. The app-side half of the harness is `embedded::Control`, an in-process server that accepts input injection, DOM and accessibility reads, screenshots and JavaScript evaluation. Its caller identity is a label the client sends in the hello frame (`right-qa`, `right-ctl`); the only OS check is that the peer is the same user (`rightkit-service/src/peer.rs`). Anything of that user can therefore drive a build that contains it, so a shipped build must not contain it. Four layers enforce that:

1. **Feature.** `embedded` is compiled only with `qa-control` (since 0.3.0; `tauri-plugin` alone no longer exposes it). An app names it only from a `qa-native` feature that is off by default, with `rightkit-control` declared `optional = true`: `qa-native = ["dep:rightkit-control", "rightkit-control/qa-control", ...]`. The registration sits inside `#[cfg(feature = "qa-native")]`. Dev lane builds (profile `release-iterate`, `packages/release/development.mjs:15`) enable it with `--features qa-native`, as the rightkit-qa app pattern documents (`crates/rightkit-qa/README.md:118`); `development.mjs` does not check that flag. Release and update lanes must not enable it, and the source layer below refuses it.
2. **Release scan (binary layer).** Every binary that compiles `embedded` contains the fixed string `RIGHTKIT-QA-CONTROL-SERVER-PRESENT` (`embedded::QA_CONTROL_MARKER`, a `#[used]` static that `Control::build` also reads, so linker dead-stripping, `/OPT:REF`, LTO and release optimisation keep it for as long as the server is kept). `right-release` scans Mach-O, PE and ELF files (`packages/release/qa-control-scan.mjs`) and fails the build, seal or upload when it finds the marker. The same scan also applies four more independent rules, so no test-automation surface of any kind ships: `shell-dev-inspect` (rightkit-shell `dev-inspect`, marker `RIGHTKIT-SHELL-DEV-INSPECT-COMPILED`), `wdio-webdriver` (the `tauri-plugin-wdio-webdriver` test driver behind the apps' `qa-native`/`dev-native`; needs 2 of 3 signatures: `tauri-plugin-wdio-webdriver`, `tauri_plugin_wdio_webdriver`, `wdio-webdriver:default`), `remote-debugging` (`--remote-debugging-port`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`) and `tauri-devtools` (`plugin:webview|internal_toggle_devtools`, `toggleDevtoolsHotkey`). A `tauri-devtools` hit is accepted only when the release's devtools allowance is content-only (an app on the allow-list in `packages/release/devtools-policy.mjs`) and the binary carries `RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED`. Each signature is matched as bytes and as UTF-16LE. The scan runs on: the raw EXE or signing set before signing, the staged MSIX payload directory before the package is packed, the finished MSIX read through its ZIP central directory (stored and deflate entries, nested containers up to depth 3, bounded sizes), the `.app` staged for a DMG (`notarize.app`, and the DMG staging directory inside `dmg build`), `target.qaControlScan` entries, the hardening targets, and every sealed file. A scan that finds no native binary at all (missing or empty staging input) fails: it never reads as clean. The scan is skipped for the development lane.
3. **Source layer.** Before it builds anything, `right-release` refuses a release or update tier whose build command (`prePackage`, `package`, the native package hook or finalizer, `postSign`, and the package.json scripts they run) enables `devtools`, `content-devtools`, `qa-native`, `qa-control`, `dev-native` or `dev-inspect` (`--features`, `--features=`, `--feature`, `-F`, `-f`, `--all-features`), or whose `src-tauri/Cargo.toml` or `Cargo.toml` lists `devtools` on `tauri`, `qa-control` on `rightkit-control` or a non-optional `tauri-plugin-wdio-webdriver`, or whose default feature set reaches any of them (`packages/release/qa-build-features.mjs`). `devtools` and `content-devtools` are accepted only for an app on the devtools allow-list (`packages/release/devtools-policy.mjs`). This holds in `--dry-run` too.
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

`set_viewport {width:1440,height:1000,settleMs?}` accepts integer CSS dimensions in `1..=16384`, applies them, reads the page's `innerWidth`/`innerHeight` back until two consecutive readings agree (bound `settleMs`, default 2000, `1..=10000`) and returns that observed size, never the request echoed. Extra fields: `requestedWidth`, `requestedHeight`, `settled`, `clamped` (settled at a different size than requested), `placementTimedOut`, and `reason` (`"clamped"` or `"unsettled"`); `ok` stays true. If the size is still changing at the bound, the last observed size comes back with `settled:false`. Before applying, it waits (bound 5 s) for `rightkit-shell`'s initial placement and state restore (`rk-shell:placement` event), so a late restore cannot undo the request. It passes effect admission and never activates, orders or makes windows key. The OS clamps a visible window to the display's work area (screen minus menu bar or taskbar and Dock), so a larger viewport comes back smaller with `clamped:true`. Hidden mode is not formally exempt: the macOS implementation sizes the frame without the visible-screen constraint, so a hidden window usually reaches the request, but the shell's own restore clamps to the monitor and a window shown later is constrained; trust the returned size and `clamped`.

- macOS computes `frameRectForContentRect:` on AppKit's main thread. It swaps the `constrainFrameRect:toScreen:` implementation for the target NSWindow only, inside a thread-local scope around `setFrame:display:NO` (`src/embedded/mac_view.rs:335-361`, `:412-416`). The window's class is not changed. The replacement stays installed and calls the original for every other window or outside that scope.
- Known limit on macOS: the size is NOT independent of the display. On a 1600x900 display, `set_viewport(1440, 900)` on a hidden window returned 1440x789 (`clamped: true`); the same request passed earlier on a larger display (CutRight, 2026-10-12, control 0.3.0; ledger 189). The constraint swap above does not prevent this clamp. Callers must check `clamped` and the returned size.
- Windows uses `SetWindowPos` with no move, z-order change or activation.
- Other platforms return a typed `unsupported` error.

On macOS, the hidden WKWebView is resized explicitly and native layout completes before page dimensions are polled outside AppKit's main-thread block. The readback retries zero or stale dimensions and transient JS failures for up to 2 s, including callback timeouts. A failure reports both the requested and the last measured dimensions.

`rightkit-qa::Control::set_viewport(width, height)` returns the measured dimensions. The viewport call is exercised by `tests/offscreen_macos.rs:242` (1440×900, with a focus check at :349). `proof.sh` does not call `set_viewport`, and no test checks hidden 1440×1000 page content.

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
- The `qa-control` lease methods do not yet expose durable state or `acknowledge_input` over the wire.
- Breaking for direct `mac` callers: `mac::click`, `type_text`, `key`, `ax_press`, `click_node`, `launch_hidden`, `capture_window`, `ax_make_key`, `key_without_raise`, `hover`, `drag` and `scroll` moved to `mac::Gated` (or `mac::unsafe_ungated`). Reads (`windows`, `main_window`, `frontmost_*`, `ax_dump`, `ax_find`, `ax_text`, `accessibility_trusted`) are unchanged.
- `webdriver::macos::AxBackend` is an adapter whose `Backend` methods act directly, and `Server::backend` is a public field. Only calls through `Server::handle` or `serve` are admitted.

## Platform support

- macOS: accessibility reads, per-pid events (`CGEventPostToPid`, never the global HID tap), the embedded WKWebView path, and captures.
- Windows: WebView2 through the `wv2-*` features, and `SetWindowPos` for viewport sizing.
- Other platforms: typed `unsupported` where the embedded path has no implementation.

## Tests and examples

- `tests/control_plane.rs`, `tests/durable_lease.rs`, `tests/gated_paths.rs`, `tests/serve_auth.rs`.
- `examples/control-test-app/` with `proof.sh` (see above).
- `registration.patch` at the crate root.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).
