# RightKit Thin Build Control v2.8 — macOS Acceptance

**Status:** macOS accepted.
**Date:** 2026-08-23.
**Scope:** macOS only, per explicit user scope. Windows is excluded from this
campaign and from every completion claim in this record.

## Accepted build

```text
source revision:    add59bea4c4584e86a72e14e1f9daad7bda6c96f
runtime version:    bootstrap-add59bea-1787445946437
runtime directory:  runtime-405f78c5b564fc8fbc0da5e300fe3c9d6e4000b20b0870cf397b7f8e42c6eac7
source tree digest: 38cbc6e86fd77fee1b774377ff213cee66008401
activation state:   COMMITTED
Rust:               1.98.0, aarch64-apple-darwin
sccache:            0.17.0, client-side
BUILD / jobs:       1 / 4
```

Live activation is recorded by
`/Volumes/D/.rightkit-managed/state/runtime-activation.json` and
`/Volumes/D/.rightkit-managed/runtime-current/runtime-manifest.json`.

## Acceptance matrix

| Area | Result | Evidence |
|---|---|---|
| Broker topology | PASS | Live verbose Cargo smoke `69c2cecb-6d32-4419-8de4-63aa7e821f99` showed `Cargo -> managed sccache -> pinned rustc`; final smoke `8454b175-c7db-429b-966b-a6e4d30f5d75` recorded `compilerBrokerRoundTrips=0` |
| Raw compiler | PASS | Focused topology tests prove raw agent `rustc`/`rustdoc` remains a top-level managed BUILD while Cargo-owned compiler calls are direct |
| macOS cancellation | PASS | Controlled Tauri cancellation `d07c8e01-f4a1-4d32-bbb9-fc7d8929d0d7` reached terminal `CANCELLED`, then `REAPED`; cancellation-race regression tests pass |
| Crash recovery | PASS | Focused thin-state tests prove only uncertain affected workspace generation rotates; live rotation preserved service recovery |
| New generation safety | PASS | Generation tests reject writes to suspect prior generations and admit only current generation |
| CONTROL during BUILD | PASS | Live control probes remained responsive during long BUILD requests; lane isolation tests pass |
| Workspace warm reuse | PASS | Same-workspace no-op/edit checks reused generation 1; final fixture Cargo check completed in 124 ms run time |
| Cache telemetry | PASS | Final receipt reports `SERVICE_OK`, reason `client-side-counters-unattributable`, and `delta=null` |
| Incremental policy | PASS | Paired experiment selects `KEEP CARGO DEFAULT`; disabled mode was slower cold and did not improve hot loop |
| Basedirs policy | PASS | Alternate-path experiment produced zero Rust hits; gate remains disabled |
| Rotated/alternate path | PASS | Requests `3053df78-73f0-4577-af15-1a6520cb8dd4`, `e81f898f-d454-4d9c-8151-cb5be26225b5`, and `4059acb1-27c4-4999-9cc6-383a0b262075` record behavior and remaining misses |
| Target cap | PASS | Tests prove idle current generations are LRU candidates and aggregate target budget becomes enforceable |
| Active protection | PASS | Tests prove active and locked generations cannot be selected or evicted |
| GC crash safety | PASS | Tests prove trash/rotation interruption remains recoverable and cannot reactivate suspect output |
| GC performance | PASS | Workspace-size metadata and reconciliation tests prove request hot paths avoid repeated full-tree scans |
| Cargo install | PASS | Policy tests prove generic `cargo install` cannot populate trusted managed tool storage |
| Bootstrap macOS | PASS | Source bootstrap produced accepted runtime/config; workspace discovery timeout is 10 seconds; incremental stays Cargo-default and basedirs stays off |
| Test suite | PASS | `pnpm --dir tools/rightkit test:build-control`: core 17, broker 169, clients 32, policy 31, staging 1; 250 total |

Windows acceptance rows are intentionally absent. They are not Mac work and
were explicitly removed from current scope.

## Cache policy decision

Full measurement detail lives in
`rightkit-thin-build-control-v2.8-cache-experiment.md`.

```text
incremental policy: keep Cargo/profile default
basedirs policy:    disabled
primary warm reuse: persistent per-workspace target generation
receipt semantics:  no fabricated per-root client-side sccache delta
```

## Live final smoke

Request `8454b175-c7db-429b-966b-a6e4d30f5d75` completed `SUCCEEDED` and
`REAPED` on generation 1:

```text
total:                       340 ms
run:                         124 ms
queue:                       6 ms
compiler broker round trips: 0
cache status:                SERVICE_OK
cache reason:                client-side-counters-unattributable
cache delta:                 null
```

This closes Mac implementation, live activation, cache-policy measurement, and
native v2.8 acceptance.
