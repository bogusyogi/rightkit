# RightKit Thin Build Control — Canonical Architecture & Migration Plan

**Status:** live on macOS; old core retained pending destructive-cleanup gate.
**Authority:** this document supersedes the previous build-control architecture and remediation plans for implementation decisions.  
**Purpose:** preserve aggressive Rust/Cargo cache reuse while preventing autonomous agents from competing for the same workstation, spawning unmanaged build trees, or leaving unsafe orphaned writers behind.

---

## 0. Live implementation state — 2026-08-22

macOS broker entrypoint now constructs `ThinJobService`. Installed shims keep
the v2 protocol and runtime-identity handshake; product repositories still use
`rightkit cargo ...`.

Current host configuration:

```text
BUILD concurrency = 1
ANALYSIS concurrency = 2
CONTROL concurrency = 4
Cargo build jobs = 4
sccache = 0.17.0 client-side mode
Rust = 1.98.0 managed toolchain
shared Cargo home = enabled
shared compiler cache = enabled
persistent workspace generations = enabled
```

Implemented and live:

- one command/resource policy, global BUILD queue, workspace read/write locks,
  and exclusive toolchain mutation;
- JSON thin state, broker singleton, persistent target generations, active
  writer recovery, process-group containment, cancellation, NDJSON logs, and
  terminal receipts;
- digest-pinned tools, with standalone sccache/Cargo extensions copied into
  managed storage while bundle-dependent Rust, package-manager, and system
  tools retain their real install paths;
- recursive Cargo-plugin calls through the managed shim;
- Cargo-owned nested `rustc`/`rustdoc` delegation without a second BUILD permit
  or broker output-path rewriting, including authenticated compiler cwd under
  parent repository, Cargo home, or target generation while rejecting unrelated
  roots; this admits Cargo path dependencies elsewhere in the same repository;
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
- source bootstrap and immutable runtime cutover.

Evidence:

```text
build-control-broker package: 288/288 tests pass
live cargo upgrade: HeardRight passes
live recursive cargo upgrade/update: CodeRight passes
live re-bootstrap: passes with intended concurrency/cache settings
```

Bootstrap originally copied `rustc` and `rustdoc` as isolated executables. That
breaks their required sibling toolchain libraries on macOS and Windows. Live
configuration now preserves bundle-relative tool paths while retaining digest
validation; managed copying is limited to standalone tools.

Managed `TMPDIR` is intentionally shallow under the external root. sccache
creates a transient startup socket below `TMPDIR`; putting that directory inside
the full workspace-generation path exceeds macOS's 103-byte Unix socket limit.

Superseded architecture/remediation/handoff documents were deleted. Old core
modules remain dormant beside the live thin core because sections 20–21 allow
their deletion only after the complete destructive macOS crash/cancellation
acceptance set. Windows Job Object code remains preserved; live Windows
acceptance is still phase 6.

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
- a distributed executor;
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

All compatible Rust workspaces may reuse cacheable compiler outputs through sccache.

This is the intended mechanism for cross-workspace compiled-crate reuse.

Do **not** share one Cargo target directory across unrelated workspaces.

Prefer client-side sccache compilation when supported and verified by the pinned sccache version:

```text
SCCACHE_CLIENT_SIDE=1
```

The desired process topology is:

```text
RightKit job process group
  cargo
    sccache client
      rustc
```

The compiler should remain inside the requesting job's process domain while the cache remains machine-global.

Do not permanently disable sccache as a containment workaround.

If sccache fails for an individual request, RightKit may fall back to direct rustc, but that degradation must be explicit in the receipt.

---

### 4.3 Layer 3 — persistent per-workspace Cargo target

Each Cargo workspace receives one persistent warm `CARGO_TARGET_DIR`.

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

After a broker crash during a writer:

```text
old orphan may still write -> generation 12
new work                -> generation 13
```

RightKit therefore does **not** need to prove that the old process is dead before allowing future work.

