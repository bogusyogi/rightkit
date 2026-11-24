# rightkit-clipboard-history

Clipboard history logic: classify copied text (colour, link, code, plain) and spot whole-text secrets.

Moved into RightKit from `pulse-windows` (Pulse) on 2026-11-01. This first release holds the store, search, classification and encryption modules; the application installs a `host::Host` for its data folder, limits, translation and logging.

The default `history` feature exposes the store, capture, paste, burst data and OLE drag. Hosts render their own strip and burst card. Use `default-features = false` for portable `detect` and `Detected` without history dependencies. `qa-views` enables clipboard fixture injection.
