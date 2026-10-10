# Changelog

## 0.1.4

- Spawn-claim locking uses std `File::try_lock` instead of a direct `libc::flock`; the direct `libc`
  dependency is removed (the remaining Unix FFI in `transport.rs` was already hand-declared).
- `write_atomic` (service records and takeover requests) now writes through
  `rightkit_fs::atomic_write_private`: unique temp file, `fsync` of file and directory, owner-only.
  The Windows transient "access denied" rename retry is kept. Adds the `rightkit-fs` dependency
  (`fs` feature only).