It only needs to ensure that future work cannot write to the same physical generation.

On restart:

1. Read the simple active-writer marker.
2. For every workspace that had an active artifact writer when the broker disappeared, retire that generation.
3. Advance that workspace to a fresh generation.
4. Best-effort reap the old process group.
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

Canonicalize the resolved path and derive the workspace key from it plus the Rust toolchain identity where necessary.

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
cargo install when it compiles Rust
raw rustc/rustdoc compilation
```

Properties:

```text
compile slot: yes
workspace/artifact access: write
global active roots on the Air: 1 initially
CARGO_BUILD_JOBS: measured bounded value, initially 3 or 4
```

Tool provisioning that compiles Rust may have a different **priority**, but it still consumes the same machine compile budget. Do not create a second independent CPU/RAM compile lane.

### TOOLCHAIN_WRITE

Examples:

```text
rustup target add
other registered Rust toolchain mutations
```

Properties:

```text
globally exclusive
```

Unknown commands fail closed.

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

Use the existing Job Object implementation.

A job must use `KILL_ON_JOB_CLOSE` semantics so broker/process failure causes owned descendants to die with the Job Object.

Windows is not considered accepted until this has been built and exercised on a real Windows host.

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

With one global BUILD root, admission can be simple:

Before BUILD:

```text
free disk >= configured safety floor?
yes -> admit
no  -> reject
```

While BUILD runs, periodically sample free disk.

If the machine reaches a critical floor, cancel the active build with an explicit storage reason.

GC may be simple:

1. Never delete the current generation.
2. Never delete a generation with an active marker.
3. Prefer retired/suspect generations.
4. Delete oldest inactive generations when total managed target storage exceeds a cap or on explicit `rightkit gc`.
5. Atomic rename to trash before recursive delete is sufficient.

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
    "status": "OK",
    "delta": {}
  }
}
```

Receipts summarize telemetry; they do not contain the full time series.

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

# Migration from the current implementation

## 18. Documentation authority

Immediately update documentation so there is one implementation contract.

This document becomes the canonical build-control architecture.

The old `RIGHTKIT-BUILD-CONTROL-ARCHITECTURE.md` must either be replaced by this document or start with:

> **SUPERSEDED. Do not implement from this document. See the Thin Build Control architecture.**

The old `BUILD-CONTROL-REMEDIATION.md` must not remain a competing implementation plan.

Either delete it after useful incident evidence is extracted, or reduce it to an historical incident/proof archive that explicitly says:

> **SUPERSEDED AS AN IMPLEMENTATION PLAN. Historical evidence only.**

Do not let an agent synthesize work from both architectures.

---

## 19. Implementation order

Implement forward from the existing system.

Do not roll product repositories back to unmanaged Cargo.

### Phase 1 — thin core beside old core

Create:

```text
thin-command-policy.mjs
thin-scheduler.mjs
thin-state.mjs
thin-cache.mjs
thin-job-service.mjs
thin-logger.mjs
thin-singleton.mjs
```

Reuse:

```text
client/shim protocol
runtime identity handshake
process-tree.mjs
Windows Job Object code
immutable tool verification where useful
runtime cutover infrastructure where useful
```

### Phase 2 — focused tests

Before changing the live broker entrypoint, prove:

1. two BUILD requests -> one RUNNING, one queued;
2. CONTROL completes during active BUILD;
3. ANALYSIS does not consume BUILD slot;
4. same workspace consecutive builds use same generation;
5. crash marker rotates only affected workspace generation;
6. workspace read/write exclusion behaves correctly;
7. nested heavy work rejects;
8. second broker fails before state mutation;
9. cache environment is injected correctly;
10. sccache degradation is explicit.

### Phase 3 — switch broker entrypoint

Change `broker.mjs` to construct the thin service.

Keep the existing network protocol so installed shims and product repositories do not need a simultaneous migration.

### Phase 4 — bootstrap/config

Make source bootstrap authoritative.

For the Air, source defaults should initially express:

