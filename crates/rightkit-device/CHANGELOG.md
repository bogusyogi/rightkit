# Changelog

## 0.2.1 — unpublished staging

- Adds an optional atomic M11 credentials adapter.

Version dates are the dates of the version-bump commits. Publication dates are not recorded in git.

## 0.2.0 (2026-10-17)

Added:
- `device_id_hash(raw) -> String`: lowercase hex SHA-256 of the raw device id, the only form sent to lease, link and Pro-bridge endpoints (rightkit#609). Local FIPS 180-4 implementation, no new dependency; tests use the FIPS vectors.

## 0.1.3

Renamed from rightkit-tauri; 0.1.2 remains published under the old name.

Added:
- `AuthorizationService::authorize_with_clock` is public (M13-O2): hosts pass a trusted clock (for example `AntiRollbackClock::trusted_now`) instead of the wall clock. `authorize` is unchanged. Tests: valid time, time past `exp`, time before `iat`, and clock failure all deny or pass as expected. Not compiled locally; verification is the CI run.

Fixed:
- Device-ID creation is now create-only across processes: `DeviceService::with_lock_file` serializes creators with an exclusive file lock, and `SecureStore::set_if_absent` (default check-then-set; override with an atomic primitive) makes a losing creator adopt the winner's ID. `DeviceService::new` and existing stores are unchanged.

- Requires `rightkit-logs` 0.2.2 (tracing feature release); no API change.

### Earlier unreleased notes

Changed:
- Ownership declaration gains fingerprints for declaration-only capabilities, at the advise tier (`bbd385ed`). `rightkit-ownership.json` only; crate code is unchanged.
- Repository URL moves to `https://github.com/bogusyogi/rightkit` (`9f3ec2d4`). Metadata only.

## 0.1.2 — 2026-10-09

Breaking:
- Minimum supported Rust version is raised from 1.81 to 1.98.0 (`343654bf`).

Changed:
- `getrandom` moves from `=0.2.17` to `0.3`. Device IDs call `getrandom::fill` in place of `getrandom::getrandom`. The ID format is unchanged (`ca41ba38`).
- The `rightkit-license` dependency changes from an exact pin (`=0.1.3`) to a caret requirement (`0.1.3`) (`ee6ec860`).
- Added the ownership declaration `rightkit-ownership.json`, which is declaration-only (`7acf96a8`).
- README documents the public surface, the lack of Cargo features, platform support, and a non-compiled example. It states that the crate has no direct `tauri` dependency (`30e300e4`, `ca41ba38`).

## 0.1.1 — 2026-07-17

Breaking:
- `MutationHook::after_set` and `MutationHook::after_delete` no longer return `Result` (`edb1f415`). Storage has already committed when they run, so they are infallible. Implementors must drop the `Result` return.
- `SecureService::get`, `set`, and `delete` return `StorageError` (fields `operation` and `message`) in place of `String` (`edb1f415`).
- `get_or_create_device` is removed (`edb1f415`). Use `DeviceService::new(store).get_or_create()`. The app no longer supplies a fallback value. The crate generates the device ID (`0367b76f`).
- `LicenseRecord::device_id` is removed. Only `signed_token` remains (`edb1f415`).
- `verified_entitlement` and `VerificationError` are removed (`edb1f415`). Use `AuthorizationService::authorize`, which returns `AuthorizationError`.
- `AuthorizationService::authorize` takes no time argument. The crate reads the clock itself (`affcd514`). Within this window, `edb1f415` had a `now_unix: i64` parameter, which `affcd514` removed. The version-0.1.0 `verified_entitlement` took `now_unix`, so the parameter has no direct replacement.

Added:
- `DeviceService<S>` with `new`, `get`, and `get_or_create`. A creation lock makes read, create, and write atomic per instance. A poisoned lock fails closed with `StorageOperation::DeviceLock` (`edb1f415`, `fb64d221`).
- Device IDs are 128-bit random values in the RFC 4122 version-4 layout, from `getrandom` (`0367b76f`). An RNG failure maps to `StorageOperation::GenerateDeviceId` (`63bb9fa5`).
- `StorageOperation` and `StorageError` (`edb1f415`).
- `AuthorizationService`, `AuthorizationDenial` (missing license, missing token, missing device, invalid token, unmet requirement), `AuthorizationError`, `PaidCommandError`, and `guard_paid_command` (`edb1f415`, `affcd514`).
- `LicenseRequirement`, re-exported from `rightkit-license` (`affcd514`). `edb1f415` had a local enum, which `affcd514` replaced.
- `LogHost` and `LogService`, `DiagnosticHost` and `DiagnosticService`, `HostOperation`, and `HostOperationError`. Each host error names the operation that failed (`edb1f415`).
- Tests: fail-closed device creation with RNG and poisoned-lock failures (`fb64d221`, `63bb9fa5`), and a private clock seam for deterministic expiry tests (`affcd514`).
- `LICENSE-MIT` and `LICENSE-APACHE` are bundled and listed in `include` (`affcd514`).

Changed:
- The `rightkit-license` pin moves from `=0.1.1` to `=0.1.2` (`31490a97`) and then to `=0.1.3` (`27b96e6e`).

Fixed:
- Device creation is atomic per `DeviceService`. Concurrent callers serialize on one lock and share one stored ID (`edb1f415`). Version 0.1.0's `get_or_create_device` had no lock, so concurrent callers could each see no device and each write one.
- Through the `rightkit-license` pin, malformed requirements and entitlement tiers now fail closed. That fix is published as `rightkit-license` 0.1.2 (`31490a97`).

## 0.1.0 — 2026-07-14

Added:
- Initial crate (`32784006`). `SecureStore` and `MutationHook` traits, `NoopHook`, and `SecureService`, which runs the hook after the store succeeds. `get_or_create_device` seeds a device ID from a caller-supplied fallback. `LicenseRecord` and `VerificationError`. `verified_entitlement` verifies a signed token through `rightkit-license` and fails closed on a missing token.
- The README marks the crate as internal during Phase 3, to be published only after all five core apps pass the shared native contract (`32784006`).
