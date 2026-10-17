# rightkit-overlay

Platform-neutral overlay state and placement policy for Right Suite apps. It owns serializable overlay state, finite-geometry checks, work-area clamping, delayed hover collapse (`HoverGate`), hit testing, and the TOPMOST and status-level contract (`HR_STATUS_WINDOW_LEVEL`). On Windows, it also provides the `windows` module, which applies `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` styles to an `HWND` and repairs actual `HWND_TOPMOST` z-order drift.

**Use it** in a native overlay host (a floating pill or a docked notch) to share state, placement, and hover policy across React and native pills.

**Do not use it** to create windows, render, or manage the app's event loop. The crate's docs say native hosts keep window creation and rendering. It also does not call AppKit; `HR_STATUS_WINDOW_LEVEL` is a constant that the host applies.

## Features

None. The crate has no Cargo features and no dependencies beyond `serde` (and `windows-sys` on Windows).

## Platform support

- `windows` module: real implementation under `cfg(windows)`. It takes an `HWND` as `isize`. `make_noactivate` must be called on the thread that owns the window, before that window receives UI messages (`src/lib.rs:233`, `:301`). `reassert_if_needed` is for a watchdog that runs after foreground or z-order notifications (`src/lib.rs:94`).
- `OverlayStyle` picks the window behaviour (`PASSIVE`: seen and clicked, never takes focus; `CLICK_THROUGH`: the pointer passes through; `KEYBOARD`: takes the keyboard while open). On Windows, `windows::apply_style` writes it to a window the host created, `set_click_through` switches click-through at run time, `show_noactivate` and `hide` show and hide without touching the foreground window, `monitor_at` gives the monitor under a point (or the nearest one) with its work area and DPI (system DPI unless the process is per-monitor DPI aware), and `foreground_fills_monitor` with `FullScreenPolicy` decides whether to show over a full-screen app. `foreground_is_borderless_fullscreen(own_pids)` is the stricter test (borderless style, exactly the monitor rectangle, not this app's own windows by pid). The host still creates and paints the window. `stack::place` lays out a stack of transient windows beside an anchor inside the work area, and `covering_window` gives the covering verdict from a z-order walk the host supplies.
- Other targets: the `windows` module compiles to stubs (`src/lib.rs:590`) that return `false`, except `monitor_at`, which returns `None`. The rest of the crate (state, geometry, hover, hit test) is portable.

Published version: see `INDEX.md` at the repository root. Release history is in [CHANGELOG.md](CHANGELOG.md).

## Example

Illustrative; not compiled as a doctest.

```rust,ignore
use rightkit_overlay::{OverlayEvent, OverlayKind, OverlayModel, Rect, WorkArea};

let mut model = OverlayModel::new(OverlayKind::HeardRightPill,
    Rect { x: 0.0, y: 0.0, width: 320.0, height: 48.0 });
model.apply(OverlayEvent::Idle); // hit_test is false until an event makes the overlay visible
model.place(WorkArea { left: 0.0, top: 0.0, right: 1440.0, bottom: 900.0 });
let inside = model.hit_test(10.0, 10.0);
```

## Tests

There is no `tests/` directory. Unit tests are in `src/lib.rs` (`mod tests`, from line 705) and cover the extended-style mapping, full-screen policy, monitor coverage and DPI scale. None of them calls the `windows` module.
