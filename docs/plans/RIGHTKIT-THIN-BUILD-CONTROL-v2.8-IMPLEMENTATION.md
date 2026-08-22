# RightKit Thin Build Control v2.8 — Completion Implementation Plan

**Status:** implementation/remediation plan.  
**Version:** 2.8.  
**Date:** 2026-08-23.  
**Scope:** finish the shipped Thin Build Control system without reopening its settled architecture.  
**Authority:** this document supersedes the v2.7 measurement/remediation protocol for **outstanding implementation work**. It does **not** replace the canonical Thin Build Control architecture except where it (a) records an implementation decision already closed by v2.7 evidence, or (b) corrects a contradiction between the canonical document and the shipped system. After v2.8 acceptance, merge the settled final state back into `docs/RIGHTKIT-THIN-BUILD-CONTROL.md` and mark this document historical/complete.

---

## 0. Executive decision

Do **not** redesign RightKit.

Keep the shipped thin model:

```text
Agent / RightKit shim
        |
        v
+-------------------------+
| Thin RightKit broker    |
|                         |
| command policy          |
| machine BUILD queue     |
| workspace exclusion     |
| process ownership       |
| managed environment     |
| state / receipts        |
+------------+------------+
             |
             v
           Cargo
             |
          sccache
             |
            rustc
```

Keep the cache/storage split:

```text
machine-global:
  CARGO_HOME
  SCCACHE_DIR
  digest-managed tools

per Cargo workspace:
  one persistent active target generation
```

Do **not** replace this with one machine-global `CARGO_TARGET_DIR`. Cargo workspaces should keep independent target trees; cross-workspace compiler reuse belongs in sccache.

v2.8 closes four remaining classes of work:

1. **Compiler topology:** implement the v2.7 evidence-backed decision to remove normal Cargo-owned `rustc`/`rustdoc` children from the broker job graph.
2. **Rust cache effectiveness:** resolve the observed near-zero Rust sccache reuse through controlled incremental-compilation and path-normalization experiments, then adopt only what the measurements justify.
3. **Multi-app storage lifecycle:** make `targetCacheCapBytes` meaningful across many active workspaces by allowing safe LRU eviction of idle current generations, without restoring the old reservation/lease machinery.
4. **Authority and operational correctness:** remove stale migration instructions, correct cache telemetry semantics, clean up policy mismatches, and make host resource defaults defensible.

The thin architecture remains closed. v2.8 is a completion pass, not an architecture-selection exercise.

---

# 1. Current live baseline

As of the v2.8 start point, the shipped system is:

```text
BUILD concurrency = 1
ANALYSIS concurrency = 2
CONTROL concurrency = 4
Cargo build jobs = 4
sccache = 0.17.0, client-side mode
Rust = 1.98.0 managed toolchain
shared CARGO_HOME = enabled
shared compiler cache = enabled
persistent workspace generations = enabled
macOS containment = accepted
Windows Job Object containment = accepted
predecessor lifecycle/reservation core = removed
```

Current successful architecture properties must survive every v2.8 change:

- one broker singleton;
- managed shims remain the agent entrypoint;
- direct unmanaged Cargo/rustc/rustup remains rejected for managed agents;
- one global BUILD admission authority;
- CONTROL remains responsive during BUILD;
- per-workspace read/write exclusion;
- one persistent active target generation per Cargo workspace;
- crash-time generation rotation for uncertain writers;
- process-group containment on macOS/POSIX;
- Job Object containment with `KILL_ON_JOB_CLOSE` on Windows;
- one machine-global `CARGO_HOME`;
- one machine-global sccache storage directory;
- client-side sccache;
- digest-pinned managed tools;
- source bootstrap reproduces the intended runtime;
- full-root receipts/logs remain the diagnostic unit.

The current architecture is not allowed to regress merely to improve benchmark numbers.

---

# 2. Evidence entering v2.8

## 2.1 Compiler-child brokerage is no longer an open architecture question

The v2.7 protocol inspected 40,039 thin receipts:

```text
40,039 thin receipts
39,186 compiler requests
31,817 rustc requests
7,366 rustc version/print probes
3 rustdoc requests
666 Cargo roots

compiler probe median runMs = 316 ms
compiler probe p90 runMs = 710 ms
compiler probe cumulative runMs = 56.5 min

19,256 compile children performed redundant cache sampling
```

v2.7 explicitly closed the decision:

> Remove normal Cargo-owned compiler children from the broker job graph.

That is an implementation decision, not a new v2.8 proposal.

The target is zero normal compiler-child broker jobs.

Agent-initiated raw `rustc` / `rustdoc` remains a managed top-level BUILD request.

---

## 2.2 Rust compiler-cache reuse is currently poor

The live measurement supplied during the v2.8 review showed approximately:

```text
Rust:
  31 hits
  6,465 misses
  ~0.48% hit rate

C/C++:
  1,098 hits
  1,263 misses
  ~46% hit rate
```

Do not treat the exact global percentage as a permanent acceptance metric: client-side sccache counters are cumulative and normal root receipts cannot currently attribute a precise per-job delta.

The measurement is nevertheless strong enough to establish:

