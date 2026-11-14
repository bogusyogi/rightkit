# Changelog

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
