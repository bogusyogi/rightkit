# RightKit Thin Build Control — Measurement & Remediation Protocol

**Status:** measurement/remediation protocol; **not a successor architecture yet**.
**Version:** 2.7-evidence-gated.
**Current architecture authority:** the existing repository canonical thin-build-control document remains authoritative until the evidence gates in this protocol pass and an explicit adoption decision is made.

This file does **not** supersede the current canonical architecture. It exists to stop architecture churn, repair measurement quality, run controlled experiments, and determine which proposed amendments are actually justified.

Older amended/remediation documents remain historical/design evidence only. Do not synthesize a new architecture by mixing contradictory clauses across documents.

**Authority rule:** evidence promotes proposals. A proposed change does not become canonical because it appears in an amended design document.

## 1. Decision

Keep the RightKit broker. Do not bypass RightKit for development Cargo work.

The current canonical architecture remains frozen while this protocol runs.

```text
CURRENT CANONICAL
      |
      | remains implementation authority
      |
      +--> Measurement / Remediation Protocol
               |
               +--> repair instrumentation
               +--> fix known control-plane defect
               +--> cold / warm / rotated cache experiment
               +--> compiler-child phase timing
               +--> BUILD=1 vs BUILD=2 capacity test
                        |
                        v
                 evidence obtained
                        |
                        v
              explicit adoption decision
                        |
                        v
             canonical document amended
             only where evidence supports it
```

No implementation agent may treat an unpassed experiment as permission to redesign the architecture.

Cargo owns builds. `rustc` owns compilation. `sccache` owns compiler-output caching. RightKit owns workstation coordination, managed environment, process ownership, cache routing, and evidence.

```text
Agent / RightKit shim
        |
        v
+-------------------------+
| Thin RightKit broker    |
|                         |
| classify root command   |
| queue / workspace lock  |
| inject managed env      |
| own process group       |
| root telemetry/receipt  |
+------------+------------+
             |
             v
           Cargo
             |
          sccache
             |
            rustc
```

RightKit is not Bazel, Nix, CI, a distributed executor, a generic result cache, or an exactly-once workflow engine.

## 2. Settled guarantees

RightKit must guarantee:

1. Agents cannot launch unmanaged Cargo/rustc/rustdoc/rustup work.
2. One shared managed `CARGO_HOME` is reused machine-wide.
3. One shared managed sccache compiler cache is reused machine-wide.
4. Each Cargo workspace normally reuses one persistent warm target generation.
5. A Cargo workspace has at most one active artifact writer for its active generation.
6. BUILD work is bounded by one machine-wide admission authority.
7. CONTROL remains responsive while BUILD work runs.
8. Cancellation kills the complete process tree owned by the admitted root job.
9. An uncertain old writer can never corrupt a future active generation.
10. A clean failure or cleanly reaped cancellation does not throw away the warm target generation.
11. External Cargo tools are managed/digest-pinned and cannot silently escape to unmanaged Cargo.
12. Slow/queued/cancelled/failed/cache-degraded work is diagnosable from one root receipt.
13. Source bootstrap reproduces intended runtime/cache/concurrency settings.
14. Product repositories own profiles, features, dependencies, LTO, codegen units, target matrices, packaging, signing, and product acceptance.

Anything not required for these guarantees stays outside build-control core.

## 3. Non-goals

Do not implement or restore:

```text
SQLite workflow/lifecycle engine
exactly-once Cargo execution
reservation ledger
global namespace quarantine as the normal safety mechanism
maintenance leases
generic command-result cache
test-result cache
adaptive token/resource market
per-transition directory fsync
product-specific build/profile policy inside RightKit
architecture changes justified only by queue duration
```

## 4. Cancellation evidence — corrected interpretation

The recent CodeRight SIGTERM sample set is **not valid evidence of an unexplained RightKit auto-cancellation mechanism**.

Known concurrent activity included deliberate cancellations to release the shared BUILD lane for higher-priority HeardRight work. Therefore:

```text
SIGTERM count != broker cancellation bug
low machine load at cancellation != broker cancellation bug
```

Until provenance is reconciled, historical cancellations are **contaminated evidence**.

### 4.1 Required cancellation provenance

Every terminal root receipt must record one exact cancellation source:

```text
USER_CANCEL
OPERATOR_CANCEL
CLIENT_SIGNAL
PARENT_CANCEL
BROKER_SHUTDOWN
DEADLINE
STORAGE_FLOOR
CONTAINMENT_FAILURE
RELEASE_PRIORITY_CANCEL
UNKNOWN
```

If multiple layers request cancellation, record the first source and the complete request list.

Do not infer automatic cancellation defects from old SIGTERM samples unless a request is classified `UNKNOWN`.

### 4.2 Implementation rule

Do not build new eviction, preemption, or cancellation architecture to explain the contaminated sample set.

For normal development BUILD work, automatic cancellation should initially be limited to:

```text
explicit user/operator cancel
broker shutdown
critical disk floor
actual containment failure
an explicitly configured run deadline, if deliberately enabled
```

Queue wait by itself does not cancel a development build.

## 5. Canonical cache model

Caching remains a first-class requirement.

### 5.1 Shared Cargo home

Use one machine-global managed `CARGO_HOME`:

```text
<rightkit-root>/global/cargo-home/
```

All managed Cargo roots use it. Do not create one Cargo home per repository, generation, or request.

### 5.2 Shared sccache

Use one machine-global compiler cache:

```text
<rightkit-root>/global/compiler-cache/
```

Cross-workspace compiled-crate reuse belongs to sccache, not to a shared Cargo target directory.

Do not share one `CARGO_TARGET_DIR` across unrelated Cargo workspaces.

### 5.3 Persistent per-workspace target

Each Cargo workspace gets one persistent active generation:

```text
<rightkit-root>/
  workspaces/
    <workspace-key>/
      generations/
        000012/
          target/
          tmp/
```

Normal repeated `check`, `build`, `test`, and `clippy` reuse `000012`.

Cargo owns normal contents of `target/`. RightKit must not create duplicate parallel artifact hierarchies around Cargo without a demonstrated need.

