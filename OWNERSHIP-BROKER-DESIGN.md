# RightKit broker ownership admission design

## Outcome

A build may run only when broker-owned evidence covers the exact source bytes the build process reads. The current broker resolves a live Cargo workspace, and `spawnManagedProcess` uses `job.request.workspace`; scanning plus a pre-spawn rehash still leaves source mutable while Cargo and build scripts run.

## Admission sequence

1. Normalize request and discover Cargo workspace and repository roots as today. Keep capability classification, scheduler admission, per-Cargo-workspace pools, memory-budget admission without a count cap, and lock-free Cargo `metadata --no-deps` observation unchanged.
2. Under a unique request ID, materialize the complete supported build-input closure beneath broker-owned external `stages/<id>/source`. Preserve modified, untracked, ignored and hidden files. Resolve and copy local Cargo path dependencies and required package-manager inputs; never omit a path because it is ignored or has a familiar generated/vendor name. Keep Cargo target, `OUT_DIR`, temp, Cargo home, compiler cache and runtime caches outside this source tree.
3. Require a host snapshot protector that attests the staged source cannot be changed by the build child or agent during execution. A copy plus chmod/read-only bits under the same OS identity is insufficient. If platform protection is unavailable or cannot attest the child-readable snapshot identity, reject with `RK_OWNERSHIP_SNAPSHOT_PROTECTION_UNAVAILABLE`.
4. Run the scanner from immutable broker runtime against staged source. Require scanner version >= protected floor 2.0.0 and catalog version >= protected floor 1. Require complete scanner coverage. Read approval/trust only from fixed platform-owned trust location; never accept request, environment, or broker config selectors. Missing enrollment/protection is typed evidence and cannot authorize duplicate findings.
5. Bind admission evidence to source snapshot digest, catalog digest, approval/trust digest, scanner/catalog versions, repository identity, commit, host and protection attestation. Cache successful scan results by `contentDigest + catalogDigest + trustDigest + approvalDigest + scannerFloor + catalogFloor`. A cache hit skips scanner work only; the protected snapshot provider must still authenticate current bytes and return the same content digest. Metadata-only freshness is not an acceptable cache key. Target scanner-cache decision latency at p95 <= 100 ms for an unchanged admitted input on both hosts; no whole-admission or whole-job latency target is claimed until measured.
6. Execute with cwd set to the admitted staged workspace. Verify snapshot identity/protection immediately before spawn and retain source until the full process tree is reaped. A protected host policy sets scanner timeout to 60 seconds by default; only protected host policy may change it. On timeout, scanner error, incomplete coverage, finding, downgrade, or identity/protection mismatch, do not spawn; persist a typed rejection receipt, emit a lifecycle block event with evidence, then clear rejected snapshot state. Required failure codes include `RK_OWNERSHIP_SCAN_TIMEOUT`, `RK_OWNERSHIP_SCAN_FAILED`, `RK_OWNERSHIP_SCAN_INCOMPLETE`, and `RK_OWNERSHIP_INPUT_CHANGED` (plus `RK_OWNERSHIP_SNAPSHOT_PROTECTION_UNAVAILABLE` for missing protection).

## Required platform and compatibility work

`packages/build-control-staging` currently only returns `stages/<requestId>` and `targets/<requestId>` descriptors and a sync-back state; it does not copy, protect, hash, or clean source. `build-control-clients/src/remote-snapshot.mjs` uses a private Git index to capture tracked changes and non-ignored untracked files, but omits ignored files and rejects submodules; it cannot define the local admission snapshot.

The snapshot provider must specify behavior for Git metadata used by `build.rs`/scripts, nested repositories, submodules, symlinks, Cargo path dependencies outside workspace/repository root, pnpm workspace links and node_modules, and scripts that write source files. Unsupported closure must fail closed. Only explicitly broker-owned output paths such as `CARGO_TARGET_DIR` and `OUT_DIR` may be writable during compilation. Cargo reads manifests and source from snapshot while all build output remains in current managed external roots.

The unchanged-input target applies to scanner-cache decision latency: p95 <= 100 ms after the protected snapshot provider supplies an authenticated content digest and the content/catalog/trust/approval/floor cache key hits. Report snapshot materialization, content hashing, protection, queue wait, scanner-cache decision and total admission durations separately. Measure whole-job admission latency on Windows and macOS before cutover; do not claim a whole-admission target before those measurements.

## Submission test matrix

Exercise actual broker submissions and assert both emitted evidence and whether a child was spawned:

- Cache hit: same authenticated content/catalog/trust/approval/floor key skips scanner; cache decision meets p95 target under repeated unchanged submissions.
- Cache miss and source change: scanner runs; changed content digest invalidates prior entry.
- Finding: duplicate blocks with typed `RK_OWNERSHIP_DUPLICATE` rejection detail, durable receipt, lifecycle block event and no child spawn.
- Downgrade: scanner or catalog below protected floor rejects before scanning/admission and records observed/floor versions.
- Protection failure: unavailable or invalid host attestation rejects and clears staged source.
- Timeout: scanner exceeds protected 60-second default and produces `RK_OWNERSHIP_SCAN_TIMEOUT`, durable receipt, cleanup and no child.
- Race: mutate/replace admitted input between snapshot verification and spawn; reject with `RK_OWNERSHIP_INPUT_CHANGED` and no child.
- Nested compiler, raw compiler and package-manager builds: prove they remain bound to admitted snapshot inputs while compiler outputs, Cargo target/cache and `OUT_DIR` remain external.
- Regression: preserve current per-Cargo-workspace pools, memory-budget admission without a count cap, and lock-free `cargo metadata --no-deps` observation.

No tests have been run for this design artifact; this matrix is acceptance work, not test evidence.

## Files in implementation scope

- `tools/rightkit/packages/build-control-staging/src/index.mjs` and `index.test.mjs`: snapshot materialization, content identity, host-protection adapter contract, bounded cleanup/reap lifecycle.
- `tools/rightkit/packages/build-control-broker/src/runtime.mjs`: stage scanner, approval verifier and catalog into immutable runtime content/provenance digest.
- `tools/rightkit/packages/build-control-broker/src/thin-job-service.mjs`: typed scan admission, evidence/cache, staged execution cwd, failure receipt/event, and post-queue/pre-spawn identity check.
- `tools/rightkit/packages/build-control-broker/src/thin-job-service.test.mjs`: real submission tests for duplicate block, clean admission, changed content/cache invalidation, downgrade, protection failure, timeout and race.

No runtime activation, bootstrap, service restart, job cancellation, `--force`, publication, HEAD/index mutation or commit is part of this design.
