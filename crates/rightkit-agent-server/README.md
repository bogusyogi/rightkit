# rightkit-agent-server

Agent server host: daemon API routes, CLI and interactive-session runtime and session hosting for coding-agent applications.

Call `run()` from a thin binary. Moved into RightKit from the CodeRight engine binary on 2026-11-01.

## Licensing (Pro entitlement)

The daemon decides Pro itself and never reads the owner's credential store. The owner app (the CodeRight shell, the only binary that reads the OS store) builds a snapshot with `rightkit_license::LicenseClient::engine_snapshot` and delivers it; the daemon holds it in an `EngineSnapshotCell` and verifies it again with `EngineEntitlements::from_snapshot` (suite key, app `coderight`, the device binding the snapshot names, expiry, and the owner's anti-rollback ledger, read-only).

Owner-side contract:

- Launch the daemon with `RIGHTKIT_LICENSE_DIR=<app data>/license` (the only licence locator; non-secret; the ledger `clock.json` lives there). Without it the daemon runs no gate and is Free. The owner's client must keep its anti-rollback ledger as the file ledger, not the credential-store ledger.
- Deliver the snapshot: `POST /internal/license-snapshot` with the daemon's per-launch bearer token, `Content-Type: application/octet-stream`, body one `rightkit_credentials::handoff` frame written with `write_message(&Message::reply_snapshot(id, client.engine_snapshot()))`. The response body is the Ack frame; check it with `expect_ack(read_message(..), id)`. `400`: not exactly one Reply frame (nothing changed). `409`: this daemon runs no licence gate.
- Deliver once the daemon is reachable (right after launch) and again after every `LicenseInvalidation` (activation, refresh, deactivation, revocation). Each delivery replaces the whole snapshot.
- Until the first delivery, and after an owner read failure or a malformed snapshot, the daemon is not Pro. An owner that cannot read the licence should still deliver, with `licence.token` as a read failure, so the daemon fails closed at once.
- The daemon accepts no verdict, feature list, key or device id from any other source. Only the owner's own process, holding the daemon's launch token, may deliver.
