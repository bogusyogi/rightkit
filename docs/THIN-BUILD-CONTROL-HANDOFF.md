# RightKit Thin Build Control — Codex Handoff

**Branch:** `rightkit/thin-cargo-broker`

**Base:** `5246afb0d06cd4a062a4ad59bdbacf41183c0ea3`

**Status:** WIP. Do not deploy yet. The branch contains the beginning of a deliberate simplification of RightKit build control. The goal is not to patch the current overengineered v2 broker; it is to replace its execution core with a small workstation Cargo traffic controller while keeping the existing client/shim protocol and platform containment code where useful.

## User goal

The actual requirement is small:

1. Agents must not launch uncontrolled Cargo/rustc/rustup work.
2. Cargo caches must be reused correctly.
3. Expensive compilation must be queued so a shared MacBook Air stays usable.
4. Cancelling a job must kill its owned process tree.
5. A broker crash must not allow an uncertain old compiler to corrupt the next build.
6. Logs/receipts must make slow or failed jobs diagnosable.

Do **not** turn this back into a build system, CI system, Bazel clone, distributed executor, or generic result cache.

## Target architecture

```text
Agent / rightkit shim
        |
        v
+-----------------------+
| Thin RightKit broker  |
|                       |
| command policy        |
| scheduler             |
| process owner         |
| cache env injection   |
| simple state/logging  |
+-----------+-----------+
            |
            v
          Cargo
            |
       sccache client
            |
           rustc

Shared machine-wide:
- CARGO_HOME
- sccache cache

Per Cargo workspace:
- persistent target generation
```

### Resource classes

Use one declarative command policy. The branch has started this in `thin-command-policy.mjs`.

- `CONTROL`: cheap probes; concurrent; no workspace lock when genuinely read-only.
- `ANALYSIS`: dependency/security analysis; repository read lock; no compile slot.
- `WORKSPACE_WRITE`: manifest/lockfile/source mutations; one writer per workspace.
- `BUILD`: compile-heavy work; one active build root on the Air initially; writer lock for that workspace.
- `TOOLCHAIN_WRITE`: globally exclusive.

Unknown commands fail closed.

### Initial Mac sizing

Keep this deterministic, not adaptive:

- `BUILD` roots: 1 globally.
- `CARGO_BUILD_JOBS`: 3 or 4; default to 4 for the current Air only after a measured run.
- `CONTROL`: 4.
- `ANALYSIS`: 2.
- workspace writers: one per workspace.
- `TOOLCHAIN_WRITE`: exclusive.

Thermal/load/memory telemetry may later *derate* admission, but do not build a dynamic token economy first.

## Caching model

Caching is a first-class requirement, not a fallback.

### Layer 1 — shared Cargo home

One machine-global `CARGO_HOME` for registry/git/source reuse.

### Layer 2 — shared sccache

One machine-global sccache cache directory. Prefer `SCCACHE_CLIENT_SIDE=1` after verifying the pinned sccache version supports and behaves correctly with it. The objective is that the actual compiler remains in the requesting job's process domain while the cache remains global.

Do not permanently disable caching to obtain containment.

### Layer 3 — persistent per-workspace target generation

Each Cargo workspace gets one warm target generation that is reused normally. Do **not** share one Cargo target directory across unrelated workspaces.

Normal build:

```text
workspace A -> generation 12 -> target/  (keep reusing)
```

If the broker crashes or a process tree cannot be proven dead:

```text
old orphan may still write -> generation 12
new work uses             -> generation 13
```

This is the main simplification. Process reaping remains important cleanup, but proving an old process is dead is no longer a prerequisite for safe future builds.

## Orphan/process containment

Keep the existing `process-tree.mjs` machinery where possible rather than rewriting it.

- POSIX/macOS: own one process group per root job. Cancel with `SIGTERM`, bounded grace, then `SIGKILL` to the whole owned group.
- Windows: keep the Job Object implementation and prove it live on Windows before claiming Windows acceptance.
- sccache: prefer client-side compilation so rustc remains in the request domain.

On broker restart:

1. Existing queued work can be cancelled/forgotten.
2. Any marker showing a workspace had a RUNNING writer means rotate that workspace's target generation before admitting a new writer.
3. Best-effort reap the old process group.
4. Do not globally wedge the scheduler if reap cannot be certified.

## What has landed on this branch

### `thin-command-policy.mjs`

