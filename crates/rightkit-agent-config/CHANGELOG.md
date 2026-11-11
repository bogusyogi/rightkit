# Changelog

## 0.1.0 (unpublished)

- New crate. The agent host configuration (layered TOML `HostConfig`, `ConfigLoader`, state root and app name, workspace trust store, instructions, presets, accounts, licence environment names, session secrets, OKF bundles) moved here from `rightkit-app-settings` 0.3.5, where it was the `agent_config` module behind the `agent_config` feature (added in 0.3.1, moved from CodeRight `coderight-config`). The 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-foundation-shell-ui.md) found it is agent product configuration, not app settings, and that it pulled `rightkit-daemon` and `rightkit-agent-policy` into a crate every desktop app adopts.
- Paths: `rightkit_app_settings::agent_config::X` is now `rightkit_agent_config::X`; the module tree under it is unchanged (for example `rightkit_agent_config::state_root::state_root`).
- Feature: `agent_config-host-env` is now `host-env`. There is no `agent_config` feature; the whole crate is the configuration.
- No behaviour change.
