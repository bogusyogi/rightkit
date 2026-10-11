# rightkit-control

Shared native desktop control for Right Suite applications: drive a real desktop app like a person would, in the background, without taking focus. It provides background native input, accessibility reads, a WebDriver bridge, an embedded Tauri control server (behind a feature), and a macOS-only `rightkit-control` CLI (`src/bin/rightkit-control.rs:5-8` exits 2 on other targets). Every effectful request passes one admission gate, and input is fenced by an epoch-based lease.

Catalog capability: `desktop-control`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## What it owns

| Module | Owns |
|---|---|
| `admission` | `EffectGate`, `AdmissionHook` (trait or closure), `AllowAll`, `Effect` and `Settlement`. Every effect is validated, admitted, executed and settled as `ok`, `denied`, `failed` or `cancelled`. |
| `lease`, `lease_store` | `InputLease` and `ControlSession`: input carries `(epoch, sequence)`. Optional durable state through `FileLeaseStore`. |
| `events` | `ControlEvent` lifecycle events (start, stop, crash, denied, lease acquired, fenced, released, rejected) via an `EventSink`. Content-free. |
| `mac` | macOS primitives. Effectful ones are reachable only through `mac::Gated`; `mac::unsafe_ungated` is the explicit opt-in. |
| `webdriver` | `webdriver::Server` with `with_gate`, plus `webdriver::macos::AxBackend`. |
| `embedded` | The Tauri control server behind the `tauri-plugin` feature. |
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
| `tauri-plugin` | no | `tauri` 2, `rightkit-service`, `serde_json` | The embedded Tauri control server (`embedded` module). |
| `offscreen-private-spi` | no | none | macOS only: WKWebView `_setWindowOcclusionDetectionEnabled:` private SPI for `offscreen` launch; without it `offscreen` fails with `OcclusionSpiNotCompiled` (`Cargo.toml:22-25`). |

There are no default features. On Windows the `tauri-plugin` feature requires one WebView2 generation (enable exactly one; if both are enabled `wv2-0_39` wins, `src/embedded/win_view.rs:13-19`): `features = ["tauri-plugin", "wv2-0_38"]` for Tauri 2.11, or `features = ["tauri-plugin", "wv2-0_39"]` for Tauri 2.12 and later (a Windows build with neither fails with a `compile_error!`). macOS and non-plugin builds need neither.

## Usage

CLI syntax from the crate docs:

```text
rightkit-control [--policy FILE] [--events] <command> ...
```

`<command>` is one of `launch click hover drag scroll click-text press keywin keyraw type key shot`, and `serve` handles click, send keys and screenshot. Read-only commands are not effects.

Rust integration goes through the gate. The default gate is allow-all, so existing callers keep their behaviour. A host supplies an `AdmissionHook` (trait or closure) to refuse effects. A denial, or a panicking hook, happens before any side effect.

## Hidden macOS pointer input

Embedded Tauri targets receive native `NSEvent` down, drag and up calls on the WKWebView responder, on AppKit's main thread. This bypasses window first-mouse handling without making a window key, showing it, or activating its app. WebKit reads `+[NSEvent pressedMouseButtons]` to build Pointer Events, so the embedded path supplies a thread-local button mask during dispatch. A second scoped hook bypasses WKWebView's asynchronous pointer text-input interception. Both Objective-C hooks forward to their original implementations outside that dispatch. No DOM input events or global HID events are synthesized.

`pointer {phase:"down"|"drag"|"up"|"move",x,y,button?,modifiers?}` supports scrubs split across requests. `click`, `move` and composite `drag` share native delivery; x and y are viewport CSS pixels. Pointer calls keep effect admission, allowlists and optional input-lease fencing. `rightkit-qa::Control::pointer` uses this service for hidden launches. Non-embedded callers use the per-pid CGEvent fallback, which cannot guarantee first-mouse delivery to inactive views.

`examples/control-test-app/proof.sh` proves trusted Pointer Events, capture, scrub value changes, modifier clicks, hidden and non-key readback, and foreground stability throughout the run. `TARGET` names the workspace managed debug directory. `APP_TARGET` names the standalone fixture's managed debug directory.

## Native viewport sizing

`set_viewport {width:1440,height:1000,settleMs?}` accepts integer CSS dimensions in `1..=16384`, applies them, reads the page's `innerWidth`/`innerHeight` back until two consecutive readings agree (bound `settleMs`, default 2000, `1..=10000`) and returns that observed size, never the request echoed. Extra fields: `requestedWidth`, `requestedHeight`, `settled`, `clamped` (settled at a different size than requested), `placementTimedOut`, and `reason` (`"clamped"` or `"unsettled"`); `ok` stays true. If the size is still changing at the bound, the last observed size comes back with `settled:false`. Before applying, it waits (bound 5 s) for `rightkit-shell`'s initial placement and state restore (`rk-shell:placement` event), so a late restore cannot undo the request. It passes effect admission and never activates, orders or makes windows key. The OS clamps a visible window to the display's work area (screen minus menu bar or taskbar and Dock), so a larger viewport comes back smaller with `clamped:true`. Hidden mode is not formally exempt: the macOS implementation sizes the frame without the visible-screen constraint, so a hidden window usually reaches the request, but the shell's own restore clamps to the monitor and a window shown later is constrained; trust the returned size and `clamped`.

- macOS computes `frameRectForContentRect:` on AppKit's main thread. It temporarily subclasses only the target NSWindow to bypass `constrainFrameRect:toScreen:` during `setFrame:display:NO`, and restores the original class immediately.
- Windows uses `SetWindowPos` with no move, z-order change or activation.
- Other platforms return a typed `unsupported` error.

On macOS, the hidden WKWebView is resized explicitly and native layout completes before page dimensions are polled outside AppKit's main-thread block. The readback retries zero or stale dimensions and transient JS failures for up to 2 s, including callback timeouts. A failure reports both the requested and the last measured dimensions.

`rightkit-qa::Control::set_viewport(width, height)` returns the measured dimensions. The fixture proof checks hidden 1440×1000 CSS content, scale-aware PNG dimensions and foreground stability.

## Control-plane guarantees

- **Admission (EFF-001).** `admission::EffectGate` validates each effectful request, asks the host `AdmissionHook` for approval, executes it, and settles it. `webdriver::Server::with_gate` (click, send keys, screenshot), the `tauri-plugin` `Control::gate`, `mac::Gated` and the CLI all route effects through it.
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
- The `tauri-plugin` lease methods do not yet expose durable state or `acknowledge_input` over the wire.
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
