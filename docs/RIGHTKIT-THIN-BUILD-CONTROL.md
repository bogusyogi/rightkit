# RightKit Thin Build Control — Canonical Architecture & Migration Plan

**Status:** live on macOS and Windows; predecessor core removed.
**Authority:** this document supersedes the previous build-control architecture and remediation plans for implementation decisions.  
**Purpose:** preserve aggressive Rust/Cargo cache reuse while preventing autonomous agents from competing for the same workstation, spawning unmanaged build trees, or leaving unsafe orphaned writers behind.

---

## 0. Live implementation state — 2026-08-23

macOS and Windows broker entrypoints construct `ThinJobService`. Installed shims keep
the v2 protocol and runtime-identity handshake; product repositories still use
`rightkit cargo ...`.

Current host configuration:

```text
BUILD concurrency = 3
ANALYSIS concurrency = 2
CONTROL concurrency = 4
Cargo build jobs = 4 macOS / 16 Windows
sccache = 0.17.0 client-side mode
Rust = 1.98.0 managed toolchain
shared Cargo home = enabled
shared compiler cache = enabled
persistent workspace generations = enabled
```

Implemented and live:

- one command/resource policy, global BUILD queue, workspace read/write locks,
  and exclusive toolchain mutation;
- `cargo run` holds BUILD plus workspace-writer admission only through
  compilation; Cargo's executable-start event moves it to a durable resident
  process state, releases both permits, and keeps cancellation/crash reaping;
- JSON thin state, broker singleton, persistent target generations, active
  writer recovery, process-group containment, cancellation, NDJSON logs, and
  terminal receipts;
- digest-pinned tools, with standalone sccache/Cargo extensions copied into
  managed storage while bundle-dependent Rust, package-manager, and system
  tools retain their real install paths;
- recursive Cargo-plugin calls through the managed shim;
- normal Cargo-owned `rustc`/`rustdoc` execution happens directly inside the
  admitted Cargo root through broker-injected `RUSTC`/`RUSTDOC`/`RUSTC_WRAPPER`
  pointing at digest-verified real tools (zero normal compiler broker child
  jobs). Compiler processes that still re-enter the broker through the managed
  `agent-bin` PATH (for example a build script invoking bare `rustc`) are
  delegated without a second BUILD permit or broker output-path rewriting,
  with authenticated compiler cwd under parent repository, Cargo home, or
  target generation while rejecting unrelated roots; this admits Cargo path
  dependencies elsewhere in the same repository;
- Cargo compiler environment forwarding preserves package/build-script inputs
  while broker-owned tool, path, cache, loader, and unstable-feature authority
  remains stripped and broker-injected;
- macOS Rust tools receive a broker-owned fallback loader path, while bootstrap
  repairs Rust 1.98's missing target-tool `libLLVM.dylib` companion link so
  compiler grandchildren work without accepting client loader overrides;
- explicit Cargo target triples registered in broker config remain selectable
  within one persistent workspace generation, including universal macOS builds;
- same-workspace nested `cargo update` delegation for workspace-writing Cargo
  plugins, with token validation, workspace-containment checks, and root-cancel
  propagation;
- one global `CARGO_HOME`, one global sccache directory, client-side sccache,
  one shallow managed temp directory, and per-workspace target generations;
- source bootstrap and immutable runtime cutover; a `--force` re-bootstrap
  preserves operator-tuned `sccache.maxSizeBytes` & `bounds` from the existing
  config (validated, with fallback to defaults) instead of silently resetting
  them; the Windows cutover PID probe resolves both the expected and observed
  node executable paths, so the nvm4w junction vs physical-path split cannot
  reject a healthy activated broker;
- native Windows Job Object containment with `KILL_ON_JOB_CLOSE`, synchronous
  launcher assignment before IPC START, creation-time process identity, cancellation, and
  conditional crash-recovery generation rotation only when process death is unproven
  (a cleanly-reaped cache-unavailable cancellation keeps the warm generation);
- runtime cutover admission fence plus live broker `DRAIN_STATUS` covering synchronous admissions
  and every queued/running root—including analysis—followed by active-root marker proof; service
  stop occurs only after both report zero; readiness-independent GC covers retired generations;
  predecessor SQLite, lifecycle, reservation, maintenance,
  server-owned sccache, and v1 migration paths are absent.

Evidence:

