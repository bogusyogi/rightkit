# RightKit Build Control — Remediation & Proof Status

**Status:** open. Source-complete, **operationally unaccepted**. As of 2026-08-22 the honest
headline is not "1 of 13 proofs closed" but **no end-to-end operational guarantee is fully
proven** — the one closed proof has been split into a narrow durable-state property that is
closed and two real crash properties that are not (P20). Five defects in the P-list are
blocking-class: P13, P14, P15, P16, P17.

**Companion document:** [RIGHTKIT-BUILD-CONTROL-ARCHITECTURE.md](RIGHTKIT-BUILD-CONTROL-ARCHITECTURE.md)
describes what the system *is* and how to operate it. This document tracks what has been
*proven* about it, what has not, and the incidents that produced the design. Where the two
disagree about a design boundary, the architecture document wins; where they disagree about
whether something is finished, **this one wins**.

**Scope:** `@rightkit/build-control-{core,broker,clients,policy,staging}` and the workspace
rules that govern them.

**Evidence basis:** live inspection of `/Volumes/D/.rightkit-managed` on 2026-08-14 —
`state/broker.json` (407 entries), 1 187 receipt files, on-disk namespace measurement, the
live process table, and runtime `v1.14-statfs-safe-20260814` sources; plus the 2026-08-21
broker crash-loop incident and the repairs recorded in §12.

Every claim below is followed by the observation that produces it. Where a claim is
inference rather than measurement, it says so.

---

## The standing rule this document exists to enforce

> **Source-level tests are not operational acceptance.**

The original remediation plan said plainly that unit-test process doubles cannot reproduce
daemon crashes, orphaned compilers, lane stalls, or disk pressure. Source-level tests were
nonetheless recorded as completion evidence, and the recovery summary was marked
"implemented in source" while thirteen live proofs stood open. That is why failures kept
arriving as surprises: the remediation was never closed, so each new failure looked like an
isolated incident instead of the unfinished work it was.

**Do not report build control as done until every proof in the table below is struck
through with the evidence that closed it.**

---

## Proof status

| # | Required proof | State |
|---|---|---|
| 1 | CONTROL probe finishes within two seconds during an active build | **open** |
| 2 | Installer becomes the next heavy request under background load | **open** |
| 3 | Client death does not kill a detached installer; explicit cancel reaps every descendant | **open** |
| 4 | Nested Cargo cannot deadlock behind a parent permit | **open** |
| 5 | A hung `statfs` or GC cannot hold the build lane | **open** |
| 6 | GC racing same-namespace admission cannot delete a live/warm target | **open** |
| 7a | ~~A pre-seeded open durable row recovers idempotently into one stable terminal receipt~~ | **closed 2026-08-21** |
| 7b | Payload launch is at-most-once across real broker crashes | **open** — see P20 |
| 7c | The terminal receipt is exactly-once across real crash boundaries | **open** — see P20 |
| 8 | A failed/cancelled build preserves its target; a warm rerun proves reuse | **open** |
| 9 | sccache failure performs a bounded restart or direct-rustc fallback | **open** — and newly load-bearing, since sccache was enabled on macOS on 2026-08-21 |
| 10 | Tool/runtime digest change gives a typed rejection and atomic rollback | **partially observed** — a real digest mismatch was seen on 2026-08-21 and behaved as designed, but rollback was not exercised |
| 11 | Real multi-repository contention passes on macOS | **open** |
| 12 | Windows Job Object plus a live Windows contention fixture pass | **open** — the C addon has never been compiled on a Windows host |
| 13 | HeardRight installer builds, installs, launches, and passes installed-runtime acceptance | **open** |

**Proof 7 closed by:** `recovery yields exactly one receipt at every open lifecycle
boundary` in `packages/build-control-broker/src/job-service.test.mjs`. It seeds a real
durable row at each of `ADMITTED`/`STARTING`/`RUNNING`/`REAPING`, recovers twice over the
same SQLite database, and asserts the receipt is byte-identical across both passes. This is
the one proof a process double legitimately *can* carry, because the durable store — not
the process — is the thing under test. Proofs 1–6 and 8–13 cannot be closed this way and
must not be.

---

## Implementation order

Seven independent external audits between 2026-08-21 and 2026-08-22 converge on this
sequence. Each row names the finding that justifies it, so the reasoning is recoverable
without re-reading the audits.

| # | Do this | Why now | Size |
|---|---|---|---|
| 1 | Land every live config fix in `bootstrap-macos-cli.mjs` defaults (P31) | Until this lands, a re-bootstrap silently restores the exact settings that caused the 2026-08-14 incident. Everything below can be undone by one `--force`. | text |
| 2 | Move `outdated` to a read-only dependency-analysis lane; make `tree` lock-aware (P29, P23) | A read-only query is holding a repository's build lane for its whole run. Cheapest correctness win available. | small |
| 3 | RUNNING-phase heartbeat with resource samples; receipt timing fields (P27, P30) | Nothing can be diagnosed mid-run today — 7 events across 24 minutes. Every later proof that cites log evidence is unprovable without this. | medium |
| 4 | Run-phase deadline, measured from RUNNING not submission (P28) | A submission-clock deadline would kill queued builds, so this must follow the lane fix, not precede it. | small |
| 5 | Transactional quarantine; explicit liveness state (P13) | Corruption-class, and a defect introduced by the 2026-08-21 fix itself: a failed gate write still reports success. | small |
| 6 | OS-enforced broker singleton before the state store opens (P14) | Corruption-class if confirmed — two schedulers over one database. Verify the claim first. | medium |
| 7 | Release scheduler permits on every path; durable circuit breakers instead (P15) | A held permit wedges every build on the machine at `buildConcurrency: 1`. | small |
| 8 | Refuse nested BUILD / WORKSPACE_WRITE / TOOLCHAIN_WRITE (P16) | `TOOLCHAIN_WRITE` is absent from `HEAVY`, so nested toolchain mutation escapes its exclusive lane today. | small |
| 9 | Decide sccache: run the containment proof, evaluating client-side mode first (P26, P17, P33) | Enabled while its condition is unproven, and the risky topology has now been observed live. Do not answer this by disabling caching permanently. | medium |
| 10 | Generation in the physical namespace path, including temp (P12) | Removes the orphan-reuse hazard structurally; the quarantine gate becomes defence-in-depth rather than the only defence. | structural |
| 11 | Namespace inspect/clear CLI with audit trail (P4) | An unattended agent that trips a quarantine is stuck with no self-service exit. | small |
| 12 | One declarative policy model generating validation, classification, and resource acquisition (P22, P6) | Four defects to date were one rule enforced in several places with one copy updated. | structural |
| 13 | Storage ledger, reconcile, and the reservation-undercount fix together (P5, section 2) | Pressure GC cannot fire while `usedBytes` is frozen; doing the ledger without the undercount fix makes the ledger inherit the lie. | structural |
| 14 | Windows: compile and prove the Job Object addon, fix the cutover guard, then resize | The largest idle capacity in the fleet — 20 threads and 63.7 GB running four compilers — but sizing is worthless until containment is proven. | platform |
| 15 | Split proofs 7b/7c and close proofs 1 and 8 (P20) | Proofs 1 and 8 need only live runs and no new code. | tests |
| 16 | Benchmark incremental against sccache per profile (P26) | Do not blanket-disable incremental; the two are strong at different workloads. | measurement |

Two decisions are Adrian's and block their rows: **which doctrine rule to displace so F0 can
land** (the core is at 6 137 of 6 144 bytes), and **whether to run the sccache containment
proof or accept the risk while it stays enabled**.

---

## Pending — open review findings not yet fixed

Raised by two independent validation passes over the consolidated docs on 2026-08-22. None
are shipping blockers; all are open. Findings that did not survive verification are recorded
at the bottom so they are not re-raised.

### P1. The sizing rule contradicts its own arithmetic — **doc defect, confirmed**

`RIGHTKIT-BUILD-CONTROL-ARCHITECTURE.md` §9 budgets "roughly 2 GB of RAM each" per
concurrent compiler, then calls 4 compilers "the honest ceiling" on an 8.6 GB host. 4 x 2 GB
is 8 GB, leaving 0.6 GB for the OS — still oversubscribed by the document's own rule. Either
the per-compiler budget or the stated ceiling is wrong. Resolve by measuring actual peak
rustc RSS on this workspace rather than adjusting the prose to match.

### P2. The `processMayBeLive` / namespace-gate relationship is ambiguous — **doc defect, confirmed**

§4 states `processMayBeLive` is read by no runtime path, then describes `#prepareNamespace`
refusing "any unproven reap", without saying what actually drives the gate. It is a broker
setting (`namespace-quarantine:<namespace>`, `job-service.mjs:360`), not the namespace row
and not the flag. The ambiguity has already caused harm: an independent reviewer read this
passage and concluded the namespace row's `state` field drives the gate, which is false.

### P3. Enabling sccache while proof 9 is open needs an explicit risk statement — **clarity gap**

`requireOwnershipProof: true` is a per-request runtime gate that degrades to direct rustc
when the proof fails; proof 9 is the unrun operational proof that the failure path behaves
under real failure. Both are simultaneously true, so this is **not** the contradiction it was
reported as. What is missing is a plain statement that macOS now depends on an unproven
failure path. Say so where sccache is enabled.

### P4. No CLI to clear a namespace quarantine

A quarantined namespace refuses heavy work until the `namespace-quarantine:<namespace>`
setting is removed by hand. For an agent operator this is a stuck state with no recovery
path. Wanted: a command that proves the process group is dead, clears the setting, and logs
the clearance; plus a diagnostic that reports directory, state entry, live processes, disk
usage against reservation, and quarantine status for one namespace.

### P5. GC never reconciles the filesystem side