## 6. Generation rotation

Generation rotation is a crash-safety mechanism, not ordinary cache invalidation.

### 6.1 Reuse the current generation after

Reuse the current generation when complete descendant reaping is proven after:

```text
successful command
non-zero Cargo exit
test failure
compile failure
clean explicit cancellation
client disconnect
normal retry
post-build release-phase failure
```

### 6.2 Rotate only after uncertain writer

Rotate only when an old artifact writer may still mutate the current generation after broker authority is lost or the complete writer tree cannot be proven reaped.

On uncertain writer recovery:

1. fence the old generation from future writers;
2. increment only that workspace generation;
3. admit unrelated work normally;
4. best-effort reap old processes;
5. retain retired generation until safe GC;
6. do not copy unvalidated target artifacts into the new generation;
7. rely on shared sccache to recover compatible compiler outputs.

An unresolved orphan must not globally wedge unrelated work.

## 7. Workspace identity and discovery

Target reuse is keyed to the Cargo workspace, not merely Git root.

Resolve workspace root through Cargo semantics, e.g. controlled `cargo locate-project --workspace`, then canonicalize it. Git repository identity may remain receipt metadata.

Workspace discovery is CONTROL and must be bounded, cached, separately timed, observable, and independent of BUILD capacity.

The previously observed ~10-second hidden discovery timeout is a control-plane defect and should be fixed independently of compiler/cache/scheduler experiments.

## 8. Command/resource policy

Use one declarative policy source.

### CONTROL

Examples: version probes, `rustc --print`, workspace discovery, locked/frozen metadata and tree.

```text
compile capacity: no
workspace access: none/read
initial concurrency: 4
```

### ANALYSIS

Examples: `cargo outdated`, `cargo audit`, `cargo deny`, non-fixing `cargo machete`.

```text
compile capacity: no unless measurement proves the tool actually compiles
workspace access: read
initial concurrency: 2
```

`cargo outdated` must not occupy the machine's only BUILD slot merely because it performs dependency resolution or uses temporary manifests.

### WORKSPACE_WRITE

Examples: update, upgrade, add, remove, set-version, fixing machete, mutating fmt.

```text
compile capacity: no unless execution actually compiles
workspace access: write
one writer per workspace
```

### BUILD

Examples: check, build, test, clippy, doc, nextest, compiling install, agent-initiated raw rustc/rustdoc.

```text
compile capacity: yes
workspace/artifact access: write
one writer for selected workspace generation
```

### TOOLCHAIN_WRITE

Registered rustup target/toolchain changes are globally exclusive.

Unknown commands fail closed.

## 9. Scheduler — freeze defaults until capacity is measured

Do **not** retire `BUILD=1` merely because a request waited a long time in queue.

Queue duration measures contention, not machine capacity.

Current safe implementation default:

```text
BUILD roots: 1 globally
CONTROL concurrency: 4
ANALYSIS concurrency: 2
one writer per workspace
TOOLCHAIN_WRITE: exclusive
CARGO_BUILD_JOBS: current measured-safe value
```

`BUILD=1` is a safe default, not a forever invariant.

### 9.1 Capacity experiment required before BUILD > 1

Compare controlled one-root and two-root runs. Record total elapsed, CPU, peak RSS, swap, disk I/O, thermal state, and workstation responsiveness.

**Capacity is an output of this study, not a pre-registered completion criterion.**

Valid outcomes include:

```text
BUILD=1 is the measured safe capacity
BUILD=2 is the measured safe capacity
further measurement is required
```

Increase to 2 only if useful aggregate throughput improves without unacceptable memory pressure, swap, thermal behavior, or workstation degradation.

If the study says the real workstation capacity is 1, that is a successful measurement result, not a failure of the architecture.

No weighted scheduler is required until simple integer capacity is proven insufficient.

### 9.2 Priority

Priority changes queue order only. It does not preempt active compilation unless a user/operator explicitly cancels it.

Tool provisioning that compiles Rust may receive higher priority but consumes the same compile capacity as product builds.

Signed release work may use exclusive admission where release correctness requires it.

## 10. Confirmed delegated-compiler implementation defect

Current source establishes a narrower but important distinction:

```text
already true:
  nested rustc/rustdoc does not acquire a second BUILD scheduler permit
  nested compiler output paths are not rewritten by the broker

still true:
  nested rustc/rustdoc is represented as a broker child job
  nested compiler jobs are compile-capable
  compile-capable child jobs run storage admission
  compile-capable child jobs call cache.prepare()
  compile-capable child jobs call cache.finish()
  ThinCache.prepare() snapshots global sccache stats
  ThinCache.finish() snapshots global sccache stats again
```

Therefore each delegated compiler child can trigger redundant root-style services even though it does not acquire another BUILD permit.

This is a **confirmed implementation/instrumentation defect**. It no longer belongs in the speculative bucket.

### 10.1 Immediate delegated-compiler fast-path remediation

For Cargo-owned delegated `rustc` / `rustdoc` children:

```text
KEEP:
  authenticate parent/root capability
  validate nested compiler cwd
  validate inherited RUSTC/RUSTDOC identity
  validate managed environment boundaries
  inherit parent's target generation
  inherit parent's managed Cargo/cache/toolchain environment
  inherit parent's cancellation/process domain
  retain parent/root traceability in debug evidence

DO NOT:
  acquire another scheduler permit                    # already true
  run storage admission again
  call ThinCache.prepare()
  call ThinCache.finish()
  take per-child sccache before/after snapshots
  create per-child cache hit/miss deltas
  expose per-child request IDs in normal user telemetry
  treat compiler children as independent root BUILD receipts
```

Root-level services happen exactly once around the Cargo root:

```text
Root Cargo job
  |
  +-- one storage admission
  +-- one authoritative cache-before snapshot
  +-- one root request/receipt
  |
  +-- delegated rustc
  |     inherit parent authority/env
  |     no storage preflight
  |     no cache snapshot
  |
  +-- delegated rustc
  |     same
  |
  +-- delegated rustdoc
  |     same
  |
  +-- complete root process reap
  +-- one authoritative cache-after snapshot
  +-- one root cache delta
```

