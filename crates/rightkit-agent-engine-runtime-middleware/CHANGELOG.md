# Changelog

## 0.1.4

- Fixed: several processes sharing one store file (`with_store_path`) no longer undo each other (CodeRight #539). Each write was the process's whole in-memory snapshot, with no lock and no re-read, so a process holding an older snapshot switched a guard (`builtin.guard-tool-arguments`) back off at its next span, and dropped other processes' spans and observations. A write is now a read-modify-write under `<store>.lock` (`rightkit-fs` `FileLock`, five-second wait): the file is re-read first; its `enabled` flags win except those this registry changed since its last write; its local plugin rows win unless this registry replaced them; its spans and observations are kept beside this registry's own (then the usual retention and 1 MiB caps). A registry also picks up other processes' flag changes at each write. A lock not taken within five seconds skips that write and keeps the changes for the next. The file format is unchanged.
- New end-to-end test `tests/two_writers_e2e.rs`: two registries on one file; a guard switched back on by one stays on after the other (holding the older snapshot) writes a span, and both spans survive. Fails on 0.1.3, passes here.

## 0.1.3

- New `EXECUTION_OBSERVATION_SCHEMA`, the one constant for the execution-observation schema id (`coderight.execution-observation.v1`, unchanged on the wire); the observation payload uses it and the agent server filters on it.

## 0.1.2

- Publishes the source changes made since 0.1.1 was published under the same version number (src/lib.rs).

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- Synced with CodeRight: the store file is rewritten at most once per second (`RuntimeMiddlewareRegistry::flush` writes held-back records), is compact JSON, and never exceeds 1 MiB (oldest spans and observations are dropped until it fits).
- First RightKit release, moved from `coderight-runtime-middleware` (engine).
