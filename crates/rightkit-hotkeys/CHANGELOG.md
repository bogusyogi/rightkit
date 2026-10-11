# Changelog

## Unreleased

Additive; nothing in this section was compiled or run when written.

- `hook_chord::HookChords`: a pure chord matcher over raw key events (exact modifier match, one `Pressed` and one `Released` per hold, matched key swallowed, injected events ignored), with tests that drive it without real input. Windows `HookHotkeys` binds chords through the `WH_KEYBOARD_LL` hook with it: the opt-in fallback for a chord `RegisterHotKey` refuses with `Conflict` (Alt+Space held by another program).
- Windows `GlobalHotkeys::register_repeating`: a registration without `MOD_NOREPEAT`, for step actions that repeat while held. `register` is unchanged (no repeat).
- Windows `GlobalHotkeys::with_state` delivers `Pressed` and `Released` for hold-to-talk. Release is detected by a short thread timer polling `GetAsyncKeyState`, running only while a chord is held (no global hook). Exactly one `Released` per `Pressed`, including on unregister and drop. `GlobalHotkeys::new` stays press-only.
- Windows `GlobalHotkeys::unregister(&Chord)` and `UnregisterError`.
- `PttAlias`: one per-platform push-to-talk alias table (Windows `lalt`, `ralt`, `rctrl`; macOS `ctrl`, `lalt`, `ralt`) used by `parse_ptt_key_name` and the macOS `PttKey::parse`. `parse_ptt_key_name` now follows the current platform's table (macOS also accepts `ctrl`, `lctrl`; Windows unchanged).
- `RegisterFailure` classifies Win32 and OSStatus registration failures. Windows `RegisterError` gains `Unsupported`, `PermissionDenied`, `ResourceExhausted`; macOS gains `ResourceExhausted`. Both enums are now `#[non_exhaustive]`.
- macOS `GlobalHotkeys::register` also treats a zero status with a null reference as an error.
- New pure `hold` module (`HoldTracker`, `KeyStateSource`); `HotkeyState` now lives there and is re-exported from `macos` and `windows`.

## 0.1.0

- First release. One hotkey foundation for Right Suite apps: chord and key-name parsing and formatting, conflict reporting (`HotkeyConflict`, `ChordRegistry`), pure push-to-talk policy and handoff state machine, plus native primitives (macOS Carbon hotkeys, CGEventTap fn chord and middle-mouse taps, Windows low-level keyboard hook).
- Unit tests cover chord parsing, formatting and conflict detection.