```text
Rust cache reuse is not currently delivering the intended cross-generation /
cross-workspace value.
```

Two plausible and source-backed causes require immediate controlled testing:

1. incremental Rust compilation;
2. absolute-path variation across target generations/worktrees.

---

## 2.3 Incremental Rust compilation and sccache are competing cache strategies

Cargo's development profile normally enables incremental compilation for workspace members/path dependencies.

sccache cannot cache incrementally compiled Rust crates.

Therefore RightKit currently has two complementary but partially conflicting caches:

```text
persistent Cargo target generation
  -> strongest for same-workspace hot edit/rebuild loops

sccache
  -> strongest for cacheable non-incremental compilation across
     cold generations, worktrees and independent workspaces
```

Do not assume both apply to the same local crate compilation.

v2.8 must measure the tradeoff and make the policy explicit.

---

## 2.4 `SCCACHE_BASEDIRS` exists but production activation is still gated

The shipped cache implementation already supports a `SCCACHE_BASEDIRS` gate.

The current production config does not enable it.

sccache documents `SCCACHE_BASEDIRS` specifically to normalize absolute path prefixes across:

- different checkout directories;
- CI/worktree locations;
- multiple project roots;
- other path-varying builds.

RightKit target generation rotation changes the absolute path:

```text
.../generations/000012/target
.../generations/000013/target
```

so path normalization must be measured now.

---

## 2.5 The current target-cache cap is not enforceable across many warm workspaces

Current GC:

- counts generation storage;
- never deletes the current generation;
- never deletes an active generation;
- deletes retired generations only.

Therefore this state is possible:

```text
targetCacheCapBytes = 150 GiB

20 idle workspaces
10 GiB current generation each
0 retired generations

managed target storage = 200 GiB
GC candidates = 0
```

The configured cap is therefore currently a retired-generation cleanup target, not a real multi-workspace cache budget.

v2.8 must add safe eviction of **idle current generations**.

---

## 2.6 Client-side sccache and the canonical receipt example disagree

The canonical document shows a cache delta in the terminal receipt.

The live client-side implementation intentionally emits no attributable per-root delta:

```text
reason = client-side-counters-unattributable
delta = null
```

The production receipt contract must describe reality.

Controlled cache experiments may use isolated global sccache counter windows; normal production receipts must not manufacture attribution they do not have.

---

## 2.7 Other source/code mismatches entering v2.8

The following are lower severity but must be resolved:

- the canonical workspace-identity text mentions toolchain identity "where necessary", while the implementation key is SHA-256 of canonical workspace path only;
- the canonical document does not explicitly record intentionally denied Cargo builtins;
- generic `cargo install` currently receives `CARGO_INSTALL_ROOT` pointing at the managed tools root, even though resulting binaries are not digest-registered;
- the RightKit root `Cargo.toml` comment still describes two concurrent builds although live/default BUILD concurrency is one;
- the Windows Cargo-job heuristic inherits an Apple-silicon-style `logical / 2` approximation that has no general x86/Windows meaning.

---

# 3. Non-goals

v2.8 must **not** introduce:

- Bazel;
- Buck;
- Nix;
- a generic build graph;
- one shared target directory for unrelated workspaces;
- a generic artifact CAS;
- a generic result cache;
- a distributed executor;
- exactly-once build execution;
- a transactional storage allocator;
- reservation ledgers;
- maintenance leases;
- adaptive token economies;
- weighted scheduling before simple capacity is proven insufficient;
- automatic BUILD concurrency above the accepted host value;
- permanent `CARGO_INCREMENTAL=0` without measurement;
- permanent `SCCACHE_BASEDIRS` activation without the controlled experiment;
- shared Cargo `build.build-dir` adoption in production;
- nightly Cargo dependency for production build control.

Prefer the smallest change that closes a demonstrated gap.

---

# 4. Decision classes

Every v2.8 item belongs to one of three classes.

## 4.1 SETTLED — implement

These require implementation, not further architecture debate:

```text
A. remove normal Cargo-owned rustc/rustdoc broker child jobs
B. correct stale canonical migration/agent instructions
C. correct client-side cache receipt semantics
D. make target-cache budget able to evict idle current generations
E. stop generic cargo install from polluting trusted managed-tool storage
F. replace the Windows logical/2 CPU heuristic
G. reconcile workspace-key documentation with actual supported identity model
H. fix stale root Cargo.toml concurrency commentary
```

## 4.2 MEASUREMENT-GATED — implement experiment, then decide

```text
I. incremental compilation policy
J. SCCACHE_BASEDIRS permanent activation
K. BUILD concurrency > 1
L. sccache compression tuning
M. Cargo resolver/default-member/workspace-policy changes where behavior changes
```

## 4.3 RESEARCH / WATCH-ONLY — do not adopt in v2.8 core

```text
N. shared Cargo build.build-dir
O. Cargo -Z feature-unification / workspace feature-unification
P. cargo-hakari unless a specific workspace proves feature-churn rebuilds
Q. remote/multi-level sccache
R. distributed compilation
```

---

# 5. Phase 0 — documentation authority repair

Do this first.

## 5.1 Canonical document cleanup

Update:

