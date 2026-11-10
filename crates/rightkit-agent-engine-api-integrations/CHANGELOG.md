# Changelog

## 0.1.2

- Publishes the source changes made since 0.1.1 was published under the same version number (src/discovery.rs, src/plugin_manifest.rs).

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-api-integrations` (CodeRight).
- Synced with CodeRight: `discover_plugin_report` / `PluginDiscovery` / `BrokenPlugin` (one broken `plugin.json` no longer hides the others), `default_plugin_dir` fails instead of falling back to the working directory, and the plugin signature provenance module is removed.
