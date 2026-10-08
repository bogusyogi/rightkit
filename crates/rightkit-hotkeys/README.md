# rightkit-hotkeys

One hotkey foundation for Right Suite apps: cross-platform shortcut policy with native macOS Carbon hotkeys, CoreGraphics event taps, and a Windows low-level keyboard hook.

- Ordinary chords use `GlobalHotkeys`.
- fn gestures, push-to-talk, and the middle mouse button use `FnHotkeyTap` and `MiddleMouseTap`.
- Every native handle stops and joins on drop.

Catalog capability: `global-hotkeys`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

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

Published version: see [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