```text
build-control suite: 304/304 tests pass
Windows native-focused package set: 57/57 tests pass
live cargo upgrade: HeardRight passes
live recursive cargo upgrade/update: CodeRight passes
macOS and Windows re-bootstrap: passes with intended concurrency/cache settings
native acceptance: docs/plans/evidence/rightkit-thin-build-control-v2.7-native-acceptance.md
macOS + Windows v2.8 acceptance: docs/plans/evidence/rightkit-thin-build-control-v2.8-acceptance.md
```

Cross-host Mac-to-Windows validation, detached SSH lifecycle, replica isolation, native-build
routing, & linker watchdog contracts live in `RIGHTKIT-DUAL-HOST-BUILD-ROUTING.md`.

Bootstrap originally copied `rustc` and `rustdoc` as isolated executables. That
breaks their required sibling toolchain libraries on macOS and Windows. Live
configuration now preserves bundle-relative tool paths while retaining digest
validation; managed copying is limited to standalone tools.

Managed `TMPDIR` is intentionally shallow under the external root. sccache
creates a transient startup socket below `TMPDIR`; putting that directory inside
the full workspace-generation path exceeds macOS's 103-byte Unix socket limit.

Superseded architecture/remediation/handoff documents and predecessor core are
deleted. macOS v2.8 measurement keeps Cargo's incremental default and leaves
`SCCACHE_BASEDIRS` disabled: neither alternative produced a material reuse win.
BUILD concurrency is 3. Resident `cargo run` processes do not consume compile
capacity after Cargo launches their executable.

---

## 1. What RightKit is actually for

RightKit is **not** a replacement build system.

Cargo owns Rust builds. `rustc` owns compilation. `sccache` owns compiler-output caching.

RightKit exists to coordinate those tools across autonomous agents on one workstation.

The required guarantees are:

1. Agents cannot launch unmanaged Cargo/rustc/rustup work.
2. Expensive compilation is queued and bounded machine-wide.
3. Cargo caches are reused correctly.
4. Each Cargo workspace keeps one persistent warm target tree.
5. Cross-workspace compiler reuse comes from one shared sccache cache.
6. Cancelling a job kills the process tree that belongs to that job.
7. A broker crash cannot let an uncertain old compiler corrupt a future build.
8. Slow or failed jobs are diagnosable from RightKit's own logs and receipts.

Anything not required to provide those guarantees should not be in the build-control core.

---

## 2. Non-goals

Do **not** turn RightKit into:

- Bazel;
- Nix;
- a generic build graph;
- a generic result cache;
- a CI system;
- a network-transparent distributed executor (cross-host orchestration stays in
  build-control clients & still submits to one host-local broker);
- an exactly-once workflow engine;
- a transactional storage allocator;
- a generalized resource-market/token scheduler.

Do not add machinery merely because the old implementation had it.

Prefer the smallest mechanism that preserves the guarantees in section 1.

---

## 3. Target architecture

```text
Agent / RightKit shim
        |
        v
+-------------------------+
| Thin RightKit broker    |
|                         |
| command policy          |
| queue / workspace locks |
| process ownership       |
| cache env injection     |
| simple state / receipts |
+------------+------------+
             |
             v
           Cargo
             |
        sccache client
             |
            rustc
```

Shared machine-wide:

```text
CARGO_HOME
sccache compiler cache
managed immutable tools
```

Per Cargo workspace:

```text
one persistent active target generation
```

The broker stays. What changes is that the broker coordinates Cargo instead of trying to become a build system.

---

## 4. Canonical cache model

Caching is a first-class requirement.

### 4.1 Layer 1 — shared Cargo home

Use one machine-global `CARGO_HOME`.

It contains reusable Cargo registry and Git/source state.

Example:

```text
<rightkit-root>/
  global/
    cargo-home/
```

Every managed Cargo job receives that same `CARGO_HOME`.

Do not create a separate Cargo home per repository or request.

---

### 4.2 Layer 2 — shared sccache

Use one machine-global compiler cache.

Example:

```text
<rightkit-root>/
  global/
    compiler-cache/
```

Compatible Rust compilations in one workspace may reuse compiler outputs across
target-generation changes. sccache 0.17 still hashes Rust compiler cwd, so safe
cross-workspace reuse remains blocked until Rust `SCCACHE_BASEDIRS` support lands.

Do **not** share one Cargo target directory across unrelated workspaces.

Prefer client-side sccache compilation when supported and verified by the pinned sccache version:

```text
SCCACHE_CLIENT_SIDE=1
```

The desired process topology is:

```text
RightKit broker
  owned foreground sccache storage gateway

RightKit job process group
  cargo
    client-side sccache
      rustc
```

