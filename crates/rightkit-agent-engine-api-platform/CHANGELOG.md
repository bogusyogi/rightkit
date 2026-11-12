# Changelog

## Unreleased

- New `openapi::MACHINE_STATUS_SCHEMA`, the one constant for the machine-status schema id (`coderight.machine-status.v1`, unchanged on the wire); the OpenAPI component uses it and the agent server emits and checks it.

## 0.1.2
- Synced with CodeRight: the unauthenticated `HealthResponse` is exactly `ok`, `service`, `version`, `contractVersion`, `protocolVersion` (`deny_unknown_fields`; new `CONTRACT_VERSION` / `PROTOCOL_VERSION`); the port, database path, storage pool and provider-key posture move to `MachineInfoResponse` in the OpenAPI components.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-api-platform` (engine).
