# Changelog

## Unreleased

- Added `neutral` (RK-7; additive, nothing existing changes, no version bump): `export_neutral` writes one JSON line per memory (draft format `membrane.memory-export.v0-draft`: body byte-exact with its SHA-256, provenance, identity, lifecycle, usage, and the dropped-embedding evidence), reading any layout read-only (`export_neutral_from_path` opens the file read-only and never creates it); `import_neutral` adds lines to a current-layout store without wiping or updating anything, idempotent on id plus body hash, one transaction per line, and returns per line `Inserted`, `AlreadyPresent` or `Rejected` with a code (hash or byte mismatch, id with a different body, deleted or quarantined id, identity or agent-source collision). Imported rows have no embedding (the caller re-embeds); the FTS5 projection is written; the `links` table is left to the caller reindex. The backup restore (`restore_cortex`, which wipes first) is not used. Written, not compiled. No unit tests, per the 2026-11-03 rule.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-context-protocol` 0.1 -> 0.2, `rightkit-logs` 0.3 -> 0.4. 0.1.1 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.1 in place (#1572). No source change in this crate since 0.1.1.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `cortex-store` (engine).
