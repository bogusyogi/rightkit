# Changelog

## 0.1.1 (unreleased)

- `store` module: `FileHistoryStore` (JSON file store, atomic write per mutation) and, with the new `sqlcipher` feature, `SQLCipherHistoryStore`, key rotation, the one-time legacy JSON migration and lost-key recovery, moved from HeardRight (`history/*`). The application selects the SQLCipher build by enabling a `sqlcipher*` feature of `rightkit-db`.
- `host` module: `HistoryHost` and `KeyVault`, the application seam (data directory, credential store, log, sync consent) installed with `install_host`.
- The encrypted database file name is `HistoryHost::sqlite_file_name` (default `history.sqlite`); existing installs keep their name. Test fault-injection switches are `RIGHTKIT_DEV_*` (with `test-mode`).

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight.
