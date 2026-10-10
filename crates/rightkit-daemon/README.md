# rightkit-daemon

Daemon host capability implementing the `daemon-api` v1 contract (`contracts/daemon-api.v1.schema.json`). `publish = false`.

## Provider slug rule

The `{provider}` segment of `/v1/oauth/{provider}/callback` and `/v1/webhooks/{provider}` must match `^[a-z0-9][a-z0-9-]{0,63}$` (lowercase letters, digits and hyphens; 1 to 64 characters; no leading hyphen). The route table enforces it at dispatch (`routes::is_provider_slug`): anything else is not a route and answers `404 not_found` before authentication or the OAuth flow. Use the same slug as the key in `DaemonConfig::webhook_secrets`; a key outside the rule can never be reached.

## Drift guards

`tests/conformance.rs` pins the route table, fixtures, error codes and lifecycle events to the schema. Events the daemon cannot emit are listed under `x-reservedEvents` in the schema.
