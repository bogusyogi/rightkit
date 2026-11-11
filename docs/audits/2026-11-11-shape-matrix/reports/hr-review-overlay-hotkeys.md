# HeardRight vs RightKit: overlay, overlay-win, hotkeys

Read-only comparison. Nothing was compiled or run; every statement is from reading source. "Verified" means I read the code on both sides. "Suspicion" means inferred, not confirmed.

Trees (abbreviated below):

- LIVE = `<scratch>/survey/heardright-live/tauri-app-next` (0.1.213, 12b7d8ce)
- MAIN = `<scratch>/survey/heardright-main/tauri-app-next` (0.1.231)
- RK = `/home/user/rightkit`

Version caveat that applies to all three packages: MAIN pins the published versions (hotkeys =0.1.2, overlay =0.2.0, overlay-win =0.1.0; `MAIN/src-tauri/Cargo.toml:87,139,141`). The RK tree is ahead and unpublished (hotkeys 0.1.3, overlay 0.2.2, overlay-win 0.1.4; `RK/INDEX.md:140,171,172`). I read the RK tree, not the published tarballs. Where a finding depends on something only in the unpublished tree, it says so.

## Verdicts

| Package | Verdict |
|---|---|
| `rightkit-hotkeys` | Right shape with listed gaps |
| `rightkit-overlay` | Right shape with listed gaps, but only for the small slice it covers (Windows window styles, clamp, hover gate). It is not a home for most of the files the inventory assigns to it. |
| `rightkit-overlay-win` | Right shape with listed gaps, for the Windows pill on MAIN. It has no counterpart in the LIVE build, which used a WebView pill on Windows and a Swift pill on Mac. |

---

## 1. rightkit-hotkeys

### 1.1 What LIVE does

Three separate mechanisms.

**Fixed global chords, Mac and Windows** (`LIVE/src-tauri/src/hotkey.rs`, `hotkey_setup.rs`). Uses `tauri-plugin-global-shortcut` (`hotkey.rs:24`, `LIVE/src-tauri/Cargo.toml:63`).

- Ctrl+Space: toggle on press; a release after 300 ms or more stops (hold-to-talk) (`hotkey.rs:26,112-145`).
- Ctrl+Shift+Space and Ctrl+Alt+Space: stop and send; the Alt form is optional and only logs if it cannot register (`hotkey.rs:147-184`).
- Ctrl+,: open hub (`hotkey.rs:186-205`).
- Per-chord debounce: 40 ms toggle, 100 ms send (`hotkey.rs:34-35,81-91`).
- Rollback of earlier chords when a later one fails; error text `register <chord> failed: <reason>` (`hotkey.rs:53-63,145,162,204`).
- Surface: `register_shortcuts`, `unregister_shortcuts`, and in `hotkey_setup.rs` `register_hotkeys_if_needed`, `publish_hotkey_conflict`, `get_hotkey_conflict` (Tauri command), `hotkey_conflict_snapshot`.

**macOS fn chords and push-to-talk** (`LIVE/src-tauri/src/fn_hotkey.rs`, `fn_hotkey/{tap,state,ffi}.rs`). One `CGEventTap` at the HID level on thread `hr-fn-tap` (`tap.rs:330-383`).

- fn+Space toggle, fn+Shift+Space stop and send, fn+Escape cancel; both key edges swallowed (`tap.rs:301-324`).
- A chord is accepted only if the tap itself saw fn go down first (`tap.rs:278-299`).
- Push-to-talk on a lone Control, Left Option or Right Option: 180 ms arm delay, release stops, fn during the hold sends, another key or modifier within the abort window cancels (`tap.rs:82-206,221-263`; `state.rs:59`).
- Hold suppression when dictation ends while the key is still down (`state.rs:134-148`).
- Re-enables the tap when macOS disables it and logs `fn_hotkey_tap_reenabled` (`tap.rs:49-58`).
- Process-wide statics. Surface: `spawn`, `set_enabled`, `set_ptt_enabled`, `set_ptt_key`, `set_dictation_active`, `set_ptt_callbacks` (`fn_hotkey.rs:30-34`).
- Callers: `LIVE/src-tauri/src/dictation_lifecycle.rs:111,196,251-252,302-313,562`; `commands/settings.rs:121,133`.

**Related code the inventory files elsewhere but which is hotkey logic:**

