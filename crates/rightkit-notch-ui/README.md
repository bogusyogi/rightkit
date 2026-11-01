# rightkit-notch-ui

Notch drawing core: screen-edge geometry and cell layout, spring motion, the hover card model and a software renderer for ring gauges, cards, menus and the emerging card body. The renderer and card model are Windows-only; the application supplies switches, thumbnails and translation through `host::Host`.

Moved into RightKit from `pulse-windows` (Pulse) on 2026-11-01: `layout.rs` (geometry only; the usage views stay in the app), `motion.rs`, `card.rs` (the model only), `render.rs`, the `Swell` type from `main.rs`, and the blur helpers from `wheel.rs`.