The compiler remains inside the requesting job's process domain while one broker-owned gateway keeps machine-global cache storage reachable. The gateway uses a managed Unix socket on macOS and a dedicated loopback port on Windows, disables idle exit, runs with explicit read-write storage configuration, and terminates when broker ownership IPC disappears. Every initial or recovery spawn re-verifies the registered executable identity and digest before execution; a changed tool emits `TOOL_CHANGED` and blocks recovery. macOS runtime preflight uses a private short state root so its derived Unix socket stays below the kernel path limit. Windows runtime preflight allocates a separate ephemeral loopback port so candidate teardown cannot race production gateway startup.

Periodic statistics are telemetry, not gateway health. A stats timeout emits a
degraded `CACHE_STATS_SNAPSHOT` without restarting gateway, cancelling builds,
or rotating workspace generations. Startup and actual recovery still require
exact storage identity proof.

Build admission consumes a fresh broker health lease proving owned gateway process liveness, endpoint reachability, and a bounded storage RPC whose parsed cache path, size limit, and version exactly match broker configuration. Bootstrap records `sccache.expectedVersion` from the digest-pinned executable; presence or prefix matches are invalid. Concurrent admissions share one probe or recovery. A failed probe permits one bounded gateway restart; failure after that window blocks compilation. `sccache --show-stats` never proves health by itself because it can emit synthetic empty statistics when no daemon exists; RightKit accepts it only between owned-process and endpoint checks and records its counters as separate telemetry.

Every compiler environment sets `SCCACHE_START_SERVER=0`, `SCCACHE_NO_DAEMON=1`, `SCCACHE_IGNORE_SERVER_IO_ERROR=0`, and `SCCACHE_LOCAL_RW_MODE=READ_WRITE`. Any upstream emergency autostart therefore remains inside requesting process group instead of escaping as an orphan. Gateway loss cancels active compile roots, invalidates their target generation, and emits typed recovery, runtime-block, admission, and statistics events.

Do not permanently disable sccache as a containment workaround.

If managed sccache is disabled, unavailable, or fails its admission probe, RightKit must reject compilation before spawning Cargo or rustc. Direct-rustc fallback is forbidden. Emit an immediate `CACHE_ADMISSION_BLOCKED` event plus a terminal receipt containing exact cache health and failure reason.

---

### 4.3 Layer 3 — persistent per-workspace Cargo target

Each Cargo workspace receives one persistent warm target directory through a
broker-owned `--target-dir` argument. RightKit does not export generation-specific
`CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` into rustc cache keys.

Example:

```text
<rightkit-root>/
  workspaces/
    <workspace-key>/
      generations/
        000012/
          target/
```

Normal `build`, `check`, `test`, `clippy`, etc. continue using generation `000012`.

Do not create a new generation for every build.

Cargo owns the contents of `target/`, including normal directories such as:

```text
target/
  debug/
    deps/
    build/
    incremental/
  release/
  <target-triple>/
```

RightKit must not invent parallel copies of Cargo's own artifact hierarchy unless there is a concrete Cargo requirement.

---

## 5. Crash-safe target generations

The generation wrapper exists only to make uncertain process death harmless.

Normal operation:

```text
workspace A -> generation 12 -> target/
workspace A -> generation 12 -> target/
workspace A -> generation 12 -> target/
```

After a broker crash during a writer whose process tree cannot be proven dead:

```text
old orphan may still write -> generation 12
new work                -> generation 13
```

RightKit first tries to reap the persisted process tree. A `REAPED` result proves
that no old writer remains, so Cargo can safely resume in the same generation as
it does after ordinary cancellation. `REAP_PENDING` means process death remains
uncertain, so new work must move to a fresh generation.

It only needs to ensure that future work cannot write to the same physical generation.

On restart:

1. Read the simple active-writer marker.
2. Reap its persisted process group.
3. On `REAPED`, mark the job abandoned and preserve the warm generation.
4. On `REAP_PENDING`, retire that generation and advance to a fresh one.
5. Admit unrelated work normally.
6. Never globally wedge the machine because one old tree could not be certified dead.

This turns orphan reaping from a global correctness prerequisite into cleanup.

---

## 6. Workspace identity

Target reuse must be keyed to the **Cargo workspace**, not merely the Git repository.

Resolve the Cargo workspace root using Cargo's own semantics, for example through a controlled:

```bash
cargo locate-project --workspace
```

or equivalent metadata query.

Canonicalize the resolved path and derive the workspace key from it:

```text
workspaceKey = SHA-256(canonical Cargo workspace root)
```