This correction improves both performance integrity and measurement integrity.

It must be implemented **before** using compiler-child timing or cache deltas as evidence.

### 10.2 What remains measurement-gated

The remaining broker hop itself is still an open architecture question:

```text
Option A:
  Cargo -> managed compiler shim -> broker delegated-compiler validation -> rustc/rustdoc

Option B:
  Cargo -> sccache -> pinned rustc/rustdoc directly inside the admitted root process domain
```

Do not decide between A and B solely from aesthetics or assumptions.

After removing redundant child root-services, measure the residual broker-hop cost and evaluate whether that hop provides any containment, identity, or audit property that cannot be enforced at root admission.

### 10.3 Canonical wording correction

The canonical statement:

> nested rustc/rustdoc delegation without a second BUILD permit

is accurate but incomplete.

It must not be interpreted as:

> nested rustc/rustdoc executes without a separate broker child job or root-style child services.

Until the fast-path remediation above lands, the deployed system still has child-job overhead and fragmented child telemetry.


### 10.4 Resolved apparent contradiction

The following two observations are **not contradictory**:

```text
A. nested rustc/rustdoc acquires no second BUILD scheduler permit
B. nested rustc/rustdoc still re-enters the broker as a child job
```

Current source proves both.

The scheduler permit is skipped for child jobs because scheduler acquisition occurs only when there is no `parentId`.

Separately, nested compiler requests are still normalized, authenticated, represented as jobs, executed through the broker path, and—until §10.1 is fixed—run compile-job root services such as storage admission and cache prepare/finish.

Therefore do not spend another measurement cycle merely deciding whether compiler children “re-enter the broker.” That topology fact is already source-established.

The remaining open question is narrower:

```text
after redundant child root-services are removed,
does the residual compiler broker hop provide enough safety/audit value
to justify its complexity and measured cost?
```

That is the only compiler-topology question the measurement campaign still needs to answer.

---

## 11. Compiler-child broker hop — evidence gate now passed

The remaining compiler-child broker hop is no longer merely a topology hypothesis.

A read-only analysis of the live thin receipt corpus reported:

```text
thin receipts examined:         40,039
compiler requests:              39,186
cargo roots:                       666

rustc requests:                 31,817
rustc version/print probes:      7,366
rustdoc requests:                    3

compiler probe runMs:
  median:                         316 ms
  p90:                            710 ms
  cumulative:                    56.5 min

compiler-child requests with
redundant before/after
sccache sampling:               19,256

child queueMs:
  median:                           1 ms
  cumulative:                     1.5 min
```

This evidence materially changes the decision.

The `rustc -vV` / `--print` probes are especially useful because current command policy classifies them as CONTROL/non-compile work. They therefore do **not** run compile-cache preparation or storage admission. Their latency isolates the managed child request/spawn/reap/broker chain much more cleanly than compile-child timings.

The reported native probe baseline is approximately 20–50 ms, versus a 316 ms managed median. Persist the native baseline command/results with the evidence artifact, but the live corpus already demonstrates that the child broker path is not merely log noise.

Scheduler admission is not the primary child-path cost: child queueing is approximately 1 ms median. The cost is the repeated request/socket/job/spawn/reap path, with additional cache/stat work for compile-capable children.

Incident evidence also reports correctness/operability harms from compiler processes being broker child jobs:

```text
orphaned compiler work surviving a failed parent
release/watchdog visibility tied to broker-owned compiler jobs
additional socket/error surface per compiler invocation
fragmented root evidence across thousands of child request IDs
```

### 11.1 Decision

**Remove normal Cargo-owned compiler children from the broker job graph.**

This is now an evidence-backed implementation decision, not a performance hypothesis.

Do not spend an intermediate implementation cycle merely optimizing the child-job fast path if direct root-owned topology can be implemented safely. The §10.1 fast-path rules remain the minimum rollback/bridge behavior, but the target is zero normal compiler-child broker jobs.

Agent-initiated raw `rustc` / `rustdoc` remains brokered as a top-level BUILD request.

### 11.2 Target topology

One admitted Cargo root owns normal compiler execution:

```text
Agent
  |
RightKit broker
  |
  +-- classify/admit Cargo root once
  +-- storage preflight once
  +-- inject exact managed compiler/cache environment once
  +-- root process group / Job Object
  +-- root cache-before snapshot
  |
Cargo
  |
sccache
  |
pinned rustc / rustdoc
  |
  +-- compiler remains inside root process domain
  +-- no compiler child broker request
  +-- no compiler UUID/job record
  +-- no child scheduler admission
  +-- no child storage preflight
  +-- no child cache snapshot
  +-- no child root-style receipt
  |
root reap
  |
root cache-after snapshot
  |
one root receipt
```

Root admission must continue to own and validate:

```text
RUSTC / RUSTDOC exact managed identity
RUSTC_WRAPPER / sccache identity
CARGO_HOME
CARGO_TARGET_DIR
target triple
toolchain/sysroot authority
loader environment
workspace/repository containment
process-group / Job Object containment
cancellation
```

Client-supplied compiler paths or wrappers remain stripped/rejected.

### 11.3 Acceptance confirmation

The topology decision is made; acceptance still must prove the implementation preserves safety.

After cutover, prove:

1. one Cargo root creates one broker BUILD request;
2. normal Cargo-owned `rustc`/`rustdoc` creates zero broker child jobs;
3. agent-initiated raw compiler requests remain managed top-level requests;
4. compiler processes remain inside the Cargo root process group / Job Object;
5. exact pinned compiler identity is used;
6. cancellation removes all attributed compiler children;
7. root receipt contains compiler/cache evidence without child request aggregation;
8. no regression in nested path-dependency/compiler cwd containment;
9. no regression in inherited Rust compiler environment validation.

Phase timing remains useful as before/after evidence, but it no longer decides whether the topology should be removed.

### 11.4 Evidence preservation requirement

Before destructive cleanup, persist the live receipt analysis used for this decision into a repository evidence artifact containing:

