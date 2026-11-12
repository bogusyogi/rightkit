# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-crash` 0.3 -> 0.4, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-logs` 0.3 -> 0.4. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## 0.1.1 (unpublished)

- Removed `crash_retention_for_test` and the `test_binary_data_root` re-export (Adrian, 2026-11-03: "My decision was no unit tests. Only e2e."; "dev mode, is mine ... real data. snapshot and restore.").
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-telemetry` (tauri-app-next).
