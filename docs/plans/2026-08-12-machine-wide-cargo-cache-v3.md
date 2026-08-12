# Machine-wide Cargo Cache V3

Status: superseded by `2026-08-12-rightkit-cargo-control-plane-v4.md`; do not execute
Scope: this Mac, every current or future Cargo workspace, external storage only

## Outcome

Every Cargo invocation launched through managed machine development surfaces uses external D storage, isolated target state, shared compiler/dependency caches, complete lifecycle metadata, bounded garbage collection, & fail-closed behavior when D is unavailable. Managed surfaces are interactive/login shells, Codex/Claude tools, launchd jobs, VS Code/Rust Analyzer, Tauri/npm/pnpm scripts, RightKit release, workspace hooks, & registered IDE/task-runner integrations. Every discoverable Cargo/rustup entrypoint on those surfaces is a guard; no payload may fall back internally. This is an operational containment contract, not an OS security boundary: deliberate execution of a hidden real toolchain binary with a scrubbed environment is outside scope, is never described as compliant, & is reported by audit when discoverable.

## External root

Resolve D by APFS volume UUID, then use `<mount>/.rightsuite/`:

```text
.rightsuite/
  bin/
  runtime/
  toolchains/rustup/
  cargo-home/{bin,registry,git}/
  cache/
    sccache/
    targets/{dev,test,release,worktree}/
    scratch/
    recovery/
    quarantine/
  state/{registry.sqlite,leases,reservations,gc,receipts}/
  logs/
```

Internal disk retains only APFS UUID locator, launchd definition, & shell bootstrap, below 1 MB total. Cache namespaces live on a dedicated APFS volume inside D's container with a hard quota equal to total cache cap; runtime, toolchains, logs, & retained release artifacts remain on D outside that quota. Setup refuses non-APFS storage unless an equally hard filesystem quota is proven.

## Machine environment

Login/bootstrap reads only expected APFS UUID + relative root from internal locator. It enumerates mounted APFS volumes, selects matching UUID, captures mount device, canonicalizes mountpoint, verifies writable external media, then exports `RIGHTSUITE_STORAGE_ROOT`, `RIGHT_RELEASE_CACHE_ROOT`, `CARGO_HOME`, `RUSTUP_HOME`, `SCCACHE_DIR`, plus external shim path before every Cargo/Rust path. It must never call `mkdir` on configured mount path before UUID/device verification. Apply to interactive shells, launchd jobs, Codex/Claude agents, VS Code/Rust Analyzer, Tauri/npm/pnpm scripts, & workspace hooks. Wrong-volume same-name mount or missing UUID fails before any path creation.

Bootstrap order is fixed: resolve UUID → verify device/mount → locate already-installed external runtime → construct environment → run doctor → permit Cargo. Toolchain installation/update follows same resolver. Current Cargo version must pass setup probes for configured environment variables & `build.build-dir`; unsupported toolchains are rejected before adoption.

## Cargo guard

Install immutable/versioned RightKit cache runtime under external root. External `cargo` shim invokes `right-cache cargo`; runtime invokes actual toolchain Cargo through `rustup which cargo` without recursion.

For every invocation, storage enforcement runs before light/heavy classification:

1. Verify configured volume UUID is mounted.
2. Find workspace root from `--manifest-path` or nearest `Cargo.toml`.
3. Canonicalize workspace path.
4. Resolve rustc commit, host triple, command class, & worktree state.
5. Key target by SHA-256 of canonical workspace path + rustc commit + host triple.
6. Select `cache/targets/<class>/<workspace>-<key>`.
7. Normalize toolchain selection from `cargo +toolchain`, `rustup run`, `RUSTUP_TOOLCHAIN`, directory overrides, toolchain files, & explicit Cargo routes; resolve matching real Cargo/rustc without changing selection semantics.
8. Overwrite `CARGO_HOME`, `RUSTUP_HOME`, `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`, `CARGO_INSTALL_ROOT`, `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR`, Cargo config location, Cargo-child `TMPDIR`, managed process `HOME`, XDG cache/config/data, npm, pnpm, node-gyp, CMake, & Tauri/native cache variables with UUID-verified managed external paths.
9. Acquire shared-cache read lease, per-entry lease/build lock, & global heavy-work reservation when applicable.
10. Run selected real Cargo.
11. Record result, size, last use, command class, toolchain selection, & actual growth.
12. Release reservation + leases in `finally`.
13. Run bounded post-build GC.