```text
tools/rightkit/docs/RIGHTKIT-THIN-BUILD-CONTROL.md
```

The document currently starts from a live post-cutover state but still contains historical instructions to:

- build the thin core beside the old core;
- switch the broker entrypoint later;
- retain predecessor code until acceptance;
- implement a migration that is already complete.

That is unsafe in an agentic repository.

Replace the historical migration sections with:

```text
HISTORICAL MIGRATION — COMPLETED
Do not implement from this section.
```

or remove them after preserving useful evidence.

The final agent directive must describe only current work.

## 5.2 Add an explicit open-items section

The canonical document must record:

```text
OPEN v2.8 WORK

1. remove normal Cargo-owned compiler children from broker job graph
2. run incremental-vs-sccache experiment
3. run SCCACHE_BASEDIRS path-normalization experiment
4. make target-cache cap enforceable across idle warm workspaces
5. reconcile client-side sccache receipt semantics
6. complete smaller policy/resource corrections listed by v2.8
```

Do not say all measurements are "deferred" when a prior protocol has already closed a decision.

## 5.3 Correct cache receipt example

Production schema:

```json
{
  "cache": {
    "mode": "sccache",
    "status": "SERVICE_OK",
    "delta": null,
    "reason": "client-side-counters-unattributable",
    "attribution": {
      "mode": "client-side"
    }
  }
}
```

`delta` is nullable.

A non-null delta is valid only when attribution semantics actually support it.

## 5.4 Correct workspace-identity wording

v2.8 supports one pinned managed Rust toolchain per broker configuration.

Workspace identity remains:

```text
workspaceKey = hash(canonical Cargo workspace root)
```

Toolchain identity belongs in:

- receipt evidence;
- compiler/cache identity;
- bootstrap/runtime identity.

Do **not** multiply persistent workspace directories merely because the pinned toolchain changes. Cargo fingerprints already determine whether existing artifacts are reusable.

If true simultaneous multi-toolchain support is added later, revisit this decision explicitly.

## 5.5 Acceptance

Phase 0 passes when:

- no current agent directive tells an implementation agent to recreate predecessor migration;
- v2.8 open work is listed once;
- cache delta is documented as nullable;
- workspace-key semantics match implementation.

---

# 6. Phase 1 — remove normal Cargo-owned compiler broker jobs

This is the highest-priority code change.

## 6.1 Target topology

Current conceptual topology:

```text
broker BUILD root
  cargo
    nested broker compiler request
      sccache
        rustc
```

Target:

```text
broker BUILD root
  cargo
    sccache client
      rustc
    sccache client
      rustc
    rustdoc
```

The Cargo root is admitted once.

Cargo-owned compiler children inherit:

- the root process group / Windows Job Object;
- root cancellation;
- root BUILD capacity;
- managed Cargo/Rust tool paths;
- target generation;
- shared Cargo home;
- shared sccache;
- target triple;
- root environment policy.

They do not:

- acquire another scheduler permit;
- create broker child request IDs;
- run root-style storage admission;
- take per-child sccache snapshots;
- create user-facing child receipts;
- perform another workspace discovery;
- re-enter the normal broker request graph.

## 6.2 Managed compiler paths

The broker must inject the real digest-verified Rust tools into the admitted Cargo root:

```text
RUSTC=<verified real rustc path>
RUSTDOC=<verified real rustdoc path>
RUSTC_WRAPPER=<verified sccache path>     # when cache enabled
```

Do not point Cargo at an agent shim merely to observe normal compiler children.

The root process domain is the containment boundary.

The existing bundle-relative Rust tool handling must remain intact on macOS and Windows.

## 6.3 Raw compiler requests remain brokered

This must still work:

```text
agent -> rightkit rustc ...
agent -> rightkit rustdoc ...
```

Such a request is a top-level BUILD.

Only **Cargo-owned** compiler execution inherits root authority and bypasses child-job creation.

## 6.4 Build scripts and proc macros

Cargo may execute build scripts/proc macros that invoke the compiler environment Cargo provides.

Those descendants remain inside the admitted root process tree and generation.

RightKit is a coordinator/containment system, not a sandbox for arbitrary Rust build scripts.

Do not add a new nested capability system to replace the compiler-child broker graph.

## 6.5 Files likely touched

```text
packages/build-control-broker/src/thin-job-service.mjs
packages/build-control-broker/src/thin-command-policy.mjs
packages/build-control-broker/src/process-tree.mjs          # tests/verification only unless needed
packages/build-control-core/...                             # compiler env/shim normalization if applicable
agent shim implementation                                  # only if required to stop compiler re-brokerage
tests covering nested compiler delegation
```

Avoid unrelated scheduler/cache rewrites in this phase.

## 6.6 Required tests

Unit/integration:

1. normal `cargo check` creates one broker root receipt;
2. normal Cargo-owned rustc/rustdoc creates zero child broker jobs;
3. root receipt still records compiler child count if cheaply observable;
4. raw agent `rustc` remains a top-level BUILD;
5. root cancellation kills all compiler descendants;
6. root timeout kills all compiler descendants;
7. macOS process-group containment still passes;
8. Windows Job Object containment still passes;
9. broker crash rotates only the affected generation;
10. restarted build cannot write to the retired generation;
11. path dependencies within the admitted repository still compile;
12. universal/explicit target triples still compile;
13. sccache client-side mode still works.