One broker configuration pins one managed Rust toolchain, so toolchain identity
is not part of the workspace key. Toolchain identity belongs to receipt
evidence, compiler/cache identity, and bootstrap/runtime identity instead;
Cargo fingerprints already determine whether existing target artifacts remain
reusable after a toolchain change. Do not multiply persistent workspace
directories merely because the pinned toolchain changes.

Git repository identity may be recorded in receipts for traceability, but it must not define the Cargo target-cache unit if Cargo itself considers several crates one workspace.

---

## 7. Command/resource policy

There must be **one declarative source of truth** for command policy.

Do not maintain separate hand-written copies in validation, classification, scheduling, config validation, and docs.

Each command declares the resources it needs.

The initial model can stay small.

### CONTROL

Examples:

```text
cargo --version
rustc --version
rustc --print ...
cargo locate-project
cargo metadata --locked / --frozen
cargo tree --locked / --frozen
```

Properties:

```text
compile slot: no
workspace access: none/read
concurrency: 4
```

CONTROL must remain responsive while a build runs.

### ANALYSIS

Examples:

```text
cargo outdated
cargo audit
cargo deny
cargo machete        # non-fix mode
```

Properties:

```text
compile slot: no
workspace access: read
concurrency: 2 initially
```

An ANALYSIS command must not hold the machine's only compile slot merely because the tool internally creates temporary manifests or performs dependency resolution.

### WORKSPACE_WRITE

Examples:

```text
cargo update
cargo upgrade
cargo add
cargo remove
cargo set-version
cargo fmt            # when mutating files
cargo machete --fix
```

Properties:

```text
compile slot: no unless the command actually compiles
workspace access: write
one writer per Cargo workspace
```

### BUILD

Examples:

```text
cargo build
cargo check
cargo test
cargo clippy
cargo doc
cargo nextest
raw rustc/rustdoc compilation
```

Properties:

```text
compile slot: yes
workspace/artifact access: write
global active compile roots on the Air: 3
CARGO_BUILD_JOBS: measured bounded value, initially 3 or 4
```

Tool provisioning that compiles Rust may have a different **priority**, but it still consumes the same machine compile budget. Do not create a second independent CPU/RAM compile lane.

Mac compiler workers are sized separately from root admission: bootstrap uses
`max(1, min(8, floor(logical CPUs * 2 / 3), floor(memory GiB / 2)))`.
This retains four workers on the 8 GiB Air & selects eight on the 12-core,
24 GiB Mac Mini. `bootstrap-macos-cli.mjs --cargo-build-jobs <count>` applies
a measured operator override through managed configuration & runtime cutover.
A build-class root still consumes the entire host compile budget; raising its
worker count does not admit competing heavy roots. Resource receipts report
both `host.cargoBuildJobs` & `host.compileBudgetUnits` alongside hardware facts.

### TOOLCHAIN_WRITE

Examples:

```text
rightkit rustup target add <registered-target>
```

Properties:

```text
globally exclusive
uses Rustup home that owns digest-registered Cargo/rustc
requested target must exist in broker targetTriples
```

macOS bootstrap registers `aarch64-apple-darwin`,
`x86_64-apple-darwin`, `aarch64-apple-ios`, &
`aarch64-apple-ios-sim`. A new app targeting one of these needs no broker
change: provision missing std through `rightkit rustup target add <target>`,
then build through `rightkit cargo ... --target <target>`. Physical iPhone
builds use `aarch64-apple-ios`; Apple Silicon Simulator builds use
`aarch64-apple-ios-sim`.

A genuinely new target requires one source-level allowlist change in host
bootstrap, an exact generated-config test, an admission test, & guarded broker
cutover. Never edit live `config.json`; next bootstrap overwrites it.

Unknown commands fail closed.

### Intentionally denied Cargo commands

`cargo install` is **intentionally denied** for managed agents. A generic agent
install would place unregistered executables beside digest-pinned managed
tools, so it fails closed with an explicit error. Managed Cargo tool
provisioning is an operator path: stage, compile, hash, register the immutable
tool identity, and only then expose it through the managed tool registry.

The following read-only builtins are currently unrouted and reject as unknown
commands. They are follow-up candidates, addable through the same declarative
command policy with tests when a concrete Right Suite tool needs one:

```text
verify-project
pkgid
report
info
config
new
init
```

Publishing/credential commands stay denied permanently:

```text
publish
package
login
logout
owner
yank
uninstall
```

---

## 8. Scheduler

Keep scheduling deterministic and boring.

Initial MacBook Air settings:

