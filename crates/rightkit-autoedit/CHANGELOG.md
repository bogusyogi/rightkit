# Changelog

## 0.1.2 (unpublished)

- Calls `rightkit_ffmpeg::verify_ffmpeg_handle` / `verify_ffprobe_handle` (renamed from `verify_*_or_development`); requires `rightkit-ffmpeg` 0.2.16.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- New, additive (feature `engine-commands`, off by default): the `cmd` module, engine command handlers moved from CutRight's `cutright-engine`: `cmd::autoedit` (auto-edit analysis, plans and apply), `cmd::autonomy` (persistent autonomy levels), `cmd::rules` (scoped editing rules and resolution), `cmd::style` (`style.measure`) and `cmd::mic` (`mic.align`). Adds optional dependencies on `rightkit-engine-host` (feature `project-cmd`) and `chrono`.

## 0.1.0 (unreleased)

- First RightKit release: CutRight's cuts, editorial, variants and rules crates as modules of one crate.
- Real-FFmpeg tests use `RIGHTKIT_FFMPEG` and the missing-asset policy.