## 6.7 Acceptance

Phase 1 passes only when live evidence shows:

```text
normal Cargo compiler broker jobs = 0
normal per-compiler user receipts = 0
raw agent rustc/rustdoc remains managed
cancelled root leaves zero attributed compiler descendants
macOS containment = pass
Windows containment = pass
```

---

# 7. Phase 2 — repair cache measurement truth

Do this before changing production cache policy.

## 7.1 Production receipts

Normal production receipts in client-side sccache mode must report:

```text
cache.mode
cache.status
cache.reason
cache.attribution
cache.delta = null when not attributable
```

Do not infer a per-root hit rate from cumulative client-side counters.

## 7.2 Controlled experiment harness

Create a bounded cache experiment harness outside normal user builds.

It may use an isolated window:

```text
ensure no competing BUILD
snapshot/zero sccache statistics for the experiment
run exactly one defined build
wait for full root reap
read sccache statistics
record results
```

If `--zero-stats` is used, it is an experiment-only operation.

Do not run it as part of normal production receipts because it destroys cumulative operational counters.

Recommended evidence artifact:

```text
docs/plans/evidence/rightkit-thin-build-control-v2.8-cache-experiment.md
```

Store:

```text
host
OS
Cargo/rustc version
sccache version
workspace commit
Cargo.lock digest
target triple
profile
feature set
rustflags
incremental mode
basedirs mode
target generation
wall time
rustc count
cacheable Rust requests
Rust hits
Rust misses
non-cacheable count/reasons
C/C++ hits/misses where relevant
peak RSS
swap
disk delta where cheap
```

## 7.3 Do not set a universal hit-rate target

Acceptance is comparative.

A cache policy is accepted if it:

- materially reduces real compilation or elapsed cold/rotated work;
- is stable across identical repeats;
- does not materially degrade the dominant developer workflow;
- leaves remaining misses explainable.

Do not invent "80%" or "90%" as a global architectural requirement.

---

# 8. Phase 3 — incremental compilation experiment

## 8.1 Question

For managed development on the actual RightKit hosts, is the better default:

```text
A. Cargo incremental enabled
or
B. Cargo incremental disabled so local crates become sccache-cacheable
```

This must be measured; do not decide from theory alone.

## 8.2 Experiment matrix

Use one representative heavy product workspace and one medium workspace.

Hold constant:

- source commit;
- lockfile;
- target triple;
- profile;
- features;
- rustflags;
- toolchain;
- sccache cache state for the intended comparison.

Run:

### I1 — hot edit cycle, Cargo default incremental

```text
persistent generation
incremental = Cargo/profile default
make one representative small source edit
build/check
record elapsed and rustc work
```

Repeat several times.

### I2 — hot edit cycle, incremental disabled

```text
same workflow
CARGO_INCREMENTAL=0
same class of small source edit
```

### I3 — cold/rotated generation, incremental enabled

```text
new empty target generation
warm global sccache
incremental = default
```

### I4 — cold/rotated generation, incremental disabled

```text
new empty target generation
same warm global sccache
CARGO_INCREMENTAL=0
```

At least one Tauri/product workspace should be represented because linker-bound leaf crates will remain non-cacheable.

## 8.3 Decision rule

If incremental enabled wins the normal edit/check loop materially, keep Cargo's default for ordinary development.

If disabling incremental materially improves the aggregate agent workload without unacceptable edit-loop regression, adopt it as the managed default.

If the workloads clearly need both modes, do **not** build automatic heuristics in v2.8. Add one explicit broker-owned configuration field:

```text
defaults.cargoIncremental = "cargo-default" | "disabled"
```

Rules:

- agents cannot override it;
- bootstrap emits the accepted host default;
- receipts record the selected mode;
- no per-request adaptive switching in v2.8.

## 8.4 Environment authority

If the field is adopted, RightKit owns:

```text
CARGO_INCREMENTAL
CARGO_BUILD_INCREMENTAL
```

Client-provided values must be stripped/rejected like other broker-owned cache/build controls.

## 8.5 Acceptance

The evidence artifact must conclude one of:

```text
KEEP CARGO DEFAULT
DISABLE INCREMENTAL
EXPLICIT CONFIG MODE REQUIRED
```

with measured results.

No permanent cache-policy code lands before that conclusion.

---

# 9. Phase 4 — `SCCACHE_BASEDIRS` path-normalization experiment

Run this immediately after or in parallel with Phase 3 measurement.

## 9.1 Important implementation refinement

Do not rely only on one static machine-wide list of source roots.

When enabled, construct trusted per-job base directories from broker-owned state.

Recommended set:

```text
repository root
Cargo workspace root                 # if different from repository root
active generation root               # .../generations/000012
```

The active generation root is important: normalizing only the parent `generations/` directory leaves the generation number in the relative path.

Never accept arbitrary client-provided `SCCACHE_BASEDIRS`.

The broker computes the value.

## 9.2 Experiment