```text
receipt corpus path
snapshot timestamp
runtime/source identity
receipt count
aggregation script/query
probe selection rule
median/p90/sum calculations
compiler-child cache-sampling count
queue timing calculation
native probe baseline command/results
```

This prevents another ephemeral-number dispute later.

## 12. sccache path normalization — confirmed configuration deficiency, performance effect still A/B gated

Near-zero observed cache reuse is a real problem. Its exact cause is **not yet proven**.

New live evidence upgrades one narrow point from hypothesis to confirmed configuration drift:

```text
live Rust cache hits:   14
live Rust cache misses: 5378
live Rust hit rate:     ~0.26%
base directories:       none
shared cache location:  managed global compiler-cache
```

Source inspection also found no broker-path `SCCACHE_*` injection in the Rust broker path, while `SCCACHE_BASEDIRS` appears only in the release path.

Therefore the current broker path is demonstrably missing a path-normalization mechanism for ordinary managed Cargo builds.

**What this proves:**

```text
broker-side path normalization is absent
generation-scoped/workspace absolute paths can participate in cache identity
the deployed configuration is deficient relative to a design that intends stable reuse across path changes
```

**What this does NOT yet prove:**

```text
that path drift explains most observed misses
that every generation rotation guarantees a cold compile
that SCCACHE_BASEDIRS will materially improve every workload
that a particular hit-rate threshold is achievable
```

The quantitative performance contribution remains A/B gated.

Additional live/historical evidence strengthens the reason to test normalization immediately:

```text
older stable-path era, Rust: approximately 225 hits / 450 misses (~33% hits)
current thin generation-path era, Rust: 14 hits / 5,378 misses (~0.26% hits)
current C/C++: 1,085 hits / 1,184 misses (~48% hits)
```

This is a strong signal but remains confounded by tool/version changes, cache eviction, source churn, and workload changes. It must not be presented as causal proof.

The correct status is:

> Missing path normalization is a confirmed RightKit configuration deficiency and a strong candidate explanation for generation-dependent misses. Its contribution to the observed poor reuse must still be measured with a controlled C-vs-D experiment.

Do not promote the deficiency itself into a claim about performance magnitude without the experiment.

Because persistent-generation rotation is an explicit RightKit safety feature and path normalization is required to make path-varying generations eligible for stable cache identity, implementation may land a **safe broker-owned normalization capability behind a controlled configuration gate** now.

Permanent production activation and any performance claim still require the controlled C-vs-D A/B result and semantic-safety review.

### 12.1 Cache attribution — exact reconciliation from live receipts

The cache-number dispute is now materially reconciled.

A read-only live inspection reported the active thin receipt store at:

```text
/Volumes/D/.rightkit-managed/rightkit-build-control/receipts/
```

with 40,039 receipts at the inspection snapshot.

The core defect is that thin job deltas are derived from a **shared cumulative sccache server counter**, while compiler children also sample that same counter. Root and child windows overlap.

Reported reconciliation:

| Figure | Actual basis |
|---|---|
| amended doc `ViewRight 14 / 5,283` | global cumulative sccache server snapshot, mislabeled as ViewRight |
| amended doc `CodeRight ~4 / 1,240` | CodeRight cargo-root receipt deltas |
| prior `ViewRight 0 / 1,105` | ViewRight cargo-root receipt sums |
| prior `global 13 / 4,983` | sums across cargo-root receipt deltas |
| later live global `14 / 5,378` | cumulative running sccache server counter |
| later ViewRight root sum `0 / 1,357` | ViewRight cargo-root receipt deltas at later snapshot |
| later all-root sum `13 / 5,254` | cargo-root deltas at later snapshot |
| roots + children `21 / 14,231` | invalid double-counted aggregation against server-true 14 hits |

This demonstrates the attribution defect:

```text
ROOT before -------------------------------- ROOT after
        CHILD A before ------- CHILD A after
           CHILD B before ---------- CHILD B after
              CHILD C before --- CHILD C after
```

Therefore:

```text
child cache deltas are invalid for per-child attribution
root+child aggregation is invalid and double-counts
global cumulative snapshots must never be labeled as app/root deltas
historical figures with different bases must not be compared as one metric
```

The active thin receipt store becomes the authoritative per-root evidence store after the attribution fix. Legacy old-core receipts remain historical only.

Before quoting another root cache rate, persist:

```text
receipt store identity
runtime/source identity
sccache server identity/restart epoch
measurement window start/end
root request ID
whether any other non-RightKit cache consumer overlapped
counter basis: global cumulative vs root delta
```

For production attribution while `BUILD=1`, prefer:

```text
before = sccache --show-stats
run one admitted Cargo root
wait for complete root process reap
after = sccache --show-stats

root_delta = after - before
```

After compiler-child broker removal, there must be no child cache snapshots.

`sccache --zero-stats` may be used only for a fully isolated experiment. It must not be the normal production attribution mechanism because it mutates the shared server counter state.

### 12.1.1 Cache-health semantics

`status: OK` means only that the sccache service responded. It must not mean “healthy reuse.”

Root receipts should distinguish at least:

```text
SERVICE_OK
COLD_EXPECTED
HEALTHY_REUSE
INEFFECTIVE
DEGRADED
DISABLED
```

Do not assign `HEALTHY_REUSE` until app/profile baselines exist.

### 12.2 Controlled cache experiment

Use one representative workspace with same source, lockfile, features, toolchain, target triple, profile, and rustflags; no competing build; no intentional cancellation; no runtime restart.

Run:

A. cold target, current global sccache
B. exact repeat, same target generation
C. new empty target generation, same global sccache
D. same as C, with safe path normalization

For each record:

```text
elapsed
rustc count
cacheable Rust requests
Rust hits
Rust misses
non-cacheable requests
non-cacheable reasons
cache errors
```

### 12.3 Decision rule

If D materially improves C, adopt the verified normalization scheme.

If D does not materially improve C, the missing normalization remains a real configuration deficiency but is not the primary explanation for the measured performance problem. Inspect incremental compilation, crate type, features, rustflags, environment, compiler arguments, target identity, build-script inputs, and toolchain/sysroot identity.

### 12.4 No invented global hit-rate target

