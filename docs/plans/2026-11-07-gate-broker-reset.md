# Gate and broker reset: target spec, interface, delete list

Draft for Adrian, 2026-11-07. Written by Legion from both contracts and both codebases as they stand (rightkit `67515ffb5`, rustright `0bbd1e3`). Nothing here is decided until Adrian says so. It replaces no contract yet.

## 1. The requirement

Adrian's three lines, unchanged since 2026-10-28 (GATE-CONTRACT §1):

> rightkit releases from mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class a first, local compile, qualification on CI and auto publish from git
>
> private repo apps built on mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class a first, local compile, qualification held for dell or mac
>
> public repo apps built on mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class A first, local compile, qualification on CI. signed installers available

Everything below exists to make those three lines true and nothing else.

## 2. Target spec (one page)

**Entry.** `rightkit-gate --to <check|compile|dev|test|package|qualify|release> --commit <sha> [--unit <id> | --units-file <f> | --changed]`. One command on every host. Nothing else builds.

**Snapshot.** The gate syncs the commit into `<data root>/snapshots/<repo-key>` by content, keeps timestamps of unchanged files, and verifies the tree id once after the sync. The checkout is never read by a build. One snapshot, one target directory, one warm cache per repository per host.

**Class A.** Runs first, in the gate's resident process, outside the broker, keyed per row on the inputs that decide the row. Red Class A starts no compile. The 43 non-negotiables of GC-A27 stay, as test names.

**Class B.** The gate plans one invocation per workspace, stage and feature round (`-p a -p b --keep-going`, every `--target` in one call). On the Mac and the Dell it runs each through `rustright cargo`. On Linux it runs cargo directly with `CARGO_TARGET_DIR` pinned to the workspace's one target.

**Broker.** Owns the machine. Decides how many cargo processes run and in what order. One cargo process per app at a time (Adrian, 2026-11-07: "One per app"; section 8), duplicates refused, different commands queued. Pins the target directory, the release profile and the host triple. Contains the job's process tree; no filesystem write confinement: the snapshot tree is verified once more after the job, and a job that changed it fails with its own refusal code (Adrian, 2026-11-08; section 8). Priority tiers reorder waiting jobs and never cancel a running one (Adrian, 2026-11-08; section 8). Runs `cargo test` with `--no-run`; the gate runs the binaries. Returns an exit code, the output directory and a receipt file. That is its whole job.

**Cutover.** No overlapping brokers, no handover, no job replay (Adrian, 2026-11-07: "If live broker handover is complex. Let’s kill it. I would rather have a working implementation where I have to kill the queue and restart then be stuck trying to do seamless handoff"). Installing a broker: pause admission, cancel every queued job and stop every running job tree the broker owns, record each as cancelled in its receipt, prove the old processes are gone, start the one new broker on the same warm target and cache. Cancelled work requires explicit resubmission; the gate does not automatically resume it. A rollback is the same procedure with the previous build. Process containment and clear failures stay. Implementation in the installed and source brokers: pending, not delivered.

