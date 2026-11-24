# Changelog

## 0.2.2

- Added `files::LiveFileIndex` (Windows): a file index whose roots and switches follow the embedding app's settings, rebuilt after file changes (watched with `rightkit-file-watch`, a four-second quiet window and a 120-second gap between rebuilds), with a revision counter and an `on_revision` callback a host uses to refresh an open query. Explicit roots walk independently, canonical aliases are deduplicated, absolute excludes apply to ordinary, dotted and verbatim paths, drive classification and scan I/O are bounded per drive so an offline volume cannot stall the others. `files::scope`, `signature` and the walk now take the settings as a value; `host::file_index_scope` is still the default source. Moved from Pulse's `windows/src/launcher_files.rs`.
- New Windows-only dependency `rightkit-file-watch` (no default features); the `windows` crate gains `Win32_System_IO`.

## 0.2.1

- Exported usage history ranking with caller-provided time and capacity, compatible with saved Windows launcher records.

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
