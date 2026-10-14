# rightkit-automation

Brand-neutral desktop automation for macOS and Windows, in Rust only. It observes apps, windows and accessibility trees, and acts on them through an input ladder that never goes to a higher tier unless the caller allows it, recording which tier ran. It replaces `rightkit-computer` (retired; see `docs/CARGO-PUBLICATION.md`). Generic parts of HeardRight's `heardright_platform` and `heardright-command-host` moved here; see `docs/MOVED_FROM.md`. HeardRight's locked rules travel with it (`docs/LOCKED_RULES.md`).

Catalog capability: `desktop-automation`.

```text
snapshot  ->  stable element ids (AX / UIA), freshness guard
act       ->  kill switch -> allowlist -> lease fence -> freshness -> secure/credential/risk
              -> admission hook -> tier 1 semantic -> 2 in-app -> 3 per-pid events -> 4 foreground (opt-in)
              -> receipt (tier used, gate that refused) -> settle -> lifecycle event
```

| Capability | Where |
|---|---|
| macOS AX tree walk, focus, native write, per-pid events, per-window capture | `src/macos/` (zero third-party dependencies) |
| Windows UIA tree walk, Invoke/Toggle/Select/Expand/Value/Scroll/Text patterns, window messages, SendInput, PrintWindow, clipboard | `src/win/` |
| macOS system-wide AX element, hit test at a point, per-call AX timeout | `macos::ax` (`system_wide`, `element_at_position`, `AxOptions`) |
| Windows raw `SendInput` batch (any VK incl. `0xE8`, scan codes, extended bit, Unicode, mouse, `dwExtraInfo` tag), modifier snapshot and restore | `win::input` (`send_input_batch`, `RawInput`, `ModifierSnapshot`) |
| Engine: ids, freshness guard, ladder, receipts | `src/engine.rs` |
| Control lease, epoch fencing, acknowledged input sequence | `src/lease.rs` |
| Admission pre-action hook | `src/admission.rs` |
| Lifecycle events, content-free metrics | `src/events.rs` |
| Allowlist, deny list, kill switch, risk classes, credential gates | `src/policy.rs` |
| Screenshot redaction, PNG decode | `src/redact.rs`, `src/png.rs` |
| Text delivery with read-back proof | `src/delivery.rs`, `src/proof.rs` |
| Fail-closed app-data root | `src/paths.rs` (via `rightkit-daemon::Suite`) |

## When to use it

- Use it when an in-process Rust app or CLI must observe and act on a desktop app in the background, with receipts for each step.
- Use it when a caller needs the kill switch, allowlist and admission hook around every effect.

Do not use it for Right Suite apps that expose their own in-app control service: use `rightkit-agent-tools` or `rightkit-qa-driver`. The crate does not run as a daemon.

## Cargo features

`in-app` (off by default): the tier-2 `in_app::ServiceBridge` client and the `paths` module, which pull in `rightkit-daemon` and `serde_json`. The `in_app::InApp` trait is always available. The Windows backend is compiled with `cfg(windows)` and depends on the `windows` crate. Without `in-app` the crate has no third-party dependencies on macOS.

## Usage

Illustrative. These calls are taken from `examples/mac_proof.rs`:

```rust
use rightkit_automation::*;

let automation = Automation::new(platform, policy); // platform: NativePlatform
let snap = automation.snapshot(Target::app(pid), Limits { max_depth: 14, max_nodes: 900 })?;
```

`policy` is a `Policy` (allowlist and deny list). `NativePlatform` is `macos::MacPlatform` on macOS and `win::WinPlatform` on Windows. See `examples/mac_proof.rs` for how the platform value is built and for an `act` call.

## Platform support

- macOS: the accessibility (AX) backend. `tests/e2e_mac.rs` and `examples/mac_proof.rs` exercise it on real apps; no run result is recorded here.
- Windows: the UI Automation backend in `src/win/`. Verify it on a Windows host before you rely on it: `rightkit cargo test -p rightkit-automation --test e2e_win -- --nocapture`.
- Threading (macOS): NSWorkspace publishes the app list and frontmost app through the main run loop. Call from the main thread of a CLI or test, or from any thread of an app whose main loop runs.
- `launch` uses the system `open` command on macOS. Window capture tries `CGWindowListCreateImage` first and falls back to `screencapture` whenever that returns no image (`src/macos/capture.rs:54-57`). Both `open` and `screencapture` run as waited children, so a strict zero-child profile should call neither `launch` nor window capture.

## Safety rules enforced here