With incremental mode explicitly recorded, run:

### B1 — same source, rotated generation, basedirs OFF

```text
generation 12 -> build
generation 13 empty -> build
```

### B2 — same source, rotated generation, basedirs ON

Same test with trusted per-job normalization.

### B3 — second checkout/worktree, basedirs ON

Build the same commit from a different absolute checkout path.

### B4 — path dependency inside repository

Confirm that a path dependency outside the Cargo workspace root but inside the repository remains safely normalized and cache behavior is correct.

Record the Phase 2 metrics.

## 9.3 Decision rule

If basedirs materially improves rotated/worktree reuse without incorrect cache behavior:

```text
enable basedirs permanently
```

If it does not:

- leave it disabled;
- retain the capability;
- explain remaining misses by incremental mode, crate type, linker invocation, features, rustflags, build-script inputs, compiler identity, or other measured causes.

## 9.4 Production config after acceptance

Prefer a semantic gate:

```text
sccache.basedirsEnabled = true
```

and compute actual paths per root job.

If the existing static `sccache.basedirs` field remains, treat it only as trusted operator additions, not the primary source of workspace/generation roots.

## 9.5 Acceptance

Required proof:

- identical rotated generation obtains the expected additional reuse, or evidence shows why it cannot;
- different absolute checkout path behaves correctly;
- no source outside trusted repository/workspace/generation roots is normalized by client authority;
- macOS and Windows path separators/case behavior are tested.

---

# 10. Phase 5 — make the target-cache budget real

This is the primary multi-app resource-management change.

## 10.1 Required semantics

After v2.8:

> `targetCacheCapBytes` is an eventual managed-target budget. When no active BUILD prevents cleanup, RightKit may retire and delete least-recently-used idle current generations to move managed target storage back toward the cap.

The cap may be temporarily exceeded:

- during an active build;
- when every candidate is active/locked;
- when one unavoidable active generation itself exceeds the cap.

`freeFloorBytes` remains the immediate safety circuit breaker.

Do not cancel healthy work merely because the soft cache budget was crossed unless the critical free-disk floor is reached.

## 10.2 Eviction order

GC order:

```text
1. suspect/retired generations
2. oldest retired generations
3. least-recently-used idle current generations
4. stop when <= cap or no safe candidate exists
```

Never delete:

- an active generation;
- a generation whose workspace write permit cannot be acquired;
- a generation after state changed since candidate selection.

## 10.3 Safe current-generation eviction

For an idle current generation:

1. choose candidate from LRU metadata;
2. acquire the same per-workspace **write exclusion** used by BUILD/WORKSPACE_WRITE;
3. re-read workspace state;
4. require `active == null`;
5. confirm candidate is still current;
6. create/advance to fresh generation `N+1`;
7. atomically persist state;
8. rename generation `N` to broker trash;
9. release workspace exclusion after the state transition/rename is safe;
10. delete trash asynchronously.

The next product build receives an empty target generation and may refill from sccache.

Do not delete a "current" directory in place without first making a different generation current.

## 10.4 LRU metadata

Add minimal cache metadata; do not restore the old storage ledger.

Workspace state should know at least:

```json
{
  "lastUsedAt": "...",
  "lastMeasuredBytes": 123,
  "lastMeasuredAt": "..."
}
```

Rules:

- update `lastUsedAt` when a BUILD/ARTIFACT_WRITE actually uses the generation;
- size is advisory, not transactionally reserved;
- missing metadata is allowed during migration;
- old state upgrades lazily/backward-compatibly.

A separate generation metadata file is acceptable if it keeps `state.json` simpler, but do not create a database.

## 10.5 Avoid full-tree scans on every GC

The current recursive `directorySize()` walk should not become a frequent synchronous maintenance loop.

Implement:

```text
startup:
  best-effort background inventory

after build:
  mark size estimate dirty

background / pressure-triggered:
  remeasure dirty generations

normal GC:
  use recent measured sizes where available
  rescan only missing/stale candidates as needed
```

It is acceptable for the size budget to be approximate between measurements.

Correctness comes from:

- workspace exclusion;
- generation rotation;
- free-disk safety floor.

Not from exact byte accounting.

## 10.6 GC triggers

Run lightweight GC/inventory:

```text
broker startup:        asynchronous, readiness-independent
after generation rotate
after BUILD terminal:  only if over/near budget or size metadata dirty
explicit rightkit gc
periodic low-frequency maintenance if needed
```

Do not perform a full target-tree walk every 5–10 second heartbeat.

## 10.7 Unenforceable-budget evidence

If RightKit cannot get under the configured cap because all candidates are active or a required active generation is itself too large, emit one concise event/receipt:

```text
TARGET_CACHE_CAP_UNENFORCEABLE
targetBytes
capBytes
activeBytes / protected candidate summary
freeBytes
```

Do not globally wedge the broker.

## 10.8 Required tests

1. many idle current generations can exceed cap before GC;
2. LRU idle current generation is rotated/evicted first;
3. active workspace is never evicted;
4. queued/building workspace cannot race with eviction;
5. state transition occurs before deletion;
6. broker crash between rotation and trash delete is harmless;
7. next build uses fresh generation;
8. retired trash cleanup is restart-safe;
9. unrelated workspaces remain usable while one is evicted;
10. cap-unenforceable condition logs but does not deadlock;
11. explicit `rightkit gc` uses the same safety path.

