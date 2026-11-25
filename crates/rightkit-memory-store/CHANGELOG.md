# Changelog

## 0.2.2

- Temporal fact operations accept the resident writer connection (`temporal::TemporalDatabase`, `TemporalFactStore<D = MemDb>`; the default store type is unchanged, so existing callers compile as before). Fact admission, validity queries, conflict resolution, lineage and supersession keep their rules; signed dates reuse `rightkit-time` 0.1.4.
- Observable event writes accept an explicit database path (`db::record_observable_event_at_path`), and hosts that select paths before opening a store get `companion_db_path(memory_path, fallback_stem, suffix)` (the host supplies the stem and suffix; the previous names stay the default for existing callers) and `scope::workspace_root_from(explicit, config_path, schema_version)` for workspace root validation with a caller-selected config path and accepted schema version.

## 0.2.1

- Added `neutral` (RK-7; additive, nothing existing changes): `export_neutral` writes one JSON line per memory (format `rightkit.memory.neutral.v1`, named after the crate family's `rightkit.<area>.<thing>.vN` schemas and distinct from `membrane.cortex-backup.v3`; import also accepts the Membrane dry-run's draft name `membrane.memory-export.v0-draft` but export never writes it; `name_source` is `id_suffix_after_scope_prefix` or `id_unqualified_legacy` and `updated` is `null` when the store has none, both as in the Membrane dry-run export of the v27 set-aside store: body byte-exact with its SHA-256, provenance, identity, lifecycle, usage, and the dropped-embedding evidence), reading any layout read-only (`export_neutral_from_path` opens the file read-only and never creates it); `import_neutral` adds lines to a current-layout store without wiping or updating anything, idempotent on id plus body hash, one transaction per line, and returns per line `Inserted`, `AlreadyPresent` or `Rejected` with a code (hash or byte mismatch, id with a different body, deleted or quarantined id, identity or agent-source collision). Imported rows have no embedding (the caller re-embeds); the FTS5 projection is written; the `links` table is left to the caller reindex. The backup restore (`restore_cortex`, which wipes first) is not used. Written, not compiled. No unit tests, per the 2026-11-03 rule.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-context-protocol` 0.1 -> 0.2, `rightkit-logs` 0.3 -> 0.4. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cortex-store` (engine).