GC walks state entries and checks whether directories exist; it never walks directories and
checks whether entries exist. Section 2 already measures the consequence: 54 tmp directories
against 41 namespaces, 13 already leaked. Wanted: a reconcile pass that lists both sides,
reports orphans in both directions, and removes a directory only after proving no compiler
owns it.

### P6. Validation rules are still single-sourced nowhere

The three defects in section 12 were one root cause: a rule enforced in more than one place
with one copy updated. Section 12.5 names the pattern but nothing prevents its recurrence.
Wanted: a rules registry the enforcement layers read from, starting with the tool allowlist,
the environment blocklist, and the redirect-argument list — each of which has been wrong
independently — plus a standing requirement that any rule change enumerate every layer that
enforces it.

### P7. `processMayBeLive` is dead metadata

Written into quarantine receipts and the event log, read by nothing. Either surface it where
a quarantine is encountered, or remove it. Carrying an unread field that looks load-bearing
is worse than either.

### P8. The architecture document does not mention the adversarial reviews

The reviews that corrected material errors in this document are recorded here and not there.
A reader of the architecture document alone does not learn that its claims were challenged.
Minor, but it is the kind of omission this pair of documents exists to prevent.

### P9. No job deadline is configured, so a slow tool runs until a human notices

`bounds.jobDeadlineMs` is `0` on macOS — no deadline at all. Measured consequence, request
`cda53b70` on 2026-08-21:

| Transition | Time |
|---|---|
| SUBMITTED | 18:41:01.935 |
| RUNNING | 18:41:02.334 |
| REAPING | 19:05:23.849 |
| CANCELLED | 19:05:23.917 |

`outdated` against HeardRight's `src-tauri` ran **24m 21s** and never completed: the receipt
records `reason: explicit-cancel`, `signal: SIGTERM`, `exitCode: 125`. Broker overhead was
399 ms, so the queue is not implicated. Nothing bounded the run; it ended when a person
killed it.

Two things follow. First, a deadline should exist, and the lane it applies to matters more
than the number. Second, `outdated` is registered `WORKSPACE_WRITE`, which serializes
against BUILD for that repository — so a 24-minute dependency query holds that repository's
build lane for its whole duration. The lane is defensible on its own terms (cargo-outdated
writes temp manifests and runs `update` internally), but combining a write lane with no
deadline is not.

### P10. Output is stored without timestamps, so a slow phase cannot be attributed

The receipt for `cda53b70` holds 1 234 bytes of bounded head/tail and a digest. The tail
proves the results table had already printed before cancellation, but nothing records *when*
any chunk arrived, and no child CPU or RSS is sampled.

**Corrected 2026-08-22.** An earlier revision called the teardown claim an "unproven
hypothesis". That was wrong, and the error was one of scope: RightKit telemetry cannot prove
it, but external profiling can and did. A live macOS sample measured **1 302 of 1 493 samples
inside `PkgStatus` hash-table destruction**, with output appearing around the 19-minute mark
of the 24-minute run — roughly five minutes spent tearing down after the answer was already
available. The correct statement is that **RightKit could not see it**, not that it did not
happen. That is the actual finding: our telemetry was the blind party, and writing "unproven"
where we meant "unobservable by us" understated our own gap.

The event log is no help either. Filtering `events.ndjson` for that request returns **seven
events total**: five lifecycle transitions in the first 400 ms, then a single gap of
**1 461.5 s**, then the terminal record. `bounds.heartbeatIntervalMs` is 5 000, so heartbeats
are being taken, but they never reach the durable log — a job is completely dark between
STARTING and REAPING no matter how long it runs. Any question of the form "was it still
making progress at minute 12?" is unanswerable today.

What exists today: `lifecycle_transitions` (per-state, millisecond), `terminal_receipts`
(exit, signal, reason, reap, namespace, capability, bounded output plus digest), and a
bounded `events.ndjson`. What is missing to answer "why was this slow": per-chunk output
timestamps, periodic child resource sampling, and `durationMs`/`queueWaitMs` carried on the
receipt itself rather than derived from transitions — the last of which is already open as
F6.2.

### P11. Dependency-upgrade tooling is only half reachable

`update`, `add` (built in since Rust 1.62), and `remove` (since 1.66) are Cargo built-ins
and now route to `WORKSPACE_WRITE`. The
cargo-edit commands `upgrade` and `set-version` are declared and registerable but stay
unreachable until an operator pins a digest, and **cargo-edit is not installed on the macOS
host** — only Windows carries `cargo-upgrade`, `cargo-add`, `cargo-rm`, and
`cargo-set-version`. Registering them therefore needs the binaries present first.

Note for whoever runs the upgrade sweep: `cargo upgrade` rewrites manifest *requirements*,
which is a different and more invasive operation than `cargo update` rewriting the lockfile.
It must run per independent workspace, and path, Git, and vendored dependencies need
separate handling. Treat a `--dry-run` pass as mandatory before the applying pass, and
expect the applying pass to be followed by a full check and test run in the same lane.

### P12. Namespace generation is not in the physical path — **the root cause the gate compensates for**

*Verified: generation lives only in SQLite; `workspaces/<namespace>` has no generation segment.*

The quarantine gate (§12.1) converts silent corruption into refusal, which is the right
trade, but it is a compensating control. Directory identity and directory path are the same
thing, so every orphan scenario re-exposes the same hazard forever. Putting the generation
into the physical path makes reuse structurally impossible and demotes the gate to
defence-in-depth:

```text
workspaces/<logical-namespace>/
  identity.json
  current -> generations/000042
  generations/000041/   # retired or quarantined
  generations/000042/   # current warm generation
```

`target/`, `build/`, `tmp/`, and install roots all move beneath the generation. Note that
temp currently sits *outside* `workspaces/`, and GC only moves workspace roots — which is
why section 2 measures leaked temp directories. Moving temp under the generation fixes both.

The safety half of this is independent of F1.3's cache-stranding question and should be
pulled forward. Related correction: the architecture's "one directory per repository/target"
wording is inaccurate — the namespace hash covers repository identity and toolchain; target
triple and profile are separated inside Cargo's own hierarchy.

### P13. Quarantine is not atomic, and a failed gate write still reports success — **confirmed defect**

*Verified in `job-service.mjs#quarantineRecovery`.*

`quarantine.namespaceGated = namespace` is assigned unconditionally after the `writeSetting`
try/catch. A failed gate write is logged and swallowed, then the receipt is written claiming
the namespace was gated, the request is terminalized, and its reservation is released. The
resulting sequence recreates exactly the corruption the gate exists to prevent, while
leaving evidence that says it did not happen:

```text
write namespace gate    -> fails (logged, ignored)
write CANCELLED receipt -> succeeds, claims namespaceGated
release reservation     -> succeeds
next job checks gate    -> absent
next job activates the namespace
```

The gate write, the outcome change, the receipt, and the reservation disposition must be one
transaction that commits all or none. `#prepareNamespace` should likewise perform a guarded
activation rather than a check followed by a separate activation.

**The state model is also wrong.** A request is stored `CANCELLED` while its process may be
alive, and the architecture's own rule is that a terminal receipt follows a *certified* reap
— but quarantine writes one precisely when the reap could not be certified. Request outcome,
process liveness, and namespace state are three orthogonal facts and should be stored as
three fields, so an unresolved recovery reads `ABANDONED` / `LIVENESS_UNKNOWN` /
`QUARANTINED`. Relatedly, `processMayBeLive: false` currently encodes *unknown* as *false*
whenever ownership is missing. Unknown is not false; the field needs three values or it
should go (see P7).

### P14. No enforced single-broker invariant — **reported, not yet verified by me**

The claim: `ManagedJobService` is constructed — opening the database, reaping prior sccache
ownership, running recovery, starting maintenance — *before* the socket is inspected and
bound, and the stale-socket helper unlinks any socket owned by the same uid without first
attempting a connection to establish that it is actually stale. A second broker could then
open the same database, read the first broker's live requests as crash residue, quarantine
them, unlink the live socket, and run a second independent scheduler. SQLite serializes
writes; it does not serialize two schedulers.

If true this is corruption-class and outranks everything else here. Fix shape: an
OS-enforced singleton (POSIX file lock, Windows named mutex) acquired **before** the state
store is opened, a broker instance id and epoch in the database and in the handshake, an
authenticated handshake attempted against any existing socket before it is removed, and
recovery/maintenance started only after singleton ownership is established. A launchd label
is not enforcement.

### P15. A scheduler permit is used as a durable safety lock, wedging the machine — **confirmed**

*Verified: `job-service.mjs` sets `job.holdPermit = true` on the reap-pending, reservation-release-failure, and REAPING-invariant paths, and the release is guarded by `if (!job.holdPermit)`.*

This is the startup wedge relocated into the running broker. An unreapable process found
while the broker is live, or three failed reservation releases, deliberately retains the
scheduler permit. At `buildConcurrency: 1` that blocks every other BUILD on the machine —
including builds in unrelated repositories — until someone restarts the broker.

Permits must always be released in `finally`. Safety belongs in durable state: quarantine
the namespace for an unreaped tree; set a machine-level heavy-admission circuit breaker for
an untrustworthy reservation ledger; fall back to a control-only mode for corrupted global
state. Section 12.1's residual-risk note describes the namespace-local behaviour that was
intended; the live path does not implement it.

### P16. Nested execution bypasses the scheduler, including exclusive toolchain mutation — **partly confirmed**

*Verified: `HEAVY = new Set(['BUILD', 'WORKSPACE_WRITE'])` — `TOOLCHAIN_WRITE` is absent.*

Nested requests take no scheduler permit and inherit the root's layout and execution domain.
Because `TOOLCHAIN_WRITE` is not in `HEAVY`, a nested `rustup target add` can run beneath a
CONTROL or BUILD root and bypass the exclusive toolchain lane entirely — mutating the
toolchain while compilers are using it. Two nested builds under one root likewise share one
target/build/temp directory with no second writer permit. The parent capability token is
injected into every descendant's environment, so any build script inherits that authority.

