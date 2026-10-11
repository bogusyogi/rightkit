# rightkit-daemon

Daemon host capability implementing the `daemon-api` v1 contract (`contracts/daemon-api.v1.schema.json`). `publish = false`.

## Provider slug rule

The `{provider}` segment of `/v1/oauth/{provider}/callback` and `/v1/webhooks/{provider}` must match `^[a-z0-9][a-z0-9-]{0,63}$` (lowercase letters, digits and hyphens; 1 to 64 characters; no leading hyphen). The route table enforces it at dispatch (`routes::is_provider_slug`): anything else is not a route and answers `404 not_found` before authentication or the OAuth flow. Use the same slug as the key in `DaemonConfig::webhook_secrets`; a key outside the rule can never be reached.

## Drift guards

`tests/conformance.rs` pins the route table, fixtures, error codes and lifecycle events to the schema. The schema example, the valid fixture and `routes.rs` must list the same routes and statuses. Error codes the daemon cannot return are listed under `x-reservedErrors`, and lifecycle events it cannot emit under `x-reservedEvents`.

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 95.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: 1 per second.
- Disk operations ceiling: 1 operations per second.
- Required scale: 10,000 submissions or more.
- Required hosts: macOS and Windows.
- Status exposure: `Daemon::status()` in `src/daemon.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
