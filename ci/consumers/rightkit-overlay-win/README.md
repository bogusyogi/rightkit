# rightkit-overlay-win 0.1.0 (registry consumer recipe)

Standalone consumer, outside the workspace, pinning `rightkit-overlay-win =0.1.0` exactly. It builds
and runs on every host, including the macOS runner, because it uses only the pure, window-free parts of
the public API:

- Animation maths: `Easing::apply` endpoints, `Animation::once` and `Animation::looping` through
  `Animation::sample`.
- Colour: `Rgba::new`, `rgb`, `from_hex`, `with_alpha`, `scaled_alpha`, `lerp`.
- Geometry: `scale_for_dpi`, `device_size`, `client_to_logical`, `hit_test` (edge-inclusive, last
  region wins) and `device_regions` (rounded outwards, degenerate regions skipped).
- Paths and layout: `rounded_rect_path`, `parse_svg_path`, `transform_path`, `bar_rects` with `BarStyle`.
- Painting into an offscreen `PaintCtx` (`fill_rounded_rect`, `pixel`, `logical_size`, `clear`).
- `OverlayWindow::create` refusing an empty name with `InvalidConfig` on every host, and, off Windows,
  refusing a valid config with `Unsupported`.

On Windows the consumer does not create a window (`cfg(windows)` skips that call): no HWND, no owner
thread, no DWM. No network, no files, no real user data.

Not covered: a live `OverlayWindow`, `OverlayHandle`, hover tracking, `UpdateLayeredWindow` presentation,
text drawing, glass layers, and per-monitor DPI.