```text
BUILD roots:               1 globally
CARGO_BUILD_JOBS:          3 or 4 after measurement
CONTROL concurrency:       4
ANALYSIS concurrency:      2
WORKSPACE_WRITE:           one writer per workspace
TOOLCHAIN_WRITE:           globally exclusive
```

Priority is queue-only:

```text
interactive human
installer/release
normal agent
background
```

Higher priority may move ahead of queued work.

It does not preempt an already-running compiler.

Do not build an adaptive token economy first.

Machine pressure signals may later **derate** admission:

- critically low free memory;
- critically low free disk;
- severe thermal pressure.

They are safety circuit breakers, not the primary scheduler.

---

## 9. Process containment and cancellation

Keep the existing platform containment implementation where it is useful.

### macOS / POSIX

Each root job owns one process group/session.

Expected topology:

```text
RightKit broker
  managed launcher / job PGID
    cargo
    build script
    linker
    rustc
    rustc
```

Cancellation:

```text
SIGTERM whole owned process group
wait bounded grace
SIGKILL whole owned process group
verify/best-effort enumerate
```

### Windows

Use the native Job Object implementation.

A job must use `KILL_ON_JOB_CLOSE` semantics so broker/process failure causes owned descendants to die with the Job Object.

Windows acceptance passed on its native host on 2026-08-22: build, normal exit,
explicit cancellation, non-zero failure, broker-crash reap, affected-workspace
generation rotation, and post-restart build all passed.

### Nested work

Do not allow nested BUILD / WORKSPACE_WRITE / TOOLCHAIN_WRITE requests to bypass scheduling.

For the first thin implementation:

- allow only an explicit narrow nested CONTROL/ANALYSIS allowlist; or
- reject nested non-control work outright.

Do not restore the old root-wide heavy capability-token scheme unless a concrete Cargo behavior requires it.

---

## 10. Broker singleton

Only one broker may operate on a machine's RightKit state.

Acquire singleton ownership **before**:

- opening/mutating state;
- recovery;
- generation rotation;
- maintenance;
- process cleanup.

The second broker must fail immediately before touching job state.

Do not treat a launchd label as the invariant.

Do not unlink a same-UID Unix socket merely because it exists. Prove it is stale or hold a separate OS-enforced singleton.

---

## 11. State model

The thin broker does not need the old SQLite workflow engine.

Persist only what is necessary to survive broker restart safely:

```text
<rightkit-root>/
  state/
    broker-lock / broker identity
  workspaces/
    <workspace-key>/
      identity.json
      state.json
      generations/
        000012/
          target/
          tmp/
  receipts/
  logs/
```

Workspace state needs to answer:

```text
current generation?
was an artifact writer active when the broker disappeared?
what was its process ownership record?
```

Job states can be:

```text
QUEUED
RUNNING
SUCCEEDED
FAILED
CANCELLED
TIMED_OUT
```

`ABANDONED` is a restart interpretation, not a distributed workflow protocol.

Queued work does not need to survive a broker restart.

Running work is not automatically replayed.

The calling agent may retry it.

Do not implement exactly-once payload execution for Cargo builds.

---

## 12. Environment injection

RightKit, not the agent, owns build/cache paths.

Managed requests must not override:

```text
CARGO_HOME
CARGO_TARGET_DIR
RUSTUP_HOME
RUSTC_WRAPPER
SCCACHE_DIR
SCCACHE_CLIENT_SIDE
TMPDIR / TMP / TEMP when RightKit controls them
```

Agents also may not bypass through:

```text
cargo --target-dir ...
absolute unmanaged cargo/rustc paths
custom RUSTC_WRAPPER
workspace aliases that redirect Cargo
unmanaged rustup run cargo/rustc
```

The installed RightKit shims remain the agent entrypoint.

Product repositories should continue invoking:

```bash
rightkit cargo ...
```

or resolving the managed Cargo shim first on `PATH`.

Do not migrate product repositories back to direct Cargo.

---

## 13. Managed external Cargo tools

Tools such as:

```text
cargo-outdated
cargo-upgrade
cargo-about
cargo-audit
cargo-deny
cargo-machete
cargo-nextest
```

must live in a stable machine-global managed tool root.

Example:

```text
<rightkit-root>/
  global/
    tools/
      cargo-edit/<digest>/...
      cargo-outdated/<digest>/...
```

Do not install them inside a workspace target generation.

Their binary identity may remain digest-pinned.

The same command policy table declares the resource class for each plugin.

Recursive plugin calls to Cargo must re-enter the managed Cargo shim.

---

## 14. Cargo configuration

