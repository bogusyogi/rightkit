# Changelog

## 0.4.4

Additive; no existing API changed. Code-only: nothing in this section was compiled or run when written, so the CI run is the verification. Requested in Orthic-Labs/rightkit#290 (HeardRight, heardright#275).

- **New: media and system keys.** `send_media_key(MediaKey)` and `MediaKey::{PlayPause, NextTrack, PreviousTrack, VolumeUp, VolumeDown, Mute}` (module `media_keys`, re-exported at the crate root). One press and release, system-wide, on macOS and Windows; other OS returns `Error::Unsupported`. macOS posts an `NSSystemDefined` subtype-8 event (`NX_KEYTYPE_PLAY` 16, `NEXT` 17, `PREVIOUS` 18, `SOUND_UP` 0, `SOUND_DOWN` 1, `MUTE` 7) built with `+[NSEvent otherEventWithType:...]` and posted with `CGEventPost` to the HID tap; Windows sends `VK_MEDIA_PLAY_PAUSE`, `VK_MEDIA_NEXT_TRACK`, `VK_MEDIA_PREV_TRACK`, `VK_VOLUME_UP`, `VK_VOLUME_DOWN`, `VK_VOLUME_MUTE` in one `SendInput` call. Like the raw Windows input path it checks no focus and writes no receipt; it refuses with `Error::Blocked` while the kill switch (`policy::kill`) is set, and the caller's event marker rides on the posted event. `MediaKey::mac_key_type()`, `windows_vk()` and `parse(name)` are public. The code tables, the `data1` layout and the name parser are pure and unit-tested on every OS; the posting itself needs a Mac and a Windows run (play-pause, volume) and is not tested yet. No Stop key: macOS has no aux key for it.

## 0.4.3

Additive; no existing API changed. Code-only: nothing in this section was compiled or run when written, so the CI run is the verification. Requested in Orthic-Labs/rightkit#177 (CutRight, bogusyogi/CutRight#5).

- **New: capture-safety probe.** `probe_capture_safety(&Target) -> CaptureSafetyReport` (module `capture_safety`) says whether a target app or window is showing a secure or sensitive surface, so a recorder can refuse or mask. The verdict is tri-state: `CaptureSafety::Safe`, `Unsafe(Vec<SafetyReason>)` or `Unknown(Vec<SafetyGap>)`. `Safe` is returned only when every check this OS defines ran and found nothing; a missing permission, a window that is gone, an unreadable or opaque focus, or an OS without a probe gives `Unknown`, never `Safe`. A finding gives `Unsafe` even when other checks failed. `report.evidence` lists each check and its result. Treat `Unknown` like `Unsafe` when you must not capture. `Safe` is not a guarantee: secrets rendered in ordinary views are invisible to these checks.
  - macOS evidence: `IsSecureEventInputEnabled` (system-wide, so it counts against every target); the target app's AX focused element, descended to the deepest focused element, with role or subrole `AXSecureTextField` (needs Accessibility; a web area or group whose inner focus is not exposed is `FocusOpaque`, a gap); `kCGWindowSharingState == 0` from `CGWindowListCopyWindowInfo` (a named window is checked wherever it is, an app target checks its on-screen windows).
  - Windows evidence: `GetWindowDisplayAffinity` of the named window, or of the app's visible top-level windows, `WDA_MONITOR` (0x1) or `WDA_EXCLUDEFROMCAPTURE` (0x11); the focused control is a password field by `ES_PASSWORD` on a focused `Edit`/`RichEdit` control, or by UI Automation `IsPassword`.
  - Other OS: always `Unknown(UnsupportedPlatform)`.
  - The classification (`classify`) and the per-OS value rules are pure and unit-tested on every OS (`src/capture_safety.rs`). The OS reads are not exercised by any test yet.
