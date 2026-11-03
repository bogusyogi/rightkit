# Changelog

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): `calc` logic.
- Added Windows `snippets` (keyword expansion) from `launcher_snippets.rs`.
- Added Windows `spot_takeover` (owner-side handover overlay).
- Added Windows `panel` (the launcher window, from `launcher.rs`), `chat` (in-launcher assistant) and `response_layout`; host trait grows the settings, key-capture, engine, hub and usage hooks the panel needs.
