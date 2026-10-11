# rightkit-hotkeys

One hotkey foundation for Right Suite apps: cross-platform shortcut policy with native macOS Carbon hotkeys, CoreGraphics event taps, and a Windows low-level keyboard hook.

- Ordinary chords use `GlobalHotkeys`.
- fn gestures, push-to-talk, and the middle mouse button use `FnHotkeyTap` and `MiddleMouseTap`.
- Every native handle stops and joins on drop.

Catalog capability: `global-hotkeys`. Release versions are tracked in [INDEX.md](../../INDEX.md) and `CHANGELOG.md`.

## What it owns

| Module | Owns |
|---|---|
| `chord` | Parsing and formatting of chords, and single-key push-to-talk aliases (`parse_chord`, `Chord`, `Key`, `Modifier`, `Side`). Display is per platform. |
| `conflict` | Conflict reports for registration failures (`HotkeyConflict`) and an in-process `ChordRegistry`. |
| `hold` | Pure press/release pairing (`HoldTracker`, `KeyStateSource`, `HotkeyState`) for APIs that only report a press. |
| `register_status` | Typed classification of OS registration failures (`RegisterFailure`). |
| `ptt` | Pure push-to-talk policy and the start and end handoff state machine (`PttPhase`, `PttHandoff`, `PttEnd`). |
| `decision` | Pure predicates shared by the platform hooks: the fn-chord gate and the middle-button check. |
| `macos` | `CGEventTap` primitive, fn-chord tap (`FnHotkeyTap`), middle-mouse tap (`MiddleMouseTap`), and Carbon global hotkeys (`GlobalHotkeys`). |
| `windows` | `WH_KEYBOARD_LL` hook and `RegisterHotKey` registration (`GlobalHotkeys`); `HookHotkeys` for chords the system refuses. |
| `hook_chord` | Pure chord matcher over raw key events (`HookChords`), used by `HookHotkeys`. |

The pure modules compile and test on every platform. Platform modules are `cfg`-gated.

## When to use it

- Use it to register global shortcuts, or push-to-talk keys, in a Right Suite app.
- Use it when conflicts with other registrations must be reported.

Do not use it for in-window shortcuts. Those belong to the UI framework.

## Cargo features

None. The crate has no `[features]` table. The Windows-only dependencies sit under `cfg(windows)`. The `macos_e2e` test is declared as a `[[test]]` target.

## Usage

Parsing is portable. Taken from `src/chord.rs`:

```rust
use rightkit_hotkeys::chord::parse_chord;

let chord = parse_chord("cmd+shift+space")?;
```

Registration and taps are platform-specific. See `src/macos` and `src/windows` for their constructors.

## Platform support

- macOS: Carbon hotkeys, and CoreGraphics event taps. Event taps need Accessibility and Input Monitoring permissions.
- Windows: `RegisterHotKey` for ordinary chords, and a `WH_KEYBOARD_LL` low-level hook.
- Other platforms: only the pure modules (`chord`, `conflict`, `decision`, `hold`, `hook_chord`, `ptt`, `register_status`, `registration`) compile.

## Tests

`tests/macos_e2e.rs` checks the Accessibility and Input Monitoring grants. It then drives private synthetic events through the real Carbon and CoreGraphics paths.
## Version and changes

Version: see [INDEX.md](../../INDEX.md). Changes are recorded in `CHANGELOG.md`.

## Windows

- Two mechanisms, both in `src/windows/mod.rs` and compiled only under `cfg(windows)`:
  - `GlobalHotkeys` runs a thread with a message loop and calls `RegisterHotKey(NULL, id, mods | MOD_NOREPEAT, vk)` for `register`; `register_repeating` omits `MOD_NOREPEAT` (`src/registration.rs:43-44`). `WM_HOTKEY` is handled on that thread. Ctrl, Alt and Shift map directly, and Meta, Super and Win all map to `MOD_WIN`. Registration errors are typed: 1409 `Conflict`, 87 and 1004 `Unsupported`, 5 `PermissionDenied`, 8, 14 and 1450 `ResourceExhausted`, anything else `Os`. `unregister(&Chord)` removes one chord; dropping unregisters every id and joins the thread.
  - Hold-to-talk: `GlobalHotkeys::with_state` delivers `Pressed` and `Released`. `RegisterHotKey` has no release event, so after `WM_HOTKEY` the loop thread starts a roughly 15 ms thread timer that polls `GetAsyncKeyState` for the chord's key and modifiers, and kills it when nothing is held. No global hook is installed. Exactly one `Released` per `Pressed`, also on unregister and drop. Limits: release latency is 15 to 30 ms; a released modifier ends the hold; a focus change alone does not; a lock screen or UAC desktop switch makes Windows report all keys up, so the hold ends at the next poll.
  - `LowLevelKeyboardHook` installs one process-wide `WH_KEYBOARD_LL` hook on its own thread. A second install returns `HookError::AlreadyInstalled`. The handler runs inside the hook procedure and returns `Pass` or `Swallow` (LRESULT 1). `KeyEvent.injected` reports `LLKHF_INJECTED`. The module comment says the handler must stay fast and hand real work to another thread.
- Push-to-talk aliases come from one table, `PttAlias`: Windows `lalt`, `ralt`, `rctrl`; macOS `ctrl` (also `lctrl`, `rctrl`, `control`), `lalt`, `ralt`. Canonical and display spellings parse back to the same alias.
- There are no Windows `FnHotkeyTap` or `MiddleMouseTap` types. Those live in `src/macos/` only, so fn, push-to-talk and middle-button support is macOS-only at HEAD.
- Dependency: `windows-sys` 0.61.2 under `cfg(windows)` (`Cargo.toml:17`), with features `Win32_Foundation`, `Win32_System_LibraryLoader`, `Win32_System_Threading`, `Win32_UI_Input_KeyboardAndMouse` and `Win32_UI_WindowsAndMessaging`. Consumers enable no Windows features.
- Not verified: the module header says it has not been built on the macOS host and must be checked on Windows before an app adopts it. Nothing here was compiled for Windows.