Remove the provisional universal 80% requirement and do not replace it with a new arbitrary 90% requirement.

Initial acceptance:

```text
same-target repeat:
  no material unnecessary recompilation outside explained behavior

rotated-target + warm sccache:
  materially fewer real compilations than genuinely cold cache,
  stable across identical repeats,
  remaining misses/non-cacheable requests explained
```

After baseline measurements exist, define per-app/per-profile thresholds from evidence.

## 13. Process containment

Keep proven platform containment.

### macOS / POSIX

One root job owns one process group/session containing Cargo, build scripts, linkers, sccache clients, and compiler children.

Cancellation: `SIGTERM` group -> bounded grace -> `SIGKILL` remaining group -> record reap evidence.

### Windows

Use one Job Object per root with `KILL_ON_JOB_CLOSE`. Windows is not accepted until exercised on a real Windows host.

### Broker crash

Crash recovery fences only workspaces with uncertain active writers. It must not globally wedge the machine, rotate unrelated generations, or hold a scheduler permit forever.

## 14. Broker singleton

Exactly one broker owns one machine's RightKit state.

Acquire OS-enforced singleton ownership before state mutation, recovery, generation rotation, maintenance, or orphan cleanup.

A second broker fails before touching state. Socket existence is not singleton proof.

## 15. Minimal state model

Persist only:

```text
broker identity / singleton
workspace identity
active generation
active writer/process ownership marker
retired generation metadata
root receipts
append-only logs
managed tool identities
```

Job states: QUEUED, RUNNING, SUCCEEDED, FAILED, CANCELLED, TIMED_OUT.

Queued work need not survive restart. Running work is not replayed automatically.

## 16. Managed environment

RightKit owns:

```text
CARGO_HOME
CARGO_TARGET_DIR
CARGO_BUILD_TARGET_DIR where used
CARGO_INSTALL_ROOT
RUSTUP_HOME
RUSTC
RUSTDOC
RUSTC_WRAPPER
SCCACHE_DIR
SCCACHE_BASEDIRS only after A/B acceptance
SCCACHE_CLIENT_SIDE when pinned-version behavior is verified
TMPDIR
TMP
TEMP
CARGO_BUILD_JOBS
```

Agents may not redirect these through target-dir overrides, unmanaged compiler paths, custom wrappers, alternate cache homes, unmanaged rustup-run Cargo, or Cargo aliases/config that escape managed authority.

Product repositories continue using managed RightKit Cargo. Do not return development workflows to unmanaged direct Cargo.

## 17. Managed external Cargo tools

Plugins live in stable machine-global tool roots:

```text
<rightkit-root>/global/tools/<tool>/<digest>/...
```

not workspace generations.

Keep digest pinning where useful. Recursive plugin Cargo calls re-enter managed Cargo policy.

## 18. Cargo configuration

Do not enforce Cargo authority with ad-hoc line regexes.

First implementation may reject ancestor/workspace `.cargo/config` and `.cargo/config.toml` with clear diagnostics.

A later structured allowlist must parse TOML and include accepted semantic configuration in relevant build/cache identity.

## 19. Disk and GC

Do not restore the reservation ledger.

With initial `BUILD=1`:

```text
free disk >= safety floor?
  yes -> admit
  no  -> reject
```

Sample free disk during active BUILD. At critical floor, cancel with typed storage reason.

GC rules:

1. never delete active generation;
2. never delete generation with possible live writer;
3. prefer retired/suspect generations;
4. rename to trash before recursive delete;
5. delete oldest inactive generations above cap;
6. cleanup failure never wedges unrelated BUILD work.

Only reconsider reservations if measured concurrent BUILD capacity >1 proves simple floors insufficient.

## 20. Root telemetry and receipts

This is the immediate implementation priority.

One root receipt must expose:

```text
root request ID
command/workspace/repository
target/profile/generation
workspaceDiscoveryMs
queueMs
rootSetupMs
cargoRunMs
compilerBrokerMs
teardownMs
totalMs
compiler process count
compiler broker round-trip count
cache counter scope
cacheable/hits/misses/non-cacheable/reasons/errors
pid/pgid or Job Object identity
reap result
cancellation provenance
exit code/signal
```

Every 5–10 seconds, bounded running samples may record elapsed, CPU/RSS when cheap, output bytes, last-output time, and free disk.

Do not fsync file + directory on every heartbeat.

Normal user output shows root request ID, queue/start/terminal state, concise phase timing, concise cache summary, and exact failure/cancel reason. Per-compiler request IDs are debug-only.

## 21. Development and release both stay brokered

Do not split into dev bypass / release broker.

Development uses the same thin broker with managed cache paths, writer lock, global BUILD admission, bounded Cargo jobs, process containment, no queue eviction, no clean-failure generation rotation, and no generic result cache.

Release may additionally require exclusive signed-build admission, release correlation, native-host enforcement, and stronger artifact/signing/notarization evidence.

## 22. Implementation order — canonical frozen until evidence exists

Do not perform another broad architecture redesign until the measurement phases below complete.

### 22.1 Allowed implementation work now

The following work is authorized immediately because it improves evidence or fixes an independently demonstrated control-plane defect:

```text
reconcile root/global/nested sccache counter attribution
remove normal Cargo-owned compiler children from the broker job graph
remove delegated-compiler per-child cache snapshots
remove delegated-compiler repeated storage admission
suppress per-child root-style cache telemetry/request IDs from normal output
preserve raw agent-initiated rustc/rustdoc as managed top-level BUILD requests
land safe path-normalization capability behind an experimental/config gate
add root before/after cache-stat deltas
add cancellation provenance
add workspace-discovery timing
add root setup / Cargo run / teardown phase timing
add compiler-child count
add compiler broker round-trip count and cumulative time
add concise root receipts
bound and cache workspace discovery
separate CONTROL timing from BUILD timing
```

This work must preserve the current canonical scheduling/cache topology unless instrumentation itself requires a minimal non-semantic hook.

### 22.2 Explicitly prohibited until evidence gate passes

Do **not** yet:

