# Changelog

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
