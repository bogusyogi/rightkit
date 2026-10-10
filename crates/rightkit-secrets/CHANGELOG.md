# Changelog

## 0.1.3

(0.1.2 was already on crates.io; the entries below ship as 0.1.3.)

- Dropped the `base64` dependency: minted DB keys (`get_or_create_key`) are encoded by a 15-line
  in-crate standard-base64 helper with RFC 4648 vector tests. Output is unchanged.

- **`WindowsTarget::KeyringCompat` is byte-compatible with the `keyring` crate's windows-native store.** Writes store UTF-8 text as UTF-16LE with no terminator (non-UTF-8 secrets stay raw bytes); reads decode UTF-16LE blobs back to UTF-8 and pass 0.1.x UTF-8 and binary blobs through. Existing keyring-written items no longer read back as NUL-interleaved bytes. The default `Slash` target is unchanged.
