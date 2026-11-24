# Changelog

## 0.1.3

- Added `text_layout::wrap_preserving(text, lines, char_budget, fits)`: a wrap that keeps the layout the text has (line breaks, indentation, tabs as four spaces, runs of spaces), takes the host's own `fits` measure for a candidate line, breaks a word that does not fit alone, looks only at the first `lines * char_budget` characters, and ends a cut text in an ellipsis. `TextPainter::wrap` (which collapses whitespace) is unchanged.
- Added `Canvas::composite_clipped(ink, origin, shown)`: lays a premultiplied canvas over another at an opacity, clipped to what the destination already covers (the loop Pulse kept for card contents and clipboard previews). Buffers are taken as rectangles; opacity is clamped to 0..=1 and a non-finite value draws nothing.

## 0.1.2

- Added `surface::Surface::present(hwnd, position, alpha)` and `surface::present_with_alpha(hwnd, canvas, position, alpha)`: publish a premultiplied BGRA surface to a layered window with a constant opacity over its per-pixel alpha, optionally moving the window in the same call. `surface::present` keeps its behaviour (alpha 255, cached surface) and now goes through `Surface::present`.
- Added the `wic` feature (off by default, Windows only): `wic::reencode_png(source, target, on_error)` writes the first frame of an image file to a new PNG. The target appears only after the encoder closes, and an existing file is preserved. Needs `rightkit-fs` 0.5 for the owned temp.

## 0.1.1

- `glyphs::rounded_rect` is now `pub` (hosts drawing their own glyphs no longer copy it).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): raii, surface, canvas, glyphs, fileicon, text_layout.