```text
make path normalization permanently active before controlled C-vs-D acceptance
change path-normalization policy globally
raise BUILD concurrency above 1
implement a weighted BUILD scheduler
introduce adaptive capacity
set a universal cache-hit threshold
change generation-rotation rules beyond the already-settled uncertain-writer correction
redesign release scheduling
redesign GC
add new lifecycle/state machinery
```

A reviewer proposal is not an implementation instruction.

### Phase 1 — preserve evidence, repair attribution, remove compiler-child brokerage

Before modifying the compiler path:

1. persist the 40,039-receipt analysis and extraction method as a repository evidence artifact;
2. record current runtime/source identity;
3. define the active thin receipt store as the authoritative root evidence store;
4. define sccache server/restart epoch and root attribution semantics.

Then implement:

5. remove normal Cargo-owned `rustc`/`rustdoc` from the broker child-job graph;
6. inject/own exact managed compiler identity at the Cargo root;
7. retain raw agent-initiated compiler brokerage;
8. eliminate per-child storage admission and cache snapshots/deltas;
9. emit one root cache-before/cache-after delta;
10. add cancellation provenance;
11. retain root phase timing and compiler process counts;
12. emit one concise root receipt.

Do not implement an intermediate permanent child-job optimization unless needed as rollback protection. The target is the §11.2 topology.

After this phase, run §11.3 acceptance confirmation before proceeding to performance conclusions.

### Phase 2 — fix known workspace-discovery defect

Fix bounded/cached CONTROL workspace discovery.

Prove:

```text
no hidden ~10s discovery stall
no BUILD permit required
discovery time separately reported
CONTROL remains responsive during BUILD
```

Also inspect the longest-wait non-BUILD root requests, especially `cargo metadata`, and verify their exact argv and lane classification. Read-only locked/frozen metadata must not consume BUILD capacity; potentially mutating metadata must retain workspace-write semantics.

Do not infer a classification defect from command name alone.

This phase does not require an architecture decision.

### Phase 3 — quantify the confirmed path-normalization deficiency with controlled cache A/B/C/D

The experiment no longer decides whether broker-side normalization is missing; that is already established. A safe normalization capability may already be present behind a controlled gate. This experiment decides how much reuse it restores, whether `SCCACHE_BASEDIRS` or an equivalent mechanism is semantically safe, and whether it should become the production default.

Use the smallest already-accepted representative app where practical, with:

```text
same source
same lockfile
same features
same toolchain
same target triple
same profile
same rustflags
no competing BUILD
no intentional cancellation
no runtime restart
```

Run:

```text
A = cold target, current global sccache
B = exact repeat, same active target
C = new empty target generation, same global sccache
D = same as C with candidate path normalization
```

Record:

```text
elapsed
rustc count
cacheable Rust requests
Rust hits
Rust misses
non-cacheable requests
non-cacheable reasons
cache errors
```

Decision:

```text
D materially better than C -> adopt verified path normalization
D approximately C          -> reject path drift as primary explanation and inspect other key inputs
```

No universal hit-rate threshold is selected before app/profile baselines exist.

### Phase 4 — compiler-topology acceptance and before/after evidence

The topology decision is already evidence-backed.

After direct root-owned compiler execution lands:

1. run §11.3 acceptance confirmation;
2. compare representative root wall time and compiler/probe overhead against the preserved pre-change receipt evidence;
3. verify compiler child broker-job count is zero for normal Cargo-owned compilers;
4. verify process containment/cancellation remains correct;
5. record the before/after result in the final evidence table.

Do not use unmanaged direct Cargo as the benchmark.

This phase confirms the fix and quantifies the win; it does not reopen the topology decision unless a safety regression is discovered.

### Phase 5 — measure machine BUILD capacity

Only after measurements above are trustworthy.

Queue wait plotted against machine load may be useful background telemetry, but it does not determine safe compile concurrency.

Compare under realistic workstation load:

```text
BUILD=1
BUILD=2
```

Record:

```text
aggregate throughput
per-build elapsed
CPU
peak RSS
swap
disk I/O
thermal state
desktop responsiveness
```

Decision:

```text
2 roots materially improve useful throughput without unacceptable host degradation -> consider BUILD=2
1 root is the measured safe/efficient capacity                                  -> retain BUILD=1
result is ambiguous                                                             -> DEFER
```

Any of those is a valid experimental outcome.

Queue duration alone is not capacity evidence.

No weighted scheduler unless simple integer capacity later proves insufficient.

### Phase 6 — generation lifecycle verification

Prove:

1. successful repeat reuses generation;
2. normal failure reuses generation after complete reap;
3. explicit clean cancel reuses generation after complete reap;
4. uncertain writer rotates only affected workspace;
5. unrelated workspace remains admissible;
6. old writer cannot reach new generation.

### Phase 7 — explicit architecture adoption

Only after phases 1–6:

1. summarize measured results;
2. list each proposed amendment;
3. mark each `ADOPT`, `REJECT`, or `DEFER`;
4. update the canonical architecture only for `ADOPT` items;
5. preserve rejected hypotheses in evidence/history, not active implementation docs.

There is no automatic promotion from this protocol to canonical architecture.

### Phase 8 — native acceptance

Run macOS acceptance.

Run Windows Job Object acceptance on a real Windows host.

Only after accepted cutover should obsolete predecessor modules be deleted.

## 23. Controlled acceptance suite

### Queue/control

Prove same-workspace BUILD serialization, CONTROL during BUILD, ANALYSIS outside BUILD capacity, queue timing in receipts, and no silent priority preemption.

### Cancellation

For every controlled cancel: source is non-UNKNOWN, whole root process domain is reaped, first source is recorded, and clean cancel does not rotate generation.

Historical contaminated SIGTERM samples are not acceptance evidence.

### Cache

Run A/B/C/D from §12. Do not claim cache health merely because sccache responds.

### Compiler topology

Measure before change. If simplified, prove one root Cargo BUILD request, inherited compiler process ownership, no child scheduler permits, zero normal child request IDs, and complete cancellation.

### Capacity

Do not increase BUILD roots without controlled host evidence.

### Restart

Kill broker during BUILD. Prove only affected workspace rotates, unrelated workspace remains admissible, old writer cannot reach new generation, and a second broker cannot mutate state concurrently.

