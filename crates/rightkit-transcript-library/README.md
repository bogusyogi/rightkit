# rightkit-transcript-library

Transcript library: delivery records (outcome, target, timings, source), history store contract
with filters, paging, retention, scoped deletes and an in-memory store.

Moved into RightKit from HeardRight (`heardright_core`: `delivery`, `history`, `net`) on
2026-11-01.

- `delivery::DeliveryRecord` and friends: the record every dictation, file transcription or cloud
  sync produces, with pure shaping helpers (draft ids, copy-fallback records, paste-attribution
  predicates that require an `InsertionProof`).
- `history::HistoryStore`: the store contract (push, finalize, drafts, query with
  `HistoryFilter`, retention pruning, origin-scoped updates and deletes); `MemoryHistoryStore`
  implements it in memory.
- `net::host_of_url`.
- `export::format_history`: JSON, Markdown, plain text and CSV output from delivery records.
  `ExportLabels` supplies user-visible wording; defaults are neutral English.

The optional `store` feature provides `store::DeliveryHistoryStore` over a keyed SQLite
connection. It owns the `delivery_history` schema, queries, in-memory mirror and legacy JSON import
with verification. A verified import preserves the plaintext source and emits a pending-cleanup
event; safe identity-bound cleanup is not implemented. The host opens and keys the connection and
retains key custody, rotation and recovery.
