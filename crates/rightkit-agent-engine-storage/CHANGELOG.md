# Changelog

## 0.3.0

- Depends on `rightkit-agent-engine-protocol` 0.4.0 (additive events and fields, see its changelog). Breaking for dependents: the crate exposes protocol types in its public API, so those types are now the 0.4 ones.
- Synced with CodeRight's newer storage code (CodeRight `crates/storage`), so CodeRight can use this crate instead of its copy.
- Migration 34 (`events_fts indexes message text only`): the `events_fts` triggers index only a user message's text and an assistant answer's content (valid JSON payloads only) instead of every event's JSON, which was about half the database; rows indexed before stay, `reindex_events_fts` rebuilds the lean form (it now reads the same rows the triggers index). The schema version is now 34, so a database migrated by this build is newer than 0.2 builds know.
- Partial assistant reply snapshot (`save_partial_reply`, `clear_partial_reply`, `load_partial_reply`, `PartialReply`): one overwritten row per session, created on first save, deleted with the session.
- Oversized-payload stubs: `Storage::restore_spilled_payloads` puts the retained full payload back into rows that carry the spill stub, and `all_events` returns whole events; the paged readers keep the stub.
- `Storage::session_tail` / `SessionTail`: how a session's conversation last ended (last turn event kind and time, turn status, last answer) from two indexed reads. `events_after_paged(session_id, after_seq, page_rows)` reads every event after a seq page by page.
- `PersistenceGapError::Storage { requested, message }`: `append_event_at_seq` reports a SQLite failure as such instead of `SeqAboveHighWater`, so a caller that waits for the writer on `SeqAboveHighWater` no longer waits on a disk-full or constraint error (bug fix). Breaking for exhaustive matches on `PersistenceGapError`.
- Quarantine no longer deletes (data-loss fix, audit M1 to M3): `quarantine_malformed_payload_events` and `quarantine_undecodable_events` keep the live row (a malformed payload is replaced by a valid-JSON `quarantinedPayload` marker that replay skips), never overwrite an existing quarantine copy (`INSERT OR IGNORE`), skip a seq that already has one without a write, and the malformed-row scan lets SQLite pick the rows (`NOT json_valid`).
- Host-supplied names: `set_product_name` (the newer-database message reads "a newer <name> ... update <name>"; default text unchanged: "a newer version ... update the app") and `set_journal_mode_env` / `journal_mode_env` (default `RIGHTKIT_SQLITE_JOURNAL_MODE`; CodeRight passes `CODERIGHT_SQLITE_JOURNAL_MODE`). CodeRight's browser artifact producer `coderight-browser` is `set_browser_artifact_producer`.
- Telemetry: unchanged. CodeRight's copy records latency through `rightkit_agent_telemetry::SqliteOpKind`/`record_sqlite_op`; this crate keeps its own `SqliteOpKind` and `set_sqlite_op_observer`, and CodeRight installs an observer that forwards to its recorder.
- Unit tests are not ported (end-to-end tests only).

## 0.2.0

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.

## 0.1.3

- Host naming option, default unchanged: `set_browser_artifact_producer` / `browser_artifact_producer()` (default `rightkit-browser`, `DEFAULT_BROWSER_ARTIFACT_PRODUCER`) for the `producer` of recorded browser artifacts. The persisted column names are unchanged (`driver_bindings.coderight_agent_id` already matches the app).

## 0.1.2 (unpublished)

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- Synced with CodeRight: migration 33 drops the unused `memory` table (the `MemoryRow` API and `write_memory` / `get_memory` / `query_memory_by_tier` / `top_memories` are removed), creates `observation_ledger` and adds the `sessions.tags` / `recovery_marker` columns in the schema instead of the best-effort column pass; `Storage::open` sets `secure_delete`; two pre-migration backups are kept; the uncalled `prune_old_events`, `delete_events_before_seq` and `compacted_sessions` are removed.
- Feature `workflow`: durable storage for the agent workflow control plane (journal writer, repository, trigger writer), moved from the CodeRight storage `workflow` module.
- First RightKit release, moved from `coderight-storage` (engine).
