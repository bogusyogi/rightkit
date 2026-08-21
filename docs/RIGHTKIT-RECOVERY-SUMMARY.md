# RightKit Recovery Summary

**Status:** **source complete, operationally unaccepted.** The architecture below is
implemented and unit-covered, but 11 of the 13 live proofs in *Required proof* are still
open. Source-level tests are not acceptance: process doubles cannot reproduce daemon
crashes, orphaned compilers, lane stalls, or disk pressure — the original remediation plan
said so, and calling this "implemented" anyway is what let EC-902 read as an isolated
urgent fix instead of unfinished remediation.

Do not report RightKit build control as done until every proof below is struck through
with the evidence that closed it.

## Confirmed failure

`cargo about --version` was classified as a heavy writer, reserved 16 GiB, waited behind shared build control, then became `CANCELLED/disconnect` without running. Current design also couples admission to maintenance, cancels work when its client socket disappears, uses stale whole-tree accounting, rewrites a 1 MB state file at lifecycle transitions, & does not fully own sccache compiler work.

## Implemented containment

Capability registry, CONTROL/BUILD/WORKSPACE_WRITE/MAINTENANCE lanes, durable
socket-independent jobs, SQLite WAL, bounded output, process ownership, statfs
admission, independent maintenance, content-addressed runtime, activation fence,
typed tool changes, & macOS cutover CLI are implemented. Unknown Cargo commands
fail closed. Windows BUILD fails closed pending native Job Object containment.

## Durable architecture

- Client submits, watches, reattaches, or explicitly cancels by request ID.
- Coordinator owns classification, durable lifecycle, scheduling, & receipts only.
- Per-request execution host owns full POSIX process group; Windows BUILD refuses startup until native Job Object ownership exists.
- Cache supervisor separately owns versioned sccache binary, daemon, socket, limits, health, & fallback.
- Maintenance worker separately owns inventory, orphan cleanup, retention, & GC.
- SQLite WAL replaces full JSON rewrites & atomically records transitions plus terminal receipt.
- Recovery is total: a row it cannot prove clean is quarantined with a typed
  receipt (`RK_RECOVERY_OWNERSHIP_AMBIGUOUS`, `RK_RECOVERY_REAP_INCOMPLETE`) and
  startup continues. Recovery never throws — a throw there runs before the socket
  binds, so one unrecoverable row used to leave the broker unable to start at all,
  with no exit but manual SQLite surgery.
- A quarantine receipt whose reap escalated sets `processMayBeLive`, which is an
  operator signal that a descendant may have survived — not a clean cancellation.

Lifecycle:

```text
SUBMITTED → CLASSIFIED → QUEUED → ADMITTED → STARTING → RUNNING
          → REAPING → SUCCEEDED | FAILED | CANCELLED | TIMED_OUT
```

Socket disconnect detaches an observer; it does not cancel work. Explicit cancellation performs TERM → grace → KILL → reap. Nested Cargo inherits parent request, permit, target, environment, process containment, & cancellation domain instead of entering queue again.

## Storage & GC

- Admission uses deadline-bounded `statfs`, free-space floor, active reservation, & one BUILD slot—not recursive size scans or stale `usedBytes`.
- Storage-probe failure never kills active work; it blocks new heavy admission with a typed result.
- Namespace size is advisory background data only.
- GC never blocks admission or terminalization.
- GC acquires namespace generation/lease, revalidates, atomically renames to managed trash, then deletes in bounded background batches.
- Failed or cancelled builds retain warm targets under normal LRU policy.
- Empty/stranded namespaces are listed first; deletion remains explicit until race tests pass.

## Tool & runtime handling

- Runtime comes only from committed, immutable, content-addressed source.
- Mutable Homebrew paths are never runtime dependencies.
- Changed pinned tool returns typed `TOOL_CHANGED` with registration action; it is never silently trusted or reported as generic failure.
- Activation stages candidate, verifies content, probes isolated service, atomically switches pointer, re-verifies executed bytes, & rolls back pointer plus service together.

## Required proof

1. CONTROL probe finishes within two seconds during active Orthic/CodeRight/Membrane build.
2. Installer becomes next heavy request under background load.
3. Client death does not kill detached installer; explicit cancel reaps every descendant.
4. Nested Cargo cannot deadlock behind parent permit.
5. Hung `statfs` or GC cannot hold build lane.
6. GC racing same-namespace admission cannot delete live/warm target.
7. ~~Broker crash at every lifecycle boundary yields no duplicate execution & exactly one receipt.~~ **Closed 2026-08-21** by `recovery yields exactly one receipt at every open lifecycle boundary` in `packages/build-control-broker/src/job-service.test.mjs`, which seeds a real durable row at each of ADMITTED/STARTING/RUNNING/REAPING, recovers twice over the same SQLite database, and asserts the receipt is byte-identical across both passes. This is the one proof a process double *can* carry, because the durable store — not the process — is the thing under test. Proofs 1–6 and 8–13 cannot be closed this way.
8. Failed/cancelled build preserves target; warm rerun proves reuse.
9. sccache failure performs bounded restart or direct-rustc fallback.
10. Tool/runtime digest change gives typed rejection & atomic rollback.
11. Real multi-repository contention passes on macOS.
12. Windows Job Object plus first live Windows contention fixture pass before Windows completion is claimed.
13. HeardRight installer builds, installs, launches, & passes installed-runtime acceptance.

## Explicit deletions

Delete `unknown = heavy`, socket-owned jobs, pure global FIFO, terminal GC, admission waiting for maintenance, hot-path tree scans, stale `usedBytes` admission, request-owned sccache, mutable tool paths, full-state JSON rewrites, & dirty runtime activation.