- `LIVE/heardright_core/src/ptt_policy.rs` (309 lines, inventory target `rightkit-automation`): the pure PTT policy and `PttHandoff`.
- `LIVE/heardright_core/src/hotkey_conflict.rs` (27 lines, target `rightkit-automation`).
- `LIVE/src-tauri/src/middle_mouse.rs` (260 lines, target `rightkit-automation`): macOS middle-click tap with AX link pass-through.
- `LIVE/src-tauri/src/recording_cancel_hook/` (target `rightkit-dictation`): Windows `WH_KEYBOARD_LL` hook for Ctrl+Escape and Windows push-to-talk.

### 1.2 What RightKit provides

`RK/crates/rightkit-hotkeys/src/` (`lib.rs:16-43`).

- Pure, all platforms: `chord` (parse and format, `PttAlias`), `conflict` (`HotkeyConflict`, `ChordRegistry`), `hold` (`HoldTracker`), `ptt` (policy and `PttHandoff`), `ptt_timer` (`ArmTimer`), `decision`, `middle_click`, `hook_chord`, `hook_detail`, `register_status`, `registration`.
- macOS: `GlobalHotkeys` (Carbon `RegisterEventHotKey`, pressed and released, main run loop; `macos/global.rs:137-229`), `EventTap` (`macos/tap.rs`), `FnHotkeyTap` (`macos/fn_hotkey.rs:472-543`), `MiddleMouseTap` and `ax_link_at` (`macos/middle_mouse.rs`).
- Windows: `GlobalHotkeys::new` / `with_state` (`RegisterHotKey` plus a release poll), `LowLevelKeyboardHook`, `LowLevelHooks` (keyboard, mouse, thread messages), `HookHotkeys` (`windows/mod.rs:263-875`).
- No Windows push-to-talk tap or Windows middle-mouse type (stated in `RK/crates/rightkit-hotkeys/README.md`, Windows section).

### 1.3 Behaviour differences

Verified matches first, because they carry most of the weight:

- `ptt.rs` is the LIVE `ptt_policy.rs` with only formatting changes and a `Default` impl. I diffed the two files (whitespace-normalised); no logic line differs.
- `FnHotkeyTap`'s state machine follows LIVE branch for branch: fn edge and send-pending (`fn_hotkey.rs:321-327` vs `tap.rs:72-80`), selected-key edge and Option side tracking (`:329-334` vs `:83-90`), suppression (`:340-346` vs `:97-106`), arm (`:347-354` vs `:107-146`), release (`:355-366` vs `:147-172`), modifier-joined abort (`:367-389` vs `:173-206`), key-down abort (`:404-429` vs `:224-263`), chord gate and swallow (`:431-469` vs `:265-324`).
- Tap placement is the same: HID, head insert, default option, same event mask (`macos/tap.rs:272-290` vs `LIVE tap.rs:343-353`).
- Arm delay 180 ms on both (`fn_hotkey.rs:32`; `LIVE state.rs:59`).

Differences:

