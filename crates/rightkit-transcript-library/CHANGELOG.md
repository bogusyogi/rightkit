# Changelog

## 0.1.4 (unreleased)

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
