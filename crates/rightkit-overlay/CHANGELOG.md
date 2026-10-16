# Changelog

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
