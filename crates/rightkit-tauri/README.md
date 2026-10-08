# rightkit-tauri

Framework-agnostic native host services for Right Suite Tauri applications. The crate has no direct `tauri` dependency.

This crate owns:

- typed secure-storage mutation ordering;
- one atomic, serialized device get-or-create service per app host, with a
  crate-owned OS-CSPRNG UUIDv4 generator and no renderer-supplied fallback;
- fail-closed authorization from a native license store plus a separate,
  authoritative native device store;
- signed-token verification of app, device, and time before Pro-tier or named
  feature requirements are evaluated;
- a paid-command guard that cannot call product logic after denial; and
- typed log and diagnostic host bridges.

**Use it** in a Right Suite Tauri app's native host, to get the shared device, license, and command-guard logic.

**Do not use it** from the renderer, or as a substitute for the app's own keyring and command wrappers.

## Tauri boundary

There is intentionally no direct `tauri` dependency. Each app owns its platform
keyring implementations, `#[tauri::command]` wrappers, managed-state registration,
command names, response serialization, and product-specific behavior. The app
stores exactly one `DeviceService` in native managed state and delegates command
logic to these services. This keeps authorization independent of renderer input
and avoids tying the shared security layer to an app's Tauri state shape or Tauri
version. `DeviceService` intentionally exposes no general device-ID setter or
delete method for command wrappers; renderer input must never establish the
authorization identity. A future device-rotation flow must be a distinct native
operation with its own reconciliation contract.

`MutationHook` is infallible by design. It runs only after storage commits; a
fallible post-commit hook would make a successful mutation look rolled back when
it was not. Hosts that need fallible follow-up work must expose that as a separate
operation with its own outcome.

## Public surface

- `SecureStore<T>`: the get, set, and delete trait that each app implements over its keyring.
- `LicenseRequirement` (re-exported from `rightkit-license`).
- `DeviceService`: the single managed device service described above.

## Features

None. The crate has no Cargo features. Dependencies: `getrandom` (pinned `=0.2.17`) and `rightkit-license`.

## Platform support

The crate has no platform-specific code. Keyring and Tauri integration live in each app.

Published version: see `INDEX.md` at the repository root. This crate has no `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage of the services, see `tests/services.rs`.

```rust,ignore
// In the app's native host: implement SecureStore over the OS keyring,
// build one DeviceService, store it in Tauri managed state, and call it
// from the app's #[tauri::command] wrappers.
```
