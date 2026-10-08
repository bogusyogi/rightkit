# rightkit-overlay

Platform-neutral overlay state and placement policy for Right Suite apps. It owns serializable overlay state, finite-geometry checks, work-area clamping, delayed hover collapse (`HoverGate`), hit testing, and the HR TOPMOST and status-level contract (`HR_STATUS_WINDOW_LEVEL`). On Windows, it also provides the `windows` module, which applies `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` styles to an `HWND` and repairs actual `HWND_TOPMOST` z-order drift.

**Use it** in a native overlay host (HeardRight's pill and Cockpit's notch) to share state, placement, and hover policy across React and native pills.

**Do not use it** to create windows, render, or manage the app's event loop. The crate's docs say native hosts keep window creation and rendering. It also does not call AppKit; `HR_STATUS_WINDOW_LEVEL` is a constant that the host applies.

## Features

None. The crate has no Cargo features and no dependencies beyond `serde` (and `windows-sys` on Windows).

## Platform support

- `windows` module: real implementation under `cfg(windows)`. It takes an `HWND` as `isize`, and it must be called before first show (`make_noactivate`), then from the host's visible-overlay watchdog (`reassert_if_needed`).
- Other targets: the `windows` module compiles to stubs that return `false`. The rest of the crate (state, geometry, hover, hit test) is portable.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest.

```rust,ignore
use rightkit_overlay::{OverlayKind, OverlayModel, Rect, WorkArea};

let mut model = OverlayModel::new(OverlayKind::HeardRightPill,
    Rect { x: 0.0, y: 0.0, width: 320.0, height: 48.0 });
model.place(WorkArea { left: 0.0, top: 0.0, right: 1440.0, bottom: 900.0 });
let inside = model.hit_test(10.0, 10.0);
```

## Tests

The crate has no `tests/` content. Its behavior is exercised by the consuming host apps.
