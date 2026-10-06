# rightkit-models

Shared local-model manifest and installer for Right Suite apps: model revisions
and quantizations, resumable verified downloads (ETag/Range, SHA-256), hardened
archive extraction (tar.gz/zip), atomic activation with rollback, and hardware
fit (disk, RAM, GPU) preflight. Product-neutral; apps supply the manifest.

See `tests/install.rs` for end-to-end usage against a local HTTP server.
