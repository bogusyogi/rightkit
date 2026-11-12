# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-context-protocol` 0.1 -> 0.2. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## 0.1.1 (unpublished)

- `ProviderError::Typed.details` is now `Option<Box<serde_json::Value>>` (clippy `result_large_err`: the error was over 128 bytes). Construct with `Some(Box::new(value))`.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane-provider-sdk` (engine).