Do not enforce a security/authority boundary with ad-hoc line regexes.

Choose one of two policies:

### Live implementation

Read ancestor/workspace `.cargo/config` and `.cargo/config.toml` as bounded,
regular, non-symlink files. Permit settings that do not redirect managed tools,
outputs, flags, aliases, or target selection; reject redirect-capable settings
with a clear error.

This exception was required by live HeardRight, whose checked-in configuration
contains only `net.git-fetch-with-cli = true`. Rejecting every Cargo config made
managed Cargo unable to run a first-party product. Target/cache/toolchain
redirection remains rejected.

### Required hardening if policy grows

Parse Cargo TOML structurally before expanding the documented subset.

If this is done, accepted configuration must participate in build/cache identity where semantically relevant.

Do not claim full TOML parsing while bounded conservative validation is active.

---

## 15. Disk behavior and GC

Do not restore the reservation ledger.

With one globally bounded BUILD queue, admission can be simple:

Before BUILD:

```text
free disk >= configured safety floor?
yes -> admit
no  -> reject
```

While BUILD runs, periodically sample free disk.

If the machine reaches a critical floor, cancel the active build with an explicit storage reason.

GC order:

1. Never delete an active generation.
2. Prefer retired/suspect generations, oldest first.
3. When total managed target storage still exceeds `targetCacheCapBytes`,
   evict **idle current generations** least-recently-used first. Evicting an
   idle current generation means advancing the workspace to a fresh generation
   under the same per-workspace write exclusion used by BUILD, then renaming
   the retired generation to trash. A current generation is never deleted
   in place without first making a different generation current.
4. Atomic rename to trash before recursive delete is sufficient.

The cap is an **eventual** budget, not a hard invariant: it may be temporarily
exceeded during an active build, while every candidate is active/locked, or
when one unavoidable active generation itself exceeds the cap. When the cap
cannot be enforced, emit one `TARGET_CACHE_CAP_UNENFORCEABLE` event and keep
admitting work; `freeFloorBytes` remains the immediate hard circuit breaker.

Workspace state carries advisory LRU metadata (`lastUsedAt`, measured size,
dirty flag). Size accounting may be approximate between measurements; safety
comes from workspace exclusion, generation rotation, and the free-disk floor,
not from byte-exact accounting. Avoid full-tree size walks in request hot
paths: measure in the background and rescan only dirty/missing idle generations.
While a generation is active, reuse its last advisory measurement and defer its
rescan until terminal maintenance runs under an idle state.

Do not rebuild:

- durable GC leases;
- trash-intent transactions;
- reservation accounting;
- namespace quarantine as the primary safety mechanism.

Generation isolation provides the safety boundary.

---

## 16. Logging and receipts

Instrumentation is part of delivery, but it should not become another synchronous bottleneck.

Use append-only structured NDJSON.

Do not fsync a log file and its directory on every heartbeat.

Record:

```text
request received/classified
queued
started
pid / pgid or Job Object identity
periodic RUNNING sample
cancel requested
exit
cache mode
cache stats delta
terminal summary
```

A RUNNING sample every 5–10 seconds may include:

```text
elapsed time
CPU time if cheap
RSS/peak RSS if cheap
output byte count
last-output timestamp
free disk
```

Windows samples now include native Job Object CPU time & peak committed memory
under `resources.processTree`, plus host logical CPU count, total memory &
minimum observed free memory. CPU accounting includes exited descendants;
`peakCommittedBytes` measures committed allocations, not physical RSS. Final
snapshots are retained for jobs shorter than the sampling interval & cancellations.

Mac samples use an installer-pinned native libproc addon, process-start identities
& managed process-group descendants. `peakMemoryBytes` is a sampled
physical-footprint peak at 25 ms intervals, labelled `sampled-physical-footprint-peak`;
it is distinct from Windows committed memory. Launcher-side `RUSAGE_CHILDREN`
recovers CPU for reaped short children, reconciled with sampled CPU using maximums
to avoid counting it twice. Graceful cancellation retains a final native snapshot;
forced launcher termination without final IPC reports `UNAVAILABLE`.
Mac activation requires a functional receipt with valid native CPU/memory fields.

Terminal receipts & events carry `resources` (`rightkit.build-resources.v1`)
alongside existing `queueMs` & `cargoRunMs`. Delegated compiler CPU is rolled up
separately; `sumPeakCommittedBytes` is a sum of child peaks, not a simultaneous
peak. Shared sccache gateway CPU is measured over the build window, with its
memory peak labelled `peakCommittedBytesSinceGatewayStart`. Combined CPU totals
are emitted only for complete readings & an exclusive cache attribution window.
Missing readers, failed queries & changed gateway epochs produce explicit
`UNAVAILABLE`, `ERROR` or `PARTIAL` measurements rather than zero usage.

