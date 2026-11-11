# Changelog

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- New (feature `engine-commands`): `cmd::retention`, the audience-retention feedback commands (`retention.import`, `retention.list`, `retention.proposals`, `retention.apply`): a retention CSV bound to one render becomes reviewable cut proposals and one timeline revision. Moved, code unchanged, from `rightkit-social-analytics` 0.1.x, which no longer depends on video crates.
- Breaking (feature `engine-commands`): requires `rightkit-engine-host` 0.2.0 (the `Env` the commands take) and `rightkit-video-project` 0.1.3; the shared project-method helpers are `rightkit_video_project::engine` (were `rightkit_engine_host::project`).

## 0.1.2 (unpublished)

- Calls `rightkit_ffmpeg::verify_ffmpeg_handle` / `verify_ffprobe_handle` (renamed from `verify_*_or_development`); requires `rightkit-ffmpeg` 0.2.16.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- New, additive (feature `engine-commands`, off by default): the `cmd` module, engine command handlers moved from CutRight's `cutright-engine`: `cmd::autoedit` (auto-edit analysis, plans and apply), `cmd::autonomy` (persistent autonomy levels), `cmd::rules` (scoped editing rules and resolution), `cmd::style` (`style.measure`) and `cmd::mic` (`mic.align`). Adds optional dependencies on `rightkit-engine-host` (feature `project-cmd`) and `chrono`.

## 0.1.0 (unreleased)

- First RightKit release: CutRight's cuts, editorial, variants and rules crates as modules of one crate.
- Real-FFmpeg tests use `RIGHTKIT_FFMPEG` and the missing-asset policy.
