# rightkit-fsindex 0.5.0 (registry consumer recipe)

Standalone consumer, outside the workspace, pinning `rightkit-fsindex =0.5.0` exactly with the default
features (`content` stays off). It uses public API only:

- Every platform: `Config::new` defaults, `WriteBudgetConfig::new` and `with_max_footprint`, the pure
  Windows Search SQL helpers (`escape_sql_literal`, `like_pattern`), `parse_usn_records` on a cursor-only
  and a truncated buffer, and `Index::open_with_backend` with the stub `UnsupportedBackend` refusing with
  `Error::Unsupported`.
- macOS: `Index::open` on a tiny temp tree (no watcher, no home directories, no state directory), then
  `search`, `folder_size`, `children_sorted_by_size`, `write_budget_status` and
  `would_exceed_write_budget` against the configured write budget.
- Other non-Windows hosts: `Index::open` is refused with `Error::Unsupported`.
- Windows: the live backend (Windows Search, USN journal) is not opened.

State: one temp directory holding three small files, removed at the end. No network, no real user data,
no state directory, no watcher.

Not covered: the live change feed, snapshots and resume, the content (trigram) feature, the macOS
FSEvents watcher and bulk listing, and the Windows backends.
