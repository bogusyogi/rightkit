# Changelog

## 0.1.1 (unreleased)

- Added `command`: the `research` command (host-opened-record search and the pure ledger, citecheck, meter, retraction and patch steps) as `ResearchOptions` and `run`, moved from Legion's `commands/research.rs`. The app maps argv onto the options and links its signal handling through `Cancellation::from_probe`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from `legion-research` (engine).
