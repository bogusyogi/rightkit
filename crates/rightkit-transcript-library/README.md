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

The encrypted SQLCipher store and export are not in this release; they stay in the app until its
credential vault, sync scheduler and telemetry hooks are parameters.