**Trust.** The gate trusts the broker's receipt. The broker admits a compile on two checks and nothing else: the working directory lies under `<data root>/snapshots/`, and the request names a Class A verdict file the gate wrote for the same commit with status pass (Adrian, 2026-11-04: "class a outside broker, needs completion before admitting to broker. enforced by broker. class a enforved by gate"; GATE-CONTRACT line 36, broker item 24). Neither hashes what the other hashed, and nothing is signed: a same-user agent can read any key the gate holds, so a signature proves nothing against the only adversary there is. The guard against an agent bypassing the gate is the pre-tool hook on the agent host and the publish step's receipt check. The broker still signs its receipt (`attestation.rs`, key readable only by the broker's user on that host): the gate does not verify it locally, CI's publication admission does (`qualification-verify.mjs`, enrolled pins in `ci/rk-pipeline.json`), because an agent that is not on a managed host cannot read the key, and that is the one adversary a signature stops. One lease stays, in its smallest form: the gate takes a snapshot lock for the commit before the sync and releases it after the batch, so a second gate process cannot refresh the snapshot under a queued compile; the broker refuses a request whose snapshot is not locked for its commit.

**Tests.** Every gate test that touches the broker runs against the installed broker on the live data root with a real small commit, never a mock and never a test broker (Adrian, 2026-11-05; decided 2026-11-08, rightkit#1536). The broker's own tests keep the harness its parity crate already has, which starts the real broker in a temp root under `/Volumes/D/scratch` for seconds, so a broker bug is found before the cutover. One installed probe command runs after every cutover: wiring, service, OS process checks, warm repeat, cancellation, concurrency, owner death. No test needs the second machine.

**Publication.** Unchanged: RightKit through CI from git; private apps through local qualification then R2; public apps through CI with signed installers.

## 3. The interface

One request, one reply. Carried by `rustright cargo <argv>` with the working directory inside the snapshot. No carrier file, no nonce, no grant, no lease handle, no signature.

Request, as the client sends it:

| Field | Value |
|---|---|
| `cwd` | a workspace directory under `<data root>/snapshots/<repo-key>` |
| `argv` | the cargo argv as the gate planned it |
| `env` | the environment the gate constructed |
| `commit` | the sha, copied into the receipt for the record |
| `classA` | path of the gate's Class A verdict file for this commit (`{commit, tree, scans, status}`); the broker refuses a compile when it is absent, names another commit or is not a pass |

Reply:

| Field | Value |
|---|---|
| `requestId` | correlation |
| `exit`, `signal` | the cargo result |
| `outputDir` | the pinned target directory, absolute |
| `receipt` | path of a JSON file the broker wrote: request, transformations applied (`--target-dir`, `--profile release`, `--no-run`, `--keep-going`), timings, `testExecutables` for a test compile, the diagnostics path |

Refusals, the complete list: cwd not under the snapshots root; no Class A verdict file with status pass for the commit; a second identical command for the workspace; a path-valued option that leaves the snapshot or the output directory; `--target-dir`, `--profile` other than release, `--config`, `-Z`; `cargo test --doc`, `nextest`.

Removed from the interface: `callingGate`, `snapshot{path, expectedTree}`, `declaredInputs`, `ownership{handle}`, `primaryCwd`, the snapshot carrier and its sha256 descriptor, the attest nonce, stage grants and their ledgers, `SNAPSHOT_LEASE_STATUS`, `GRANT_RECEIPTS`, `QUALIFY`, `CONSUME_PREPARE` and `CONSUME_COMMIT`, signed `snapshotEvidence`, `stageEvidence` and `resultEvidence`, `freshness` cause evidence, `failureClass`, `nestedChildren`, broker capabilities for adapters. The broker digest pin stays where CI's publication admission reads it (`ci/rk-pipeline.json`).

Adapters (pnpm, node, xcodebuild, gradle, cmake, QA) go through the broker as the same request, with the tool's argv in place of cargo's. Their runs invoke cargo inside (a Tauri, Xcode or Gradle build step; today's `nestedChildren`), so the `rustright cargo` shim on the child's PATH submits each nested cargo as a NESTED job under the parent's admission, with its target a fixed subdirectory of the app's one target (`nested_target`, as today). Environment pins alone enforce nothing. No capability table, no adapter grammar, no signed `nestedChildren` evidence in the gate. A nested cargo reached by absolute path bypasses the shim: a listed gap, not silently accepted. On Linux the gate runs them directly with the same pinned environment.

## 4. Keep and delete, gate side (`packages/release`)

Signing, notarization, DMG, MSIX, NSIS, iOS, Android, updater, R2, ownership-catalog rules and release config are untouched by this plan. The list below is the gate-to-broker interface and the acceptance machinery only.

**Delete** (about 12,200 lines):

| Module | Lines | Why |
|---|---|---|
| `gate-acceptance.mjs` | 3,333 | becomes `node --test` files against the broker step 1 of section 7 settles; the NN ids are the test names |
| `build-ownership.mjs` | 1,578 | replaced by a per-repository lock file of about 100 lines (pid, start time, reclaim when dead) |
| `snapshot-broker.mjs` | 808 | carrier ledger, lease status reader |
| `snapshot-request.mjs` | 722 | carrier writer, receipt verifier |
| `provenance-verify.mjs` | 413 | signature, cause evidence, BR19 fields |
| `native-adapters.mjs`, `native-tool-planning.mjs`, `adapter-env.mjs`, `pnpm-adapter-grammar.mjs` | 896 | adapter grammar and capability negotiation; adapters send the one request shape |
| `admission-host.mjs`, `admission-files.mjs`, `qualification-verify.mjs`, `ci-compile-admission.mjs` | 945 | hosted admission re-verifying broker receipts |
| `freshness-census.mjs`, `build-helper-trace.mjs` | 756 | census is reported by the daily job from Cargo's JSON, not judged; trace replaced by the post-job snapshot tree check |
| `cargo-shapes.mjs`, `cargo-ledger.mjs`, `spawn-ledger.mjs`, `carrier-census.mjs`, `carried-cargo.mjs`, `carried-adapter-run.mjs`, `carried-cargo-run.mjs` | 875 | carrier plumbing |
| `broker-wire.mjs`, `broker-handshake.mjs`, `broker-admission.mjs`, `broker-pins.mjs` | 793 | handshake, capability query, digest pins |
| `legion-freeze-consumer.mjs`, `direct-origin.mjs`, `linux-qualifier.mjs`, `handle-revocation.mjs`, `owner-process-proof.mjs`, `ownership-recover.mjs` | 623 | proposed, never activated |
| `stage-ledger.mjs`, `compile-inputs.mjs`, `rustright-stage.mjs`, `candidate-build-receipt.mjs`, `direct-receipt.mjs` | 725 | declared inputs and receipt projections |
| `unit-list-compile.mjs`, `cargo-width.mjs`, `clippy-equivalence.mjs` | 687 | the grouped publisher scheduler, the `--cargo-jobs` matrix and the batch-equivalence prover (section 8) |
| `missApproval` and the `RK_GATE_MISS_APPROVAL` file (`bin/rightkit-gate.mjs`), the twelve `RK_GATE_*` benchmark switches | inside the gate | a cache miss is a cache miss, never an approval request (section 8) |

**Shrink:** `bin/rightkit-gate.mjs` (5,297) loses the carrier, ownership, capability and route-probe paths; estimate under 2,000 after. `deterministic-scans.mjs` (3,979) keeps every scan, loses the carried and brokerCarried branches.

**Keep as is:** `snapshot.mjs`, `snapshot-check.mjs`, `snapshot-identity.mjs`, `gate-serve.mjs`, `node-check-cache.mjs`, `dependency-identity.mjs`, `cargo-target-shim.mjs` (Linux output pinning), `cross-targets.mjs`, `affected-units.mjs`, `clippy-route.mjs`, `gate-coverage.mjs`, `broker-exception.mjs`, `broker-client.mjs` (the one spawn of `rustright cargo`), `stamp-relay.mjs`.

## 5. Keep and delete, broker side (`crates`)

`rustright-install` (19,381 lines), `rustright-state`, `rustright-cache` and `rustright-process` are not assessed here beyond what the interface touches.

**Delete** (about 9,000 lines):

| Module | Lines | Why |
|---|---|---|
| `job_service/snapshot.rs` | 2,172 | leases, completions, status protocol; replaced by one mutex per workspace path |
| `job_service/cargo_evidence.rs` | 1,528 | turns on Cargo's fingerprint log and classifies every rebuild cause; the daily warmth job reads Cargo's JSON instead. The `testExecutables` listing it also produces moves to `finish_impl.rs` (about 60 lines) |
| `job_service/check_admission.rs` | 1,200 | tree hashing with a temp index, declared-input digests, grant containment; replaced by the cwd check and the path-option check (about 150 lines) |
| `job_service/handover_impl.rs` | 1,007 | seamless handover at cutover (interface item 22), with cross-broker token adoption and progress-based transfer. Killed by Adrian, 2026-11-07 (section 8); section 2's cutover replaces it |
| `job_service/contained.rs`, `job_service/write_locations.rs` | 655 | Seatbelt filesystem write confinement; replaced by the post-job snapshot tree check (Adrian, 2026-11-08; rustright#234) |
| `job_service/cancel_impl.rs`, the preemption watch | about 100 | forecast-based preemption by cancel; priority tiers reorder waiting jobs only (Adrian, 2026-11-08; rustright#234) |
| `job_service/forecast.rs`, `forecast_query.rs` | 587 | memory forecast cohorts |
| `job_service/qualify_impl.rs` | 456 | `QUALIFY` is the gate's job |
| `job_service/snapshot_grants.rs` | 373 | grants and ledgers |
| `job_service/consume_impl.rs`, `bin/rustright-consume-*.rs` | 373+ | Legion consume transaction, never activated |
| `job_service/test_run.rs` | 199 | the two-pass test runner; `--no-run` is one line |
| `job_service/carrier_sweep.rs` | 156 | carrier files no longer exist |
| `job_service/attestation.rs`, protocol attestation and ES256 | 109+ | stays: CI's publication admission verifies the receipt signature against the enrolled pins; only the gate's local per-call verification goes |
| `job_service/build_history.rs`, `warmth.rs` | 111 | retired on 2026-11-02 already |
| `rustright-parity` scenarios that test the deleted protocol | part of 5,051 | keep `Lab`, `BrokerUnderTest`, `Client` |

**Keep:** `scheduler.rs` (per-workspace queue, duplicate refusal, token pool, priority tiers), `run_impl.rs`, `finish_impl.rs`, `journal_impl.rs`, `normalize_impl.rs` (target-dir, profile, keep-going, no-run transformations), the Windows Job Object containment, the watchdog, the installer's drain and rollback.

## 6. The contracts

Both contracts are replaced by one file, `rightkit/docs/BUILD.md`, of at most three pages:

1. The three-line requirement.
2. The interface of section 3, verbatim.
3. One list of invariants, each with the test name or refusal code that enforces it. An invariant with neither is not written.
4. A decision log: date, one line, the commit. No quotes, no relays, no history of what was superseded.

Rule for the file from then on: a new sentence enters only with its test name or refusal code in the same commit, or an explicit GAP marker that names what is not yet enforced; an obligation without a test is listed, never deleted. A check that exists today goes only in the commit where its replacement's test passes; a GAP marker covers obligations that have no implementation yet and never licenses removing one that has. The GC-A27 table survives as the test list in `packages/release/gate.test.mjs` and nowhere else.

## 7. Order of work

Each step is one agent, one repository, measured on the Mac before the Dell. No step starts until the previous one's tests pass against the broker step 1 settles.

1. **The inner loop, decided (rightkit#1536).** Gate tests: the installed broker on the live data root, a real small commit. Broker tests: the parity crate's existing harness in a temp root. After every cutover: the one installed probe command. Adrian, 2026-11-08: "Choose what you think is better shape"; the split was chosen because with handover retired a broker build reaches the live root only by cutover, and both real broker bugs of 2026-11-05/07 were found after install. No `--lab` flag is added to the broker.
2. **Broker: the new request.** Accept `cwd`, `argv`, `env`, `commit`; keep every old field optional and ignored for one release so the installed gate keeps working during the switch.
3. **Gate: send the new request, read the new reply.** Delete the carrier, ownership and verify modules of section 4 in the same change. Tests run against the broker step 1 settles.
4. **Gate: adapters run direct.** pnpm, node, QA, xcodebuild, gradle in the snapshot with pinned output roots.
5. **Broker: delete the old request path and section 5's modules.**
6. **Contracts: write `BUILD.md`, delete both contracts.**
7. **Install on the Mac, run the 43 tests, then the Dell.**

Done 2026-11-08 05:05 IST, gate side (rightkit 49af57af8, b0065b7c3, reworded for G28 with contract line GC-A18 and issue rightkit#1537; GC-A18 amended to match): the class B store rule (a row is stored only by the invocation that ran it; no grouped-pass projection, no bisection), the `--jobs` lookup fix, no cache-miss approval, the benchmark switches gone. Package tests 168 of 169 (one pre-existing ownership failure, untouched). Not yet run against the real target: that is the next approval.

Verified on the real target 2026-11-08 12:55Z (Adrian: "Approve the run"; batch 051b, checkout gate at fbdc0595d, four crates, Mac, installed broker): Class A 1168 checks, 1167 cached; Class B 119 rows, 97 pass, 1 fail, Class B wall clock 2 min 20 s. Store rule held: 86 grouped rows in 11 invocations, 44 cache files written, every group under its own `*:batch:*` key, no member key written with group attribution (`/Volumes/D/scratch/gate-ready/store-rule-check.py`). Two defects the run exposed, filed: the serial test row repeats the plain row's compile byte for byte and the broker refuses it `RK_APP_BUILD_IN_PROGRESS`, which the gate reads as a failed snapshot verification (rightkit#1539); 103 of 119 rows missed the cache with no input change because `PNPM_HOME`, `NPM_CONFIG_CACHE` and the broker socket and agent-bin paths key every cargo row (rightkit#1540). The first attempt (051a) was refused by preflight G28 for missing citations; the commits were reworded in place (fbdc0595d).

Review of step 1 (rightkit#1538, Codex chat on Adrian's instruction, 2026-11-08), three findings, all confirmed at the cited lines and fixed in one commit (GC-A18 amended to match): (1) a test group's cached pass dropped each member's own run, so a member rejected cold for a SKIP marker could be accepted warm: the group row now stores `memberRuns` and is a hit only with every member's own passing run (`storedPassRow`, `usableStoredPass`); (2) passes projected onto member keys before the store rule stayed readable: a stored row carrying `batchedWith`/`batchCommand` is never read, and the class B key schema is `/3`, leaving every `/2` pass behind (the target directory stays warm); (3) grouping ran ahead of the feature-equivalence proof section 8 required: the planner now resolves each member alone and the group with `cargo tree` (`feature-partition.mjs`), keeps only the members whose own resolution the group preserves, partitions the rest, and names each split in the notes; the partition is cached on the manifests and the lock. Measured on the Mac against the real workspace (read-only `rustright cargo tree`, no compile): the four 051b crates partition in 2.2 s into {color, contrast, decimal} and {theme} (`proc-macro2` resolves `{proc-macro}` for theme and `{default,proc-macro}` in the group). Package tests: 175, 174 pass, 1 fail (`ownership-auto-recover`, pre-existing, unrelated). Not yet verified by a gate run; the cold/warm agreement on the real target is the next approved run.

ETA: unmeasured. The first step is settling the inner loop (step 1), and a measured run of it sets the rest.

## 8. What the 2026-11-07 gate-chat reports add

Read after the draft above: `/Volumes/D/scratch/static-audit/` (chat-accounting, f-why-not-caught, bcd-keys-verdicts, e-switches, a-feature-conflicts), sent by the gate-owner chat on Adrian's instruction. They do not change sections 2 to 7. They add one finding and three deletions.

**The cache layer is wrong in both directions.** Misses: keys carry the whole scan environment (`RK_SNAPSHOT_TARGETS`, `CARGO_TARGET_DIR`, `--jobs`, the executor prefix, the bytes of six carrier files, whole crate directories, the `.` and `crates/` tree ids), so a comment edit re-keys every class B row and the `--jobs` fix in 67515ffb does not work because the stored command still carries it. False hits: a failed grouped invocation stores per-crate passes under each member's own key, a bisected group's halves store passes the current run marks failed, and the next run flips red to green. Grouped invocations unify features: 296 conflicting pairs across 28 crates, so a grouped pass proves nothing about the crate alone. Twelve benchmark switches, two inert, none with a test, and `MERGE_FEATURE_ROWS` writes unified passes under plain keys.

This is the direct product of two contract lines: "every optimization measured is a non-negotiable" and "EVERY FUCKING THING SHOULD BE CACHED". Together they made batching mandatory before it was correct and made the cache store what the batch produced.

**Rule for the reset.** A row is stored under a key only when the invocation that produced it ran exactly that row's command. A grouped invocation stores one row, under the group's key, and a member is reported from it only when the member's resolved feature set in the group equals its own. Until the planner computes that from `cargo metadata` and splits groups on conflict, class B runs per crate. The warm target directory already shares every compiled dependency between invocations, so per-crate rows cost process start-up, not compile time. NN17 and NN40 wait for that planner; they are not deleted.

**Deleted with it.** The miss-approval mechanism: about 12 approval requests in 3 hours on 2026-11-06, most caused by the gate's own key edits, with 5 of 9 admitted wasted. A miss runs the row. The benchmark switch matrix. The grouped publisher scheduler.

**Process findings the reports make, restated once.** Tests used mocks shaped like the change, not the runner's real `project`, `issued` and key functions. Fixes went in one per real run, each run costing a release and an install. The gate was published and installed four times with the contract test failing 31 of 38. Section 2's tests rule and section 6's one-sentence-one-test rule are the answer to all three.

**From the broker-owner chat** (`/Volumes/D/scratch/brosef-handoff/`, agent-failure-log and par-bench-report, same instruction):

- **A locked clause was rewritten by an agent and cited back as the lock (E10).** Commit 43f2169a changed BROKER-CONTRACT item 20 from "builds of different workspaces, of one repository or of several, run side by side" to "parallel across repositories, not within one", on the strength of a one-line answer to a three-option question, then used the new text to refuse in-repository parallelism. Not reverted; Adrian's call. Under section 6 this cannot happen silently: a sentence with no test is not in the file, and a changed sentence changes a test name in the same commit.
- **The decision underneath it is a real contradiction, and Adrian has to pick.** One target directory per app (GC-A15, item 17) and workspaces of one app building side by side (item 20 as locked) cannot both hold: Cargo takes a lock on the build directory, so two workspaces sharing one target run one at a time whatever the broker admits. Either one target per app and one cargo process per app at a time, with parallelism inside that process, or one target per workspace and side-by-side workspaces. **Decided: one target per app** (Adrian, 2026-11-07, answering "One target per app, or one per workspace?": "One per app"). RightKit is one workspace since GC-T24, so for RightKit the two readings are the same thing, and HeardRight's three workspaces share dependencies that a second target would compile twice. Item 20's text therefore reads as 43f2169a left it, but as Adrian's decision of 2026-11-07, not the agent's edit of 2026-11-05; the broker owner records that in the broker contract.
- **"Max parallelization" is bounded by Cargo's own graph, not the broker.** Measured on the Mac: a 31-crate clippy batch used 1.3 to 4 of 12 cores; build CPU never passed 60 % in any run; a warm check of one crate used 11 % of one core. A warm `check` or `clippy` after a one-line edit does fingerprint checks and metadata-only compiles down a dependency chain (`rightkit-fs` has 112 dependents), so low CPU is the expected shape, not a broker defect. The levers that remain are the ones the draft already names: one cargo call per workspace and stage, the gate's 22 s gap between jobs (the broker's post-job maintenance gap is fixed in b0c19db), and nothing re-run that has not changed. Fifty timed runs of switch combinations cannot find parallelism that the graph does not have.
- **Installs are the inner loop today, and they collide.** The Mac broker was cut over four times under a running 2,376-row gate batch, the batch was lost three times, and the Mac broker then stayed frozen for about 12 hours. Two real broker bugs (a 20 s admission loop re-entering every 27 ms; a full maintenance `lstat` of 73k fingerprint directories after every job) were found only in production runs. Section 7 step 1 is what moves these to a test on one machine.

- **Revisions after the Codex chat's review (2026-11-07).** Three corrections against Adrian's typed words: broker-enforced Class A completion restored (2026-11-04); adapters stay brokered because their runs invoke cargo; handover stayed on his 2026-11-05 words until he retired handover on 2026-11-07 (next bullet). One withdrawal: the temp-root lab broker, against his 2026-11-05 words, awaiting his word (section 7 step 1).

- **Handover killed (Adrian, 2026-11-07 16:49Z, Codex chat "Investigate broker and release pipeline", verified with `pulse chat said`).** "If live broker handover is complex. Let’s kill it. I would rather have a working implementation where I have to kill the queue and restart then be stuck trying to do seamless handoff". Interface item 22 goes: no seamless transfer, no overlapping brokers, no progress-based handover, no cross-broker gateway or token adoption, no automatic replay of jobs as handover. Section 2's cutover is the replacement; `handover_impl.rs` returns to the delete list. The Codex chat records the decision in both contracts and the RustRight issue.

- **Confinement and preemption (Adrian, 2026-11-08, rustright#234).** He asked for the pros and cons of filesystem write confinement; the recommendation was to drop it, detect a changed snapshot instead, and let priority reorder without cancelling; his answer: "ok, go on". Filesystem write confinement goes on every host (Seatbelt on the Mac; Windows already applies none, broker item 19); the one tree verification runs again after each job and a changed snapshot fails the job. Process-tree containment stays. Preemption by cancel goes; the 2026-10-17 priority tiers reorder waiting jobs only. Discussed with chat astra at his request the same day; agreed on every item, these two were his to settle.

- **Inner loop (2026-11-08, rightkit#1536).** Adrian: "Choose what you think is better shape". Chosen: gate tests on the live root against the installed broker; broker tests on the parity crate's existing temp-root harness; one installed probe after each cutover. Section 2 (Tests) and section 7 step 1 carry it.

## 9. What this does not change

The requirement. Class A first. Commit-only builds from one warm snapshot. One target per app (decided 2026-11-07, section 8). Release profile only, no debug, no dev profile. Tests compile in the broker and run outside it. Every failure reported in one run. Linux direct. The broker alone decides count and order. Signing identity per product. Publication routes. The 43 non-negotiables.
