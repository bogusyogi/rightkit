# Changelog

## 0.2.0

- Breaking: the cells are the host's. `layout::Cell` is a host-defined value (`Cell::new(id, glyph, section)`: the host's id, the ring's glyph key and what a click opens) instead of Pulse's fixed set (`Cpu`, `Disk`, `Claude`, `Codex`, `Profile`, `Account`, `Send`, `Tools`); `Cell::ALL` and `CELL_COUNT` are gone, `set_shown` gives the cells (nothing is shown until it is called; `count()` is at least 1). The cell kept when the notch is minimised is the host's too: `set_anchor(Some(cell))` (Pulse: its Send cell), and `send_cell()` is `anchor_cell()`.
- Breaking: `card::Mark` is `None` or `Glyph(Glyph)`, the host's glyph, instead of the provider list; the Claude card's header layout (round button centred on the title row) is `CardContent::inline_head`, for any card.
- Product wording removed from the view model's documentation (accounts, providers, Send). The notch mechanics (geometry, motion, hit-testing, the renderer, cards) are unchanged.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): layout, motion, card model, render, swell.
