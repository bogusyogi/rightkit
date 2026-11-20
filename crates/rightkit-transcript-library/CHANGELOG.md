# Changelog

## 0.1.6 (unreleased)

- Added (feature `managed-store`): `DeliveryHistoryStore::open_managed(EncryptedStore, legacy_json_path, events)`. The history store runs every query, write and migration (schema, search-column backfill, legacy `history.json` import) on the `EncryptedStore`'s managed writer (`EncryptedStore::with_writer`), so one connection serves history and the lifecycle owner, and a rotation, restore or recovery of that store drains and reopens the connection the history uses. The host no longer opens a second connection, re-reads the credential for it, or closes and reopens the history around a rotation. `DeliveryHistoryStore::store()` returns the lifecycle owner by reference; `DeliveryHistoryStore::rotate_key(RotationRequest) -> RotationReport` rotates through it.
- Changed: `DeliveryHistoryStore` is generic over where its connection comes from, `DeliveryHistoryStore<C = rusqlite::Connection>`. Every existing use is unchanged: `DeliveryHistoryStore::open_connection(conn, path, events)` and `connection()` still take and give a bare `rusqlite::Connection`, and a type written `DeliveryHistoryStore` still means the raw-connection store. The `HistoryStore` impl covers both.
- Behaviour while the lifecycle owner is busy: a history operation on a managed store returns a `HistoryError` (`history database: store is busy ...`) during a rotation, restore or recovery instead of waiting, and the next call after it settles succeeds. A store left blocked by an unresolved transition answers every operation with the recovery-required error until `EncryptedStore::recover_rotation` settles it. The in-memory mirror is untouched by a rotation.
- Unchanged: schema, SQL, filters, retention, the in-memory mirror and the legacy import. An existing database opens and reads exactly as before.

## 0.1.5 (unreleased)

- Add `file_store::FileHistoryStore` as a JSON fallback history store.

## 0.1.4

- Add a distinct `clipboard_preserved` recovery reason using the existing extensible record format.

- Add portable JSON, Markdown, plain text and CSV export for delivery records, with host-provided labels.
- Add optional `store` feature for the `delivery_history` keyed-connection store, schema migration,
  history queries and legacy JSON import with verification. Plaintext cleanup remains pending;
  key custody, recovery and rotation remain host-owned.

## 0.1.2 (unreleased)

- Removed test-only code: `DeliveryRecord::for_test` and `TargetSnapshot::test_target` (Adrian, 2026-11-03: "make sure no debug assertions in their respective apps/ repos"; "My decision was no unit tests. Only e2e."; "dev mode, is mine. im the fucking owner, my data. no dummy data ... real data. snapshot and restore."). Release behaviour unchanged.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from HeardRight.