---

# 11. Phase 6 — `cargo install` and managed-tool storage

Current environment injection points `CARGO_INSTALL_ROOT` at the trusted managed tools root.

Generic agent `cargo install` must not be allowed to create unregistered executables in the same namespace as digest-managed tools.

## 11.1 v2.8 policy

Generic agent `cargo install` should fail closed.

Managed Cargo extension/tool provisioning must use a dedicated operator path:

```text
install staging
  -> compile/install
  -> hash output
  -> register immutable tool identity
  -> promote/copy into digest-addressed managed tools root
```

If a product workflow genuinely requires `cargo install`, add a separate untrusted/staging install root, never the trusted registry root.

## 11.2 Acceptance

- no agent `cargo install` writes directly into `global/tools`;
- every executable reachable through a trusted RightKit shim remains digest-registered;
- bootstrap-managed external Cargo tools still work.

---

# 12. Phase 7 — command-policy documentation

Do not widen the command surface merely because Cargo has more builtins.

Document the current denied set explicitly.

Examples currently worth recording:

```text
verify-project
pkgid
report
info
config
new
init
publish
package
login
logout
owner
yank
uninstall
```

Classify them as:

```text
INTENTIONALLY DENIED
or
FOLLOW-UP CANDIDATE
```

For v2.8, default to **denied** unless a real Right Suite tool requires one.

Publishing/credential/spend/mutation commands remain denied.

Read-only commands such as `verify-project` or `pkgid` may be added later through the same declarative command policy, with tests, if a concrete tool needs them.

Do not add a second hand-maintained command table.

---

# 13. Phase 8 — host resource defaults

## 13.1 Keep BUILD concurrency at one

`BUILD=1` is accepted today.

Do not raise it as part of v2.8 unless the existing controlled host capacity experiment is rerun and proves useful aggregate throughput without unacceptable:

- peak RSS;
- swap;
- disk pressure;
- thermal pressure;
- workstation responsiveness loss.

## 13.2 Fix Cargo-job derivation

Remove the generic:

```text
performanceCores = logicalCPUs / 2
```

assumption from Windows.

Use a host-neutral conservative default:

```text
logical = available parallelism
memoryCeiling = floor(totalMemoryGiB / 2)

cargoBuildJobs =
  max(1,
    min(
      4,
      logical,
      memoryCeiling
    )
  )
```

The hard ceiling of 4 remains the accepted default until host measurement authorizes more.

Platform-specific overrides may exist only when backed by host evidence.

## 13.3 Correct stale comments

Update `tools/rightkit/Cargo.toml` commentary so it no longer claims the broker admits two concurrent builds when the shipped/default configuration admits one.

Comments that agents may treat as architecture must match runtime truth.

---

# 14. Phase 9 — RightKit Cargo workspace hygiene

These are product/workspace improvements, not broker safety mechanisms.

Do them only after the broker-core phases above are stable.

## 14.1 Test resolver 3

Current virtual workspace uses:

```toml
resolver = "2"
```

Rust/Cargo 1.98 supports resolver 3, which is Rust-version aware.

Create a branch/test change:

```toml
[workspace]
resolver = "3"
```

Then run:

```text
cargo metadata --locked
cargo check --workspace --locked
cargo test --workspace --locked
cargo tree -d
```

Confirm no unacceptable dependency-resolution/MSRV change.

Adopt only if clean.

This is not required for the cache fix.

## 14.2 Default members / package selection

A virtual workspace with no `default-members` selects all members for root `build/check/test/bench`.

Audit actual developer/agent usage.

If root commands routinely compile crates unrelated to the current task, choose one of:

```text
A. define intentional default-members
or
B. require package-targeted development commands (-p)
```

Full-suite verification remains explicit:

```text
cargo check --workspace
cargo test --workspace
```

Do not choose default members solely to improve a benchmark; choose them to match the normal development intent.

## 14.3 Centralize common workspace policy

Evaluate:

```toml
[workspace.package]
[workspace.dependencies]
[workspace.lints]
```

Centralize only genuinely common values.

Good candidates:

- repository;
- license;
- edition/MSRV where truly uniform;
- common serde/serde_json versions;
- common lint policy.

Do **not** centralize a dependency when members intentionally require incompatible feature/default-feature policy.

The goal is dependency/feature consistency, not cosmetic deduplication.

## 14.4 Duplicate-dependency audit

Record:

```text
cargo tree -d
```

for the RightKit workspace and each large product workspace.

Investigate only meaningful duplicates:

- expensive compile-time crates;
- large native dependencies;
- duplicate major/minor versions that can realistically converge.

Do not force version convergence when compatibility requires duplicates.

## 14.5 Keep heavy implementation in libraries, binaries thin

sccache cannot cache linker-invoking crate types such as large `bin`, `cdylib`, `dylib`, and `proc-macro` leaf steps.

For large product applications, prefer:

```text
app-core / app-services / app-domain libraries
thin binary/Tauri entrypoint
```