## 24. Obsolete machinery to delete after accepted thin cutover

After live macOS acceptance and rollback proof, remove obsolete predecessor implementation rather than maintaining two systems:

```text
old state-store.mjs and tests
old maintenance-worker.mjs and tests
old job-service.mjs and tests
old scheduler.mjs and tests
old fsync-heavy event log if fully replaced
old server-side sccache ownership machinery if no longer needed
old v1 migration/drain paths when no host requires them
```

Remove obsolete config/schema concepts:

```text
SQLite lifecycle DB
lifecycle transition workflow
exactly-once Cargo execution
reservationBytes / reservation ledger
usedBytes reservation accounting
global namespace quarantine
processMayBeLive as global admission gate
maintenance leases
trash-intent transactions
holdPermit safety behavior
global wedge on unresolved reap
adaptive token scheduler
generic result cache
per-transition directory fsync
```

## 25. Things that must not be deleted

Keep:

```text
RightKit broker
agent shims/direct-Cargo enforcement
client submit/watch/status/cancel/attach
runtime identity handshake
one declarative command policy
shared CARGO_HOME
shared sccache
persistent per-workspace target generation
bounded CARGO_BUILD_JOBS
bounded global BUILD admission
per-workspace writer exclusion
toolchain-write exclusivity
process-group containment
Windows Job Object implementation
managed digest-pinned external tools
root logs/receipts
simple retired-generation GC
native-host release/signing admission
```

## 26. Documentation authority

The current repository canonical thin-build-control architecture remains the sole implementation authority during measurement.

This protocol has narrower authority:

```text
it may add instrumentation
it may fix independently proven control-plane defects
it may run controlled experiments
it may produce evidence-backed recommendations
```

It may **not** independently authorize disputed architecture changes.

The amended v2.0 document remains a proposal.

The earlier v2.1 measurement-gated document is superseded by this protocol because it incorrectly described itself as the implementation successor before the evidence gates had passed.

The v2.3 update added the confirmed broker-side path-normalization deficiency without promoting a performance conclusion.

This v2.4 update adds the confirmed observability defect: parallel old/thin receipt stores plus shared cumulative sccache counter semantics make historical per-job cache rates non-authoritative. It also clarifies that measured BUILD capacity is an output, not a pass/fail completion gate, and separates compiler-child performance evidence from architectural-simplification evidence.


This v2.5 update adds a source-confirmed delegated-compiler defect: nested compiler children already skip a second scheduler permit, but still execute root-style storage admission and `ThinCache.prepare()` / `ThinCache.finish()`, producing redundant global sccache snapshots and child cache deltas. Those redundant services are now authorized for immediate removal. The remaining compiler broker hop itself remains measurement-gated.


This v2.6 update resolves the false either/or between “no second BUILD permit” and “compiler children still re-enter the broker”: current source establishes both simultaneously. It also closes the document-review loop until the bounded measurement campaign produces the final decision table.


This v2.7 update incorporates a read-only live receipt-corpus analysis that closes the remaining compiler-topology gate: 39,186 of 40,039 thin receipts are compiler requests, non-compile compiler probes show ~316 ms median managed run time, child scheduler queueing is ~1 ms median, and 19,256 compile children performed redundant cache sampling. Normal Cargo-owned compiler child brokerage is therefore promoted from a proposal to an evidence-backed removal decision, subject to post-change safety acceptance.

v2.7 also records the exact basis reconciliation for the previously conflicting cache figures and treats path normalization as an implementation capability whose production activation remains A/B-gated.

After the experiments:

1. create a short evidence summary;
2. record `ADOPT / REJECT / DEFER` for every disputed change;
3. amend the repository canonical architecture explicitly;
4. mark proposal documents historical;
5. never ask an implementation agent to synthesize contradictory architecture documents.

**Evidence promotes proposals, not the other way around.**

### Document-loop stop rule

No further architecture-comparison document, reviewer synthesis, or model vote is required before the measurement campaign completes.

New review input may update this protocol only when it contributes one of:

```text
new source-confirmed implementation fact
new persisted runtime evidence
a correction to measurement validity
a safety defect requiring immediate containment
```

Pure architectural preference, another threshold proposal, or another restatement of the same gates does not change implementation authority.

The v2.7 receipt-corpus analysis qualifies under this rule because it supplies new live runtime evidence and closes a previously open gate.

The next authority-changing artifact is the measured `ADOPT / REJECT / DEFER` table, not another architecture essay.

## 27. Agent implementation directive

> Treat the current repository canonical RightKit thin-build-control document as the architecture authority, with this v2.7 protocol authorizing evidence-backed remediation and measurements.
>
> Before code changes, persist the live receipt-corpus analysis that produced the 40,039-receipt / 39,186-compiler-request findings, including aggregation script/query, timestamp, runtime/source identity, and probe baseline. Do not allow these numbers to remain ephemeral reviewer prose.
>
> Remove normal Cargo-owned `rustc`/`rustdoc` from the broker child-job graph. The evidence gate for this decision has passed: compiler probes that do not run compile cache/preflight still show material managed child-path overhead, while scheduler queueing is negligible. One admitted Cargo root must directly own normal sccache/rustc/rustdoc children inside its process group / Job Object.
>
> Preserve exact broker-owned compiler identity, target/cache/toolchain environment, cwd/workspace containment, cancellation, and raw agent-initiated compiler management. Agent-initiated raw `rustc`/`rustdoc` remains a top-level managed BUILD request.
>
> Eliminate child storage admission, child `ThinCache.prepare()` / `ThinCache.finish()`, child cache deltas, child root receipts, and normal per-child request IDs. Take one authoritative cache-before snapshot and one cache-after snapshot around the Cargo root.
>
> Treat the active thin receipt store as the authoritative root evidence store after attribution repair. Never aggregate root and child cache deltas. Persist sccache server/restart epoch and measurement-window semantics.
>
> Fix the independently demonstrated bounded/cached `cargo locate-project --workspace` 10-second control-plane defect. Audit long-wait non-BUILD requests by exact argv/lane rather than command name.
>
> Land a semantically safe path-normalization capability behind a controlled configuration gate. Missing normalization is a confirmed configuration deficiency, but production activation/performance claims still require the controlled cold/warm/rotated C-vs-D A/B.
>
> Do not introduce an 80% or 90% universal cache gate. Establish per-app/profile baselines after controlled measurements.
>
> Keep `BUILD=1` while remediation/cache experiments run. Queue duration does not prove capacity. After measurements are trustworthy, run controlled BUILD=1 vs BUILD=2 under representative host conditions. Treat `1`, `2`, or `DEFER` as valid outcomes.
>
> Do not benchmark by bypassing RightKit with unmanaged direct Cargo. Do not restore SQLite lifecycle, reservations, quarantine, exactly-once execution, maintenance leases, generic result caching, adaptive token scheduling, or per-transition fsync machinery.
>
> Run compiler-topology acceptance after cutover; it confirms safety and quantifies the performance win, but does not reopen the topology decision unless a safety regression is found.
>
> Finish with the measured `ADOPT / REJECT / DEFER` table and update the canonical architecture once.