| # | Difference | LIVE | RightKit | Status |
|---|---|---|---|---|
| H1 | PTT usage telemetry. LIVE records start (key name) and stop (source, action, key, hold ms, whether fn was seen) from inside the tap. `FnHotkeyCallbacks` has no hook that carries hold duration, stop reason or key. | `fn_hotkey/tap.rs:133,154-160,189-195,243-249` | `macos/fn_hotkey.rs:78-90` (only `ptt_start/stop/stop_send/cancel` and a string `diag`) | Verified. Note MAIN removed `telemetry_upload` altogether (0 references in MAIN, 128 in LIVE), so on MAIN this is part of a wider removal, not something the package alone caused. |
| H2 | fn+Escape accepted log `cancel_input_candidate ... disposition=accepted` | `tap.rs:305-315` | none (`fn_hotkey.rs:460-461`) | Verified |
| H3 | Tap re-enable log `fn_hotkey_tap_reenabled`. `EventTap` counts re-enables, but `FnHotkeyTap` uses plain `EventTap::spawn`, keeps the tap private and exposes neither the count nor a hook. | `tap.rs:49-58` | `macos/tap.rs:137-148`, `macos/fn_hotkey.rs:474,492` | Verified |
| H4 | `ptt_hold_suppressed reason=dictation_ended_while_ctrl_down` log | `state.rs:143-146` | none (`fn_hotkey.rs:531-542`) | Verified |
| H5 | Diagnostics run synchronously inside the event-tap callback. LIVE moved `ptt_chord_ignored_late` and `fn_input_candidate` onto spawned threads; RightKit calls `diag` inline, and MAIN passes `telemetry::log_message` as `diag`. | `tap.rs:199-204,253-260,289-296` | `fn_hotkey.rs:343,384-387,424-427,450-455`; `MAIN/src-tauri/src/fn_hotkey.rs:56` | Difference verified. Whether it is slow enough to get the tap disabled is a suspicion. |
| H6 | Rejection log when the fn chord path is disabled: LIVE returns silently; RightKit logs a rejection on every fn+Space or fn+Escape key-down while disabled. Field layout also differs (LIVE has `reason=cached_fn_up`). | `tap.rs:273-276,293` | `fn_hotkey.rs:438-457` | Verified. Affects log volume and any log parser, not behaviour. |
| H7 | `HotkeyConflict` is not `Serialize` and takes the required list and message as arguments. LIVE's type derives `Serialize`, is emitted to the renderer and returned from a Tauri command. | `LIVE/heardright_core/src/hotkey_conflict.rs:3-27`; `hotkey_setup.rs:21,31` | `conflict.rs:13-34` | Verified. HeardRight needs a wrapper or a serde derive to adopt it. |
| H8 | Windows hold release. RightKit ends a hold when the key or any required modifier goes up (`hold.rs:31-37`), with 15 to 30 ms poll latency. | plugin behaviour | `hold.rs`, README Windows section | RightKit side verified. I did not read the plugin's source, so whether LIVE differs is not verified. |
| H9 | Global chord backend changed from the Tauri plugin to RightKit Carbon / `RegisterHotKey`. | `hotkey.rs:24,105` | `macos/global.rs`, `windows/mod.rs:593-790` | Verified that the app-level semantics (debounce, 300 ms threshold, optional Alt chord, rollback, error text) are kept in `MAIN/src-tauri/src/hotkey.rs:30-39,191-269`. OS-level equivalence with the plugin not verified. |
| H10 | PTT start callback thread. LIVE spawns a thread per press; RightKit runs `ptt_start` on the single arm-timer thread. | `tap.rs:120-145` | `fn_hotkey.rs:188-211` | Verified. Deliberate (changelog 0.1.2, rightkit#180). No functional loss seen. |
| H11 | Middle click. LIVE runs the Accessibility link lookup inside the tap callback; RightKit swallows at once, looks up on a worker and replays the click if it was a link. | `LIVE/src-tauri/src/middle_mouse.rs:173-210` | `macos/middle_mouse.rs:116-180` | Verified. A gain (rightkit#181: the blocking AX call got the tap disabled). |
| H12 | PTT key names. LIVE accepts `ctrl`, `lalt`, `ralt`; RightKit accepts those plus aliases. | `state.rs:97-109` | `fn_hotkey.rs:44-51` | Verified. Superset. |

### 1.4 Does MAIN call the package?

Yes, for nearly everything on macOS and for the global chords on Windows. MAIN no longer has `fn_hotkey/{tap,state,ffi}.rs`, `ptt_policy.rs` or its own middle-mouse tap.

- `MAIN/src-tauri/src/fn_hotkey.rs:9,58` (`FnHotkeyTap::spawn(TapLocation::Hid, ...)`); the file is a 102-line facade.
- `MAIN/src-tauri/src/hotkey.rs:23-24,68,88` (`parse_chord`, `HotkeyState`, `windows::GlobalHotkeys::with_state`, `macos::GlobalHotkeys::new`). No `tauri-plugin-global-shortcut` dependency remains; a stale comment still names it (`MAIN/src-tauri/src/app_run.rs:487`).
- `MAIN/src-tauri/src/middle_mouse.rs:8,34` (`MiddleMouseTap::spawn`, `ax_link_at`).
- `MAIN/src-tauri/src/dictation_lifecycle.rs:135,276` (`ptt::run_feedback_before_action`).
- `MAIN/src-tauri/src/recording_cancel_hook/state.rs:9,292-298` and `hook.rs:11,130,317,417` use the pure `ptt` types and `parse_ptt_key_name`.

Still own code on MAIN:

- Windows keyboard hook: `MAIN/src-tauri/src/recording_cancel_hook/hook.rs:76` calls `SetWindowsHookExW(WH_KEYBOARD_LL)` itself, not `rightkit_hotkeys::windows::LowLevelHooks`.
- Windows middle mouse: `MAIN/src-tauri/src/middle_mouse_win.rs:64` (`WH_MOUSE_LL`).
- Conflict type: `MAIN/heardright_core/src/hotkey_conflict.rs` (unchanged), used at `MAIN/src-tauri/src/hotkey_setup.rs:106`.
- Debounce and tap-versus-hold policy: `MAIN/src-tauri/src/hotkey.rs:129-239`.

### 1.5 Changes after LIVE

Look deliberate:

- Arm timer made single and cancellable (rightkit#180), middle-click lookup moved off the tap (rightkit#181), F21 to F24 handling. All in `RK/crates/rightkit-hotkeys/CHANGELOG.md` under 0.1.2.
- Global chords moved off the Tauri plugin (`MAIN/src-tauri/Cargo.toml:86-87` comment).
- `telemetry_upload` removed app-wide (covers H1).

Look like regressions or unreviewed losses:

- H2, H3, H4: three diagnostic lines that LIVE emits and MAIN cannot, because the package has no place to emit them.
- H5: logging moved back onto the tap callback thread, the opposite of what LIVE did.
- `MAIN/src-tauri/src/fn_hotkey.rs:98-102`: `set_dictation_active` is dropped when the tap does not exist yet. LIVE stored it in a static regardless (`LIVE state.rs:134`). Verified; the practical effect (a recording already active when the tap starts) is a suspicion.
- macOS registration now hops to the main thread and waits up to 10 s (`MAIN/src-tauri/src/hotkey.rs:285-302`), and it is called from `setup` (`MAIN/src-tauri/src/app_run.rs:615,725`). If Tauri does not run the closure inline when already on the main thread, this stalls 10 s and fails. Suspicion, not verified.

### 1.6 Verdict: right shape with listed gaps

The macOS tap, the PTT policy and the chord registration reproduce the LIVE logic closely, and MAIN already runs on them. The gaps are observability (H2 to H5), a `Serialize`-able conflict type (H7), and the Windows side: the package has the hook primitives, but HeardRight's Windows push-to-talk and Ctrl+Escape hook and its Windows middle-mouse hook have not moved.

---

## 2. rightkit-overlay

### 2.1 What LIVE does

LIVE has two different pills.

**macOS: a native Swift pill.** Package `LIVE/macos-native/HeardRightPillMac/`, driven from Rust through a C bridge.

- `Sources/HeardRightPillMac/PillPanel.swift` (644): borderless non-activating `NSPanel` at level 25, `[.canJoinAllSpaces, .stationary]`, explicitly no `fullScreenAuxiliary` (`:181-203`); 50 ms hover watchdog (`:122-127`); capsule sizing and placement.
- `PillModel.swift` (595), `PillRootView.swift` (394), `PillMotion.swift`, `PillTokens.swift`, `FrameAnimator.swift`: state, visuals, motion.
- `AuxiliaryViews.swift` (324): clip card, recent list, hint card.
- `SplashPanel.swift` (548), `PillLaunchHandoff.swift`, `HubTransitionPanel.swift`: launch splash that flies into the pill, hub open and close animation.
- `LIVE/src-tauri/src/pill_bridge_macos.rs` (381): the FFI. Rust to Swift: `hr_pill_init`, `push_event`, `reveal`, `set_visible`, `is_visible`, `set_clipboard_items`, `close_clipboard`, `set_hint`, splash and hub animation calls (`:49-66`). Swift to Rust callbacks: copy text, update history item, open hub, stop, start, stop and send, cancel, recover from error (`:228-373`).

**Windows: a WebView pill.** `LIVE/src-tauri/src/pill/state.rs` says so at `:4-9`; the window renders `LIVE/src/pill/PillOverlay.tsx`.

- Window styling: `NOACTIVATE | TOOLWINDOW`, clear `APPWINDOW`, a `WM_MOUSEACTIVATE` subclass, topmost band re-assert (`pill/state.rs:718-882`).
- Hover poll with a 1-tick enter and 3-tick leave gate (`pill/state.rs:985-1157`; constants in `LIVE/heardright_core/src/pill/model.rs:76-118`).
- `pill_overlays.rs` (921): clip and hint WebView windows, positioning, ready watchdogs, click-outside close.
- `pill_webview_probe.rs` (160): WebView2 controller health probe.

**Shared, both platforms:**

- `LIVE/heardright_core/src/pill/model.rs`: `PillEvent` (`Hide, Idle, Starting, Recording, Voice{level}, Processing{cancellable}, Success, Status{label}, Error{label}`, `:17-50`), `clamp_into_work`, `pill_should_be_visible`, `PillHoverGate`, `event_for_message`, `state_for_event`.
- `LIVE/src-tauri/src/pill/state.rs`: `install`, `shutdown`, `show_event`, `dispatch_new_event`, `show_temporary_event`, `enforce_visibility_invariant`, a 250 ms polling visibility guardian (`:225-273`), fullscreen suppression of every pill state (`:275-306`).
- Fullscreen tests: Windows `pill/state/windows.rs` (borderless style required, same monitor as the pill, DWM frame bounds, shell overlay classes excluded); macOS `pill/state/macos.rs` (AXFullScreen first, geometry fallback).
- `pill/commands.rs` (630): Tauri commands `pill_resize`, `pill_hitbox`, `reveal_pill`, `take_pill_intro`, and others.
- `pill_dispatch_queue.rs` (128): ordered, generation-checked event queue.

### 2.2 What RightKit provides

`RK/crates/rightkit-overlay/src/lib.rs` (1280 lines) and `stack.rs` (120). It owns no event loop, timer or rendering (`README.md`).

- Portable: `OverlayEvent`, `PillMode`, `OverlayModel<Kind>`, `Rect`, `WorkArea::clamp`, `HoverGate`, `OverlayStyle` (`PASSIVE`, `CLICK_THROUGH`, `KEYBOARD`), `FullScreenPolicy`, `fills_monitor`, borderless and covering predicates, `FullScreenProbe`, `MonitorPlacement`, `stack` layout.
- Windows (`windows` module, `:465-1127`): `make_noactivate`, `install_noactivate_mouse_guard`, `is_out_of_topmost_band`, `reassert_topmost`, `reassert_if_needed`, `apply_style`, `create_overlay_window`, `create_layered_overlay_window`, `create_message_only_window`, `set_capture_excluded`, `set_click_through`, `show_noactivate`, `hide`, `monitor_at`, `foreground_fills_monitor`, `foreground_is_borderless_fullscreen`, `foreground_fullscreen_on`.
- macOS: nothing. The non-Windows `windows` module is stubs returning `false`, `None` or `0` (`:1128-1199`). The crate's changelog says macOS full-screen is left to the host (0.2.2 entry).
- Outside this crate, RightKit has a Swift module `RK/swift/RightKitSwift/Sources/RightKitOverlay/Overlay.swift` (804 lines): `RightKitOverlayPanel` (configurable `NSPanel`), hit regions, a pointer tracker, a full-screen evaluator. Neither LIVE nor MAIN's `HeardRightPillMac/Package.swift` depends on it.

### 2.3 Behaviour differences

Verified matches:

- `windows::make_noactivate`, `noactivate_wndproc`, `install_noactivate_mouse_guard`, `reassert_topmost` are line-for-line ports of LIVE (`lib.rs:510-651` vs `LIVE pill/state.rs:745-882`).
- `HoverGate` equals `PillHoverGate`, same 1 and 3 ticks (`lib.rs:14-15,1217-1245` vs `LIVE model.rs:79-118`).
- `WorkArea::clamp` is the same rule as `clamp_into_work`, in `f64` instead of `i32` (`lib.rs:1201-1214` vs `LIVE model.rs:58-71`).

Differences:

| # | Difference | LIVE | RightKit | Status |
|---|---|---|---|---|
| O1 | No macOS implementation. The LIVE Mac pill (panel, model, visuals, clip, hint, splash, hub animation, FFI bridge) has no counterpart in the crate. | `LIVE/macos-native/HeardRightPillMac/Sources/HeardRightPillMac/*`, `pill_bridge_macos.rs` | `lib.rs:1128-1199` (stubs) | Verified |
| O2 | Event vocabulary. `OverlayEvent` has no `Voice { level }` and no `Success`. HeardRight drives the recording bars with `Voice` many times a second. | `LIVE/heardright_core/src/pill/model.rs:31-41` | `lib.rs:28-38` | Verified. `PillEvent` cannot be replaced by `OverlayEvent` as it stands. |
| O3 | Mode mapping. LIVE maps `Starting` to the Processing render state; RightKit has a separate `PillMode::Starting`. | `model.rs:192` | `lib.rs:19-26,44` | Verified |
| O4 | `OverlayModel::apply` makes the overlay visible for any non-Hide event. LIVE's visibility also depends on the "always show resting pill" setting and on full-screen (`pill_should_be_visible`, `event_for_visibility`). | `model.rs:83-89`; `pill/state.rs:57-63` | `lib.rs:1267-1273` | Verified |
| O5 | Windows full-screen test. `foreground_fullscreen_on` does not require a borderless window and counts `QUNS_BUSY`. LIVE requires no caption and no thick frame precisely so a maximised window on a monitor without a taskbar is not treated as full-screen. Using `foreground_fullscreen_on` would bring that case back. | `LIVE pill/state/windows.rs:36-45` | `lib.rs:1083-1126` (0.2.2, unpublished) | Code difference verified; the on-screen effect is inferred, not run. |
| O6 | `foreground_is_borderless_fullscreen` is closer to LIVE but uses exact rectangle equality on `GetWindowRect`, has no shell-class exclusion and no "same monitor as the pill" rule. LIVE uses a 2 px tolerance, DWM extended frame bounds, excludes six shell classes and checks the pill's monitor. | `LIVE pill/state/windows.rs:27-34,58-100`; `pill/state.rs:309-333` | `lib.rs:1010-1037` | Verified |
| O7 | Shell class list differs. `SHELL_OVERLAY_CLASSES` lacks LIVE's `TaskSwitcherWnd`, `TaskSwitcherOverlayWnd`, `Windows.UI.Core.CoreWindow`, and adds taskbar and desktop classes. | `LIVE pill/state.rs:322-333` | `lib.rs:321-331` | Verified |
| O8 | No event dispatch queue, no visibility guardian, no temporary-event timer. | `pill_dispatch_queue.rs`; `pill/state.rs:225-273,416-557` | none (crate owns no loop or timer by design) | Verified. By design; stays app code or needs another owner. |
| O9 | No `event_for_message`, no hover poll intervals (90 and 40 ms), no hitbox margin. | `model.rs:76-78,157-175` | none | Verified |

Inventory note (verified by opening the files): several LIVE files assigned to `rightkit-overlay` are not overlay code. `Sources/HeardRightCloudSync/CloudSyncMain.swift` (1124) is a CloudKit helper; `Sources/HeardRightAppleCredentials/main.swift` (357) is keychain access; `Sources/HeardRightShortcuts/main.swift` (193) is App Intents; `Sources/HeardRightMenuBar/main.swift` (166) is a status-item helper; `CloudSyncNotifications.swift` (573) is CloudKit push handling; `Tests/HeardRightCloudSyncTests/IncrementalPushTests.swift` (222). That is about 2,600 of the 12.5k lines. Roughly another 4,900 are the Swift pill UI and about 2,200 are tests. This accounts for most of the 10% line match: the crate was never going to contain those lines.

### 2.4 Does MAIN call the package?

Both. MAIN uses the crate for Windows window primitives and keeps its own model, state machine, guardian and full-screen tests.

Calls into the crate:

- `MAIN/src-tauri/src/pill/state.rs:819` (`make_noactivate`), `:841` (`reassert_topmost`), `:858` (`ex_style`), `:914` (`make_noactivate` in the title sweep).
- `MAIN/src-tauri/src/hub_window.rs:743-753` (`create_overlay_window` with `OverlayStyle::PASSIVE` for the hub transition shell).
- `MAIN/src-tauri/src/pill_native_win/driver.rs:446` (`monitor_at`); `gfx.rs:50` (`OverlayStyle::PASSIVE`).
- `MAIN/heardright-command-host/src/focus.rs:602` (`ex_style::NOACTIVATE`).

Not used anywhere in MAIN: `OverlayModel`, `OverlayEvent`, `PillMode`, `WorkArea`, `FullScreenPolicy`, any `foreground_*` function (searched).

Own code still on MAIN: `MAIN/heardright_core/src/pill/model.rs` (149) and `hover.rs` (148); `MAIN/src-tauri/src/pill/state.rs` (1029), `state/guardian.rs` (247), `state/windows.rs` (135), `state/windows_events.rs` (193), `state/macos.rs`, `state/macos_events.rs` (312); `pill/commands.rs` (744); `pill_overlays.rs` (688); `pill_bridge_macos.rs` (440); `pill_dispatch_queue.rs` (109); the whole Swift package.

### 2.5 Changes after LIVE

Look deliberate:

- Windows noactivate and topmost code replaced by the crate (identical logic, comment at `MAIN/src-tauri/src/pill/state.rs:814-819`).
- Guardian changed from a 250 ms poll to OS events with a 30 s fallback (`MAIN/src-tauri/src/pill/state/guardian.rs:3-33`). MAIN's own comment records the cost: a pill buried in the z-order with no foreground event stays buried until the next event (`MAIN/src-tauri/src/pill/state.rs:269-273`). That is a stated trade-off, and a behaviour LIVE did not have.
- Full-screen: only the resting pill is hidden, an active pill stays up, and the Mac panel gains `fullScreenAuxiliary` (heardright #162; `MAIN/src-tauri/src/pill/state.rs:427-428`; `MAIN .../PillPanel.swift:13-15,208-213`). LIVE hid every state and its panel comment said "Intentionally no FullScreenAuxiliary" (`LIVE .../PillPanel.swift:198`). This reverses a LIVE decision on purpose.
- Windows full-screen adds `SHQueryUserNotificationState` but keeps the borderless fallback and the same-monitor rule (`MAIN/src-tauri/src/pill/state/guardian.rs:222-247`). Stricter than RightKit's `foreground_fullscreen_on` (see O5).
- Mac pill: error label shows the real label instead of "Failed", error stays 5 s instead of 2.5 s, recovery is bound to the error generation (`PillModel.swift` diff lines 240, 585-590); hidden panel stops the hover watchdog (M-23, `PillPanel.swift:57-60,104-106`); splash stall controls (heardright #117, `SplashPanel.swift`).
- Swift tests removed (`Tests/` exists only in LIVE), in line with the end-to-end-only instruction recorded in the RightKit changelogs.

Unclear, needs the owner's call:

- `HR_APP_DATA_DIR` and `HR_NATIVE_SNAPSHOT_DIR` overrides removed from the Swift pill (`PillModel.swift` LIVE `:147-153`; `AuxiliaryViews.swift` LIVE `:272-277`). They look like test hooks; whether any QA flow still needs them is not verified.

### 2.6 Verdict: right shape with listed gaps, for a narrow slice

For what it covers (Windows overlay window styles, topmost repair, clamp, hover gate, monitor lookup) the crate is a faithful port and MAIN already uses it. It cannot stand in for HeardRight's pill model (O2 to O4), its Windows full-screen rule (O5 to O7) or any of the Mac pill (O1). Treating all 12.5k inventory lines as "moves into rightkit-overlay" is the wrong reading: the Mac pill needs a Swift owner, and the state machine, queue and guardian need either a different package or to stay in the app.

---

## 3. rightkit-overlay-win

### 3.1 What LIVE does

Nothing native on Windows. The LIVE Windows pill, clip card and hint card are WebView2 windows (`LIVE/src-tauri/src/pill/state.rs:4-9,638-693`; `pill_overlays.rs:104-215,272-340`), rendered by `LIVE/src/pill/PillOverlay.tsx`, `MiniClipboard.tsx`, `HintPopup.tsx`. The LIVE release named in the brief is the Mac build, where this package does not apply at all.

### 3.2 What RightKit provides

`RK/crates/rightkit-overlay-win/src/` (about 5,300 lines). Windows only; elsewhere `OverlayWindow::create` returns `Unsupported` (`lib.rs:58-60`).

- `OverlayWindow` / `OverlayHandle` (`window.rs:277-523`): a layered, topmost, never-activating window on its own thread. Commands: `set_frame`, `set_work_area_clamp`, `show`, `hide`, `set_painter`, `invalidate`, `animate`, `stop_animation`, `set_hit_regions`, `events`, `reassert_topmost`, `present`, `set_focusable`, `close`, `hwnd`.
- Events (`events.rs:18-46`): hover enter, move, leave, click, DPI changed, monitor changed, closed; in 0.1.3 also char, key and focus.
- `PaintCtx` software canvas (`paint.rs`), paths and SVG path parsing, icons, squircles, glass, GDI text, animation with easing, per-monitor DPI.
- Policy re-used from `rightkit-overlay` (`lib.rs:99-101`).

### 3.3 Behaviour differences

| # | Difference | Where | Status |
|---|---|---|---|
| W1 | Different technology from LIVE. Visual and interaction parity with the WebView pill lives in MAIN's app code (`MAIN/src-tauri/src/pill_native_win/visuals.rs`, 1820 lines, a port of the Swift pill), not in the crate. | `MAIN/src-tauri/src/pill_native_win/mod.rs:1-19` | Verified that it is a rewrite. Parity itself is not verifiable by reading; not verified. |
| W2 | No in-place text editing in the clip card at the pinned 0.1.0. LIVE's WebView clip card has a textarea and writes back to history. MAIN draws the pencil and opens the Hub instead. | `LIVE/src/pill/MiniClipboard.tsx` (textarea, `update_history_item`); `MAIN/src-tauri/src/pill_native_win/clipboard.rs:44-48` (`INLINE_EDIT_SUPPORTED = false`); `driver.rs:1024-1025` | Verified. RightKit 0.1.3 adds `set_focusable` and key events for this (`RK/crates/rightkit-overlay-win/CHANGELOG.md`, #1318), unpublished and not adopted. |
| W3 | No drag support in the crate. | `RK/crates/rightkit-overlay-win/README.md`, "No drag" | Verified. I found no drag in LIVE's pill code either (no match in MAIN's native pill; LIVE not exhaustively searched), so probably no loss; not verified. |
| W4 | Work-area clamp is on by default and would move HeardRight's pill, which sits 4 pt above the work area. MAIN turns it off. | `MAIN/src-tauri/src/pill_native_win/gfx.rs:53-55`; README "work-area clamp" | Verified |
| W5 | `click_through` in `OverlayConfig` is window shaping by hit regions, not the style bit. Anything painted outside a hit region is not shown. | README "click_through is window shaping"; `gfx.rs:50-51` | Verified from the README and the call site; the `win/mod.rs` implementation was not read line by line. |
| W6 | Fonts, the intro sound and cursor lookup are app code beside the crate. | `MAIN/src-tauri/src/pill_native_win/gfx.rs:177-226` | Verified |
| W7 | WebView health probing is gone because there is no WebView. | `LIVE/src-tauri/src/pill_webview_probe.rs` (160) vs `MAIN` (8) | Verified |

### 3.4 Does MAIN call the package?

Yes, as the only renderer of the Windows pill, hint and clip overlays, through one seam file: `MAIN/src-tauri/src/pill_native_win/gfx.rs:18,25-28,49-57` (`OverlayWindow::create`, `OverlayConfig::new`, `set_work_area_clamp`), with `PaintCtx` calls at `:108-175`. Events are consumed at `driver.rs:832-843`. The app still carries about 5,300 lines of its own on top (`pill_native_win/*`): model, visuals, driver, clip, hint, prompt. The crate's README lists this exact call surface as its contract ("API contract and deviations").

### 3.5 Changes after LIVE

Look deliberate:

- The whole native Windows pill (heardright #294, `MAIN/src-tauri/src/pill_native_win/mod.rs:1`; MAIN also has `docs/HANDOFF_WIN32_PILL.md`). LIVE's `pill/state.rs:7` comment says an earlier native Win32 pill had been removed, so this is a second reversal and clearly intended.

Regression against LIVE Windows behaviour:

- W2, clip card editing. Recorded in MAIN's own code as a known gap.

### 3.6 Verdict: right shape with listed gaps

The crate was written against MAIN's native pill and MAIN uses exactly the surface the crate documents, so the shape fits. The gaps are the pin (0.1.0 lacks `set_focusable` and `present`, so clip editing is missing on Windows) and the fact that parity with the pill users see has not been shown by any run I could read. It says nothing about the Mac release.

---

## Top gaps, in order

1. The Mac pill has no RightKit owner in these packages (O1). The LIVE Mac release's overlay is entirely `HeardRightPillMac` Swift plus `pill_bridge_macos.rs`, unchanged in ownership on MAIN.
2. `OverlayEvent` cannot carry HeardRight's `Voice` and `Success` events (O2), so the pill model cannot move as is.
3. RightKit's new Windows full-screen check would hide the pill under a maximised window on a taskbar-less monitor, the case LIVE fixed (O5).
4. Hotkey diagnostics lost or moved onto the tap thread (H2 to H5).
5. Windows clip card editing lost on MAIN until overlay-win 0.1.3 or later is published and adopted (W2).
6. `HotkeyConflict` needs `Serialize` before HeardRight can drop its own (H7).
7. Windows push-to-talk and middle-mouse hooks still app-owned although the package has the primitives.
8. About 2,600 inventory lines targeted at `rightkit-overlay` are CloudKit, keychain, App Intents and menu-bar code.

## Not verified

- Anything at run time: no build, no test, no app launch.
- The published crate contents at the pinned versions.
- `tauri-plugin-global-shortcut` internals (H8, H9).
- Whether Tauri runs `run_on_main_thread` inline on the main thread (1.5).
- `rightkit-overlay-win/src/win/mod.rs`, `paint.rs`, `path.rs` internals; I read the API, README and changelog.
- Visual parity of the Windows native pill with either the LIVE WebView pill or the Mac pill.