Started the single-source command/resource policy:

- CONTROL / ANALYSIS / WORKSPACE_WRITE / BUILD / TOOLCHAIN_WRITE.
- separates compile work from analysis.
- `outdated`, `audit`, `deny`, `machete` default to ANALYSIS.
- `upgrade`/`set-version` are WORKSPACE_WRITE.
- `nextest` is BUILD.
- `metadata`, `tree`, `fetch` are read-like only when `--locked`/`--frozen`; otherwise they are workspace writes.
- unknown commands reject.

Review this file first and extend the table instead of creating another validator.

### `thin-scheduler.mjs`

Started deterministic scheduling:

- global BUILD limit.
- per-workspace readers/writer exclusion.
- global TOOLCHAIN_WRITE exclusion.
- small lane concurrency limits.
- queue priority only; no mid-build preemption.

## Next implementation steps

Implement in this order.

### 1. Finish `thin-state.mjs`

Keep it small. JSON files are sufficient for this workstation use case.

Required state only:

```text
rightkit-build-control/
  global/
    cargo-home/
    compiler-cache/
    tools/
    active/
  workspaces/<workspace-key>/
    identity.json
    state.json
    generations/000001/
      target/
      build/
      tmp/
  logs/
receipts/
```

State needs to answer only:

- current generation for workspace;
- whether a root job was active when the broker disappeared;
- terminal receipt lookup.

Do not recreate the SQLite lifecycle schema, reservation ledger, maintenance leases, namespace quarantine table, or exactly-once execution machinery.

### 2. Add a small `thin-cache.mjs`

Responsibilities:

- build the shared cache environment;
- inject `RUSTC_WRAPPER=sccache`;
- inject `SCCACHE_CLIENT_SIDE=1` when supported;
- expose bounded `sccache --show-stats --stats-format=json` snapshots;
- record hit/miss deltas in terminal receipts;
- if cache invocation itself fails, report degradation clearly and use direct rustc only for that job.

Prefer the existing `sccache-supervisor.mjs` only for pieces worth retaining. Do not retain server ownership complexity solely for server-side compiler execution if client-side mode removes the reason for it.

### 3. Add a small `thin-job-service.mjs`

Reuse the existing client protocol (`SUBMIT`, `WATCH`, `STATUS`, `CANCEL`) so product repos and installed shims do not need migration.

A root job lifecycle can be simple:

```text
QUEUED -> RUNNING -> SUCCEEDED | FAILED | CANCELLED | TIMED_OUT
```

`ABANDONED` exists only as restart interpretation, not as a distributed execution protocol.

Job service responsibilities:

1. normalize request;
2. classify from the one policy table;
3. resolve Cargo workspace root;
4. acquire scheduler permit;
5. select current target generation;
6. inject cache/build environment;
7. spawn with existing process containment;
8. stream stdout/stderr;
9. cancel TERM -> grace -> KILL;
10. write simple receipt;
11. always release scheduler permit.

Do not implement nested BUILD/WORKSPACE_WRITE/TOOLCHAIN_WRITE. If Cargo/plugin recursion re-enters RightKit, allow only a narrow explicit nested CONTROL/ANALYSIS allowlist or simply reject nested non-control work in the first version.

### 4. Broker singleton

Acquire an OS-enforced exclusive singleton **before** opening/mutating broker state.

On macOS a simple atomic lock file/`flock` equivalent or exclusive ownership file with robust stale-owner verification is enough. Do not unlink an existing live socket merely because it has the same UID.

### 5. Replace active broker entrypoint

Once the thin service is covered by focused tests, switch `broker.mjs` from the old `ManagedJobService` to the thin service.

Keep the current runtime/client handshake unless there is a concrete reason to remove it; it already prevents client/runtime skew.

### 6. Update bootstrap/config

The new bootstrap should express the actual intended Mac settings from source, not a manually-mutated live JSON:

- build concurrency 1;
- analysis concurrency 2;
- control concurrency 4;
- cargo build jobs 4 (or measured value);
- shared Cargo home;
- shared sccache enabled when verified;
- no reservation fields;
- no GC transaction fields;
- no durable lifecycle DB path requirement.

External tools must install to a stable machine-global tool root, never inside a workspace generation.

### 7. Logging

Use append-only NDJSON without fsync on every heartbeat.

Persist:

- received/classified;
- queued/start;
- process pid/pgid;
- cancel/exit;
- cache mode and stats delta;
- one periodic RUNNING sample every 5–10s (elapsed, RSS/CPU if cheap, output bytes, last output time, free disk);
- terminal summary.

Receipt should include at least:

```json
{
  "id": "...",
  "command": "cargo check",
  "workspace": "...",
  "generation": 12,
  "queueMs": 1200,
  "runMs": 42000,
  "exitCode": 0,
  "signal": null,
  "cache": { "mode": "sccache", "status": "OK", "delta": {} }
}
```

### 8. Simple GC only after correctness

Do not rebuild current `maintenance-worker.mjs`.

First version:

- only inactive, non-current generations are candidates;
- delete oldest retired generations when total target storage exceeds cap or explicit `rightkit gc` is requested;
- never delete the current generation;
- never delete a generation with an active marker;
- atomic rename to trash before recursive delete is sufficient.

## Old machinery to remove after live acceptance

Do not delete these before the thin broker is proven on the Mac; archive or leave unused during the migration. After acceptance, remove the obsolete active implementation and tests tied only to it.

Primary deletion candidates:

- `src/state-store.mjs`
- `src/state-store.test.mjs`
- `src/maintenance-worker.mjs`
- `src/maintenance-worker.test.mjs`
- old `src/job-service.mjs`
- old `src/job-service.test.mjs`
- old `src/scheduler.mjs`
- old `src/scheduler.test.mjs`
- old bounded/fsync-heavy `src/event-log.mjs` if the thin logger replaces it completely
- old sccache server ownership/restart code that becomes unnecessary under proven client-side compilation
- v1 recovery/drain code after all installed machines are confirmed migrated
- old architecture/remediation documents after this architecture becomes accepted

Configuration/schema concepts to delete when no longer referenced:

- SQLite lifecycle database requirements;
- lifecycle transition table semantics;
- reservation ledger / `reservationBytes`;
- namespace quarantine settings and manual clear path;
- durable maintenance/trash leases;
- `usedBytes` reconciliation machinery;
- exactly-once crash claims;
- scheduler `holdPermit` safety behavior;
- generic result cache proposals;
- adaptive token-bucket scheduling proposals.

Do **not** delete:

- agent shims / direct-Cargo enforcement;
- client protocol and runtime identity handshake unless replaced deliberately;
- immutable/digest-pinned managed tool checking;
- process-group containment;
- Windows Job Object implementation;
- shared Cargo home;
- shared sccache;
- persistent per-workspace target generations;
- simple queue/status/cancel/attach;
- useful logs and receipts.

## Workspace identity

Use Cargo's own workspace semantics rather than Git root heuristics. Resolve the workspace root through Cargo (`cargo locate-project --workspace` or equivalent controlled metadata) and canonicalize it. Git root may remain receipt metadata but should not define target-cache identity.

## Acceptance tests before deployment

Minimum live macOS proof:

1. Two simultaneous BUILD requests -> exactly one runs, one queues.
2. CONTROL request completes while a BUILD is active.
3. ANALYSIS request (`outdated`/`audit`) does not consume the compile slot.
4. Same workspace BUILD followed by BUILD reuses the same generation and is measurably warm.
5. Cold target + warm sccache shows real cache hits.
6. Cancel active build -> zero attributed compiler processes remain.
7. Kill broker during active build -> restart rotates only that workspace generation and unrelated workspace builds remain admissible.
8. Restarted build cannot write to the old suspect generation.
9. Two attempted brokers -> second one refuses before touching job state.
10. Cache failure -> visible degraded receipt, no silent rebuild mode.
11. `rightkit status`, `attach`, and `cancel` work through existing clients.

Windows acceptance is separate: compile/run the Job Object addon on a real Windows host and repeat containment/contention tests there.

## Non-goals

Do not implement unless a later measured need proves them necessary:

- arbitrary command-result caching;
- test-result caching;
- hermetic action graphs;
- remote execution;
- exactly-once payload launch across broker crashes;
- transactional disk reservations;
- dynamic thermal token economics;
- generic resource ontology beyond the five classes above;
- automatic killing of processes based only on path/name heuristics;
- reimplementation of Cargo dependency/workspace resolution.

## Branch hygiene

This branch is intentionally WIP. Continue from it, run focused Node tests for each new thin module, then switch the broker entrypoint only after the thin service tests are green. Do not deploy this branch as-is.