Together this defeats `buildConcurrency`, per-repository exclusion, toolchain exclusivity,
disk reservations, and the one-writer namespace guarantee. Until a real nested-execution
design exists, nested work should be restricted to an explicit read-only allowlist
(`metadata --locked`, `locate-project`, compiler `--print` probes, `rustup which`,
`rustup target list --installed`) and nested BUILD / WORKSPACE_WRITE / TOOLCHAIN_WRITE
refused outright. The existing test proves only that a CONTROL root cannot mint a nested
BUILD; it must also cover nested WORKSPACE_WRITE and TOOLCHAIN_WRITE.

### P17. sccache enablement fails the architecture's own containment condition — supersedes P3

The architecture requires broker-owned lifecycle **and child-process containment** before
sccache is enabled. What is implemented proves server *ownership*: the server is a broker
descendant, its socket has the expected owner and permissions, and it answers a stats probe.
It does not prove per-request compiler containment — the Cargo job and the persistent
sccache server are in different process groups, and in server-side compilation the compiler
children belong to the server's tree, so cancelling the job's group cannot certify that the
compiler attributed to that request ended.

There is also a durable-recovery inconsistency: when a persisted sccache server cannot be
reaped at startup the ownership setting is cleared anyway and sccache is degraded for that
broker lifetime, so a second restart has no record with which to retry the reap. The
shutdown path correctly retains ownership in the equivalent case.

P3 framed this as a wording gap. That was too soft. **Set `sccache.mode` back to `disabled`
until a live cancellation proof passes** — hotfix forward, do not roll the runtime back.
Acceptance must record pid/ppid/pgid (or Job Object membership) for every compiler and show:
every compiler attributable to one request; cancellation leaving zero attributed compilers;
cancelling one request not killing another's compiler; a broker crash leaving no untracked
server or compiler; a failed server reap retaining its ownership record; and a bounded,
recorded fallback to direct rustc. `SCCACHE_CLIENT_SIDE` is worth evaluating against the
pinned version, since it keeps the compiler in the requesting job's tree.

### P18. Cargo configuration enforcement is regex-based, not the rejection the docs claim — **confirmed**

*Verified: `utilities.mjs` scans the file line-by-line for a small set of patterns.*

The architecture says a workspace configuration file is rejected outright. The code instead
reads the TOML as text and matches line-oriented regexes, so quoted keys, dotted keys, and
target-scoped `linker`/`runner` forms can slip through. Cargo configuration has far more
execution-affecting surface than the scanner covers. Digest-pinning the compiler and then
accepting a text file that can select an unregistered linker or wrapper is a control-boundary
defect, not a parsing gap. Pick one coherent policy: reject every ancestor `.cargo/config*`
outright as documented, or parse TOML structurally against a documented schema and hash the
accepted configuration into the request identity. Do not enforce a TOML authority boundary
with regular expressions.

### P19. `cargo fetch` is missing from the protected built-in set — **confirmed defect**

*Verified: `CARGO_BUILTIN_COMMANDS` in `command-capabilities.mjs` does not contain `fetch`.*

`fetch` is a real Cargo built-in, so it can still be registered as an external command and
redirected — the exact hole P-section item 12.4 was meant to close, left open by a
hand-maintained list. The list should be generated or validated against the pinned Cargo
version during runtime activation rather than typed by hand.

### P20. Proof 7 is overclaimed and should be split — **confirmed**

The closing test seeds durable rows at `ADMITTED`/`STARTING`/`RUNNING`/`REAPING`, constructs
a service, and asserts the receipt is byte-identical across two recoveries. No process is
launched and no broker is killed, and the loop omits `SUBMITTED`/`CLASSIFIED`/`QUEUED`. What
it proves is real but narrow. Restate as:

| | Proof | Status |
|---|---|---|
| 7a | A pre-seeded open durable row recovers idempotently into one stable terminal receipt | **closed** |
| 7b | Payload launch is at-most-once across real broker crashes | **open** |
| 7c | The terminal receipt is exactly-once across real crash boundaries | **open** |

7b and 7c need a helper executable writing a unique fsynced side-effect record, with broker
termination injected at each launcher boundary. Under this split the honest headline is that
**no end-to-end operational guarantee is fully proven**, not "1 of 13 closed".

### P21. The control plane shares the busiest storage domain, with per-transition fsync

Database, event log, socket, runtime state, receipts, target trees, temp, and compiler cache
all live on one external volume, so a latency event takes out both the build data and the
evidence needed to diagnose it. Compounding it: synchronous SQLite, WAL with
`synchronous = FULL`, one transaction per lifecycle state, an awaited required-log write per
event, and an event log that can rewrite and rename its retained tail rather than rotating a
segment. A `--version` probe can therefore incur several synchronous commits and fsyncs on
the same disk a compilation is using. Not benchmarked here — structural, not measured. Split
the control plane onto internal storage, move SQLite work off the broker event loop, use
segmented append-only logs, and fsync ownership-critical and terminal events rather than
every transition.

### P22. One declarative policy model, not one registry — extends P6

P6 asked for a shared rules registry. The stronger form: the root cause of the crash loop
was not "two validators" but the absence of an authoritative policy model. A command should
declare orthogonal resources — workspace lock, artifact writer, cpu tokens, memory class,
toolchain lock, cargo-home lock, network class, cache mode, namespace requirement — acquired
in one canonical order. Config validation, classification, resource acquisition, external
command validation, documentation, and tests should all be generated from that one
declaration. Today `HEAVY` alone drives disk reservation, namespace activation, directory
creation, storage sampling, sccache startup, compiler injection, and admission, so an
unlocked `metadata`, `fmt`, `clean`, or `outdated` can create artifact directories and
reserve build disk without compiling anything. Keep unknown commands fail-closed.

### P23. `cargo tree` is classified CONTROL unconditionally

Without `--locked`/`--frozen`, resolution may rewrite `Cargo.lock`. `tree` should classify
like `metadata`: CONTROL when locked or frozen, WORKSPACE_WRITE otherwise.

### P24. External plugin invocation is only partially Cargo-compatible

Forwarding the subcommand fixed the immediate failure, but the broker passes the original
argv straight through without canonicalising Cargo-global options that appear *before* the
subcommand, and the execution environment does not set `CARGO`, which plugins use to invoke
Cargo again. Set `CARGO` to the managed shim so recursive invocations stay brokered, and
either translate or explicitly reject unsupported global options.

### P25. The two documents still mix four kinds of content

Normative architecture, current implementation, future remediation, and incident history are
interleaved, which is why they can contradict themselves. A live example inside this
document: F1.1 is withdrawn, yet the section summary still counts the ~1.9x multiplier it
removed and the sequencing table still schedules it. Proof status also does not distinguish
unit, fault-injection, integration, and live evidence. The suggested split is an architecture
specification carrying only MUST/SHOULD invariants, an operations runbook describing only the
deployed commit, a validation matrix with one row per requirement and a column per evidence
class, and an incident/decision record. Adopting it is a decision for Adrian, not a defect.

### Standing question

**Who clears a namespace quarantine when no human is at the wheel?** Define the
agent-session policy before the first unattended trip, not after.

### P26. Caching is a first-class resource, not a bolt-on — **supersedes P17's remedy**

P17 concluded "set `sccache.mode` back to `disabled` until a live cancellation proof
passes". The diagnosis stands; **the remedy was wrong** and is withdrawn. A build controller
whose answer to a containment gap is to permanently stop caching has traded one incomplete
design for another — the target is not

```text
correct ownership + rebuild everything
```

it is

```text
correct ownership + deterministic cancellation + warm targets
  + sccache + incremental where it helps + bounded parallelism
```

Disabling is an emergency fallback, not an architecture. The correct topology keeps cache
*storage* machine-global while keeping every `rustc` inside the requesting build domain:

```text
broker-owned shared cache state
        |
   lookup / store
        |
  request A domain            request B domain
    cargo -> sccache client     cargo -> sccache client
      -> rustc (in domain A)      -> rustc (in domain B)
```

That is exactly what the current server-side mode does not give us, and it is why the
containment proof fails — not a reason to stop caching. **Evaluate `SCCACHE_CLIENT_SIDE`
against the pinned version first**, since it runs the compiler in the client process and
would leave rustc in the requesting job's tree. Keep one broker-owned server; the local disk
backend is not designed for several servers over one cache directory.

Three layers, each with a distinct job, rather than one global switch:

| Layer | Best at |
|---|---|
| Cargo warm target directory | same workspace, small edits, repeated local work |
| sccache | clean or GC'd targets, branch switches, cross-workspace reuse, agent/CI churn |
| Registry + toolchain cache | source acquisition |

Incremental compilation should therefore be **profile-aware, not globally disabled**. F1.4
proposes switching `CARGO_INCREMENTAL=0` off for test/check/clippy/release; that must not
land as a blanket rule without benchmarking, because incremental and sccache are strong at
different things and the doc currently pays for incremental state *and* loses sccache hits on
the same crates. Sketch of the profiles to measure rather than assume: interactive
development (incremental on, sccache on), agent check/clippy (benchmark both), release and
installer (incremental off, sccache on), ephemeral clean build (incremental off, sccache
most valuable).

**Promote the cache to a modelled service** with lifecycle ownership, an immutable binary,
cache directory and byte cap, stats, hit/miss telemetry, health state, restart budget,
fallback mode, and a compiler-containment proof — and put its result in every receipt, so
these three outcomes stop being indistinguishable:

```text
build succeeded, cache healthy
build succeeded, cache degraded
build succeeded, direct rustc fallback
```

