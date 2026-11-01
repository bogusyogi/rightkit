# rightkit-disk-cleanup

Disk usage scanning and safe cleanup for desktop apps.

- `scan`: bounded, metadata-only filesystem scan that never follows links and records volume, snapshot and clone identity.
- `rules`, `cleanup_scan`, `cleanup`: cleanup rules with evidence, reclaimable-space findings, and an apply/undo journal that revalidates every item before it moves.
- `duplicates`, `folder_growth`, `history`, `storage_browser`: duplicate files, folder growth between scans, scan history and a read-only browser over a report.
- `store`, `snapshot`, `activity`: crash-safe snapshot store with bounded reads and the applied-cleanup activity log.

The host app names its per-app state folder once with `cleanup_scan::set_state_app_name` (default `RightKit`).

Moved into RightKit from Pulse's shared core on 2026-11-01.
