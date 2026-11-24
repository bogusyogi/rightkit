# Changelog

## 0.2.2

- The card renderer's two premultiplied pixel loops (a strip picture laid over a body, card contents laid over a body) are `rightkit_win_ui::canvas::Canvas::composite_clipped`; the loops in `render` are gone and the output is unchanged. Requires `rightkit-win-ui` 0.1.3.

## 0.2.1

- `layout::headline_outside()`: whether `draw_headline_outside` was called (the headline window drawn outside whatever the switches say). Default `false` until it is called, as before; the flag was private, so a host building its own `CellView`s could not read it. Pulse's `views` reads it where `weeklyHeadline` would swap the rings.

## 0.2.0

- Breaking: the cells are the host's. `layout::Cell` is a host-defined value (`Cell::new(id, glyph, section)`: the host's id, the ring's glyph key and what a click opens) instead of Pulse's fixed set (`Cpu`, `Disk`, `Claude`, `Codex`, `Profile`, `Account`, `Send`, `Tools`); `Cell::ALL` and `CELL_COUNT` are gone, `set_shown` gives the cells (nothing is shown until it is called; `count()` is at least 1). The cell kept when the notch is minimised is the host's too: `set_anchor(Some(cell))` (Pulse: its Send cell), and `send_cell()` is `anchor_cell()`.
- Breaking: `card::Mark` is `None` or `Glyph(Glyph)`, the host's glyph, instead of the provider list; the Claude card's header layout (round button centred on the title row) is `CardContent::inline_head`, for any card.
- Product wording removed from the view model's documentation (accounts, providers, Send). The notch mechanics (geometry, motion, hit-testing, the renderer, cards) are unchanged.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): layout, motion, card model, render, swell.
