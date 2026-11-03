# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added `protocol` (channel `Scope` and the publish job and command payloads), `calendar_time`, `util` and `quota`: per-account pacing and the `next` selector over a host-implemented `PublishQuotaStore`. Additive.
- Added `machine` (`MachineStore`: the phase machine, `step_once` and `reconcile_once`), `deletion` (`DeletionStore`; media removal via rightkit-fs), `accounts` (`AccountStore`), `util::WriteTxn`, `jobs::PublishJobStore` (the host's job table: load, save, transaction, role gate) and `resolve` (`publish.resolve_unknown`).

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine publish adapters, http, media, platform, api_versions; postright-protocol publish payload).
