# Changelog

## 0.1.0 (unreleased)

- Synced with CodeRight: migration 33 drops the unused `memory` table (the `MemoryRow` API and `write_memory` / `get_memory` / `query_memory_by_tier` / `top_memories` are removed), creates `observation_ledger` and adds the `sessions.tags` / `recovery_marker` columns in the schema instead of the best-effort column pass; `Storage::open` sets `secure_delete`; two pre-migration backups are kept; the uncalled `prune_old_events`, `delete_events_before_seq` and `compacted_sessions` are removed.
- Feature `workflow`: durable storage for the agent workflow control plane (journal writer, repository, trigger writer), moved from the CodeRight storage `workflow` module.
- First RightKit release, moved from `coderight-storage` (engine).
