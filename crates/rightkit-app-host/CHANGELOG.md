# Changelog

## 0.1.1 (unpublished)

- README: the crate is optional (an app needs it only with the five companion crates that read it) and does not repeat a state-root or host registry elsewhere in RightKit; no code change. Recorded by the 2026-11-11 shape review redraw (`docs/audits/2026-11-11-shape-matrix/reports/pulse-crates-redraw.md`), pending the owner's confirmation.
- Manifest only: dependency requirements and workspace metadata changed since 0.1.0 was published; no source change.

## 0.1.0 (unreleased)

- First release: `AppHost` trait, `install`/`host`, `NeutralHost`, and helpers (`state_root`, `state_directory`, `account_home`, `env_var`, `settings`, `setting_bool`, `published_state`). Replaces the direct reads of Pulse's store and notch state in the moved crates.
