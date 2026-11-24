# Changelog

## 0.1.3

- Depends on rightkit-sandbox 0.7.0. `run_plugin_entry_with_policy_and_adapter` takes the 0.7.0 `ProcessLaunchAdapter`; hosts must depend on the same rightkit-sandbox version.

## 0.1.2

- Publishes the source changes made since 0.1.1 was published under the same version number (src/error.rs, src/runtime.rs).

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-plugins` (CodeRight).
- New `PluginError::Refused` and `Conflict`; the plugin process environment is built in one place (inherit, allow and deny lists, network variables stripped).
