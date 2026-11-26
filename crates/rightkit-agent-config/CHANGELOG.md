# Changelog

## 0.1.4

- Publish the dependency requirements main already declares: `rightkit-roots` 0.3.0, where the published 0.1.3 requires 0.2.0; `rightkit-fs` 0.5.0, where the published 0.1.3 requires 0.4.9. The 0.1.3 manifest on crates.io stays as published; this version carries the new requirements.

## 0.1.3 (2026-11-16)

- `set_workspace_trust_rule(fn(&Path) -> bool)` (once per process, returns `false` if already set): a host with no trust-store file supplies the workspace-trust decision, which replaces the persisted-store lookup in `is_workspace_trusted`. Default unchanged. `is_workspace_trusted_in` still reads the store it is given.

## 0.1.2

- Requires `rightkit-agent-policy` 0.2.0 (dependency requirement raised; no API change here).

## 0.1.1

- Host naming options, defaults unchanged: `set_state_dir_name` / `state_dir_name()` (default `.rightkit`; the workspace config, presets, agents folder, trust-store fallback) and `set_env_prefix` / `env_prefix()` / `env_name(suffix)` (default `RIGHTKIT`; the model, permission-mode, web-search, Telegram and CLI-acquisition overrides in `apply_env`), beside the existing `set_app_name`. Call once at start-up. The licence and trust-store variables keep their `RIGHTKIT_` names.

## 0.1.0 (unpublished)

- New crate. The agent host configuration (layered TOML `HostConfig`, `ConfigLoader`, state root and app name, workspace trust store, instructions, presets, accounts, licence environment names, session secrets, OKF bundles) moved here from `rightkit-app-settings` 0.3.5, where it was the `agent_config` module behind the `agent_config` feature (added in 0.3.1, moved from CodeRight `coderight-config`). The 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-foundation-shell-ui.md) found it is agent product configuration, not app settings, and that it pulled `rightkit-daemon` and `rightkit-agent-policy` into a crate every desktop app adopts.
- Paths: `rightkit_app_settings::agent_config::X` is now `rightkit_agent_config::X`; the module tree under it is unchanged (for example `rightkit_agent_config::state_root::state_root`).
- Feature: `agent_config-host-env` is now `host-env`. There is no `agent_config` feature; the whole crate is the configuration.
- Dependencies: `rightkit-roots` 0.1.0 replaces `rightkit-daemon` for `Suite` and `SuiteError`, which moved there in rightkit-daemon 0.8.0; nothing else from the daemon was used (2026-11-11 shape review, docs/audits/2026-11-11-shape-matrix/reports/family-foundation-runtime.md). The state root is unchanged.
- No behaviour change.
