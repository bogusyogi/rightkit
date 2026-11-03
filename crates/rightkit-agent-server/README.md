# rightkit-agent-server

Agent server host: daemon API routes, CLI and interactive-session runtime and session hosting for coding-agent applications.

Call `run()` from a thin binary. Moved into RightKit from the CodeRight engine binary on 2026-11-01.

## Licensing (Pro entitlement)

The daemon decides Pro itself and never reads the owner's credential store. The owner app (the CodeRight shell, the only binary that reads the OS store) builds a snapshot with `rightkit_license::LicenseClient::engine_snapshot` and delivers it; the daemon holds it in an `EngineSnapshotCell` and verifies it again with `EngineEntitlements::from_snapshot` (suite key, app `coderight`, the device binding the snapshot names, expiry, and the owner's anti-rollback ledger, read-only).

Owner-side contract:

- Launch the daemon with the licence locators (all non-secret; every `RIGHTKIT_LICENSE_*` variable is read once and removed from the daemon's environment): `RIGHTKIT_LICENSE_DIR=<app data>/license` (the ledger `clock.json` lives there; without it the daemon runs no gate and is Free), `RIGHTKIT_LICENSE_OWNER_EXE=<absolute path of the owner executable>`, and the owner's signing identity, `RIGHTKIT_LICENSE_OWNER_TEAM=<Apple team id>` on macOS and/or `RIGHTKIT_LICENSE_OWNER_SIGNER=<Authenticode signer common name>` on Windows. Without the owner executable and at least one signing identity the daemon hosts no delivery endpoint and stays Free; it never accepts an unsigned owner. The owner's client must keep its anti-rollback ledger as the file ledger, not the credential-store ledger.
- Deliver the snapshot over the daemon's private IPC (rightkit#1501): connect to service `engine-license-<daemon port>` with `rightkit_daemon::Client::connect_expecting` (the daemon's own executable), say `hello` as app `coderight`, and call `license.snapshot` (`rightkit_daemon::LICENSE_SNAPSHOT_METHOD`) with `{"frame": "<standard base64>"}`, the frame being one `rightkit_credentials::handoff` frame written with `write_message(&Message::reply_snapshot(id, client.engine_snapshot()))`. The result `{"ack": "<base64>"}` is the Ack frame; check it with `expect_ack(read_message(..), id)`. The daemon identifies your process first (same user, the owner executable pinned by pid and start time, and its code signature) and answers `forbidden` to anything else; `invalid_params`: not exactly one Reply frame (nothing changed). The old `POST /internal/license-snapshot` route no longer exists.
- Deliver once the daemon is reachable (right after launch) and again after every `LicenseInvalidation` (activation, refresh, deactivation, revocation). Each delivery replaces the whole snapshot.
- Until the first delivery, and after an owner read failure or a malformed snapshot, the daemon is not Pro. An owner that cannot read the licence should still deliver, with `licence.token` as a read failure, so the daemon fails closed at once.
- The daemon accepts no verdict, feature list, key or device id from any other source. Only the owner's own signed process may deliver.