## 28. Completion definition

This protocol is complete when:

- RightKit remains sole managed agent entrypoint;
- shared Cargo home reuse is proven;
- sccache counter semantics are reconciled;
- warm-target reuse is proven;
- rotated-target cache behavior is measured and remaining misses explained;
- cancellation provenance is explicit and controlled cancels leave zero attributed writers;
- generation rotation occurs only for genuinely uncertain writers;
- workspace discovery is bounded/cached/observable CONTROL work;
- normal Cargo-owned compiler children no longer create broker child jobs;
- delegated compiler children no longer run root-style storage/cache-stat services;
- per-child cache deltas and normal child request IDs are eliminated;
- root receipts expose phase timing and cache truth;
- compiler-child brokerage is retained or removed based on measured cost;
- BUILD capacity is selected from controlled host measurements, with `1` accepted as a valid measured result;
- same-workspace writer safety remains enforced;
- CONTROL remains responsive;
- macOS containment acceptance passes;
- Windows Job Object acceptance passes before Windows is called complete;
- obsolete workflow/reservation/quarantine machinery is removed after accepted cutover.


## 29. Bounded closeout campaign

The architecture discussion is closed while the following work executes.

Run in this order:

1. **Preserve evidence + repair compiler topology**
   - persist the live 40,039-receipt analysis and extraction method;
   - establish authoritative thin receipt/cache semantics;
   - remove normal compiler children from the broker job graph;
   - eliminate child cache/preflight/root-telemetry work;
   - run compiler topology/process-containment acceptance.

2. **Known control-plane defect**
   - fix bounded/cached workspace discovery and the literal 10-second timeout behavior;
   - inspect long-wait non-BUILD commands by exact lane/argv.

3. **Cache campaign**
   - cold target/current cache;
   - same-target warm repeat;
   - rotated target/no normalization;
   - rotated target/candidate normalization;
   - decide production normalization activation and establish baselines.

4. **Capacity campaign**
   - controlled BUILD=1 vs BUILD=2 under representative workstation conditions.

5. **Decision**
   - produce `ADOPT / REJECT / DEFER`;
   - update the canonical architecture once.

Do not add another compiler-topology decision experiment. The live receipt corpus has closed that gate; only post-change safety acceptance and before/after quantification remain.

---

## 30. Required final evidence table

At protocol completion, produce exactly this decision artifact before changing canonical architecture:

| Question | Measurement | Result | Decision |
|---|---|---|---|
| Which receipt store and cache counters are authoritative? | live thin-store audit + server-window semantics | thin store authoritative after repair; historical bases reconciled | persist attribution contract |
| Are root cache counters trustworthy? | before/after root attribution with server/restart epoch | measured result | ADOPT fix / further diagnose |
| How much does the confirmed missing path normalization harm sccache reuse? | cache C vs D | measured delta | ADOPT permanent normalization / choose alternative / DEFER |
| Did direct root-owned compiler topology preserve safety and remove measured child overhead? | §11.3 acceptance + before/after receipt evidence | measured result | ACCEPT / ROLLBACK-FIX |
| What BUILD capacity is safe and useful? | BUILD=1 vs BUILD=2 | measured result | BUILD=1 / BUILD=2 / DEFER |
| When should generations rotate? | clean fail/cancel/uncertain-writer tests | measured result | confirm/correct rule |
| What cache reuse is normal? | per-app/profile controlled baselines | measured result | define regression thresholds afterward |

No additional architecture review is required before this table exists. Source-backed corrections may be added, but they do not restart architecture selection.

## 31. Source-confirmed implementation facts entering this protocol

The following are no longer reviewer hypotheses:

```text
1. Nested compiler children do not acquire a second BUILD scheduler permit.
2. Nested compiler children are still represented as broker child jobs.
3. Compile-capable jobs run storage admission.
4. Compile-capable jobs call ThinCache.prepare().
5. ThinCache.prepare() launches a global sccache stats snapshot.
6. Compile-capable jobs call ThinCache.finish().
7. ThinCache.finish() launches another global sccache stats snapshot.
8. Therefore delegated compiler children currently produce redundant root-style cache/stat work.
9. resolveCargoWorkspace currently uses a literal 10,000 ms timeout.
10. ThinCache does not currently inject SCCACHE_BASEDIRS in the ordinary broker cache environment.
```

Implementation may immediately correct facts 3–8 for delegated compiler children and the independently demonstrated workspace-discovery defect.

Additional live evidence entering v2.7:

```text
40,039 thin receipts inspected
39,186 compiler requests
31,817 rustc requests
7,366 rustc version/print probes
3 rustdoc requests
666 Cargo roots
compiler probe median runMs = 316 ms
compiler probe p90 runMs = 710 ms
compiler probe cumulative runMs = 56.5 min
19,256 compile children performed redundant cache sampling
child queueMs median ~= 1 ms
child queueMs cumulative ~= 1.5 min
```

This closes the normal compiler-child brokerage decision.

Path normalization's performance magnitude and safe BUILD concurrency remain experimental questions.
