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

Also (0.2.1):

- `Store::adopt_then_install`: partial adoption in one locked install: adopt
  every present, pin-matching file (hard link, else copy), download only the
  missing or mismatching ones, one atomic swap. Pins are required; nothing is
  recorded until every file verifies; preflight counts only bytes still needed.
- `Store::with_legacy_alias_map` / `with_legacy_alias_paths`: per-id alias
  locations, including inside the store root (`<root>/<id>/rN`, never a
  store-owned name) or a flat runtime dir allowed with
  `Store::with_legacy_alias_allowed_root`. Real paths are never replaced.
- `Store::with_url_rewrite(base)`: test/dev seam that sends downloads to a
  loopback mock (`http://` on `127.0.0.0/8`, `localhost` or `[::1]` only;
  never read from the environment). `Store::download_url` shows the result.

See `tests/install.rs`, `tests/adoption.rs`, `tests/partial_alias_rewrite.rs`
and `tests/e2e_resume.rs` (rightkit-qa `MockServer`: Range, dropped
connections, servers without resume) for end-to-end usage.
