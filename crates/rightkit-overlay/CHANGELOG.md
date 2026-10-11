# Changelog

## Unreleased

Additive.

- `OverlayStyle` (`PASSIVE`, `CLICK_THROUGH`, `KEYBOARD`) and `OverlayStyle::ex_style`: the extended window style for a topmost layered tool window that does or does not take focus and is or is not click-through. `ex_style` holds the `WS_EX_*` values so the mapping is tested on every OS.
- `FullScreenPolicy` (`StayAbove`, `Hide`) and `fills_monitor`: whether an overlay shows while a foreground window covers its monitor.
- `MonitorPlacement` (bounds, work area, DPI, `scale()`).
- Windows: `windows::apply_style`, `set_click_through` (switch `WS_EX_TRANSPARENT` at run time), `show_noactivate`, `hide`, `monitor_at` (monitor under a point with its DPI) and `foreground_fills_monitor`. The non-Windows stubs return `false` or `None`.

## 0.1.1 - 2026-10-10

- Metadata alignment: add `repository`, `rust-version = "1.98.0"`, an `include` list, `LICENSE-MIT` and `LICENSE-APACHE`, matching the other RightKit crates.
- Remove product names and a workstation path from the crate docs and README. Public items, including `OverlayKind` variants and `HR_STATUS_WINDOW_LEVEL`, and their serialized values (`heard_right_pill`, `cockpit_notch`) are unchanged.