- Never types credentials. Payloads that look like credentials, and credential-labelled or secure fields, are refused at every tier, including delivery. No override flag.
- Evidence never lands relative to the working directory. `JsonlEvidence::open` and `screenshot` require absolute paths, and `paths::evidence_dir()` fails closed.
- Screenshots are redacted before they are returned. If a region must be masked and the file cannot be rewritten, the capture is deleted and the call errors.
- The per-app allowlist (`Policy`) is checked before launch when the spec names the app, and again on the launched process.
- The kill switch (`kill()`) fails every act, snapshot and batch step closed.

## Tests and examples

- `tests/e2e_mac.rs`: real TextEdit and Calculator, launched in the background. The frontmost app is verified unchanged, and everything opened is closed afterwards. `harness = false`, so it runs on the main thread. It needs Accessibility and Screen Recording for the test host. Without them it prints `SKIP e2e_mac` and returns; `RIGHTKIT_REQUIRE_ASSETS=1` makes it fail instead (`tests/e2e_mac.rs:4-5`, `:177-183`). A managed-broker run lacks both, so run the built binary from a granted shell.
- `tests/e2e_win.rs`: real Notepad on Windows. It is type-checked on every OS.
- `tests/engine.rs`: the engine against a simulated desktop (`mock::MockPlatform`). It covers states that no harmless real app exposes: secure field, recreated editor, lossy clipboard, and a native write that reports success falsely.
- `tests/kill.rs`: kill-switch behaviour.
- `examples/mac_proof.rs` and `examples/win_smoke.rs`: evidence-producing proofs.

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).

## Windows

- Status: the earlier check (`rightkit cargo check -p rightkit-automation`, 2026-10-08, as recorded in this README) compiled `src/win/` against `windows` 0.61. The crate now declares 0.62, which that check does not cover, and no 0.62 compile is recorded here. `tests/e2e_win.rs` has not been run on Windows (its header says so). Its frontmost-app check reports `foreground changed` (`tests/e2e_win.rs:70`), so the run depends on desktop focus.
- Version: `Cargo.toml` declares `windows` 0.62 with 16 features.
- Tree and patterns: UI Automation via `CoCreateInstance` (in-process, multithreaded COM). Reads Invoke, Toggle, SelectionItem, ExpandCollapse, Value, Scroll and Text patterns (`src/win/uia.rs`).
- Tier 3 (targeted): `PostMessageW` of `WM_CHAR` or key messages to the focused child window. Modifier chords are refused as `Unsupported` and need tier 4.
- Tier 4 (foreground, explicit opt-in): `SendInput` for keys and Unicode text. Clicks move the cursor with `SetCursorPos` and restore it afterwards. Held modifiers are released first. `SendInput` has no window target, so the intended app must already be foreground.
- Activation: `SetForegroundWindow`, with `AttachThreadInput` to the foreground thread as the fallback. Capture: `PrintWindow` with `PW_RENDERFULLCONTENT` (2) into a compatible bitmap. Clipboard: Win32 `DataExchange`, unicode text read and write, with snapshot and restore.
- Launch: `CreateProcessW` suspended with `SW_SHOWNOACTIVATE`, assigned to a kill-on-close job with an optional `ActiveProcessLimit`, then resumed. A window is adopted only if its pid is the started pid, or a new process with the same app id inside the job. If no window appears within 5 s, the call returns an error and the launched tree is torn down.
- Limits: `status()` reports UIPI. A non-elevated process cannot drive an elevated app unless UIAccess signing is used. The code does not check session or desktop state before `SendInput`.
- Dependency: `windows` 0.62 under `cfg(windows)`, with features Foundation, Graphics_Gdi, Security, Storage_FileSystem, Storage_Xps, System_Com, System_DataExchange, System_JobObjects, System_Memory, System_Ole, System_Threading, System_Variant, UI_Accessibility, UI_HiDpi, UI_Input_KeyboardAndMouse and UI_WindowsAndMessaging. Consumers enable no Windows features. `WinPlatform::new()` sets per-monitor DPI awareness v2 and joins the COM MTA on the calling thread; `WinPlatform::new_minimal()` does neither (see its doc comment for what that leaves to the host). 
- Run on a Windows host: `rightkit cargo test -p rightkit-automation --test e2e_win -- --nocapture`. The test header says it has not been run on Windows.
- Raw input (`win::input`): `send_input_batch(&[RawInput])` is one `SendInput` call from typed key, Unicode and mouse items, with an optional `dwExtraInfo` tag (`SendOptions`). `ModifierSnapshot` records held modifiers so a caller that releases them can restore them. No policy, focus or receipt checks: it is for hosts that own their own policy. Unit tests build the `INPUT` array without injecting.
