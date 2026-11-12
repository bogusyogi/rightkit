# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-app-settings` 0.3 -> 0.4, `rightkit-embed` 0.6 -> 0.7, `rightkit-job-queue` 0.5 -> 0.6, `rightkit-models` 0.5 -> 0.7, `rightkit-onnx` 0.2 -> 0.3. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- `db::rebuild_step`, `db::RebuildStep` and `db::REBUILD_BATCH` are public: the resumable full-text rebuild that `migrate_v15` registers for a populated mailbox had no caller reachable from outside the crate, so it could never finish (its only callers were unit tests, since removed). The host app's background job drives it.
- `data_dir`: docs no longer name a test-mode refusal; the per-process fallback folder after a host refusal is `<app>-no-data-dir-<pid>` (was `<app>-under-test-<pid>`).
- Pre-migration backup hooks are closures (rightkit-db `BackupName`/`VerifyBackup`, rightkit#770): the key, timestamp name and message-count check are captured per migration, replacing the thread-local that carried them.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from MailRight (src-tauri db/*, db_schema, db_sync_apply, attach_cache, safety).
