# Changelog

## 0.1.1

- Ships the change below, made after 0.1.0 was published, under a new version so the repository never differs from the published 0.1.0 (#1572).
- The chat text it relays is masked by `rightkit_remote_gateway::redact` (the gateway core's redaction, unchanged) instead of `rightkit_owner_approvals::redact`, which moved. No behaviour change.

## Unreleased

- The product name comes from the app host (`rightkit_app_host::AppHost::product_name`, default `RightKit`) in the "runs <name> as root" and "<name> Settings, agentBrowser" refusals (they said "the app" / "the app's settings") and in the thread names (`<name lower case>-browser`, `<name lower case>-browser-watch`; they were `rightkit-browser...`).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (pulse-core browser/*).