where the product architecture naturally supports it.

Do not refactor small binaries purely for cache aesthetics.

---

# 15. Research/watch list — explicitly not v2.8 production changes

## 15.1 Cargo `build.build-dir`

Cargo 1.98 exposes a separate intermediate `build-dir` with path templates including:

```text
{workspace-root}
{cargo-cache-home}
{workspace-path-hash}
```

This is relevant to future multi-workspace cache design.

Do **not** point multiple RightKit generations at a shared mutable build directory in v2.8.

Reason:

RightKit's crash-safety invariant depends on generation rotation giving a future build a physically different mutable target namespace from an uncertain orphan.

A shared `build-dir` could reintroduce a mutable namespace shared by:

```text
old uncertain compiler
new post-restart build
```

Before adoption, a future design must prove:

- Cargo locking semantics;
- orphan-writer safety;
- compatibility with generation rotation;
- path/cache identity;
- tool compatibility with separated artifact/build directories.

Track; do not ship.

## 15.2 Cargo workspace feature-unification

Current Cargo documentation discusses workspace feature unification, but the feature remains behind Cargo's unstable `-Z feature-unification` gate in the current toolchain line.

Production RightKit remains stable-Cargo only.

Track stabilization.

## 15.3 cargo-hakari

Consider only for a large workspace that demonstrates feature-set churn causing dependency rebuild cascades.

It is workspace-local; it does not create shared compilation across independent Right Suite repositories.

Measure before introducing a workspace-hack crate.

## 15.4 Remote/multi-level sccache

sccache supports remote and multi-level backends.

Do not add them merely because Mac and Windows hosts both exist.

Different compilers/targets naturally partition much of the cache, and network/storage operational cost is not justified until a real cross-machine reuse case exists.

---

# 16. Final canonical cache model after v2.8

If both measurement gates pass, the expected model is:

```text
MACHINE
│
├── global CARGO_HOME
│
├── global sccache storage
│     ├── client-side compilation
│     └── trusted per-job path normalization
│
├── workspace A
│     └── active generation 14 / target
│
├── workspace B
│     └── active generation 8 / target
│
└── workspace C
      └── active generation 3 / target
```

Hot same-workspace behavior:

```text
persistent target generation
+ measured incremental policy
```

Cold/rotated/worktree behavior:

```text
sccache
+ measured incremental policy
+ SCCACHE_BASEDIRS if accepted
```

Disk pressure behavior:

```text
retired generations first
then idle current generations by LRU
active work protected
free-floor remains hard safety circuit breaker
```

This is still Cargo-native build control, not a second build system.

---

# 17. Implementation order

Execute in this order.

## P0 — authority and topology

```text
1. repair canonical documentation authority
2. remove normal Cargo-owned compiler broker jobs
3. rerun macOS + Windows containment acceptance
```

Do not begin broad cache tuning until P0 is green.

## P1 — cache truth

```text
4. correct production cache receipt schema
5. add isolated cache experiment harness/evidence format
6. run incremental experiment
7. run SCCACHE_BASEDIRS experiment
8. adopt only accepted cache settings
```

## P1 — multi-app storage

```text
9. add workspace LRU metadata
10. add safe idle-current-generation eviction
11. make target budget eventual/enforceable
12. remove repeated whole-tree GC scans from hot paths
```

## P2 — policy/resource cleanup

```text
13. close cargo install trusted-root gap
14. document denied Cargo builtins
15. fix Windows Cargo-job heuristic
16. fix stale Cargo.toml comments
17. reconcile workspace-key documentation
```

## P3 — workspace optimizations

```text
18. resolver 3 test
19. default-members/package-selection audit
20. workspace dependency/package/lint centralization
21. cargo tree -d audit
22. product thin-bin/heavy-lib audit where worthwhile
```

## Watch only

```text
Cargo build.build-dir sharing
unstable feature-unification
cargo-hakari without measured need
remote/multi-level sccache
BUILD > 1 without host measurement
```

---

# 18. Required acceptance campaign

Create:

```text
docs/plans/evidence/rightkit-thin-build-control-v2.8-acceptance.md
```

The campaign is complete only when every required row has evidence.

| Area | Required proof |
|---|---|
| Broker topology | Normal Cargo root produces no compiler child broker jobs |
| Raw compiler | Agent raw rustc/rustdoc remains top-level managed BUILD |
| macOS cancellation | Root cancel leaves zero attributed compiler descendants |
| Windows cancellation | Job Object cancel leaves zero descendants |
| Crash recovery | Only affected workspace generation rotates |
| New generation safety | Restarted build cannot write old suspect generation |
| CONTROL | Remains responsive during BUILD |
| Workspace warm reuse | Same workspace reuses current generation |
| Cache telemetry | Production receipts do not claim unattributable deltas |
| Incremental policy | Evidence-backed conclusion recorded |
| Basedirs policy | Evidence-backed conclusion recorded |
| Rotated target | Cache behavior measured and explained |
| Worktree/alternate path | Cache behavior measured and explained |
| Target cap | Idle current generations can be LRU-evicted safely |
| Active protection | Active generation is never evicted |
| GC crash safety | Rotation/trash interruption is harmless |
| GC performance | No synchronous repeated full-tree scan in request hot path |
| Cargo install | Generic install cannot pollute trusted tool root |
| Bootstrap macOS | Reproduces accepted cache/concurrency settings |
| Bootstrap Windows | Reproduces accepted cache/concurrency settings |
| Test suite | All existing + v2.8 tests green |

