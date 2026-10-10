# Changelog

## 0.3.2

- Internal: model-digest and pipeline-fingerprint SHA-256 now go through `rightkit_fs::digest`
  (`digest` feature only); the direct `sha2` 0.10 dependency is gone. Fingerprints are unchanged.
- `VectorStore::open` / `in_memory` open the connection through `rightkit-db`'s bootstrap
  (`DbOptions::open_writer`, `open_in_memory`): WAL with verified fallback, 5 s busy timeout,
  owner-only database file. Bootstrap failures that are not SQLite errors surface as
  `VectorError::Sqlite` with `SQLITE_CANTOPEN` and the original message. `from_connection` is
  unchanged for callers that bring their own connection.
