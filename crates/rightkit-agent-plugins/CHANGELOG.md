# Changelog

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-plugins` (CodeRight).
- Synced with the product plugin loader: local-folder install (`PluginLoader::install`, `InstalledPlugin`, `validate_plugin_id`) with an atomic stage-and-swap and no retained previous version; the plugin marketplace (`Marketplace`, `MarketplaceEntry`, `CatalogSort`), scopes (`PluginScope`, `ScopedPlugins`), `versions`, `available_rollback_versions` and `rollback` are removed; hidden folders are not discovered; new `PluginError::Refused` and `Conflict`; the plugin process environment is built in one place (inherit, allow and deny lists, network variables stripped).
