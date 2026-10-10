# Changelog

## 0.1.2 - 2026-10-10

- Drop `parking_lot`: the registry uses `std::sync::Mutex` and recovers a poisoned lock, which preserves the previous no-poisoning behaviour.
- Drop the pinned `sha2 =0.10.9`: the source fingerprint uses `rightkit_fs::digest::sha256_bytes` (`rightkit-fs` with only the `digest` feature). Fingerprints are byte-identical to before, so cached derivatives stay valid.
- No public API changes.
