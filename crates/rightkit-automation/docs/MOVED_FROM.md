# What moved from HeardRight (`7775b80d`), and what did not

HeardRight's `heardright_platform` (8,040 lines) and `heardright-command-host` (7,592 lines) are frozen at `7775b80d` until HeardRight consumes this crate. After that HeardRight **deletes** its copies; there are no parallel edits. Nothing here is a divergent fork: the crate is the maintained home of the generic behaviour, re-expressed behind one `Platform` trait with the same rules and edge cases.

| HeardRight source | Disposition |
|---|---|
| `platform/src/macos/{ffi,window_query,frontmost,keyboard,clipboard}.rs` | Moved: `src/macos/{ffi,appkit,mod,events}.rs` (NSWorkspace, windows, frontmost, per-pid CGEvent, clipboard transaction). |
| `platform/src/macos/{observation,focus/*}.rs`, `macos_focus_continuity.rs` | Split. Generic AX read / pin / native write / proof: `src/macos/ax.rs`, `delivery.rs`, `proof.rs`. **Not moved (product)**: focus observer leases, continuity tokens, polish/context harvest (`focus/context.rs`, `focus/polish.rs`, `harvest.rs`). Rules 1, 3, 4, 5 carried (see LOCKED_RULES.md). |
| `platform/src/windows.rs` (1,072) and `windows_focus_continuity.rs` (470) | Focus/continuity only in HeardRight. The UIA tree walk, Invoke/Value/Toggle/Select/Expand/Scroll patterns and TextPattern proof are **new work** in `src/win/uia.rs` (365 lines) with Win32 plumbing in `src/win/sys.rs` (560) and `src/win/mod.rs` (126). HeardRight's continuity tokens stay product. |
| `command-host/src/macos_input.rs`, `command_dispatch/{keys,mouse}.rs` | Tiers 3 and 4: per-pid and foreground key, click, scroll events: `src/keys.rs` (chords), `src/macos/events.rs`, `src/win/sys.rs`. Windows `release_held_modifiers` hygiene ported (`held_modifier_keyups`). **Not ported**: the experimental Alt nullifier modes (`ChordNullifier::{ScanCodeE8,VirtualKeyE8}`, `send_chord_with_generic_alt_release`) and shortcut telemetry; spoken-word to chord parsing is product. |
| `command_dispatch/screenshot.rs` (792) | Per-window capture: `src/macos/capture.rs` (CGWindowListCreateImage, `screencapture -l` fallback) and `src/win/sys.rs::capture` (PrintWindow). **Not ported**: full-screen capture to clipboard / Pictures folder, exclusion of HeardRight's own pill window (product). |
| `command-host/src/app_launch.rs` (686) | Launch by bundle id / exe path without activation, new instance only, allowlist pre-check: `Automation::launch`. **Not ported**: spoken-name resolution ("open figma") and Start-menu alias scan (product). |
| `command-host/src/{focus,delivery/*}.rs` | Delivery with pin, read-back proof, tier escalation, clipboard transaction: `src/delivery.rs`, `src/proof.rs`. Dictation delivery policy and context binding stay in HeardRight. |
| `command-host/src/{command_classify,command_dispatch/{power,transforms,entry}}.rs` | Product (voice commands, power, text transforms). Not moved. |
| `command_host app_data_root()` (cwd fallback when env vars are missing) | **Fixed, not moved**: `paths::evidence_dir()` fails closed; evidence and screenshot paths must be absolute. |

Known gaps against HeardRight, all generic and all candidates for follow-up: AXObserver-based focus/settle notifications; screenshot-excluding-an-app; a single persistent UIA MTA worker; the Alt-nullifier input modes; the web-area ancestor walk (rule 2); Windows `menu_press`.

## Moved in from `rightkit-qa-driver` 0.4.0 (automation 0.4.0)

Same rule as above: the code has one maintained home, here, and `rightkit-qa-driver` re-exports it under its old paths instead of keeping a copy.

| `rightkit-qa-driver` source | Now |
|---|---|
| `src/admission.rs`, `src/events.rs`, `src/lease.rs`, `src/lease_store.rs` | `src/control/{admission,events,lease,lease_store}.rs` (kill switch added to the gate). Not merged with the engine's `admission`, `lease`, `events`: a different model. |
| `src/mac.rs` | `src/macos/background.rs` (kill switch added to `unsafe_ungated`; `Gated` lost its `Default`). |
| `src/offscreen.rs`, `offscreen_mac.rs`, `offscreen_win.rs` | `src/offscreen.rs`, `offscreen_mac.rs`, `offscreen_win.rs`, minus the two Tauri helpers (they need `tauri` and stay in `rightkit-qa-driver`). `launch_offscreen` obeys the kill switch. |
| `src/keys.rs` | `src/keys.rs::appkit`, with the keycodes derived from `keys::mac_keycode` (no second table). |
| `src/json.rs` | `src/json.rs` (`str_field`, `num_field`, `base64_encode` added; `esc` was identical). |
| `tests/control_plane.rs`, `tests/durable_lease.rs`, `tests/frontmost_macos.rs` | the same file names under `tests/`. |
| `src/embedded/*`, `src/webdriver.rs`, `src/cli.rs`, `tests/{gated_paths,offscreen_macos,offscreen_windows,serve_auth}.rs`, the control-test-app fixture | **Not moved.** The embedded server goes to `rightkit-qa` behind `control` in a later step; the WebDriver bridge and CLI wait for that decision. |
| `src/in_app.rs` `ServiceBridge` (automation's own client of that server) | **Deleted.** `in_app::InApp` is the contract; the implementation will live next to the server. |

Known gaps against `rightkit-qa-driver`, left on purpose: `macos::background` still declares its own CoreFoundation, CoreGraphics and AX externs next to `macos::{ffi,ax,events}` (macOS code cannot be compiled or compared on the Windows host that did this move); the raw `Gated` primitives are not re-expressed over `Platform`; the engine's typed lease and the PTY-002 `InputLease` are two implementations of one idea.
