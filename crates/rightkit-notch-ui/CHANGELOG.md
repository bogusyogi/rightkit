# Changelog

## 0.2.2

- The card renderer's two premultiplied pixel loops (a strip picture laid over a body, card contents laid over a body) are `rightkit_win_ui::canvas::Canvas::composite_clipped`; the loops in `render` are gone and the output is unchanged. Requires `rightkit-win-ui` 0.1.3.
- Added `glide::Glide<P>` and `glide::union_window`: a hover card moving to another ring as one body. The body (centre, length, extent) glides on `motion::GLIDE`, the neck anchor follows on `motion::NECK_TRAIL` after `NECK_TRAIL_DELAY`, the old contents fade out as the new fade in over `CROSSFADE_SECONDS`, and `retarget` turns a glide in flight round with the speed it has (each number is a spring of its own, the fade restarts from the opacities it had). `P` is the host's card content and is only carried; the host draws the layers (`render::render_emerging`) at `pose()`, `olds()` and `new_level()`. Moved from Pulse's `windows/src/main.rs`.
- Added `swell::SwellPair` (`slide` and `open`): the two values every popped-out card has, with `rise` (slide out now, open a beat later), `fall` (close now, slide in a beat later), `hold_out` (a card already out is replaced: both retarget to fully out now), `step`, `moving`, `values` and `reset`. The clipboard strip's preview shares the two springs with the hover card, so a preview that replaces an existing one retargets both at once and a fresh one rises with the beat.

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
