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
| `ptt` | Pure push-to-talk policy and the start and end handoff state machine (`PttPhase`, `PttHandoff`, `PttEnd`). |
| `decision` | Pure predicates shared by the platform hooks: the fn-chord gate and the middle-button check. |
| `macos` | `CGEventTap` primitive, fn-chord tap (`FnHotkeyTap`), middle-mouse tap (`MiddleMouseTap`), and Carbon global hotkeys (`GlobalHotkeys`). |
| `windows` | `WH_KEYBOARD_LL` hook and `RegisterHotKey` registration (`GlobalHotkeys`). |

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
- Other platforms: only the pure modules (`chord`, `conflict`, `ptt`, `decision`) compile.

## Tests

`tests/macos_e2e.rs` checks the Accessibility and Input Monitoring grants. It then drives private synthetic events through the real Carbon and CoreGraphics paths. It never emits audio.

## Version and changes

Version: see [INDEX.md](../../INDEX.md). Changes are recorded in `CHANGELOG.md`.

## Windows

- Two mechanisms, both in `src/windows/mod.rs` and compiled only under `cfg(windows)`:
  - `GlobalHotkeys` runs a thread with a message loop and calls `RegisterHotKey(NULL, id, mods | MOD_NOREPEAT, vk)`. `WM_HOTKEY` is handled on that thread. Ctrl, Alt and Shift map directly, and Meta, Super and Win all map to `MOD_WIN`. Error 1409 becomes `RegisterError::Conflict`. Dropping unregisters every id and joins the thread.
  - `LowLevelKeyboardHook` installs one process-wide `WH_KEYBOARD_LL` hook on its own thread. A second install returns `HookError::AlreadyInstalled`. The handler runs inside the hook procedure and returns `Pass` or `Swallow` (LRESULT 1). `KeyEvent.injected` reports `LLKHF_INJECTED`. The module comment says the handler must stay fast and hand real work to another thread.
- There are no Windows `FnHotkeyTap` or `MiddleMouseTap` types. Those live in `src/macos/` only, so fn, push-to-talk and middle-button support is macOS-only at HEAD.
- Dependency: `windows` 0.61 under `cfg(windows)`, with features `Win32_Foundation`, `Win32_System_LibraryLoader`, `Win32_System_Threading`, `Win32_UI_Input_KeyboardAndMouse` and `Win32_UI_WindowsAndMessaging`. Consumers enable no Windows features.
- Not verified: the module header says it has not been built on the macOS host and must be checked on Windows before an app adopts it. Nothing here was compiled for Windows.
- Pending: `tasks/handoffs/2026-10-08/inventory/hotkeys-windows-sys.patch` migrates the crate to `windows-sys` 0.61.2 and rewrites the hook and hotkey code to match. It is not applied at HEAD.

