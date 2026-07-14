# rightkit-tauri

Framework-agnostic native host services for Right Suite Tauri applications.

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

This crate is internal during Phase 3. Publish it only after all five core apps
pass the shared native contract; apps must then use one exact crates.io pin.
