# Changelog

## 0.1.1 (unreleased)

- New, additive: `env::Env`, the engine's process environment (operating system, executable, app data root, home) with `resource_dir()`; and `apps`, installed-app discovery for an `apps` command (`AppEntry`, `discover_installed`, `discover_with` with an injectable `Host`, `apps_report`), moved from CutRight's `cutright-engine`. Adds dependencies on `rightkit-daemon` and the `doctor` feature of `rightkit-job-queue`.

## 0.1.0 (unreleased)

- First RightKit release, built from the engine framework pieces of GenRight (`genright-engine`, `genright-protocol`).
- `doctor`: the engine doctor report (identity, contract version, capabilities, platform, install location, data-folder readiness, host readiness merged in), from `genright-engine` `main_doctor.rs`.