```text
BUILD concurrency = 1
ANALYSIS concurrency = 2
CONTROL concurrency = 4
Cargo build jobs = measured 3 or 4
shared Cargo home = enabled
shared sccache = enabled after client-side containment proof
persistent workspace generations = enabled
```

Remove live-config-only tuning.

A re-bootstrap must reproduce the intended system.

### Phase 5 — live macOS acceptance

Before deleting the old code, prove on the actual machine:

1. two simultaneous BUILD requests queue correctly;
2. CONTROL stays responsive;
3. `cargo outdated`/analysis does not hold the compile slot;
4. warm workspace build reuses its generation;
5. cold target + warm sccache produces real cache hits;
6. active build cancellation leaves zero attributed compiler processes;
7. broker kill during build rotates the affected generation;
8. restarted build cannot write into the suspect generation;
9. unrelated workspace builds remain admissible after another workspace crashes;
10. second broker is rejected before touching state;
11. re-bootstrap reproduces the intended cache/concurrency settings;
12. receipts expose queue/run/cache evidence.

### Phase 6 — Windows acceptance

Build and prove the Job Object implementation on a real Windows host before claiming Windows support complete.

---

# What to delete from the current system

## 20. Delete only after thin macOS acceptance

Once the thin broker is live and the acceptance tests pass, remove the old active implementation instead of carrying two systems forever.

Primary deletion candidates:

```text
tools/rightkit/packages/build-control-broker/src/state-store.mjs
tools/rightkit/packages/build-control-broker/src/state-store.test.mjs

tools/rightkit/packages/build-control-broker/src/maintenance-worker.mjs
tools/rightkit/packages/build-control-broker/src/maintenance-worker.test.mjs

old tools/rightkit/packages/build-control-broker/src/job-service.mjs
old tools/rightkit/packages/build-control-broker/src/job-service.test.mjs

old tools/rightkit/packages/build-control-broker/src/scheduler.mjs
old tools/rightkit/packages/build-control-broker/src/scheduler.test.mjs
```

Delete the old fsync-heavy event-log implementation if `thin-logger.mjs` fully replaces it:

```text
event-log.mjs
event-log.test.mjs
```

Delete broker-owned sccache server/process-ownership machinery that is no longer necessary **only after** client-side sccache behavior is proven with the pinned version.

Delete old v1 migration/drain code only after no installed host can still require it.

---

## 21. Configuration/schema concepts to remove

Remove these when the thin broker no longer references them:

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

Do not preserve a config field just because an old test expects it.

Rewrite tests to the thin architecture.

---

## 22. Things that must NOT be deleted

Keep:

```text
RightKit broker
agent shims
direct-Cargo enforcement
client protocol / status / cancel / attach
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

---

# Agent implementation directive

Use the following as the task instruction when continuing the work:

> Implement the RightKit Thin Build Control architecture in this document. Treat it as authoritative over the previous architecture/remediation plans. Do not redesign it and do not reintroduce the old SQLite lifecycle, reservation, quarantine, exactly-once, maintenance-lease, generic result-cache, or adaptive token scheduler machinery.
>
> Preserve the existing RightKit agent/shim entrypoint and client protocol unless a concrete incompatibility forces a change. Agents must continue to use managed RightKit Cargo rather than direct Cargo.
>
> The core goals are: one shared Cargo home, one shared sccache compiler cache, one persistent warm target generation per Cargo workspace, one globally bounded compile queue on the MacBook Air, per-workspace read/write exclusion, process-group/Job Object cancellation, crash-time target-generation rotation, and useful structured receipts.
>
> Implement the thin core beside the old one first; switch the broker entrypoint only after focused tests pass. Do not delete the old implementation until the live macOS acceptance suite passes. After acceptance, delete the obsolete modules and configuration concepts listed in sections 20–21 rather than maintaining two implementations.
>
> When uncertain, prefer deleting complexity over inventing new machinery, provided the eight guarantees in section 1 remain true.
