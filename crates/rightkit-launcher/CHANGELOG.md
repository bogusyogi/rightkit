# Changelog

## 0.2.0

- Breaking: the crate is the portable launcher core. The Win32 panel and what only it uses (`panel`, `chat`, `snippets`, `spot_takeover`, `store`, the `qa-views` feature) moved to the new crate `rightkit-launcher-win` 0.1.0, which depends on this one; `response_layout` (the AI answer display model) moved to `rightkit-overlay-win` 0.1.5.
- Breaking: `host::Host` keeps only what the core uses: `translate`, `https_get`, `log_info`, `file_index_scope`, `ignore_file_name`, `rates_ready`. The panel's services (settings, engine, hub, key capture, drives, clipboard strip) are `rightkit_launcher_win::host::PanelHost`; `EngineFailure`, `LauncherHotkey`, `AltChord` and `Drive` moved there. `host::log_info` is public.
- `json` is the one copy of the bounded JSON reader; `rightkit-clipboard-history` 0.2.0 re-exports it instead of keeping an identical file.
- Description and README corrected (the crate is more than the calculator).
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

## Unreleased before 0.2.0

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): `calc` logic.
- Added Windows `snippets` (keyword expansion) from `launcher_snippets.rs`.
- Added Windows `spot_takeover` (owner-side handover overlay).
- Added Windows `panel` (the launcher window, from `launcher.rs`), `chat` (in-launcher assistant) and `response_layout`; host trait grows the settings, key-capture, engine, hub and usage hooks the panel needs.