**Add an effectiveness gate.** A cache that runs and returns a 2% hit rate has not succeeded.
Measure hit rate, cacheable-compile rate, bytes read and written, and time saved across cold
target + cold cache (baseline), warm target, cold target + warm cache, a small edit, and a
branch switch.

This also attacks the disk problem from section 2. Roughly 22 GB sits in per-namespace target
trees today because losing one means a fully cold rebuild. With an effective compiler cache
that stops being true, so target-cache GC can be far more aggressive — cache-aware GC and
disk pressure are the same problem.

**Revised order:** make sccache containment correct (not disabled); then the single-broker
invariant (P14); transactional quarantine (P13); nested resource ownership (P16); intelligent
warm-target retention; benchmark incremental against sccache per workload; cache-aware GC;
and only then tune `-j`, linker, profiles, and crate structure.

### P27. RUNNING has no heartbeat, so a live job cannot be observed at all

The heartbeat timer exists only while a request is queued and is cleared at admission, which
is why `events.ndjson` holds seven records across a 24-minute run (P10). Persist sampled
progress *while RUNNING*: elapsed time, output bytes, last-output timestamp, CPU, and RSS.
Without it no deadline, stall detector, or effectiveness measurement can be trusted, and
every later proof that depends on log evidence is unprovable.

### P28. Deadlines must start at RUNNING, not at submission

`bounds.jobDeadlineMs` is measured from submission and therefore includes queue time, so
raising it above zero today would kill legitimate builds that merely waited behind the
machine-wide lane. What is needed is a **run-phase** deadline applied to bounded analysis
commands, with builds staying release-owned. Regression tests must assert that the clock
starts at RUNNING, and that a timeout reaps the whole process tree and records `TIMED_OUT`.

### P29. `outdated` is in the wrong lane

A dependency query writes temporary manifests; it does not mutate the source repository.
Holding `WORKSPACE_WRITE` makes it serialize against BUILD for that repository, which is how
one 24-minute query held HeardRight's build lane. It belongs in a low-concurrency
dependency-analysis lane that is read-only with respect to the checkout. `machete --fix`
does mutate and stays `WORKSPACE_WRITE`; default `machete` analysis does not. `upgrade` and
`set-version` genuinely rewrite manifests and correctly stay `WORKSPACE_WRITE`, as do
`update`, `add`, and `remove`.

### P30. Receipts carry no timing or resource evidence

Add `queueWaitMs`, `runDurationMs`, `firstOutputAt`, `lastOutputAt`, stdout and stderr byte
counts, and the resource samples from P27. Today every one of these must be reconstructed
from `lifecycle_transitions`, and the output-timing fields must survive bounded head/tail
retention — the retention policy discards exactly the evidence a slow run needs.

### P31. Live config and bootstrap source have diverged — **confirmed, and it silently reverts every fix**

*Verified in `bootstrap-macos-cli.mjs`.*

Every configuration fix recorded in this document was applied to the **live** `config.json`.
None of them were applied to the bootstrap that generates it. A fresh bootstrap, a
`--force` regeneration, or a rebuilt machine therefore restores exactly the settings the
incidents were caused by:

| Setting | Live (fixed) | Bootstrap source | Consequence of re-bootstrap |
|---|---|---|---|
| `bounds.buildConcurrency` | 1 | **2** | 2 x 4 = 8 compilers on 8.6 GB — the swap/OOM condition of 2026-08-14 |
| `defaults.cacheMode` | `managed` | **`disabled`** | every build recompiles from source |
| `sccache.mode` | `managed` | **`disabled`** | as above |
| `bounds.jobDeadlineMs` | 0 | **0** | matches, and both are wrong (P28) |
| `externalCommands` | 5 | **1** | the four registered plugins disappear |

This is the same defect class as section 12.5 — a rule held in two places with one copy
updated — applied to configuration rather than code, and it is arguably worse, because
nothing fails loudly. The machine simply returns to its pre-remediation behaviour and the
next incident looks new. **Any config change recorded here must land in the bootstrap
defaults in the same change**, and the bootstrap defaults are the authoritative statement of
intent, not the live file.

Note the bootstrap's sccache comment is a deliberate policy — "a separately-decided
activation, not a bootstrap default" — which is defensible. The concurrency and external
command drift is not.

### P32. F3.6 is already closed in source — this document is stale

*Verified in `tool-registry.mjs`: identity is `(path, size, mtimeNs, ctimeNs)` and `sameIdentity`
short-circuits re-hashing.*

Section 3.5 / F3.6 describes ~45 MiB read and SHA-256'd on every request as a live cost, and
several later summaries repeat it. The memoisation the fix proposed has already landed. The
finding should be struck through rather than carried, and it is a reminder that this document
mixes current implementation with future remediation (P25) — a reader planning work off it
would have re-implemented a fix that exists.

The same audit reports `--target` injection live and one target tree rather than two, which
would close F1.2. **Not independently verified here** — record as reported, and confirm before
striking F1.2.

### P33. sccache containment risk observed live, not merely inferred

An audit watching the running system reported the sccache server spawning `rustc` children
inside the *server's* process group rather than the requesting job's — the exact topology P17
describes. If that observation holds, compilers started on a cache miss are structurally
unreachable by the broker's process-group kill, so cancelling a build cannot certify its
compilers ended. That moves proof 9 from theoretical to observed and makes P26's client-side
evaluation the deciding experiment. Independent confirmation is still required before this is
treated as measured.

### P34. Two more open items worth naming

- **The `cargo install` that provisioned cargo-edit held the machine's only BUILD slot for
  its whole compile**, because a toolchain-provisioning install is classified as ordinary
  BUILD at `buildConcurrency: 1`. Tool provisioning and product compilation should not
  contend for the same single slot. This is P15's wedge shape appearing through normal
  operation rather than through failure.
- **Plugins installed through the broker land inside a GC-reclaimable workspace namespace**
  (`workspaces/<ns>/build/install/bin`), so pinning a tool digest at that path pins it to a
  path maintenance may delete. Managed tool binaries need a stable location outside any
  workspace namespace. This compounds the mutable-path trap in architecture section 7 rather
  than replacing it.

### Findings raised and rejected

- **"sccache enablement contradicts `requireOwnershipProof`"** — rejected; see P3. The runtime
  gate and the operational proof are different things.
- **"The quarantine gate reads the namespace row's `state` field"** — rejected; it reads a
  broker setting. See P2.
- **"Typo `powershe` at architecture line 279"** — rejected; that line is a complete
  ` ```powershell ` fence. No such string exists in the file.
- **"Prototype-chain keys were a designed protection that a refactor removed"** — rejected as
  stated. The protection was accidental, an adversarial review judged the attack
  unexploitable, the accidental behaviour was then removed by an own-property lookup, and a
  test caught it. The current explicit check is stronger than what preceded it.

---

## 12. Repairs landed 2026-08-21

Three defects in the same area, found in sequence. The second and third were caused by
incomplete fixes to the first, which is itself the lesson: this area has more than one
validator for the same rule.

### 12.1 Recovery could not finish (startup-fatal)

`#recover()` threw on an open request with no durable process ownership, and again on a
process tree it could not prove reaped. Both run **before the socket binds**, so one
unrecoverable row left the broker unable to start at all, with no exit but manual SQLite
surgery. Recovery now quarantines such a row with a typed receipt
(`RK_RECOVERY_OWNERSHIP_AMBIGUOUS`, `RK_RECOVERY_REAP_INCOMPLETE`) and continues, recording
`processMayBeLive` when the reap escalated.

An adversarial review on 2026-08-21 found the first version of this fix **unsafe** and it
was corrected before shipping. A namespace is one directory per repository/target and its
generation is not part of the path, so terminalizing the row without gating the namespace
would have let a later job reuse the very directory a surviving compiler was writing into —
trading a broker that cannot start for silent concurrent writers. Quarantine now records a
durable namespace gate and `#prepareNamespace` refuses heavy work there with typed
`RK_NAMESPACE_QUARANTINED`. The gate applies to **any** unproven reap, not only an
escalated one: "could not prove the tree is gone" is as unsafe for directory reuse as
"proved it is still alive". CONTROL work is unaffected.

**Residual risk, not yet closed:**

- The gate must be cleared by an operator; nothing clears it automatically, and no CLI
  exists for it yet. A machine that quarantines a namespace will refuse heavy work there
  until the setting is removed by hand.
- `releaseReservation` still frees byte accounting at quarantine time, so the free-space
  floor undercounts a still-growing orphan's output. Successor to proof 6.
- `processMayBeLive` is recorded in the receipt and log but is **not read by any runtime
  code path**. It is an operator signal only.

### 12.2 External command registration was gated twice (crash loop)

A hardcoded `about`/`nextest` literal gated plugin registration in **two** validators —
the capability registry and config validation. Opening only the registry left config load
rejecting the same row; because the broker exits non-zero before binding while launchd
`KeepAlive` restarts it, the result was a crash loop, not a clean refusal. Both validators
now apply the same bounded-token rule, and the registry additionally refuses to shadow a
non-external built-in.

### 12.3 The plugin subcommand was stripped (silent misinvocation)

The broker removed the external subcommand from argv before spawning the plugin binary, so
`cargo-outdated` was invoked without `outdated` and failed parsing its own invocation. This
was masked because `cargo-about` — the only registered plugin — tolerates the missing
subcommand; `nextest` would have failed identically. The old filter also removed the first
matching token anywhere in the line, so an argument whose value equalled the command name
was stripped instead.

### 12.4 External registration could claim un-routed Cargo built-ins

The shadowing guard checked only the ~20 subcommands enumerated in
`DEFAULT_COMMAND_CAPABILITIES`. Every real Cargo built-in absent from that map — `fix`,
`add`, `remove`, `publish`, `package`, `login`, `owner`, `yank`, `uninstall` — was
registerable as an external tool, silently defeating the "unknown commands reject"
invariant for exactly the commands most worth protecting. An explicit built-in set now
rejects them, and prototype-chain keys are refused explicitly rather than by accident.

