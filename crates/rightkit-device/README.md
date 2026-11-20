# rightkit-device

Framework-agnostic native host services for Right Suite Tauri applications. The crate has no direct `tauri` dependency.

This crate owns:

- typed secure-storage mutation ordering;
- one atomic, serialized device get-or-create service per app host, with a
  crate-owned OS-CSPRNG UUIDv4 generator and no renderer-supplied fallback;
  A blank stored id (empty or whitespace only) counts as absent and is replaced; a
  non-blank id is never replaced.

Licence authorization lives in `rightkit-license`; the log and diagnostic host bridges moved to
`rightkit-logs` (still re-exported here for one release).

**Use it** in a Right Suite Tauri app's native host, to get the shared device-identity and secure-storage logic.

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
- `DeviceService`: the single managed device service described above.
- `device_id_hash(raw)`: lowercase hex SHA-256 of raw device id for lease, link & Pro-bridge requests.
- Also public in `src/lib.rs`: `SecureService` and `MutationHook`.

## Features

`credentials` enables OS credential-store adapter; `license-client` enables that adapter & its `rightkit_license::DeviceIdSource` implementation. Default features are empty. Dependencies include `getrandom` 0.4, `rightkit-license` 0.3.3 & `rightkit-logs`; credential features add `rightkit-credentials` & `serde_json`.

## Platform support

The crate has no platform-specific code. Keyring and Tauri integration live in each app.

Published version: see `INDEX.md` at the repository root. Changes are recorded in `CHANGELOG.md`.

## Example

Illustrative; not compiled as a doctest. For compiled usage of the services, see `tests/services.rs`.

```rust,ignore
// In the app's native host: implement SecureStore over the OS keyring,
// build one DeviceService, store it in Tauri managed state, and call it
// from the app's #[tauri::command] wrappers.
```

## Lazy host service (0.5.2)

`host_service::LazyDeviceService<S>` lazily holds one shared `Arc<DeviceService<S>>`; host supplies store construction & log sink. Successful construction is cached; failed construction is logged & retried on next call. `device_id_reported` delegates to `DeviceService::get_or_create` & logs storage errors without logging identity. `UnsupportedDeviceStore` refuses storage on unsupported platforms.

Host retains credential item names, storage format, interaction policy, command wrappers & its static cell. Module requires no additional feature or dependency.
