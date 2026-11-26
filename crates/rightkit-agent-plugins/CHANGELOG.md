# Changelog

## 0.1.4

- `PluginLoader::install` (stages in a hidden folder and swaps with renames, keeps no previous version, never follows a symlink), `PluginLoader::remove`, `InstalledPlugin`, `ReplacedPlugin` and a public `validate_plugin_id`, all re-exported at the crate root. The id check now also refuses a leading dot.
- `PluginLoader::discover` skips hidden folders. `enable` and `disable` fail with `Refused` or `NotFound` for an unsafe id or a plugin folder that is gone, instead of creating the folder.
- `PluginManifest::active_ui_contributions`: the contributions a host may show, none for a disabled plugin or invalid slot declarations.
- The plugin process variables `PLUGIN_NETWORK`, `PLUGIN_ROOT`, `WORKSPACE_ROOT` and `PLUGIN_ENV_INHERITED` take the prefix of `rightkit_agent_config::env_prefix()` (default `RIGHTKIT`, so the names are unchanged unless a host calls `set_env_prefix`). New dependency on rightkit-agent-config 0.1.3.

## 0.1.3

- Depends on rightkit-sandbox 0.7.0. `run_plugin_entry_with_policy_and_adapter` takes the 0.7.0 `ProcessLaunchAdapter`; hosts must depend on the same rightkit-sandbox version.

## 0.1.2

- Publishes the source changes made since 0.1.1 was published under the same version number (src/error.rs, src/runtime.rs).

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-plugins` (CodeRight).
- New `PluginError::Refused` and `Conflict`; the plugin process environment is built in one place (inherit, allow and deny lists, network variables stripped).