`cargo new/init/fmt/metadata/tree/fetch` receive external home/temp/wrapper policy before skipping heavy-work admission. In particular `fetch` must write registry/Git data only to external Cargo home. Any command capable of creating build output also receives guard-owned explicit `--target-dir <registered-entry>` after caller override validation. `cargo install` always receives guard-owned external `--root`; caller `--root`, `CARGO_INSTALL_ROOT`, & `install.root` redirects are rejected unless exactly registered.

Payload means toolchains, Cargo sources/Git state, targets, compiler caches, build-script/proc-macro/native-builder scratch, package-manager caches, generated installers, & temporary build data. Guarded children receive a minimal external process home plus explicit Keychain/credential-agent access & validated read-only config snapshots; tests inventory internal writes. Arbitrary deliberate absolute writes by project code require OS sandboxing & are outside this cache-containment contract.

## Classes & retention

| Operation | Class | Retention |
|---|---|---|
| build/check/run/clippy/fix/doc | dev | 7 idle days |
| test/nextest/bench | test | 24 idle hours |
| RightKit release | release | latest successful target per app |
| Git worktree | worktree | 48 idle hours |
| install/probe/temp manifest | scratch | remove after success; 6-hour crash TTL |
| interrupted migration | recovery | 48 hours |

All debug, release, incremental, deps, build scripts, examples, docs, package output, timings, integration-test temporary data, & local native intermediates live inside registered entries. Unrelated workspaces never share raw Cargo target directories; safe cross-workspace reuse comes from sccache + shared Cargo dependency/source cache.

## Bypass prevention

Guard rejects every caller `--target-dir`, `--root`, `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`, `CARGO_INSTALL_ROOT`, `SCCACHE_DIR`, `RUSTC_WRAPPER`, `CARGO_HOME`, `RUSTUP_HOME`, `HOME`, XDG, temp, or known child-cache override unless it exactly matches UUID-verified registered state. It strips caller target/install routing & injects guard-owned explicit paths. RightKit release receives short-lived scoped ownership token bound to request digest, registry entry, process identity, & expiry.

Cargo configuration precedence is explicit: setup owns external `$CARGO_HOME/config.toml`; guard discovers every `.cargo/config`/`.cargo/config.toml` from workspace to filesystem root plus each `--config` file/value. It TOML-parses inputs & rejects storage/compiler-wrapper keys including `build.target-dir`, `build.build-dir`, `install.root`, `build.rustc`, `build.rustc-wrapper`, `build.rustc-workspace-wrapper`, relevant `[env]` overrides, or aliases that invoke Cargo/rustup through an executable or shell. Only declarative aliases whose final argv is revalidated by guard remain allowed. Parsing ambiguity, duplicate legacy/config.toml precedence, unreadable config, or unsupported Cargo version fails closed. Tests freeze Cargo precedence behavior for supported versions.

Managed `rustup` shim routes `rustup run <toolchain> cargo` through the same normalized selector. IDE/task-runner configuration receives external homes + shim path explicitly, not through assumed shell inheritance. Global external Cargo config supplies external workspace-hashed `build-dir` + sccache as defense-in-depth for direct actual Cargo with inherited managed environment. Static repo audit rejects absolute toolchain Cargo/rustup paths. Doctor reports any visible unguarded Cargo/rustup executable ahead of shims, discoverable real binary, or registered integration lacking environment proof. Contract tests cover nested shell execution, executable aliases, `env -i`/scrubbed environments, explicit binaries, `cargo +toolchain`, `rustup run`, overrides, & `cargo install --root`; managed surfaces must reject or route every case.