Mac delegated compiler memory uses `sumPeakMemoryBytes`; gateway memory
uses `peakMemoryBytesSinceGatewayStart`, with the same attribution rules.

These observations do not change admission, Cargo job counts, profiles or quotas.

Terminal receipt should include approximately:

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
  "cache": {
    "mode": "sccache",
    "status": "NO_COMPILATION",
    "healthStatus": "SERVICE_OK",
    "reuse": {
      "status": "NO_COMPILATION",
      "hits": 0,
      "misses": 0,
      "bypassed": 0,
      "hitRate": null
    },
    "delta": {
      "compile_requests": 0
    },
    "strictNoFallback": true,
    "health": {
      "ownership": "broker",
      "gatewayPid": 1234,
      "endpoint": "/managed/tmp/sccache-gateway.sock",
      "sampledAt": "2026-08-24T12:00:00.000Z",
      "ageMs": 18,
      "leaseMs": 10000,
      "storageSampledAt": "2026-08-24T11:59:45.000Z",
      "storage": {
        "location": "Local disk: \"/managed/compiler-cache\"",
        "maxSizeBytes": 21474836480,
        "version": "0.17.0",
        "configuredRwMode": "READ_WRITE"
      }
    }
  }
}
```

`cache.delta` is nullable. Broker attributes cumulative client-side counters
only when one root owns the full counter window. Overlapping roots record
`delta: null`, `NOT_MEASURED`, & `concurrent-cache-clients`. Exclusive warm
no-op roots record exact `NO_COMPILATION`; exclusive recompiles record exact
hits & misses. Receipts never manufacture attribution they do not have.

`cache.healthStatus` reports service health. `cache.reuse.status` reports reuse
as `PROVEN`, `NO_COMPILATION`, `BYPASSED`, `MISS`, `MIXED`, or `NOT_MEASURED`.
`hitRate` covers cacheable compiles. `bypassed` stays separate because cheap
compiler probes & non-cacheable compilation are not equivalent cache misses.
`SERVICE_OK` is never a reuse result. Cache reports group only exact
`cacheCohort` values: host, workspace, command, profile, feature set, target,
toolchain, compiler identity, & cache epoch.

Managed cache environments set `CARGO_INCREMENTAL=0`. Cargo's development
default produces incremental rustc jobs that sccache refuses, so permitting it
would make `check`, `test`, & development builds systematically bypass cache.

`cache.health` is admission proof, not a stats-process result. Periodic
`CACHE_STATS_SNAPSHOT` events carry cumulative counters independently from job
admission and receipts.

Receipts summarize telemetry; they do not contain the full time series.

Run `node packages/build-control-broker/src/cache-proof-cli.mjs --iterations 3`
on each native host for deterministic reuse acceptance. It primes
`fixture-rust` with a tiny library build, clears only its marked live generation target, then requires at
least one hit & zero misses on every identical recompilation.

---

## 17. Result caching

Do not build generic broker-level result caching now.

Specifically, do not initially cache:

```text
cargo metadata stdout
test results
arbitrary command output
```

Those require complete semantic input identities and/or hermeticity guarantees.

Use the caches that already have the correct semantics:

```text
Cargo's warm target tree
sccache
Cargo registry/git cache
```

Only add command-result caching later for a narrowly-defined, demonstrably pure operation with a complete cache key.

---

# Migration from the previous implementation

## 18. HISTORICAL MIGRATION — COMPLETED. Do not implement from this section.

The thin core was built beside the predecessor core, the broker entrypoint was
switched to `ThinJobService`, source bootstrap became authoritative, and live
macOS + Windows native acceptance passed on 2026-08-22. The predecessor SQLite
lifecycle, reservation, maintenance, server-owned sccache, and v1 migration
cores were then removed (commit `328f2e073`). No migration work remains.
Acceptance evidence lives in
`docs/plans/evidence/rightkit-thin-build-control-v2.7-native-acceptance.md`.

## 19. v2.8 completion state

The macOS and Windows tracks are complete. Acceptance evidence lives in
`docs/plans/evidence/rightkit-thin-build-control-v2.8-acceptance.md` and cache
measurements live in
`docs/plans/evidence/rightkit-thin-build-control-v2.8-cache-experiment.md`.

Settled results:

1. normal Cargo-owned compiler processes execute directly under the admitted
   Cargo root with zero compiler broker round trips;
2. incremental policy is `KEEP CARGO DEFAULT`;
3. `SCCACHE_BASEDIRS` remains disabled after a negative alternate-path gate;
4. target budget can evict idle current generations by LRU while active
   generations remain protected;
5. client-side sccache receipts report service health without fabricated
   per-root deltas;
6. generic `cargo install` fails closed for managed storage and remaining
   policy/resource corrections are implemented.

Windows source bootstrap reproduces BUILD=3, Cargo jobs=16, the 150 GiB target
budget, client-side sccache, Cargo-default incremental behavior, and disabled
`SCCACHE_BASEDIRS`. Native compiler-active cancellation ends `REAPED` through
the Job Object with no surviving launcher or descendants; post-cancel work
reuses the generation successfully.
# Thin-core closure

## 20. Predecessor removal — completed

Commit `328f2e073` removed old state store, maintenance worker, job service,
scheduler, fsync-heavy event log, broker-owned sccache server, legacy fixtures,
and v1 migration/drain paths after macOS and Windows native acceptance. Commit
`54f2991b7` moved retired-generation sizing after broker readiness so cache GC
cannot block service activation.

---

## 21. Removed configuration/schema concepts

Schema rejects:

```text
SQLite lifecycle database requirement
lifecycle transition table
exactly-once crash semantics
reservationBytes
reservation ledger
usedBytes reservation accounting
namespace quarantine setting
processMayBeLive as a scheduling gate
maintenance leases
trash-intent transaction machinery
holdPermit
global wedge behavior for unresolved reaps
adaptive token-bucket scheduler proposals
generic command-result cache proposals
per-transition durable fsync requirement
```

Source bootstrap emits thin-only configuration on both hosts.

---

## 22. Things that must NOT be deleted

Keep:

```text
RightKit broker
agent shims
direct-Cargo enforcement
client protocol / list / status / cancel / attach
runtime identity handshake unless deliberately replaced
one command-policy source of truth
shared CARGO_HOME
shared sccache
persistent per-workspace target generations
bounded CARGO_BUILD_JOBS
global BUILD queue
workspace read/write exclusion
toolchain exclusivity
process-group containment
Windows Job Object implementation
managed/digest-pinned external tools
simple logs and receipts
simple retired-generation GC
```

These are the actual product.

`rightkit list` is one-shot discovery for in-flight roots plus recent terminal
requests. Live summaries include request ID, command, workspace, state, elapsed
time, output bytes, last output time, & attached-client count. `rightkit status
<request-id>` returns same live context or durable terminal receipt;
`rightkit attach <request-id>` replays bounded broker output and returns terminal
result. Client disconnect never cancels broker ownership or discards receipt.
Agents must recover detached work through these commands rather than resubmit.

---

# Agent implementation directive

Use the following as the task instruction when continuing the work:

> Implement the outstanding items of the RightKit Thin Build Control v2.8
> completion plan (`docs/plans/RIGHTKIT-THIN-BUILD-CONTROL-v2.8-IMPLEMENTATION.md`)
> against this canonical architecture. Do not redesign the thin architecture and
> do not reintroduce the old SQLite lifecycle, reservation, quarantine,
> exactly-once, maintenance-lease, generic result-cache, or adaptive token
> scheduler machinery.
>
> Preserve the RightKit agent/shim entrypoint and client protocol. Agents must
> continue to use managed RightKit Cargo rather than direct Cargo. Normal
> Cargo-owned compiler execution stays inside the admitted Cargo root process
> domain; raw agent `rustc`/`rustdoc` remains a top-level managed BUILD request.
>
> The settled guarantees are: one shared Cargo home, one shared sccache compiler
> cache, one persistent warm target generation per Cargo workspace, one globally
> bounded compile queue, per-workspace read/write exclusion, process-group / Job
> Object cancellation, target-generation rotation only for unproven process
> death, and truthful structured receipts. A job cancelled for
> sccache-unavailability whose process tree reaped cleanly keeps its warm
> generation: Cargo persists a crate's fingerprint only after its compile
> succeeds, so the interrupted crate is simply rebuilt (2026-08-29 — rotating
> on cache blips was the dominant cause of full dependency rebuilds).
>
> Cache policy changes (incremental compilation, `SCCACHE_BASEDIRS`) land only
> behind their measured evidence gates. Do not invent a universal cache-hit
> target. When uncertain, prefer deleting complexity over inventing new
> machinery, provided the guarantees in section 1 remain true.