### 12.5 What these have in common

Each was a **duplicated rule with one copy updated**. Any future change in this area should
first enumerate every place the rule is enforced. §E of the standing review checklist
exists for this reason.

---

## Configuration changes landed 2026-08-21 (macOS)

| Field | Before | After | Rationale |
|---|---|---|---|
| `sccache.mode` / `defaults.cacheMode` | `disabled` | `managed` | Implemented and unit-tested but shipped off; every managed build recompiled from source. Makes proof 9 load-bearing. |
| `bounds.buildConcurrency` | 2 | 1 | 2 × 4 jobs = 8 compilers on an 8.6 GB host, ~1 GB each. rustc exceeds that routinely; the result is swap, kernel-killed compilers, and `exitCode: null` receipts — the 2026-08-14 signature. |
| `externalCommands` | 1 (`about`) | 5 | `outdated`, `machete`, `audit`, `deny` were installed but unreachable. |

Windows remains unresized at the time of writing: 20 threads and 63.7 GB running 2 × 2 = 4
compilers, with `targetCacheCapBytes` at 32 GiB against 610 GB free.

---

## 0. Why this document exists at all

On 2026-08-14, between 14:00 and 17:30 UTC, a HeardRight installer build failed repeatedly. It produced no DMG. Two full compile attempts died, one orphaned `rustc` process tree, one wedged the machine-wide lane so hard that only a broker restart cleared it, and a 36-minute retry was executed against an unchanged failing configuration.

The system that manages all Rust builds in this workspace **wrote nothing during that window**:

| Observability surface | State on 2026-08-14 |
|---|---|
| `console.*` / `stderr.write` calls in `index.mjs`, `scheduler.mjs`, `process-tree.mjs`, `sccache-supervisor.mjs` | **0** |
| `/Volumes/D/.rightkit-managed/logs/` | empty; created Aug 13, never written |
| `/Volumes/D/.rightkit-managed/broker-r17.log` | **0 bytes** |
| `~/Library/Logs/RightKit/build-control.stderr.log` | 4 922 bytes total lifetime; last write **Aug 14 00:11**, fourteen hours before the incident |
| `exitCode` on every `FAILED` receipt before 17:09 | **`null`** |

The two build failures at 16:13:18 and 16:57:00 are recorded as `execution-failure`. That classification is **weaker than it looks**: `index.mjs:207` assigns it whenever `boundedOut` is false and `result.code !== 0`, and `result.code` is `null` on any signal-based close. So `execution-failure` covers all of — Cargo genuinely exiting non-zero, an external SIGKILL (OOM, thermal, a human), *and* `managed-launcher.mjs`'s own failure paths, which `process.exit(125)` on a 30-second start-message deadline or on IPC disconnect before start. All four collapse into one label.

It therefore **rules out only the broker's own disk-floor kill** (`reservation-overrun`). It does not establish that Cargo failed on its own, and RightKit interference cannot be excluded. Their exit codes were never persisted. **The cause of the primary failure is unrecoverable from the record.** The 36-minute retry was not negligence; it was the only move available to an operator with no information.

### 0.1 The rule that caused this

This was not an oversight by any single agent. `docs/agent-rules/workspace.md` states:

> Create process files only when Adrian or protocol requires them; otherwise keep reasoning in chat & execution output as evidence.

and `docs/agent-rules/legion.md`:

> **Evidence before claims.** Use existing command, test, delivery, or artifact output. Create separate proof only when Adrian or required protocol asks.

and the Minimize policy begins its rung ladder at `NOT_BUILD` with:

> Delete work unable to change current decision or advance B.

A logging subsystem is, under all three rules simultaneously, forbidden work: it is separate proof nobody asked for, it advances no state B, and no gate requires it. Every agent that built this system **followed policy correctly and produced an unobservable system.** The receipts that *were* built satisfy the one thing doctrine does demand — "authenticated Arcane receipts" — and receipts answer *"did this happen and was it authorized"*, never *"why did it fail"*.

Compounding it: receipts are written **only at the terminal transition**. A record that exists only after an event has ended cannot, by construction, explain a stall. Of 242 GC receipts, 223 report `reclaimedBytes: 0` and 19 report real reclaims totalling 7.57 GB — but **not one** records when GC began, how long it held the admission lock, or which requests were blocked behind it. The receipts prove GC ran; they cannot tell you what it cost.

### 0.2 Fix F0 — amend the doctrine before amending the code

Add to `docs/agent-rules/workspace.md`:

> Instrumentation is not "separate proof". Any long-running service, queue, scheduler, or process supervisor must emit a structured lifecycle event log as part of its first delivery. A service whose failures cannot be diagnosed from its own output is incomplete, not minimal.

Without F0, the next system built here repeats this exactly. **F0 is the highest-priority item in this document** and it is a text edit.

**Attempted and blocked 2026-08-22.** `docs/agent-rules/workspace.md` is at **6 137 of its
6 144-byte cap and 60 of its 60-line cap — seven bytes of headroom**. The rule cannot be added
without displacing an existing one, and which rule to drop is Adrian's decision, not an
agent's. The attempt was reverted and `manage.py check` is clean. The finding is sharper than
the fix: **the doctrine's own size budget is what prevents the doctrine fix**, so F0 is not a
text edit as this document claimed — it is a trade. The wording drafted, for whoever makes the trade: *ship a lifecycle event log with every
long-running service as delivery, never as separate proof.* The broker's own missing RUNNING
heartbeat (P27) is the code half and is open regardless.

---

## 1. Goal: one shared cache, not many

**Observed:** 28 GB under `/Volumes/D/.rightkit-managed`, of which 22 GB is `rightkit-build-control/workspaces/` (41 namespaces) and 5.7 GB is `global/` (2.3 GB `cargo-home`, 3.4 GB `compiler-cache`).

The registry and the compiler cache are already correctly shared and global. **All duplication is in the per-namespace artifact trees.** There are five independent multipliers, and they compose.

### 1.1 Defect — two artifact trees per namespace

`index.mjs:68` sets both:

```
CARGO_TARGET_DIR        = <ns>/targets/<triple>/<profileKey>
CARGO_BUILD_BUILD_DIR   = <ns>/build
```

`CARGO_BUILD_BUILD_DIR` is Cargo's newer intermediate-artifact split; `CARGO_TARGET_DIR` holds final artifacts. Setting both makes one build populate two trees.

Measured on namespace `60b39b18…`:

```
build/     7.7 GB
targets/   102 MB
```

**F1.1 — WITHDRAWN.** The original text claimed this was ~1.9× duplication. **It is not duplication.** Re-inspection of the namespace shows `targets/<triple>/<key>/{debug,<triple>}` holding final artifacts (102 MB) and `build/debug/{deps,incremental,build,…}` holding intermediates (7.7 GB) — that is Cargo's target-dir/build-dir split *working as designed*. Removing `CARGO_BUILD_BUILD_DIR` would **relocate** 7.7 GB into `target/`, not eliminate it. The multiplier claimed here was fabricated by inferring duplication from the existence of two trees without inspecting their contents. Benchmark before touching this layout; there is currently no evidence it wastes anything.

### 1.2 Defect — host tree and explicit-target tree coexist

Inside the same namespace's `build/`:

```
build/debug                    5.8 GB
build/aarch64-apple-darwin     1.9 GB
```

Cargo writes to `<dir>/debug` when no `--target` is passed and `<dir>/<triple>/debug` when one is. Both happen in this workspace — 7.7 GB of near-identical artifacts for one crate graph.

**Fix F1.2** — the broker injects `--target <defaults.targetTriple>` into every managed Cargo invocation that does not already carry one. `compilationTarget()` at `index.mjs:277` already parses and validates the flag; this extends it from validation to normalisation. Every build then lands in exactly one tree.
**Risk:** passing `--target` explicitly disables build-script/proc-macro artifact sharing with the host, so the first build after this change is a full rebuild and proc-macros compile separately. This is a one-time cost and the steady state is smaller. It must be measured, not assumed.

### 1.3 Defect — namespace keyed on git checkout, not Cargo workspace

`build-control-core/src/index.mjs:112`:

```js
namespace = sha256([repositoryIdentity, toolchain, target, buildProfile])
```

where `repositoryIdentity` is the **git checkout root** (`resolveRequestRoots`, `index.mjs:260`). Consequences:

- One git repo containing several independent Cargo workspaces shares one namespace and one writer lease — they serialise against each other for no reason.
- One Cargo workspace spanning two git repos gets two namespaces and rebuilds twice.

Measured HeardRight fragmentation:

```
18.21 GiB across 18 namespaces for one product
  7.79 GiB  heardright
  5.33 GiB  heardright/tauri-app-next/src-tauri
  3.97 GiB  heardright/tauri-app-next/heardright-engine
  0.85 GiB  heardright/tauri-app-next/heardright_platform
  0.26 GiB  heardright/tauri-app-next/heardright_core
```

Only `/Volumes/D/claude/heardright` contains a `.git`; the subdirectories do not. So the sub-crate identities are **legacy** — created by a runtime that used the execution cwd as identity, which the current source explicitly notes it replaced ("The three roots the broker previously collapsed into `request.workspace`", `index.mjs:254`). Identity timeline confirms the cutover:

```
heardright/tauri-app-next/*   Aug 13 17:29 .. Aug 14 10:08   (stops)
heardright                    Aug 13 21:29 .. Aug 14 17:22   (continues)
```

**~10.4 GiB is stranded**: no future request will ever resolve to those identities, so nothing will reuse those trees. The identity fix shipped without a migration or reclaim step.

