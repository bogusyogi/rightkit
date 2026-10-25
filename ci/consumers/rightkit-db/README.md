# rightkit-db 0.5.3 registry consumer (PK03)

Standalone consumer, outside the workspace, pinning `rightkit-db =0.5.3` exactly with the `bundled`
feature so no system SQLite is needed. It exercises the plain-SQLite public API: `DbOptions::new`
with `open_writer_migrated` (two versioned migrations applied once, then a reopen that applies
none), `write_txn` with a committed insert read back through `open_read_only`, `open_existing_writer`
refusing a missing file, a read-only `ReadPool`, and `open_in_memory`.

State: a temp directory removed on exit. No network, no OS keychain, no real user data. Run only by
the CI consumer lane.

Not covered: the encrypted lifecycle (`sqlcipher` features, `ProtectedKeyStore`, `rotate_key`,
`recover_rotation`, `relocate`, `StoreTransition`). It needs a SQLCipher build and key material, so
PK03-3 to PK03-6 must add their own gate rows. This recipe must be revisited if the plain-SQLite API
named here changes.
