# Changelog

## 0.1.1 (unreleased)

- New, additive (feature `engine-commands`, off by default): the `cmd` module, engine command handlers moved from CutRight's `cutright-engine`: `cmd::autoedit` (auto-edit analysis, plans and apply), `cmd::autonomy` (persistent autonomy levels), `cmd::rules` (scoped editing rules and resolution), `cmd::style` (`style.measure`) and `cmd::mic` (`mic.align`). Adds optional dependencies on `rightkit-engine-host` (feature `project-cmd`) and `chrono`.

## 0.1.0 (unreleased)

- First RightKit release: CutRight's cuts, editorial, variants and rules crates as modules of one crate.
- Real-FFmpeg tests use `RIGHTKIT_FFMPEG` and the missing-asset policy.
