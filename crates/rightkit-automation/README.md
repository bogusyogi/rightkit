# rightkit-automation

Brand-neutral desktop automation on macOS and Windows, Rust only. One implementation for CodeRight (computer use), HeardRight (dictation delivery), CutRight/GenRight QA and `right-ctl`. Replaces `rightkit-computer` (folded in, deprecated) and the generic parts of HeardRight's `heardright_platform` and `heardright-command-host` (moved, not copied: see `docs/MOVED_FROM.md`).

```text
snapshot  ->  stable element ids (AX / UIA), freshness guard
act       ->  kill switch -> allowlist -> lease fence -> freshness -> secure/credential/risk
              -> admission hook -> tier 1 semantic -> 2 in-app -> 3 per-pid events -> 4 foreground (opt-in)
              -> receipt (tier used, gate that refused) -> settle -> lifecycle event
```

| Capability | Where |
|---|---|
| macOS AX tree walk, focus, native write, per-pid events, per-window capture | `src/macos/` (~1.2k lines, zero third-party deps) |
| Windows UIA tree walk, Invoke/Toggle/Select/Expand/Value/Scroll/Text patterns, window messages, SendInput, PrintWindow, clipboard | `src/win/` (1.2k lines; **not compiled or run on Windows yet**) |
| Engine: ids, freshness guard, ladder, receipts | `src/engine.rs` |
| Control lease, epoch fencing, acked input sequence (CodeRight PTY-002) | `src/lease.rs` |
| Admission pre-action hook (CodeRight EFF-001) | `src/admission.rs` |
| Lifecycle events, content-free metrics | `src/events.rs` |
| Allowlist, deny list, kill switch, risk classes, credential gates | `src/policy.rs` |
| Screenshot redaction (secure + credential fields + caller hook), PNG decode | `src/redact.rs`, `src/png.rs` |
| Text delivery with read-back proof (HeardRight rules) | `src/delivery.rs`, `src/proof.rs` |
| Fail-closed app-data root | `src/paths.rs` (via `rightkit-service::Suite`) |

## Safety rules enforced here

* Never types credentials: payloads that look like credentials, and credential-labelled or secure fields, are refused at every tier, including delivery. No override flag.
* Evidence never lands relative to the working directory (`JsonlEvidence::open` and `screenshot` require absolute paths; `paths::evidence_dir()` fails closed).
* Screenshots are redacted before they are returned; if a region must be masked and the file cannot be rewritten the capture is deleted and the call errors.
* Per-app allowlist (`Policy`) is checked before launch when the spec names the app, and again on the launched process.
* Kill switch (`kill()`) fails every act, snapshot and batch step closed.

## Windows

- Status: `src/win/` has not been compiled or run on Windows. `tests/e2e_win.rs` says in its header that it has not been run on Windows by the author.
- Version caveat: `Cargo.toml` declares `windows` 0.61 with 15 features. The handoff says the code uses 0.62 names, and `inventory/windows-0.62.patch` bumps the manifest. I could not confirm the name mismatch from source: `HWND`, `SendInput`, `CF_LOCALE` and `GetAncestor` are identical in windows 0.61.3 and 0.62.2 in the local registry. Apply the staged patch and compile before trusting this section. The patch also moves process, sandbox, qa, service and speech-client to 0.62.
- Tree and patterns: UI Automation via `CoCreateInstance` (in-process, multithreaded COM). Reads Invoke, Toggle, SelectionItem, ExpandCollapse, Value, Scroll and Text patterns (`src/win/uia.rs`).
- Tier 3 (targeted): `PostMessageW` of `WM_CHAR` or key messages to the focused child window. Modifier chords are refused as `Unsupported` and need tier 4.
- Tier 4 (foreground, explicit opt-in): `SendInput` for keys and Unicode text. Clicks move the cursor with `SetCursorPos` and restore it afterwards. Held modifiers are released first. `SendInput` has no window target, so the intended app must already be foreground.
- Activation: `SetForegroundWindow`, with `AttachThreadInput` to the foreground thread as the fallback. Capture: `PrintWindow` with `PW_RENDERFULLCONTENT` (2) into a compatible bitmap. Clipboard: Win32 `DataExchange`, unicode text read and write, with snapshot and restore.
- Launch: `CreateProcessW` suspended with `SW_SHOWNOACTIVATE`, assigned to a kill-on-close job with an optional `ActiveProcessLimit`, then resumed. A window is adopted only if its pid is the started pid, or a new process with the same app id inside the job. If no window appears within 5 s, the call returns an error and the launched tree is torn down.
- Limits: `status()` reports UIPI. A non-elevated process cannot drive an elevated app unless UIAccess signing is used. The code does not check session or desktop state before `SendInput`.
- Dependency: `windows` 0.61 under `cfg(windows)`, with features Foundation, Graphics_Gdi, Storage_FileSystem, Storage_Xps, System_Com, System_DataExchange, System_JobObjects, System_Memory, System_Ole, System_Threading, System_Variant, UI_Accessibility, UI_HiDpi, UI_Input_KeyboardAndMouse and UI_WindowsAndMessaging. Consumers enable no Windows features. Construction sets per-monitor DPI awareness v2.
- Run on a Windows host: `rightkit cargo test -p rightkit-automation --test e2e_win -- --nocapture`. The test header says it has not been run on Windows.

## Tests

* `tests/e2e_mac.rs` — real TextEdit and Calculator, launched in the background, frontmost verified unchanged, everything closed afterwards. `harness = false` (must run on the main thread). Needs Accessibility and Screen Recording for the *test host*; absent permissions FAIL, never skip. A managed-broker run has neither: run the built binary from a granted shell (`BIN=$(ls -t <target>/debug/deps/e2e_mac-* | grep -v '\.d$' | head -1); $BIN`).
* `tests/e2e_win.rs` — real Notepad on Windows. Type-checked on every OS; run it on the Windows host (`rightkit cargo test -p rightkit-automation --test e2e_win -- --nocapture`).
* `tests/engine.rs` — the same engine against a **simulated** desktop (`mock::MockPlatform`). Kept only for states no harmless real app exposes (secure field, recreated editor, lossy clipboard, lying native write).
* `examples/mac_proof.rs`, `examples/win_smoke.rs` — evidence-producing proofs.
