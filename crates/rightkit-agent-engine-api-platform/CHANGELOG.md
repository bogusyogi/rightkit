# Changelog

## 0.1.1

- Synced with CodeRight: the unauthenticated `HealthResponse` is exactly `ok`, `service`, `version`, `contractVersion`, `protocolVersion` (`deny_unknown_fields`; new `CONTRACT_VERSION` / `PROTOCOL_VERSION`); the port, database path, storage pool and provider-key posture move to `MachineInfoResponse` in the OpenAPI components.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-api-platform` (engine).
