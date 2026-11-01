# Changelog

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): `detect`, `model`, `search`, `seal`.
- Added Windows `win` (clipboard calls), `img` (picture conversion) and `shelf` (nearby shelf).
- Added Windows `strip` (the clipboard strip, from `clip_strip/*`) on rightkit-notch-ui.
- Added Windows `burst` (the copy burst, from `clip_paste/burst.rs`); the preview now returns the card content and the host's `burst_changed` replaces the send-panel call.
