# Changelog

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-plugins` (CodeRight).
- New `PluginError::Refused` and `Conflict`; the plugin process environment is built in one place (inherit, allow and deny lists, network variables stripped).
