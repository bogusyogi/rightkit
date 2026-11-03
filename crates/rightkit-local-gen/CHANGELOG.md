# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-local-image` (genright).
- `service_host`: the `local_image.*` named commands over one lazily started `LocalImageService` per host (`Host::dispatch`, `Host::warm`, `Host::shutdown`), with `HostError` codes taken from the service's `snake_case:` message prefix. Moved from `genright-engine` `local_image_service.rs`.
- `probe`: the debug-build-only machine-lock probe (`<engine> local-model acquire|status|identity|sleep|spawn-orphan|tied-owner|memory-launch`), now taking the data folder from the caller. Moved from `genright-engine` `probe.rs`.
