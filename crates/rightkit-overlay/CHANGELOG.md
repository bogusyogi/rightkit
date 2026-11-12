# Changelog

## 0.2.3

- **`FullScreenProbe::require_borderless`** (additive field, default `false`: today's behaviour). `windows::foreground_fullscreen_on` then counts only a foreground window without a caption and without a sizing border (`style_is_borderless`), measured by its DWM frame bounds (`DWMWA_EXTENDED_FRAME_BOUNDS`, falling back to `GetWindowRect`). A maximised ordinary window on a monitor without a taskbar no longer counts. The shell notification state still applies: a presentation counts, an exclusive Direct3D or Store-app full-screen counts when the window covers the monitor whatever its style, and `QUNS_BUSY` adds nothing beyond the borderless test. Downstream struct literals of `FullScreenProbe` must add the field (`..Default::default()` needs nothing).
- **A foreground event source**: `windows::run_foreground_event_pump(on_ready, on_event) -> bool` runs WinEvent hooks on the calling thread until `ForegroundPumpStop::stop` (new foreground window, minimise and restore system-wide; move and resize of the foreground window only, for its process). It sleeps with no timer and reports `ForegroundEvent::{Focus, Geometry}` from `on_event`, which must only enqueue. `false` when the hooks cannot be installed or a pump already runs; the non-Windows stub returns `false`. New types `ForegroundEvent` and `ForegroundPumpStop`. Features added: `Win32_Graphics_Dwm`, `Win32_System_Threading`, `Win32_UI_Accessibility`.

## 0.2.2 (unpublished)

- #1021 full-screen verdict for a host's overlays. Additive. `windows::foreground_fullscreen_on(monitor: WorkArea, probe: &FullScreenProbe) -> bool` and `FullScreenProbe { own_pids, ignore_classes, use_shell_notification_state, edge_tolerance_px }` with `Default` (no own pids; `SHELL_OVERLAY_CLASSES` ignored; shell notification state on; tolerance 2 px). `covers_monitor_within(window, monitor, tolerance_px)` is the pure edge test. The foreground window counts on the monitor it overlaps only, so a full-screen program on another monitor does not hide this one. It counts when it covers the monitor within the tolerance, or when `SHQueryUserNotificationState` reports `QUNS_BUSY`, `QUNS_RUNNING_D3D_FULL_SCREEN`, `QUNS_PRESENTATION_MODE` or `QUNS_APP`, even with a caption or odd bounds. Alt-Tab and Task View hosts are excluded by class. `foreground_is_borderless_fullscreen` is unchanged. Feature added: `Win32_UI_Shell` (`SHQueryUserNotificationState`). Decision for macOS: no Accessibility implementation in this crate; the host supplies the verdict through the shell's per-overlay callback, and the crate stub returns `false`. Note: `QUNS_APP` is reported for a Store app in the foreground, so a Store app that fills a monitor on that monitor counts, as the issue asks.
- #413 overlay half, capture exclusion. Additive, but a public field: `OverlayStyle::exclude_from_capture: bool` (`false` in `PASSIVE`, `CLICK_THROUGH`, `KEYBOARD`). `apply_style`, `create_overlay_window` and `create_layered_overlay_window` apply it through `windows::set_capture_excluded(hwnd, excluded) -> bool` (`SetWindowDisplayAffinity` with `WDA_EXCLUDEFROMCAPTURE`, or `WDA_NONE` to clear). A `false` flag leaves the window's affinity alone. Downstream struct literals of `OverlayStyle` must add the field: `rightkit-overlay-win/src/win/mod.rs:332` and `:356`; `rightkit-win-ui/src/raii.rs:63` uses `..style` and needs nothing. The macOS half (`NSWindow.sharingType = .none`) needs RightKitSwift and is not in this crate.
- #868 layered overlay creation and a message-only listener window. Evidence: issue #361 item 5 (HeardRight) asked for an unlayered creator because `apply_style(PASSIVE)` adds `WS_EX_LAYERED`, which "may not paint without SetLayeredWindowAttributes"; a layered window with no pixels from `UpdateLayeredWindow` or `SetLayeredWindowAttributes` does not paint. So `create_overlay_window` still strips the layered bit and still refuses click-through. Decision: `windows::create_layered_overlay_window(hinstance, class_name, title, rect, style, pixel_alpha_empty_hit_regions: bool) -> isize` creates a `WS_EX_LAYERED` window (with `WS_EX_TRANSPARENT` when `style.click_through`) only when the caller asserts it is per-pixel-alpha and has empty hit regions; otherwise a click-through request returns `0`. A click-through layered window with hit regions is refused because `WS_EX_TRANSPARENT` would pass the clicks meant for those regions (the #361 case); this crate cannot see hit regions, so the flag is the caller's assertion. The caller must paint with `UpdateLayeredWindow` (`ULW_ALPHA`) before showing it. `windows::create_message_only_window(class_name) -> Result<isize, WindowCreateError>` makes a `HWND_MESSAGE` window for a listener such as a clipboard-format listener; it uses the process module handle, so the class must be registered on that module. The crate had no equivalent; `rightkit-tauri-shell/src/clipboard.rs:761` and `rightkit-clipboard-history/src/capture.rs:138` create theirs directly. Features added: `Win32_System_LibraryLoader` (`GetModuleHandleW`) and `Win32_UI_Shell`.
- Not compiled or run. No tests were added (Adrian: end-to-end only).

## 0.2.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.2.0 (unreleased)

- Breaking: replace product-specific OverlayKind with caller-owned OverlayModel<Kind>; serialization follows caller kind, preserving existing wire values when caller retains its enum mapping.
- Breaking: remove product status-window-level constant; native host supplies level explicitly (25 retains previous behavior). Native Windows style/creation behavior is unchanged.
- Add caller-kind behavior coverage & Windows native create/show/hide/destroy smoke source. Native execution evidence is recorded separately by integration owner.

## 0.1.2 (2026-10-17)

Additive. Code-only: nothing in this section was compiled or run when written.

- `stack` module (#548): `StackLayout`, `stack::stack_above` and `stack::place` lay out a stack of transient windows (approval bubbles) beside an anchor. The stack centres on the anchor, stacks above or below it, one gap apart, keeps `margin` inside the work area and shifts by a persisted drag offset. Pure and tested on every OS.
- `style_has_no_frame` and `WS_BORDER_BIT`, `WS_DLGFRAME_BIT` (#645): a lone `WS_BORDER` or `WS_DLGFRAME` leaves a window frameless; the whole caption or a `WS_THICKFRAME` frames it. `is_foreign_fullscreen_overhang` combines that with `fills_monitor` (overhang included) and the own-process check. Existing `style_is_borderless` and `is_foreign_borderless_fullscreen` are unchanged.
- `rects_overlap`, `CoveringCandidate`, `covering_window` and `MAX_COVERING_WALK` (#525): the covering verdict over a z-order walk, from plain values. It skips hidden, own-process and cloaked windows and returns the first overlapping class name, or `"not_topmost"`. The Windows z-order walk that fills the candidates is not added: it needs the `Win32_Graphics_Dwm` and `Win32_System_Threading` windows features, which `Cargo.toml` does not enable.

## 0.1.2

Additive. Code-only: nothing in this section was compiled or run when written.

- `OverlayStyle` (`PASSIVE`, `CLICK_THROUGH`, `KEYBOARD`) and `OverlayStyle::ex_style`: the extended window style for a topmost layered tool window that does or does not take focus and is or is not click-through. `ex_style` holds the `WS_EX_*` values so the mapping is tested on every OS.
- `FullScreenPolicy` (`StayAbove`, `Hide`) and `fills_monitor`: whether an overlay shows while a foreground window covers its monitor.
- `MonitorPlacement` (bounds, work area, DPI, `scale()`).
- Windows: `windows::apply_style`, `set_click_through` (switch `WS_EX_TRANSPARENT` at run time), `show_noactivate`, `hide`, `monitor_at` (monitor under a point with its DPI) and `foreground_fills_monitor`. The non-Windows stubs return `false` or `None`.
- Borderless full-screen check (Orthic-Labs/rightkit#244): `windows::foreground_is_borderless_fullscreen(own_pids: &[u32])` is true when the foreground window has neither `WS_CAPTION` nor `WS_THICKFRAME`, is exactly the rectangle of its monitor, is visible and not minimised, and does not belong to this process or to any pid in `own_pids` (helper processes of the same app). The desktop and shell windows do not count. `foreground_fills_monitor` is unchanged and stays the lenient test. The non-Windows stub returns `false`. The pure parts are cross-platform and tested: `style_is_borderless`, `rect_equals_monitor` (exact, stricter than `fills_monitor`), `is_own_pid`, `is_foreign_borderless_fullscreen`, `WS_CAPTION_BITS`, `WS_THICKFRAME_BIT`.

## 0.1.1 - 2026-10-10

- Metadata alignment: add `repository`, `rust-version = "1.98.0"`, an `include` list, `LICENSE-MIT` and `LICENSE-APACHE`, matching the other RightKit crates.
- Remove product names and a workstation path from the crate docs and README. Public items, including `OverlayKind` variants and `HR_STATUS_WINDOW_LEVEL`, and their serialized values (`heard_right_pill`, `cockpit_notch`) are unchanged.
