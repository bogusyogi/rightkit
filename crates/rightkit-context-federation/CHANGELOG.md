# Changelog

## 0.2.1

- Requires `rightkit-repo-graph` 0.3 (was 0.2): its `framework_intelligence::FiFile` now borrows its text. No source change in this crate (#1572).

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-context-core` 0.1 -> 0.2, `rightkit-context-protocol` 0.1 -> 0.2, `rightkit-context-provider-sdk` 0.1 -> 0.2, `rightkit-repo-graph` 0.1 -> 0.2. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `membrane-federation` (engine).
