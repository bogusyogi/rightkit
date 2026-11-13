# Changelog

## 0.1.1

- `glyphs::rounded_rect` is now `pub` (hosts drawing their own glyphs no longer copy it).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): raii, surface, canvas, glyphs, fileicon, text_layout.
