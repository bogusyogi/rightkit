# rightkit-tauri

Native host services for Right Suite Tauri applications. It provides generic
secure-storage mutation ordering with app hooks, durable device reconciliation,
and fail-closed signed-entitlement verification through `rightkit-license`.

Platform keyring implementations, app command registration, product protocols,
and app-specific post-mutation behavior remain in thin host adapters.

This crate is internal during Phase 3. Publish it only after all five core apps
pass the shared native contract; apps must then use one exact crates.io pin.
