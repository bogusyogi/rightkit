# rightkit-notch-ui

Notch drawing core: screen-edge geometry and cell layout, spring motion, the hover card model and a software renderer for ring gauges, cards, menus and the emerging card body. The renderer and card model are Windows-only; the application supplies switches, thumbnails and translation through `host::Host`.

The cells are the application's: it defines each `layout::Cell` (its id, the glyph drawn in the ring and what a click opens), sets the list with `layout::set_shown` and the cell kept when the notch is minimised with `layout::set_anchor`, and fills each cell's `CellView` with its own readings. Card titles carry the glyph the application chooses (`card::Mark::Glyph`). Nothing in the crate knows which product it serves.

Moved into RightKit from `pulse-windows` (Pulse) on 2026-11-01: `layout.rs` (geometry only; the usage views stay in the app), `motion.rs`, `card.rs` (the model only), `render.rs`, the `Swell` type from `main.rs`, and the blur helpers from `wheel.rs`. Since 0.2.0 Pulse's cell set (CPU, disk, Claude, Codex, accounts, Send, Tools) is Pulse's own.