## Registry & concurrency

Each cache entry records schema, ID, canonical workspace path/hash, class, normalized toolchain identity, triple, created/last-used/last-success/last-failure times, measured size, lease identity including PID + OS process-start token + executable identity, retention, pinned state, & rebuildable state. Every guarded command uses durable registry + per-entry lock; same-entry builds serialize. Lease acquisition precedes target directory creation; release runs in `finally`; stale recovery requires PID/start-token mismatch.

Every Cargo-home, registry/Git, & sccache consumer holds a shared maintenance lease. GC/update/migration requires exclusive maintenance lease before touching shared data. sccache uses its native size-bounded eviction; filesystem pruning requires exclusive lease, coordinated server shutdown, clean restart, & health probe. Global admission lock serializes heavy admission, limits concurrent heavy builds, records conservative per-build growth reservation across target/scratch/sccache/Cargo-home categories, & subtracts all active reservations from available capacity. Reservations release in `finally`; crash recovery validates PID/start token, reconciles actual growth, & updates P95/high-water estimators. Live watermark monitor stops new writers & gracefully interrupts active work before hard quota exhaustion. GC revalidates source revision, marker, physical containment, volume UUID/device, leases, reservations, PID start identity, & registry transaction before mutation.

## Budgets

| Class | Budget |
|---|---:|
| Cargo registry/Git cache | 24 GiB |
| sccache | 32 GiB |
| dev targets | 72 GiB |
| test/bench targets | 24 GiB |
| release targets | 24 GiB |
| scratch/install | 8 GiB |
| recovery | 8 GiB |
| total | 192 GiB |

Desired D free space is 300 GiB; build hard floor is 220 GiB. Dedicated APFS cache volume enforces 192-GiB aggregate cache quota regardless of racing writers. Before heavy work, GC prunes toward 300 GiB. Admission requires measured free bytes minus active reservations to preserve 220 GiB plus conservative selected-command growth; otherwise build fails before writes. No internal fallback. Initial class budgets are provisional policy defaults: migration captures baseline, 72-hour soak measures peak/working-set/P95 growth, & final budgets require explicit receipt while total cap + free-space floors remain invariant.

## GC

Prune crashed scratch, expired tests, expired worktrees, stale recovery, superseded release targets, idle dev targets by LRU, sccache LRU, then Cargo registry/Git cache. Never prune active lease, pinned entry, current release target, current/previous signed artifact, or irreplaceable evidence.

Deletion acquires global GC lock + exclusive shared-cache maintenance lease + registry transaction, validates target twice, atomically renames entry into same-volume quarantine, commits quarantine receipt, deletes asynchronously, remeasures filesystem free bytes, & commits reclaimed-byte result. Unreadable paths, traversal errors, size errors, marker mismatch, registry disagreement, or volume-identity uncertainty are hard doctor/GC failures—never counted as zero or skipped. Run before/after heavy work, after interruption, at login, every 30 minutes via LaunchAgent, at watermark crossing, & manually.

Unknown directories inside managed root fail doctor instead of being ignored. Reconciliation detects missing entries, unmarked directories, dead leases, duplicate targets, path escapes, size drift, & wrong-volume mounts.

## Local-target prevention

First Cargo use automatically registers any new workspace. Scoped audit rejects real `target/`, `src-tauri/target` except owned link, `.cache/cargo-target`, `.cache/*target*`, legacy `.right-release/cache`, legacy target backups, or custom target roots. New scaffolds receive `target/` + `.cache/` ignores. Pre-build/post-build workspace audit plus scheduled registered-workspace audit prevent recurrence.

## Artifact boundary

Signed/published installers are artifacts, not cache. Retain locally current + one rollback artifact; GitHub Releases remains durable publication. Models/product assets require explicit manifest classification. Benchmark evidence is retained only when referenced by a durable evidence manifest. Everything else build-derived is rebuildable cache.

## Migration

