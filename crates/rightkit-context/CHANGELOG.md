# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-context-client` 0.1 -> 0.2, `rightkit-context-protocol` 0.1 -> 0.2, `rightkit-crash` 0.3 -> 0.4, `rightkit-memory-core` 0.1 -> 0.2. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane` (engine).
