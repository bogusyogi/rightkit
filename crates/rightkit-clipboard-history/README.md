# rightkit-clipboard-history

Clipboard history logic: classify copied text (colour, link, code, plain) and spot whole-text secrets.

Moved into RightKit from `pulse-windows` (Pulse) on 2026-11-01. This first release holds the store, search, classification and encryption modules; the application installs a `host::Host` for its data folder, limits, translation and logging.

The default `history` feature preserves the complete store and platform API. Use `default-features = false` for portable `detect` and `Detected` without history dependencies. `qa-views` enables `history`.
