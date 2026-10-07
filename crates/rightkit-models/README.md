# rightkit-models

Shared local-model manifest and installer for Right Suite apps: model revisions
and quantizations, resumable verified downloads (ETag/Range, SHA-256), hardened
archive extraction (tar.gz/zip), atomic activation with rollback, and hardware
fit (disk, RAM, GPU) preflight. Product-neutral; apps supply the manifest.

Also (0.2.0):

- `Store::adopt` / `adopt_with`: verify an existing install against the pins
  and record it without downloading (hard link on the same volume, else copy;
  atomic swap; fails closed, never records a partial adoption).
- `Store::resolve` / `resolve_or_legacy`, and the opt-in legacy flat alias
  (`Store::with_legacy_alias_root`: `<alias root>/<id>` links to the active
  revision, maintained on install, adopt, rollback, removal, uninstall and GC).
- Disk preflight credits staged, same-key installed (hard-linked) and shared
  files whose pins match (`Event::PreflightCredit`).
- Shared runtimes: `FileSpec::shared = Some(kind)` places a file under the
  app-configured `Store::with_shared_root(kind, dir)`; traversal-checked,
  pinned, never overwritten (`ErrorCode::Conflict`), kept on uninstall.
- `InstallOptions::pause` keeps staging and resumes (HTTP Range with a strong
  ETag, else a clean restart); `cancel` discards staging. `Store::paused`,
  `Store::discard_staging`.
- `Requirements::{cuda_vram_gb, mps_memory_gb, remote_only}`,
  `FitKind::{RentalOnly, Api}`, `Hardware::probe_with(HardwareOverride)`
  (app-supplied path or JSON, lenient shape), `ProfileAlias`.

See `tests/install.rs` and `tests/adoption.rs` for end-to-end usage against a
local HTTP server.
