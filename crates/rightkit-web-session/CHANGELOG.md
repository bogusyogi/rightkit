# Changelog

## Unreleased

- The chat text it relays is masked by `rightkit_remote_gateway::redact` (the gateway core's redaction, unchanged) instead of `rightkit_owner_approvals::redact`, which moved. No behaviour change.

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (pulse-core browser/*).