- **New: AX attribute query path that bypasses the interaction switches (macOS, Orthic-Labs/rightkit#244).** `macos::ax::El::attribute(name) -> Result<AttrValue>`, `attribute_names()` and `attributes(&[..])` read an attribute with its full type and its own error. `AttrValue` (`#[non_exhaustive]`) is `NoValue`, `Str`, `Bool`, `Int`, `Float`, `Point`, `Size`, `Rect`, `Range`, `Element`, `List` or `Other(cf_type_id)`; a missing attribute is `Err(Unsupported)`, no Accessibility grant is `Err(PermissionDenied)`, a hung app is `Err(Platform(-25204, ..))`. Why this is allowed while interaction is disabled: the kill switch (`policy::kill`), `Policy`, the allowlist, admission and leases guard effects (input, writes, actions, focus changes). The query is `AXUIElementCopyAttributeValue` or `...CopyAttributeNames` only: it changes nothing in the target and posts no event. It still needs the Accessibility grant and the element's messaging timeout applies. The older `str_attr`, `bool_attr`, `f64_attr`, `el_attr`, `list`, `frame` and `pid` are also ungated but return `None` for every failure. Pure parts (`AXValue` type codes, `kAXErrorNoValue` against other errors) are unit-tested in `ax.rs`; they and the conversion need a Mac run.
- Confirmed from #188 (names unchanged): `El::set_point(x, y)`, `El::set_size(w, h)`, `El::set_frame(Rect)`, and the caller marker `marker::set_event_marker`, `event_marker`, `is_own_event`, `EVENT_SOURCE_USER_DATA_FIELD` (42).
- Internal: FFI adds `AXUIElementCopyAttributeNames`, `AXUIElementGetTypeID`, `AXValueGetTypeID`, `AXValueGetType` and `CFNumberIsFloatType`.
- Internal: `win::sys::all_top_level` is `pub(crate)`; `win::uia::El::password_state` returns `Option<bool>`; `IsSecureEventInputEnabled` is declared in `macos::ffi` (links Carbon).

## 0.4.2

Code-only change; nothing in this section was compiled or run when written.

Added (Orthic-Labs/rightkit#188):

- **AX write of point and size (macOS).** `macos::ax::El::set_point(x, y)` writes `AXPosition`, `El::set_size(w, h)` writes `AXSize`, and `El::set_frame(Rect)` writes position then size (window move, maximise, restore). They build the `AXValue` with `AXValueCreate`, need the Accessibility grant (`PermissionDenied` otherwise) and map an unsupported attribute to `Unsupported`. The app may clamp the result; read `El::frame` back to confirm.
- **Window frame write (Windows).** `win::set_window_frame(window_id, Rect)` moves and resizes a top-level window (`SetWindowPos`, no activation, no z-order change), in physical pixels.
- **Caller marker on posted events.** New `marker` module: `set_event_marker(i64)`, `event_marker()`, `is_own_event(i64)` and `EVENT_SOURCE_USER_DATA_FIELD` (42). A non-zero marker is stamped on every event the crate posts: macOS `kCGEventSourceUserData` on the engine's tier 3 and tier 4 events (key chords, text, clicks, scroll) and on every `macos::background` event; Windows `dwExtraInfo` of the engine's tier 4 `SendInput` events. A caller's own `CGEventTap` reads field 42 and compares with `is_own_event`. The default is `0` (no marker), which posts exactly as before.

## 0.4.1

Added `macos::ax::El::retain_raw`, `retain_raw_with` and `from_raw` (unsafe), and `El::as_raw`, to wrap an `AXUIElementRef` the caller already holds (for example a pinned focus element) so `set_str` and the other `El` methods work on it. `retain_raw` retains (caller keeps its reference); `from_raw` takes ownership of a +1 reference; `retain_raw_with` also applies an `AxOptions` timeout. Each returns `None` for null. macOS only; not compiled locally.

Added `Policy::deny(app_id)`. A denied id is refused by `Policy::check` before the allow list, so it is refused even when it is also allowed. Deny is case-insensitive and returns `Error::Blocked` (`DeniedBy::Policy`).

## 0.4.0

Breaking (pre-1.0 minor). The generic input code that lived in `rightkit-qa-driver` moved here, so production no longer reaches QA code. Tier 2 is now a trait only.

Removed:

- **`in_app::ServiceBridge`, the `in-app` feature and the `serde_json` dependency.** The client of the QA control server is gone. `in_app::InApp` stays and is now documented as the whole tier-2 contract (what `handles` and `perform` must do, which `Error` kinds mean what, and that policy is the engine's job, not the channel's). The channel that used to be `ServiceBridge` is implemented outside this crate (planned: `rightkit-qa`, behind its `control` feature). Migrate: implement `InApp` and call `Automation::add_in_app`.
- **`in-app` feature renamed `paths`.** It now gates only the `paths` module and pulls only `rightkit-daemon`.

Added (moved from `rightkit-qa-driver` 0.4.0, behaviour kept except where listed under "Stricter"):

- **`control`**: `control::admission` (EFF-001 `EffectGate`, `AdmissionHook`, `EffectRequest`, `Settlement`, `CancelToken`), `control::lease` (PTY-002 `InputLease`, `ControlSession`), `control::lease_store` (`LeaseStore`, `MemoryLeaseStore`, atomic `FileLeaseStore`, execution identity and state) and `control::events` (`ControlEvent`, `EventSink`). These are separate from the engine's typed `admission`, `lease` and `events` modules on purpose: different model (a single lease object with durable state and an unacknowledged range, against the engine's per-pid in-memory registry).
- **`control::policy_hook::PolicyAdmission`**: a `Policy` as an `AdmissionHook`. Deny by default; the target must be `pid:<n>` resolving (through the caller's resolver) to an allowed app; launches are checked by executable name; the built-in deny list and host-process protection apply.
- **`macos::background`** (macOS): per-pid CGEvent input, AX reads (`ax_dump`, `ax_find`, `ax_text`), window listing, the frontmost-app reader, hidden launch and window capture. Effects go through `Gated` (EFF-001) or the named `unsafe_ungated` opt-out.
- **`offscreen`**: `launch_offscreen`, `launch_offscreen_gated`, `start_native`, qualification and the session report. The Tauri window helpers (`start_for_window`, `start_from_env`) stay in `rightkit-qa-driver` because they need `tauri`. Cargo feature `offscreen-private-spi` moved with it.
- **`keys::appkit`**: raw AppKit keycodes and `CGEvent` flag bits (`named`, `chord`, `function_char`, `named_char`, `ansi_char`, `FLAG_*`). Every keycode is derived from `keys::mac_keycode`, so there is one table. Its name grammar differs from `parse_chord` by design: `delete` is the macOS Delete key (backspace), punctuation has names, `cmd++` is cmd+shift+`=`.
- **`json`**: `str_field`, `num_field`, `base64_encode` (flat extraction for the WebDriver wire and screenshot replies). `esc` was identical in both crates.

Stricter than either crate before (the kill switch and allowlist now reach everything that moved in):

- `policy::kill()` denies every effect admitted through an `EffectGate` (stage `Policy`, reason `kill switch engaged`, checked before the hook and again before execute), makes `launch_offscreen` return the new `OffscreenError::KillSwitch`, and turns every `macos::background::unsafe_ungated` function into a no-op that reports failure.
- `Gated` has no `Default`. Building one means passing a gate or calling `Gated::allow_all()`, a named opt-out of the allowlist.

Tests moved with the code: admission, lease and event tests (`tests/control_plane.rs`), the durable lease store (`tests/durable_lease.rs`), the macOS frontmost reader (`tests/frontmost_macos.rs`), and the unit tests of `json`, `keys::appkit`, `offscreen` geometry and `control`. New: `PolicyAdmission`, key-table equivalence against the retired `rightkit-qa-driver` table, the kill switch across the control plane, and `InApp` through a fake channel in `tests/engine.rs`. All use temp directories and fake targets.

## 0.3.0

First published release (0.1.x and 0.2.x were internal to the repository).

Additive. No existing signature or default changes. Gap ledger #63 (macOS AX) and #64 (Windows raw input).

- **macOS: public `macos::ax` surface.** `ax::system_wide()` (`AXUIElementCreateSystemWide`) and `ax::element_at_position(x, y)` (`AXUIElementCopyElementAtPosition`, global top-left display points, `Result<Option<El>>`: `None` when nothing accessible is there, an error for a hung app, missing permission or any other AX failure). `ax::system_wide_with(&AxOptions)`, `ax::element_at_position_with(x, y, &AxOptions)`, `El::app_with(pid, &AxOptions)` and `El::set_timeout(Duration)` take a per-call `AxOptions { timeout: Duration }` (`AXUIElementSetMessagingTimeout`). The default stays 1.5 s (`ax::AX_TIMEOUT`, `ax::DEFAULT_TIMEOUT`); `El::app(pid)` is unchanged. The module was private; only the new entry points and `El` are public, the other `ax` helpers stay crate-internal.
  - Behaviour to know: Apple makes a timeout set on the system-wide element the process-wide default for AX elements without their own timeout, and AX has no getter to restore the old value. Every system-wide entry point therefore sets that default (to 1.5 s, or to the value passed). Documented on `AxOptions`.
- **Windows: raw input** (`win::input`, re-exported from `win`). `send_input_batch(&[RawInput])` builds one `SendInput` call from typed items: `RawInput::Key` (any virtual key including the unassigned `0xE8`, `wScan`, the extended-key bit, `KEYEVENTF_SCANCODE`), `Unicode` units and `Mouse` events (relative and absolute move, five buttons, both wheels). `SendOptions { extra_info }` with `send_input_batch_with` stamps `dwExtraInfo` on every event so a host's own low-level hook can recognise its injection. The extended bit is set automatically for the E0 keys (`is_extended_vk`: right Ctrl and Alt, Windows and Apps keys, navigation cluster, PrintScreen, NumLock, numpad divide) and can be overridden with `RawInput::with_extended`. `RawInput::menu_mask()` is the `0xE8` press/release pair that keeps a following Alt or Win release from opening the menu bar or Start menu.
- **Windows: `ModifierSnapshot`.** `capture()` (or `capture_with` a caller's key-state probe, `from_held`) records the specific left and right Shift, Ctrl, Alt and Win keys that are down; `restore()` / `restore_with(&SendOptions)` re-press them, and `release_inputs()` / `restore_inputs()` return the events for splicing into the same atomic batch. The existing engine `foreground` path still releases held modifiers without restoring them; this crate does not change that.
- **Windows: `WinPlatform::new_minimal()`.** Performs no DPI-awareness call and no COM initialisation. `WinPlatform::new()` keeps both, and its doc now says exactly what they are (process-wide per-monitor-v2 DPI awareness, MTA on the calling thread). UIA methods still join the MTA lazily on their calling thread; input injection, window listing, capture and clipboard never do.
- Tests: Windows unit tests build the `INPUT` array and check type, `wVk`, `wScan`, `dwFlags`, `mouseData` and `dwExtraInfo` without injecting. macOS tests cover the timeout conversion and default, the hit-test error mapping, the untrusted-process refusal, and a live system-wide and hit-test case that skips when Accessibility is not granted.

## 0.2.0

- **`in-app` feature (off by default).** `rightkit-daemon` and `serde_json` are now optional behind `in-app`. It gates `in_app::ServiceBridge` and the `paths` module (`evidence_dir`, `per_run_evidence_dir`). The `in_app::InApp` trait and `Automation::add_in_app` stay available without it, so a caller can supply its own channel. Consumers of `ServiceBridge` or `paths` add `features = ["in-app"]`; no workspace crate used either (`rightkit-agent-tools` uses neither).
- Fix the stale "zero third-party dependencies" manifest comment.