Stop active Cargo/RightKit work, acquire exclusive maintenance lease, verify no live compiler/process-start token, inventory every source path + source volume identity into immutable classification manifest, & classify preserved artifacts/evidence. Same-volume sources atomically rename into source-volume recovery quarantine. Cross-volume sources use resumable checksummed copy into inactive D staging, fsync file/tree/receipt, validate source + destination manifests, atomically activate on D, then retain source quarantine through explicit rollback window before deletion. A moved cache never becomes active directly: validate marker, toolchain, workspace identity, containment, & registry schema first; only then activate. Invalid/stale material remains quarantined. Migrate active Rust toolchains, Cargo registry/Git cache, sccache, valid release targets, & installed Cargo binaries into unified root. Quarantine CodeRight, Membrane, HeardRight, CutRight, ViewRight, worktree, dispatch, `.build-home/tmp`, `.cache/cargo-target`, legacy `.right-release`, dated Cargo/cache backup build outputs. Write idempotent prepare/stage/copy-or-rename/validate/activate/expire receipt per path. Verify all suites before quarantine expiry; rollback follows receipts without deleting last validated source.

## Product & diagnostics

Expose `right-cache status|doctor|audit|prune|migrate|explain|cargo`. RightKit Hub displays category usage, active leases, watermarks, unmanaged paths, reclaimable bytes, last GC, migration state, & safe prune control.

Workspace doctor fails unless volume identity before path creation, cache-volume quota, shim precedence, direct rustup/Cargo visibility, real Cargo separation, external toolchains/home/XDG/child caches/temp/build/install dirs, supported Cargo feature probes, normalized toolchain selection, config-precedence scan, integration environment proofs, sccache native cap, scheduler, registry, shared/exclusive leases, capacity reservations/PID-start tokens, local-target audit, RightKit version parity, budget, & free-space watermark pass.

## Rollout

1. Freeze Cache V3 schema, ADR, threat model, & failing tests.
2. Build storage resolver, quota volume, registry, shared/exclusive lease + reservation model, GC, cargo/rustup shims, & doctor.
3. Integrate RightKit release target bridge + release retention.
4. Install external runtime, environment bootstrap, LaunchAgent, IDE/agent configuration.
5. Migrate existing cache/toolchains without duplication.
6. Publish breaking RightKit release & update every suite app together.
7. Run focused, contract, light-command storage, install-root, Cargo-config precedence, executable-alias, toolchain-selection, absolute-path/static-audit, scrubbed-environment, descendant-write inventory, shared-cache race, concurrent reservation, quota exhaustion, PID reuse, interruption, unreadable-directory, low-space, no-volume, wrong-volume, Tauri, IDE, delegated-agent, cross-volume migration-resume, & security tests.
8. Run 72-hour soak with zero unmanaged targets & cache within cap.

## Acceptance

- `command -v cargo` resolves external RightKit guard.
- Fresh project needs no manual cache setup & creates no local target.
- Full suite build writes no Cargo payload internally.
- Missing/wrong D fails before writes.
- Every managed-surface override/bypass route is rejected or forced external; doctor enumerates unsupported direct absolute binaries explicitly.
- `cargo install` cannot redirect install root internally.
- Toolchain choice is identical across `cargo +toolchain`, rustup, environment, directory override, & toolchain-file routes.
- `cargo fetch`, metadata, fmt, init/new & every light command inherit external Cargo/Rustup/temp policy.
- Cargo config precedence tests prove no project or CLI config can redirect build/cache writes.
- Concurrent same-workspace builds serialize under PID-start-aware lease; shared-cache GC cannot race any consumer.
- Concurrent heavy builds cannot exceed aggregate quota or admit against already-reserved capacity.
- Active entries survive GC; dead leases recover safely.
- All app-local/legacy targets are gone.
- All apps use same current RightKit cache contract.
- Scheduled GC has current successful receipt.
- Filesystem quota proves managed cache cannot exceed 192 GiB.
- D returns to at least 300 GiB free after migration/prune.
- Hub & doctor report zero unmanaged namespaces.