**Fix F1.3** — key the namespace on the **Cargo workspace root**, resolved from the manifest (`--manifest-path` when present, else the cwd's nearest `Cargo.toml` with `[workspace]`), not the git root. This is the unit Cargo itself shares a target directory across, which makes it the only correct namespace unit. Keep git identity as a *label* in receipts for traceability.
**Plus F1.3b** — a one-time reclaim for identities that no longer resolve. Not automatic deletion: a `rightkit gc --stranded --dry-run` that lists them and requires an explicit confirm.

### 1.4 Defect — incremental compilation left on for one-shot runs

Live process table shows `-C incremental=<ns>/build/debug/incremental` on managed `rustc` invocations. Incremental state is large, is worthless for a CI-style or agent-driven one-shot run, and pollutes the very trees being measured.

**Fix F1.4** — set `CARGO_INCREMENTAL=0` in `executionEnvironment` for `test`, `check`, `clippy`, and any `--release` build; leave it on only for a developer-interactive profile that agents do not use. sccache already covers cross-run reuse and does it better, machine-wide.
**Note:** sccache and `-C incremental` are partially incompatible — sccache declines to cache incremental compilations. So today's configuration is paying for incremental state *and* losing sccache hits on the same crates. F1.4 fixes both.

### 1.5 Defect — non-build commands mint permanent namespaces

`mkdirMany(directoryRoots(entry.roots))` at `index.mjs:146` runs for **every** admitted request, before build-capability is consulted. `cargo --version` in a throwaway directory creates a full namespace tree that persists.

Evidence: of 41 namespaces, **20 belong to disposable workspaces** — 14 `right-release-tool-probe-*` directories under `/Volumes/D/.build-home/adrdsouza/tmp/` that no longer exist, plus `fixture-rust`, `health-workspace`, `live-identity-fixture-*`, `/private/tmp/ork-ws`. Eight workspace directories are 0 bytes. These are permanently unreclaimable by the current GC because the source directory is gone and only 7-day retention can touch them.

**Fix F1.5** — create only `temp` and `cargoHome` for non-build-capable requests. Defer the artifact-tree `mkdir` to the point where a build-capable request has acquired its permit.

### 1.6 Summary of section 1

| Multiplier | Factor | Removed by |
|---|---|---|
| Dual target/build trees | ~1.9× | F1.1 |
| Host tree + explicit-target tree | ~2.3× | F1.2 |
| Sub-crate identity fragmentation | up to 5× per product | F1.3 |
| Incremental state | 15–30% | F1.4 |
| Probe namespaces | 20 of 41 namespaces | F1.5 |

These compose. A realistic post-fix steady state for the current 12 real repositories is **6–9 GB**, against 22 GB today. That figure is an estimate and must be verified by measurement after F1.1–F1.5, not accepted on this document's authority.

---

## 2. Goal: storage accounting that actually accounts

### 2.1 Defect — growth tracking is dead code

`index.mjs:193` declares `let growth = 0;`. It is **never assigned again**. `index.mjs:208` returns it as `actualGrowthBytes`. `initialFree` is computed at `:194`, re-read at `:200`, and never used for anything.

Therefore `#terminal()` (`index.mjs:72`) always adds 0 to `usedBytes`.

Evidence: 343 of 355 terminal entries carry `actualGrowthBytes: 0`. The 12 non-zero values (up to 2.44 GB) are from an **older runtime** — this is a regression, not a missing feature.

### 2.2 Defect — the fallback measurement can never complete

`#measureUsedBytes` (`index.mjs:104`) scans the whole managed tree bounded to `maxEntries: 20_000, deadlineMs: 1_000`. Against a 28 GB tree it returns `complete: false` every time, and `#used` is only updated when `complete` is true.

Evidence: `usedBytes` is frozen at **9 738 181 741** (9.07 GiB) across **469** reconcile receipts and **242** GC receipts, while the tree holds 28 GB. Sampled GC receipts show `scannedEntries` of 3 875–12 289 against a `maxEntries` of 20 000 — the **1-second deadline**, not the entry cap, is the binding constraint.

**Consequence:** `quotaWatermarkBytes` (160 GiB) can never be reached, so **pressure** GC can never fire. Retention GC does work: 19 of 242 GC receipts reclaimed real space, 7.57 GB in total, including one 1.5 GB event — and two of those receipts report a real reclaim *alongside* the frozen `usedBytes: 9738181741`, which is direct proof that reclamation and accounting are decoupled. So the defect is precise: **GC reclaims, but the broker never learns that it did, and can never act on pressure.** Do not read this section as "GC is inert."

**Fix F2.1** — replace whole-tree scanning with an incremental **namespace size ledger**: one small JSON per namespace holding `{bytes, measuredAt}`, updated by a single bounded `du` of *that namespace only* after each build-capable release. `usedBytes` becomes the sum of ledger entries — O(namespaces), not O(files).
**Fix F2.2** — a `rightkit gc --reconcile` command that performs the full, unbounded, authoritative scan **off the request path**, on demand or on a timer when the machine is idle. Correctness and latency stop competing.
**Fix F2.3** — delete `growth`/`initialFree` rather than repair them. With F2.1 the per-request delta is redundant; leaving a dead variable that looks like accounting is worse than having none.

### 2.3 Defect — GC cannot reclaim what it does not remember

`gcApply` (`index.mjs:79`) iterates `gcCandidates()`, which is derived **entirely from in-memory entries**. A namespace directory whose entries have been deleted, or that predates the current state file, is invisible to GC forever. There is no directory-driven sweep.

**Fix F2.4** — GC enumerates `workspaces/` **from disk**, joins against state, and treats a directory with no live owner as a candidate. Directory-driven, not entry-driven.

### 2.4 Defect — `temp` is outside the tree GC deletes

`roots.temp` is `<base>/tmp/<ns[0:16]>` (`build-control-core/src/index.mjs:121`) — a sibling of `workspaces/`, not a child of `roots.root`. `gcApply:95` removes only `candidate.roots.root`.

Evidence: **54 tmp directories against 41 namespaces** — 13 already leaked.

**Fix F2.5** — either move `temp` under `roots.root`, or extend the GC removal to `[roots.root, roots.temp]`. Moving it is cleaner and makes containment checks uniform.

### 2.5 Defect — free-space floor set to a quarter of the volume

`config.json` sets `freeFloorBytes: 236223201280` = **220 GiB** on a 927 GiB volume with 689 GiB free. When free space crosses that line, `sample()` sets `boundedOut` and terminates the build tree as `reservation-overrun`.

Evidence: 4 such kills, three of them HeardRight (`check`, `check --bin heardright_next_bin`, `about generate`) on Aug 13.

**Fix F2.6** — set the floor to a real safety margin (20–30 GiB) and make it a percentage-based derivation with an absolute floor, not a hand-entered byte constant. A build must be killed because the disk is genuinely about to fill, not because a quarter of it is in use.

### 2.6 Defect — configuration keys are silently ignored

`config.json` sets `bounds.pollIntervalMs: 1000`. The broker reads `diskSampleIntervalMs`, `heartbeatIntervalMs`, `processGraceMs`, `gcIntervalMs` — **none of which appear in the config at all**. `pollIntervalMs` matches nothing and is discarded without complaint, and all four real bounds run on defaults that were never chosen.

**Fix F2.7** — validate the config against a schema at startup and **fail to boot** on unknown keys. A managed-build broker that silently ignores its own configuration is not managed.

---

## 3. Goal: agents check system resources before launching builds

### 3.1 What exists

`MachineFifoScheduler` (`scheduler.mjs:6`) is a single machine-wide FIFO lane, one active build, no priority, no deadline, no reservation, and — critically — **no awareness of machine resources at all**. It admits by position in a queue, never by whether the machine can take the work.

It is doing its job right now: membrane's `cargo test` is ACTIVE, orthic's is QUEUED behind it. That is the design working. The design is the problem.

### 3.2 Defect — cheap probes contend with full builds

`isBuildCapableRust` (`scheduler.mjs:42`) is an **inverted allowlist**: everything not in `[--version, -V, version, metadata, locate-project, read-manifest, search, help]` takes the heavy lane. That wrongly includes `fmt`, `tree`, `about`, `clean`, `update`, `vendor`, `generate-lockfile`, `--list`, and every unknown external subcommand.

Evidence — subcommand distribution across 407 requests:

```
metadata 189   (cheap, correctly classified)
-vV/-V   111   (cheap, correctly classified)
about     28   ← heavy lane, wrongly
build     24
check     20
test      16
fmt        8   ← heavy lane, wrongly
target     5   ← heavy lane, wrongly
toolchain  2   ← heavy lane, wrongly
generate-lockfile 2  ← heavy lane, wrongly
clippy     1
```

45 of 407 requests took the machine-wide build lane without needing it. Two are recorded as `CANCELLED/disconnect` after waiting: `cargo -vV` at 17:12:50 and `cargo about --version` at 17:22:54. `coderight/docs/agent-rules.md` instructs agents to run `rightkit cargo fmt --check` — which today queues behind LTO builds.

**Fix F3.1** — invert the classification. Maintain an explicit **heavy set** (`build, test, bench, check, clippy, doc, rustc, rustdoc, install, nextest`), treat everything else as light, and require an explicit policy entry to add a subcommand to the heavy set. Unknown subcommands default to light. Failure mode changes from "cheap probe blocks the machine" to "an unrecognised heavy command runs unmetered once, and is then classified" — a strictly better failure.

### 3.3 Defect — admission ignores CPU, memory, and thermal state

This is Adrian's stated requirement and **nothing in the current design addresses it**. Concurrency is 1 because that is the only setting the scheduler has, not because 1 is the right number. A `cargo check` on a small crate and a Tauri release LTO link are given identical treatment.

**Fix F3.2 — resource-aware admission.** Replace the boolean lane with a token-bucket admission controller:

- Each request declares a **weight** derived from its class: light = 0, `check`/`clippy`/`fmt` = 1, `test` = 2, `build` = 3, `build --release` = 4.
- The broker samples real machine state on a fixed interval: 1-minute load average, free + inactive memory, `sysctl hw.ncpu`, and (macOS) thermal pressure.
- Capacity = `max(1, floor(ncpu / 4))` tokens, reduced to 1 when free memory is below a floor or load average exceeds `ncpu`, and to 0 (drain) under serious thermal pressure.
- A request is admitted when its weight fits the currently free tokens; otherwise it queues **with its measured position and an estimate**, which the existing HEARTBEAT channel already carries.

This gives Adrian the actual goal — five agents cannot start five builds — while allowing a `cargo check` and a `cargo fmt` to proceed alongside one compile instead of serialising behind it.

**Fix F3.3 — per-request `-j` derivation.** Set `CARGO_BUILD_JOBS` from the tokens granted rather than letting every build default to all cores. Today a single admitted build saturates the machine regardless of what else is queued; that is why the Air becomes unusable.

**Fix F3.4 — priority and deadline.** Two classes: `interactive` (a human is blocked) preempts `batch` in queue order but never mid-build. A queued request past its deadline emits a `DEADLINE_EXCEEDED` event and, for batch work, is rejected with a clear reason rather than waiting forever.

### 3.4 Defect — client disconnect destroys queued work

`disconnect()` at `index.mjs:125-129` cancels the scheduler ticket and terminalises the entry as `CANCELLED/disconnect`, exit 125. A client that times out, or an agent whose session ends, discards work that had already waited in the queue.

Evidence: **9** terminations carry a `disconnect` reason — 7 recorded as `FAILED` by earlier runtimes, 2 as `CANCELLED` by the current one — including `cargo about --version` at 17:22:54 after a 5-minute wait and `cargo -vV` at 17:12:50.

**Fix F3.5** — separate **detach** from **cancel**. Disconnect detaches: the request keeps its queue position and runs to completion, with output written to the event log and the receipt retrievable by id. Explicit cancellation stays a distinct, deliberate operation. Add `rightkit status` and `rightkit attach <id>`.

### 3.5 Defect — ~45 MiB is read and SHA-256'd on every request, including every cheap probe

`index.mjs:133` verifies the tool digest unconditionally, on the hot path, **before** classification or admission:

```js
const file = await readFile(tool.path); if (digest(file) !== tool.sha256) throw new Error('registered tool digest mismatch');
```

`#verifiedOptionalTool` then does the same for `rustc` and `rustdoc`, and the sccache tool is digested at `:136`. Measured binary sizes:

```
cargo    31 867 672 B  (30.4 MiB)
rustdoc  12 021 256 B  (11.5 MiB)
rustc       412 504 B
```

≈ **45 MiB read and hashed per request**, with no mtime/inode/size memoisation. Across the 407 recorded requests that is roughly 18 GB of I/O and hashing — and **300 of those requests were `cargo metadata` or `--version`**, the very traffic section 3 redesigns the scheduler to make cheap. A token-bucket admission controller does nothing about this; it is a fixed per-request tax that dwarfs the queueing overhead for light work.

**Fix F3.6** — cache the digest keyed on `(path, size, mtimeNs, inode)`, re-verifying in full only when any component changes. Security posture is preserved: an attacker who can rewrite the binary without changing size, mtime **and** inode already owns the machine. Re-verify in full on broker start and on an interval regardless of the cache.

*(This defect was found by adversarial review, not by the original analysis.)*

---

## 4. Goal: no orphaned processes — and why they orphan

### 4.1 Root cause, measured

The live process table shows:

```
sccache server   pid 67769   PPID 1   PGID 67768   SESS 0
build launcher   pid  2564   PGID 2564
sccache clients  pid 33089, 33090   PGID 2564  (inside the build's group)
```

`spawnManagedProcess` (`process-tree.mjs:6`) launches with `detached: true`, so the launcher becomes a process-group leader and Cargo and its `rustc` children inherit that group. `terminateProcessTree` signals `-pgid`. That correctly reaps everything **in the group**.

The sccache **server** is not in the group. It daemonises to `PPID 1` in its own session and group, and it **outlives every request**. On a cache miss, the server — not the client — spawns the compiler. Those compiler processes are children of the server, in the server's group, and are therefore **structurally unreachable by the broker's group kill**.

That is the proposed mechanism behind "`tokenizers` and `image` `rustc` descendants survived because process-tree cleanup failed": the cleanup did not fail, it was aimed at a group that never contained those processes.

**Confidence: PLAUSIBLE, not CONFIRMED.** The code facts are confirmed — `detached: true` group leadership and `-pgid`-only signalling are in the source, and the process topology above was observed live. What is *not* directly observed is an sccache server with `rustc` children at the moment of a kill; the snapshot showed the server idle with no children, and adversarial review could not re-derive the topology after the server exited. The reasoning relies on sccache's documented architecture (client forwards to a persistent daemon; the daemon executes the compiler on a miss).

**Proof step before implementing F4.1:** start a cold-cache build, and while `rustc` is running, record `ps -o pid,ppid,pgid,sess` for every `rustc`. If their PGID is the launcher's, this diagnosis is wrong and the orphan has another cause. **Do this first — F4.1 and F4.2 both depend on it.**

`SccacheSupervisor` compounds it (`index.mjs:147`): it is constructed **once**, from the first request's environment, and `ensureStarted` memoises. Every later request's `SCCACHE_DIR` is silently ignored, and nothing in the broker owns the server's lifecycle, records its pid, or reaps it.

**Fix F4.1** — the broker owns the sccache server explicitly: start it itself at boot with a known pid recorded in durable state, in a group the broker can signal; verify on each request that the running server matches the expected configuration; stop it on shutdown. The server's group joins the reap set for a cancelled build.
**Fix F4.2** — if F4.1 proves impractical (sccache's daemonisation may resist supervision), the fallback is `SCCACHE_NO_DAEMON=1` or dropping `RUSTC_WRAPPER` for cancellable builds, accepting lower cache hit rates for reliable termination. **This trade-off is Adrian's call, not a technical default.**

### 4.2 Defect — an unreapable tree wedges the whole machine

`assertReaped` (`index.mjs:220`) throws `RK_PROCESS_NOT_REAPED`. The handler at `:160` sets `holdPermit = true`, which makes the `finally` at `:163` skip `permit?.release()`, and the entry is **never terminalised** — it stays `ACTIVE`, holding both its writer lease and the single global scheduler permit, until the broker restarts.

This is the FIFO deadlock the adversarial audit flagged, and **it fired**: Sol's 17:09–17:13 entry — "killing processes did not release durable ACTIVE lease, forcing broker restart" — is this code path. It is not theoretical and it is not fixed in v1.14.

**Fix F4.3** — an unreapable tree quarantines **its namespace**, never the machine. Terminalise the entry as `FAILED/reap-pending`, release the scheduler permit, retain the writer lease for that one namespace, and record the surviving pids in the event log so a subsequent sweep or a human can act. One stuck build must never stop every other repository.

### 4.3 Defect — no orphan sweep at any point

`reapPersistedProcessTree` runs only during `#load()` and only for entries persisted as `ACTIVE`. A process orphaned by a crash before persistence, or by the sccache path in 4.1, is never seen again by anything.

**Fix F4.4** — a startup and periodic **orphan sweep**: enumerate processes whose executable path or environment points into `/Volumes/D/.rightkit-managed`, cross-reference against live entries, and reap those with no owner. Report every reap to the event log. Age threshold configurable; default 30 minutes.
**Fix F4.5** — `rightkit doctor` reports orphans, stale leases, stranded namespaces, ledger drift, and sccache server state in one read-only command. Today diagnosing any of this requires reading `state/broker.json` by hand.

---

## 5. Goal: no unnecessary `cargo test`

**This goal's premise is only partly supported by the evidence, and the real waste is elsewhere.** Across 407 requests:

```
cargo test        16 invocations total
  coderight 5, heardright 3, heardright_platform 3, orthic 2, others 1 each
  exact-duplicate commands: 1 (coderight ran an identical test command 4×)
  redundant runs attributable to repetition: 3
```

16 test runs across two days is not the problem. The measured excess is:

```
cargo metadata   189 invocations  (46% of all requests)
cargo about       28 invocations
cargo build       24 invocations
```

`cargo metadata` at 189 calls is Tauri and `cargo-about` re-interrogating the same unchanged manifests. `cargo about` at 28 calls is the "legal notices rerun for 5–10 minutes on every retry" item — regenerating identical license output from an unchanged dependency set.

**Fix F5.1 — content-addressed result cache for derivable outputs.** Key on `sha256(Cargo.lock + manifest set + toolchain + relevant features)`. On hit, replay the stored stdout and exit 0 without spawning. Apply to `cargo metadata` and `cargo about` first — both are pure functions of the manifest set, both are hot, and both are safe to cache. Estimated elimination: ~200 of 407 requests.
**Fix F5.2 — test-result cache, second priority.** Same key plus a hash of the source tree; on hit, skip the run. Correctness risk is real (env-dependent tests, time, network, filesystem state), so this must be opt-in per repository via `.rightkit.json` and must be bypassable with `--no-cache`. **Do not build F5.2 before F5.1** — it carries more risk for a fraction of the benefit.
**Fix F5.3 — policy, not machinery.** The workspace rules should direct agents to `cargo check`/`clippy` during iteration and reserve `cargo test` for pre-commit and pre-release gates. Three redundant runs is a behaviour problem; it does not justify a caching subsystem on its own.

---

## 6. Goal: proper logs

### 6.1 The event log

**Fix F6.1** — append-only NDJSON at `<externalRoot>/logs/broker-<date>.ndjson`, one line per lifecycle transition, `fsync` on terminal events only (not on every line — that would add latency to the hot path).

Required events and fields:

| Event | Fields |
|---|---|
| `request.received` | `id, shim, argv, cwd, repositoryIdentity, namespace, clientPid, weight, class` |
| `request.classified` | `id, buildCapable, weight, reason` |
| `queue.enter` / `queue.position` / `queue.exit` | `id, position, waitedMs, capacityTokens, freeTokens` |
| `resource.sample` | `loadAvg, freeMemBytes, ncpu, thermalPressure, freeDiskBytes` |
| `admit` | `id, tokensGranted, jobs, admittedAt` |
| `process.spawn` | `id, pid, pgid, command` |
| `process.exit` | `id, pid, exitCode, signal, durationMs` |
| `process.terminate` | `id, pid, signal, escalated, reaped, survivingPids` |
| `sccache.state` | `serverPid, started, stats delta` |
| `gc.start` / `gc.end` | `trigger, candidates, scannedNamespaces, reclaimedBytes, durationMs, blockedRequestIds` |
| `error` | `id, phase, message, stack` |

### 6.2 Failure receipts must carry the failure

**Fix F6.2** — every `FAILED` and `CANCELLED` receipt gains `exitCode`, `signal`, `durationMs`, `queueWaitMs`, and the **last 8 KB of stderr**. Today stdout and stderr are streamed to the client socket (`index.mjs:202`) and then discarded — the diagnostic data already exists in memory and is deliberately thrown away.

This single field is what makes 16:13:18 and 16:57:00 diagnosable. Nothing else in this document does.

### 6.3 Retention and hygiene

**Fix F6.3** — rotate logs daily, retain 14 days, cap total log bytes. The log must never become the thing that fills the disk.
**Fix F6.4** — prune runtimes. `installManagedRuntime` (`runtime.mjs:13`) never removes old versions: **35 `runtime-*` directories** exist (plus the `runtime-current` symlink), including `r2` through `r17` of one version deployed in a single day, plus orphaned `runtime-source.*`, `verify.*`, and `verify-metadata.*` staging directories. Keep current + previous 2, delete the rest, and clean staging on both success and failure paths.
**Fix F6.4b** — ~~`runtime.mjs` has **no test file**~~ **Corrected 2026-08-22:** this claim was false. `packages/build-control-broker/src/runtime.test.mjs` exists and carries 14 tests (8 passing, 6 failing on the fixture defect in P-section item P-TESTS). The real finding is narrower and still stands: the install/rollback path is thinly covered and its failing fixtures mean the deployment machinery is not currently proven. An external review propagated the false version of this claim verbatim, which is what surfaced it — a documentation error becomes an analysis error one hop downstream. The atomic symlink switch, the health probe, and the rollback-on-failed-activation path — the exact machinery that produced those 35 directories, and the machinery through which *every fix in this document will be deployed* — is entirely uncovered. Cover it before wave 1 ships through it.
**Fix F6.5** — prune receipts. 1 187 receipt files for 407 entries; `state/broker.json` is 1 MB and is rewritten with `fsync` on **every** state transition, including all 189 `cargo metadata` calls. Move terminal entries to a rotating archive and keep the hot state file to live entries only.

---

## 7. Sequencing

Ordered by *"what makes the next failure diagnosable"* first, then blast radius.

| Wave | Items | Rationale |
|---|---|---|
| **0** | F0, F6.1, F6.2 | Without these, no later fix can be verified. Smallest change, largest information gain. |
| **1** | F4.1/F4.2, F4.3, F4.4, F2.6, F2.7 | The two confirmed incidents (orphaned trees, wedged lane) plus the config that kills healthy builds. |
| **2** | F3.1, F3.2, F3.3, F3.4, F3.5 | Resource-aware admission. Depends on wave 0 to prove the scheduler behaves. |
| **3** | F2.1–F2.5, F1.5 | Storage accounting and GC. Depends on the event log to prove GC does anything. |
| **4** | F1.1, F1.2, F1.4 | Cache-layout consolidation. Forces a full rebuild, so it lands when the system is otherwise stable. |
| **5** | F1.3, F1.3b | Namespace re-keying — the largest semantic change; strands the existing cache once more, deliberately and with a reclaim path. |
| **6** | F5.1, F5.3, then F5.2 | Result caching. Pure optimisation; last. |

---

## 8. Verification — every gate must be proven able to fail

A prior finding in this workspace: *a green run from a check that inspects nothing is worse than no check.* Two CodeRight gates were passing over nothing. The Node tests that certified this system — 104 at the time of the 2026-08-14 measurement, 252 in the broker package as of 2026-08-22 — exercise injected `spawn` and `statfs` doubles — doubles never daemonise, never orphan, never stall, never fill a disk. That test suite went green **because** it tested a world where every defect in this document is impossible.

Each fix therefore ships with a test that is **first demonstrated to fail against current `main`**:

| Fix | Test | Must fail today because |
|---|---|---|
| F2.1/F2.3 | After a real build, `usedBytes` increased by ≥ the measured namespace delta | `growth` is dead; would have caught this on day one |
| F2.2 | `--reconcile` on a >20 GB tree returns `complete: true` | 1 s deadline guarantees `false` |
| F2.4 | A namespace directory with no state entry is GC-eligible | GC is entry-driven |
| F2.5 | After GC, no `tmp/<ns>` remains for a deleted namespace | 13 leaked today |
| F3.1 | `cargo fmt --check` does not acquire the heavy lane | inverted allowlist |
| F3.2 | Under synthetic load ≥ ncpu, a second build queues | no resource input exists |
| F3.5 | Client disconnect during queue → request completes; receipt retrievable by id | disconnect cancels |
| F4.1 | Cancelling a build leaves zero `rustc` under the sccache server's group | server is out of group |
| F4.3 | An unreapable tree leaves the scheduler permit free and other namespaces admissible | `holdPermit` wedges the lane |
| F6.2 | A build failing with exit 101 produces a receipt containing `exitCode: 101` and stderr | `exitCode: null` on every pre-v1.14 failure |

**No fix in this document is accepted on the basis of a passing unit test alone.** Each wave closes with a live multi-repository contention run: three repositories, one heavy build, one `cargo check`, one `cargo fmt`, one client killed mid-queue, measured against the event log.

---

## 9. Open questions for Adrian

1. **F4.2** — if the sccache server cannot be brought under broker supervision, do you accept lower cache hit rates in exchange for reliably cancellable builds?
2. **F1.3** — re-keying namespaces strands the current 22 GB one final time. Land it in the same window as F1.1/F1.2 (one rebuild for all cache changes), or separately?
3. **F3.2** — target concurrency on the Air: is `max(1, ncpu/4)` right, or do you want a hard ceiling of 1 heavy build with light work allowed alongside?
4. **F5.2** — is opt-in test-result caching wanted at all, given the measured redundancy is 3 runs?

---

## 10. What this document does not fix

Named explicitly so the next attempt does not assume otherwise:

- The HeardRight build failures at 16:13:18 and 16:57:00. They are `execution-failure` with no recorded exit code, and as section 0 establishes, that label also covers external signals and the managed launcher's own 30-second startup abort — so **RightKit interference is not excluded**, only the disk-floor kill is. **F6.2 makes the next occurrence diagnosable; nothing here explains the last one.**
- `cargo-about --config` false positive, cleanup creating a forbidden local `target`, two parallel `cargo-about` writers, the Tauri panic on a vanished `cargo metadata` child. All HeardRight-side; RightKit changes cannot reach them.
- The absence of an installer. No fix here produces a DMG.

---

## 11. Adversarial review record

Reviewed 2026-08-14 by an independent Sonnet agent instructed to falsify, with access to the same primary sources. **Verdict: FAIL.** Corrections applied above:

| Review finding | Outcome |
|---|---|
| CRITICAL — "all 241 GC receipts report `reclaimedBytes: 0`" is false | **Upheld.** Re-measured: 242 receipts, 19 with nonzero reclaim, 7.57 GB total. The original claim generalised from a 5-receipt tail sample. Sections 0.1 and 2.2 corrected; the diagnosis narrowed from "GC is inert" to "GC reclaims but accounting never learns". |
| CRITICAL — "7 disconnect terminations" is false (reviewer counted 2) | **Rejected.** Re-measured: 7 `FAILED` + 2 `CANCELLED` entries carry a `disconnect` reason = 9. The reviewer counted only `CANCELLED`-state receipts and missed the 7 legacy `FAILED` ones. The original figure was correct and conservative; text now states 9 with the split. |
| MAJOR — "not a RightKit kill" overstates what `execution-failure` proves | **Upheld.** Section 0 and section 10 rewritten. `result.code === null` on signal close, and `managed-launcher.mjs` exits 125 on its own 30 s deadline; all collapse into one label. |
| MAJOR — per-request digest verification cost unexamined | **Upheld, new defect.** Added as 3.5 / F3.6: ~45 MiB read and hashed per request, 300 of 407 requests being cheap probes. |
| MAJOR — `runtime.mjs` install/rollback has no tests | **Upheld.** Added as F6.4b. |
| MINOR — 36 vs 35 runtime directories; "three" vs four defaulted bounds; live-system drift (408 entries, 30 GB) | **Upheld**, corrected. Drift is expected on a running system. |
| UNVERIFIABLE — sccache process-group escape could not be re-derived | **Upheld as a confidence downgrade.** Section 4.1 now marked PLAUSIBLE with a mandatory proof step before F4.1/F4.2. |

The review independently reproduced, exactly: the dead `growth` variable, zero logging calls, all log-file states to the byte, `exitCode: null` on pre-17:09 failures, the namespace formula, the dual target/build-dir assignment, the 7.7 G/102 M and 5.8 G/1.9 G splits, every config bound, the inverted allowlist, **every row** of the subcommand distribution, all five HeardRight per-component sizes to two decimals, the identity-cutover timeline to the second, and the `holdPermit` deadlock path.
