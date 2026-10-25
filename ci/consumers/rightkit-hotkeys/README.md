# rightkit-hotkeys 0.1.2 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-hotkeys =0.1.2` exactly. The crate has no features, so none are selected; this is the dependency CutRight, HeardRight and Pulse declare. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed assertion. The same source compiles and runs on Windows, macOS and Linux.

## What it proves

These are the pure modules, which behave the same on every platform:

- Chords. `parse_chord` reads modifiers and one key case-insensitively, normalises modifier order and the `cmd`, `option`, `esc` and `return` aliases, and refuses an empty chord, a chord without a key, two keys, an unknown token and a repeated modifier (`ChordError`). Canonical spellings round-trip. `Chord::format` for Mac, Windows and Other. `Key::parse`, the macOS keycodes and Windows virtual keys of a set of keys, and F21 to F24 having no macOS code (`NO_MAC_KEYCODE`).
- Modifiers and platform. `parse_modifier_token` with sides, `Platform::current` for the host, and the push-to-talk alias table (`PttAlias` `supported`, `parse`, `canonical`, `display` and `modifier` for Windows, Mac and Other; `parse_ptt_key_name` on the host).
- Conflicts. `ChordRegistry` refuses a chord bound to another owner whatever its spelling, is idempotent for the same owner and frees a chord on release; `HotkeyConflict` names the failed required shortcut.
- Hold tracking and failures. `HoldTracker` pairs every press with exactly one release through the `KeyStateSource` seam; `RegisterFailure` maps Win32 codes and Carbon `OSStatus` values.
- Push-to-talk policy. `PttHandoff` start, end and completion in every order (a release during start is applied when the start completes, a cancel carries its abort cause), `format_abort`, `should_arm_ptt`, the chord abort window, the hold-suppression and fn-chord predicates, and `run_feedback_before_action` ordering.
- The raw-event matcher. `HookChords` matches a chord with exactly its modifiers, swallows the chord key and never a modifier, reports one `Pressed` per hold and one `Released` on key-up or modifier-up, ignores injected events, refuses a second binding (`AlreadyBound`), and `unbind` and `release_all`. `Registrations` drives a fake OS hotkey table: ids, `MOD_NOREPEAT` flags, conflict and not-registered paths, a refused unregister that keeps its record, and a stale record that is dropped.
- Decoding. `KeyEventDetail::from_raw`, `MouseEvent::from_raw`, `classify_mouse_message` and `is_thread_message`.
- Middle click. `MiddleClickGate`, `handle_other_mouse` and `run_job` keep the lookup off the tap callback (a toggle, a replayed click, a stale lookup, a vanished worker). `ArmTimer` fires once, cancels, re-arms to replace a pending deadline, stops on drop, and clamps its delay.

On Windows and macOS only, the platform types are checked by name at compile time (nothing is constructed), so an unintended removal of an exported type fails the build.

## What it deliberately does not prove

- Any native registration or tap. Carbon hotkeys, `CGEventTap`, `FnHotkeyTap`, `MiddleMouseTap`, `RegisterHotKey`, `WH_KEYBOARD_LL` and the message-loop threads need an interactive desktop session, Accessibility and Input Monitoring grants, or a real keyboard, so they are host-bound and left to the crate's own platform tests.
- Behaviour of the platform types beyond their existence.

State: in-memory values and short-lived timer threads only. No network, no files, no process spawn, no user data, no real input.
