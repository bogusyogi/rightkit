# rightkit-clipboard-history

Clipboard history logic: classify copied text (colour, link, code, plain) and spot whole-text secrets.

Moved into RightKit from `pulse-windows` (Pulse) on 2026-11-01. This first release holds the store, search, classification and encryption modules; the application installs a `host::Host` for its data folder, limits, translation and logging.
