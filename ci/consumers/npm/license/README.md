# @rightkit/license npm consumer (npm.@rightkit/license)

Packed-archive consumer for `@rightkit/license` 0.3.0 (the workspace version). The unit is installed only from the sealed `file:` archive. The reviewed closure has no registry dependencies, so the consumer manifest declares none.

Surface (exact `exports` keys): `.` and `./testing`. The consumer asserts the sorted key list of the root entry (14 names) and the shape of the `./testing` condition map.

Scenarios (8): export surface exact and `./testing` unresolvable under default conditions; status projection keeps only the safe view fields and refuses malformed views; the five frozen native command names and transient-input validation; native error mapping to fixed English text with no raw detail; the change subscription (projection, stop on a bad payload, idempotent cleanup); access helpers fail closed (busy, unresolved, tombstone, disabled, expired, revoked, unverifiable); the fixed message catalogue; the browser-QA mock, loaded in a child Node process with `--conditions=development`.

What it proves: the public client works against a hand-written `invoke` and `listen` pair that stands in for the Tauri bridge. It sends exactly `rightkit_license_status`, `rightkit_license_activate`, `rightkit_license_refresh`, `rightkit_license_deactivate` and `rightkit_license_request_trial`, listens on `rightkit://license-changed`, discards unknown fields, never lets a raw native message, token or cause reach a `LicenseClientError`, and treats busy or unresolved views as unlicensed. The `./testing` export is blocked in default resolution and resolves only under the `development` condition, where `createLicenseMock` runs a full activate, refresh, deactivate and trial cycle in memory and carries the `RIGHTKIT_LICENSE_TESTING_DO_NOT_SHIP` sentinel.

What it does not prove: the native side. No Tauri runtime, no `rightkit-license` crate, no licence server, no credentials, no persistence and no system clock are involved, so the real command results, lifecycle transitions and entitlement authorization stay with the native tests. Declaration files are not type-checked. The final-artifact sentinel scan of an app bundle and the production export-condition gate belong to the app release checks, not to this recipe.

Required environment: `RIGHTKIT_CONSUMER_ARTIFACT` (absolute `.tgz` path), `RIGHTKIT_CONSUMER_ARTIFACT_SHA256`, `RIGHTKIT_CONSUMER_EXPECTED_VERSION` and `RIGHTKIT_CONSUMER_RESOLUTION` (`packed` before upload, `fetched` after registry readback). The runner provides them.

`provenance.mjs` is the shared helper: it compares the SHA-256 of every regular file in the supplied archive with the installed file and returns the checked export URLs the consumer imports. It is a byte-identical copy of the helper beside the other npm recipes.

No network, no user data, no credentials. The only child process is the local Node executable for the `development` condition check. One JSON line on stdout, exit 0 only when all eight scenarios pass. `nativeHostQualified` is always false.