---

# 19. Definition of done

v2.8 is complete when:

1. the canonical document contains no stale migration directive;
2. normal Cargo-owned compiler invocations no longer appear as broker child jobs;
3. root process containment remains proven on macOS and Windows;
4. cache receipts tell the truth under client-side sccache;
5. incremental compilation policy has a measured decision;
6. `SCCACHE_BASEDIRS` has a measured decision;
7. the observed poor Rust cache reuse is either materially improved or its remaining non-cacheable work is explained;
8. `targetCacheCapBytes` can reclaim idle current workspace generations rather than only retired generations;
9. GC cannot race active workspace writers;
10. GC sizing is not a synchronous repeated hot-path tree scan;
11. generic `cargo install` cannot populate trusted tool storage;
12. host Cargo-job defaults no longer rely on the Windows `logical/2` pseudo-performance-core assumption;
13. source bootstrap reproduces the accepted v2.8 configuration;
14. one v2.8 native acceptance artifact records the final evidence;
15. the final settled state is merged into `RIGHTKIT-THIN-BUILD-CONTROL.md`;
16. this v2.8 implementation plan is marked `COMPLETE — HISTORICAL IMPLEMENTATION EVIDENCE`.

---

# 20. Agent implementation directive

> Implement RightKit Thin Build Control v2.8 from this document.
>
> Do not redesign the thin architecture. Preserve one shared Cargo home, one shared sccache store, one persistent target generation per Cargo workspace, one globally bounded BUILD queue, workspace read/write exclusion, process-group/Job Object containment, crash-time generation rotation, managed tool identity, and source-reproducible bootstrap.
>
> First repair documentation authority and implement the v2.7-settled compiler-topology decision: normal Cargo-owned rustc/rustdoc execution must remain inside the admitted Cargo root process domain without creating broker child jobs. Raw agent rustc/rustdoc remains a top-level managed BUILD.
>
> Then repair cache measurement truth and run the controlled incremental-compilation and SCCACHE_BASEDIRS experiments. Do not permanently disable incremental compilation or permanently enable path normalization before the evidence gates pass. Do not invent a global cache-hit target.
>
> Make target-cache storage manageable across many apps by safely retiring and LRU-evicting idle current generations under the existing workspace exclusion model. Do not restore the old reservation ledger, maintenance leases, exactly-once workflow machinery, global wedge behavior, or generic result cache.
>
> Keep BUILD concurrency at its accepted value unless a separate controlled host-capacity experiment proves a higher value is beneficial and safe.
>
> Treat Cargo build.build-dir sharing, unstable feature-unification, cargo-hakari, remote sccache, and other broader optimizations as watch/research items unless this document explicitly promotes them through measurement.
>
> Every semantic change must land with focused tests and must preserve macOS and Windows native containment acceptance. At completion, merge the settled final behavior into the canonical Thin Build Control document and mark v2.8 historical.

---

# 21. Source notes

Internal implementation/evidence reviewed for v2.8:

```text
tools/rightkit/docs/RIGHTKIT-THIN-BUILD-CONTROL.md
rightkit-thin-build-control-v2.7-measurement-remediation-protocol.md
packages/build-control-broker/src/thin-cache.mjs
packages/build-control-broker/src/thin-state.mjs
packages/build-control-broker/src/thin-job-service.mjs
packages/build-control-broker/src/thin-scheduler.mjs
packages/build-control-broker/src/thin-command-policy.mjs
packages/build-control-broker/src/config.mjs
packages/build-control-broker/src/bootstrap-macos-cli.mjs
packages/build-control-broker/src/bootstrap-windows-cli.mjs
tools/rightkit/Cargo.toml
v2.7 native acceptance evidence
live sccache measurement supplied during v2.8 review
```

External references checked:

- Cargo Book — Configuration: `build.target-dir`, `build.build-dir`, `build.incremental`, cache auto-cleaning  
  https://doc.rust-lang.org/cargo/reference/config.html
- Cargo Book — Workspaces / default members / workspace inheritance  
  https://doc.rust-lang.org/cargo/reference/workspaces.html
- Cargo Book — Optimizing Build Performance  
  https://doc.rust-lang.org/cargo/guide/build-performance.html
- Cargo Edition Guide — resolver 3 / Rust-version-aware resolver  
  https://doc.rust-lang.org/edition-guide/rust-2024/cargo-resolver.html
- sccache README — Rust caveats and `SCCACHE_BASEDIRS`  
  https://github.com/mozilla/sccache/blob/main/README.md
- sccache Configuration — client-side mode, basedirs, cache sizing/compression  
  https://github.com/mozilla/sccache/blob/main/docs/Configuration.md
- sccache Rust notes — incremental compilation and linker caveats  
  https://github.com/mozilla/sccache/blob/main/docs/Rust.md
