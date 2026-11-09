# Build

The one contract for how RightKit's gate and RustRight's broker build, from the reset plan (`docs/plans/2026-11-07-gate-broker-reset.md`). It replaces `docs/GATE-CONTRACT.md` and RustRight's `docs/BROKER-CONTRACT.md` at step 7 of that plan. Until every clause of those two files is listed here with its test, refusal code or a GAP marker, they stay operative for every clause this file does not list (plan section 6: an obligation without a test is listed, never deleted); deleting them is the last commit of step 7 and is required for the reset to be complete. Precedence: this file and Adrian's reset decisions (the plan's decision log and section 3) override every old clause they supersede; an unlisted old clause never reinstates what the reset removed: live handover, job replay, the snapshot carrier and its grants, broker capability negotiation, memory forecasts, priority cancellation, filesystem write confinement or build ownership. Rule for this file: a sentence enters only with its test name or refusal code in the same commit, or a GAP marker that names what is not yet enforced. An obligation without a test is listed here, never deleted. A check that exists today goes only in the commit where its replacement's test passes.

## 1. The requirement

Adrian's three lines (2026-10-28):

> rightkit releases from mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class a first, local compile, qualification on CI and auto publish from git
>
> private repo apps built on mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class a first, local compile, qualification held for dell or mac
>
> public repo apps built on mac, dell or linux / on dell and mac, via broker / on linux no broker / but in all cases single point of entry with class A first, local compile, qualification on CI. signed installers available

## 2. The interface

The same text is section 3 of the reset plan.

One entry: `rightkit-gate --to <check|compile|dev|test|package|qualify|release> --commit <sha>` with optional unit/scope selection. Gate owns selected-commit snapshot, Class A, row caches, test execution & qualification; broker owns machine resources, queue & process containment. Mac/Dell compile through broker; Linux runs directly with equivalent pinned output/environment. One persistent snapshot & warm target per app/host; unchanged source retains timestamps. Compilation uses release profile.

**Request:** gate spawns `rustright cargo <argv>` or `rustright run <adapter> -- <argv>` inside `<data root>/snapshots/<repo-key>`, with `RUSTRIGHT_GATE_COMMIT=<full sha>` & `RUSTRIGHT_CLASS_A=<absolute verdict path>`. Client owns wire framing. No gate-created nonce, carrier, grant, capability query or local signature verification. Broker maps snapshot to existing app target through adjacent `rightkit.snapshot-state/1` state: `{repo,repoKey,commit,tree}`.

**Class A:** gate atomically writes `rightkit.class-a-verdict/1` only after every planned row passes, including valid cache hits: `{commit,tree,repoKey,workspace,scope,status:"pass",scans:[{id,status:"pass"}]}`. Workspace is snapshot-relative (`.` at root); scope is full or sorted directories. Gate enforces exact/superset scope coverage & derives child-workspace verdicts from that same completed scope. Broker checks schema, exact commit/tree/repository/cwd bindings & nonempty unique passing scan ids at admission & before spawn. Missing/partial markers, missing/red/malformed/misbound verdicts refuse compile; observations/version probes are exempt. Class A & noncompiling Cargo reads run outside broker against pinned tools, using existing warm targets where needed.

**Lifetime:** gate acquires `${snapshot}.lock` before sync & holds it through queued/running jobs & tests: `{pid,identity,commit,token}`. Birth identities use `rustright-process`: `darwin:<pid>:<lstart>` under C locale, `linux:<pid>:<starttime>`, `win32:<pid>:<Unix ms>`. Broker rechecks lock before spawn & cancels on owner loss. Gate releases only after every row settles: its own job's durable terminal receipt that proves the process tree gone (`reap` REAPED with the signed `descendants` tree-contained, or the job never spawned), explicit pre-admission refusal, cache hit or no submission; a terminal receipt of an unreaped or forced-terminal job keeps the lock. Every stage records pending calls before submission & counts all outcomes, and so does an npm or node suite whose cargo goes through the runtime wrapper (cargo-target-shim.mjs) on a seam run; unreadable ledgers, killed clients, missing/malformed results, unmatched receipts & exceptions retain ownership. A dead owner's lock is reclaimed only after exact-owner `SNAPSHOT_STATUS` returns IDLE for unchanged state: no admission, queued/running/finalizing job, unreaped process or persisted writer. Unreachable/unknown never authorizes reclaim; live owners are never reclaimed.

**Reply:** exactly one stderr line `rustright-result {requestId,exit,signal,outputDir,receipt,receiptPersisted,code?,message?}`. Correlation stays present if submission may have happened. Paths are absolute or null. Executed commands retain actual exit/signal, including when broker postconditions fail; unexecuted refusals/client errors use 125/null & stable code plus one-line message. Success requires durable receipt. Gate compares receipt job id, admitted argv, cwd & adapter with its invocation, then judges postconditions before exit. Receipt carries transformations, output, timings, diagnostics, forwarded environment, test executables & `gate {commit,tree,snapshot,classA,snapshotUnchanged}`. Broker verifies snapshot tree once after process-tree shutdown; changed source fails `RK_SNAPSHOT_CHANGED`.

**Queue/tests/adapters:** one Cargo root per app; identical requests refuse, distinct requests queue; different apps share available resources. Priority orders waiting jobs without cancellation. Broker pins target, registered triples, release profile, `--keep-going` & test/bench `--no-run`; `test --doc` & nextest execution refuse. Gate runs only complete receipt-listed executables with matching path/hash. Missing/null lists fail; zero tests require an empty list. Serial rows reuse plain execution or digest-checked cached executables; failed/unsettled plain rows fail dependents. Missing/changed cached executables permit one fresh compile; E2E serial compile-only rows always reuse plain pass. Group caches preserve each member's outcome & feature equivalence; only actual invocation keys are written. Row keys include compilation inputs, excluding incidental transport paths & scheduling knobs. Brokered adapters pass nested Cargo through managed shims & parent admission; a compile an adapter starts uses that adapter's own target, a compile a Cargo job starts a fixed subdirectory of its target (rustright normalize_impl.rs nested_target); stage callers preserve verdict scope, targets & pending ledger. GAP: absolute compiler paths can bypass shims.

**Publication:** broker signs receipts; CI verifies enrolled host/runtime pins, job-id uniqueness, successful exit, dispatched commit/tree, unchanged snapshot, one snapshot/verdict directory & complete exact stage-command coverage. Paths compare using enrolled host semantics. Gate does not authenticate same-user files. Receipts bind work, not unit/stage labels or a particular door run: identical argv/cwd/commit/tree is reusable evidence. Public publication remains CI from git; private apps qualify locally before separate build-free R2 upload.

**Cutover:** pause admission, cancel queued work, stop running owned trees, record durable outcomes, prove processes gone, then start one broker over existing warm roots. Completion winning cancellation keeps its proven completed outcome. Rollback follows same sequence. Cancelled work requires explicit resubmission. No live handover, automatic job replay, priority cancellation or filesystem write confinement; process containment stays. Stable refusals include `RK_GATE_REQUEST`, `RK_CLASS_A_INVALID`, `RK_SNAPSHOT_CWD`, `RK_SNAPSHOT_STATE`, `RK_SNAPSHOT_LOCK`, `RK_SNAPSHOT_PATH`, `RK_GATE_COMMAND`, `RK_PROFILE_REFUSED`, `RK_APP_BUILD_IN_PROGRESS` & `RK_RECOVERY_PENDING`.

## 3. Invariants

### Gate

Each line names the test that holds it (`<file>`: the test's name) or the refusal code.

1. A Class A verdict exists only for a complete, green plan and binds commit, tree, repository, workspace and scope. `packages/release/gate-seam.test.mjs`: "the Class A verdict exists only for a complete green plan and binds commit, tree, repository and workspace"; "a verdict is bound to the scope its class A covered".
2. A seam call names the verdict of its own directory. `packages/release/seam-repairs.test.mjs`: "D1. a stage child's seam call names the verdict of its own directory".
3. Exactly one `rustright-result` line is the result; none or several fails the row (`NO_RESULT_LINE`, `AMBIGUOUS_RESULT_LINE`). `packages/release/gate-seam.test.mjs`: "exactly one rustright-result line is the result".
4. The receipt's postconditions are judged before the exit code; a killed client fails; an exit 0 passes only for a job the broker finished SUCCEEDED (`RK_SNAPSHOT_CHANGED`, `RECEIPT_MISMATCH`, `CLIENT_KILLED`, `RECEIPT_NOT_SUCCEEDED`). `packages/release/gate-seam.test.mjs`: "the receipt's postconditions are judged before the exit code".
5. A row is settled only by the terminal receipt of its own job that proves its process tree gone (complete reap and a contained tree, or never spawned), or by a coded refusal before admission. `packages/release/seam-repairs.test.mjs`: "2. settled only by the terminal receipt of this request's job".
6. The lock body is `{ pid, identity, commit, token }` in the broker's identity spelling; a dead holder is reclaimed only on an exact IDLE echo. `packages/release/gate-seam.test.mjs`: "the lock body is { pid, identity, commit, token }"; "only an exact SNAPSHOT_STATUS echo with state IDLE proves a dead holder's snapshot idle".
7. A stage child's seam call is pending in the stage ledger before it is sent; a pending row that cannot be written submits nothing (`RK_CARGO_LEDGER`); every ledger file counts and an unreadable record is unsettled. `packages/release/seam-repairs.test.mjs`: the three tests numbered 1.
8. A cached failure submits nothing and is settled; only a FAILED receipt's compile failure is cached; the lock waits for every seam run to count its rows, and for every process group a dead holder started (POSIX: `<lock>.children` group records, kept until the whole group is gone; Windows: the holder in a named job whose out-of-job holder answers fresh counts; an unreadable record, an unanswered count or a self-check OUT keeps a dead holder's lock busy; `snapshot-lock.test.mjs`). `seam-repairs.test.mjs`: "D4/D5", "NN41 on a broker host"; `packages/release/snapshot-lock.test.mjs`: "a dead holder's lock is not reclaimed while a process group it recorded still runs". `packages/release/seam-repairs.test.mjs`: "D4/D5".
9. The runtime cargo wrapper sends a suite's compile through the seam on a seam run. `packages/release/seam-repairs.test.mjs`: "D6".
10. A Swift build through the seam takes `--package-path .` and no gate scratch path. `packages/release/seam-repairs.test.mjs`: "3. a Swift build through the seam".
11. A seam adapter's receipt target is mapped to every Cargo workspace under its directory. `packages/release/seam-repairs.test.mjs`: the two tests numbered 4.
12. A test compile's executables run only from the receipt's list; an empty listing passes only against an empty list (`TEST_EXECUTABLE_UNSIGNED`, `TEST_EXECUTABLE_MISSING`). `packages/release/seam-repairs.test.mjs`: "D8 / zero list".
13. A serial test row never compiles twice: it reuses the plain row's executables, or a cached plain row's proven ones, and fails on a failed plain row (`TEST_DEPENDENCY_FAILED`). `packages/release/serial-row.test.mjs`: all eight tests.
14. A group's pass is stored under the group's key only and is a hit only with every member's own run. `packages/release/feature-partition.test.mjs`: "a test group's stored pass carries each member's own run"; "a pass projected from a group onto a member's own key is never read".
15. A group runs as one invocation only for members whose feature resolution it preserves. `packages/release/feature-partition.test.mjs`: "partitionByFeatures keeps the equivalent members together".
16. A row's key holds only what its tool reads. `packages/release/feature-partition.test.mjs`: "a row's inherited environment is keyed on what its tool reads (rightkit#1540)".
17. CI admits a seam qualification only from the signature against the enrolled pin and the signed gate binding, with every stage command covered and paths compared as the host compares them. `packages/release/qualification-verify.test.mjs`: the 26 tests.
18. The direct read wrapper passes a seam compile through and refuses a bare compile in the snapshot. `packages/release/gate-seam.test.mjs`: "the direct read wrapper passes a seam compile".

GAP: the gate's integration tests against the installed broker (plan section 7, steps 4 and 8); every line above is a test of the gate alone.

### Broker

These name prepared checks in RustRight (written by its owner), not passing executions.

| Invariant | Enforcement / check in RustRight |
|---|---|
| Every compile requires passing Class A bound to commit, tree, repository & cwd, or verified parent admission; probes are exempt. | `RESET-A`; `gate_admission::tests::class_a_requires_exact_schema_bindings_and_unique_passing_scans`; `RK_CLASS_A_INVALID` |
| State, verdict & live process-birth lock identity are checked at admission & before spawn; owner loss cancels work. | `snapshot_state_and_class_a_are_rechecked_before_spawn`, `verdict_repository_and_workspace_are_rechecked_before_spawn`, `snapshot_lock_rejects_replacement_wrong_commit_and_reused_pid`; `RESET-O` |
| Broker alone assigns CPU/memory & queue order; one Cargo root per app, identical requests refused, distinct requests queued; priority never cancels running work. | `P3`, `the_sum_of_charged_units_never_exceeds_the_compile_budget`, `the_live_memory_guard_keeps_the_reserve_free`; `RK_APP_BUILD_IN_PROGRESS`; `retired_priority_cancel_is_readable_but_cannot_be_requested` |
| Output stays in existing app target; release profile, registered targets & allowed paths are enforced. | `RESET-W`, `P2`; `RK_PROFILE_REFUSED`, `RK_TARGET_NOT_REGISTERED`, `RK_SNAPSHOT_PATH`, `RK_GATE_COMMAND`; `named_admission_path_cannot_follow_a_link` |
| Process containment stays; snapshot is verified once after complete job-tree shutdown. Changed source fails while actual child exit remains recorded. | `RESET-W`, `RESET-O`; `RK_SNAPSHOT_CHANGED`; `executed_command_keeps_actual_exit_and_signal_including_broker_postcondition_failure` |
| Broker compiles tests once with `--no-run`; executable evidence is complete or explicitly unavailable. | `RESET-W`; `chunk_boundaries_keep_every_executable`, `oversize_or_malformed_lines_never_produce_a_partial_executable_list`, `executable_cap_refuses_whole_list_while_diagnostic_cap_does_not_drop_executables`, `completed_empty_cargo_stream_is_an_empty_executable_list` |
| Restart cancels/settles work, proves owned processes stopped, preserves warm roots & requires explicit resubmission; completion winning cancellation keeps its outcome. | `RESET-C`; `pure_queued_or_started_requests_end_interrupted_after_restart`, `completion_winning_after_running_census_keeps_its_outcome_with_full_stop_proof`, `completion_race_requires_brokers_consistent_late_cancel_evidence` |
| Stale-lock proof includes admissions, queued/running/finalizing work, unreaped processes & old persisted owners; unknown is never idle. | `queued_running_finalizing_and_terminal_unreaped_work_block_snapshot_reclaim`, `persisted_writers_and_journals_are_scoped_and_unattributed_ownership_refuses`, `standing_old_record_blocks_even_without_claim_or_with_completion`; `RK_RECOVERY_PENDING` |
| Root CLI emits one result for validation, admission, transport & execution outcomes; possibly submitted work retains its id before ACK. | `root_submission_validation_failure_without_markers_emits_one_result`, `reset_validation_failure_emits_exactly_one_result_for_either_partial_marker`, `transport_failure_after_admission_retains_correlation`; `RK_RECEIPT_WRITE_FAILED` |
| Receipt signature covers retained gate/result fields; retired admission protocols cannot return. | `reset_receipt_verifies_without_retired_evidence_wrappers`, `reset_receipt_refuses_unsigned_changes_to_every_retained_field`, `retired_handover_and_carrier_requests_are_refused` |

Verification pending: RESET-A/W/C/O & migrated P1/P2/P3/recovery are prepared, not run. Installed installer cutover/rollback, two-app concurrency, adapters & gate-to-CLI acceptance remain required on each host independently. Existing mechanisms above are not GAPs; absent execution evidence is not permission to delete them.

### Kept from before the reset

The 42 GC-A27 non-negotiables are named checks of `packages/release/gate-acceptance.mjs` (`NN_ALL`), run on each host by `gate-acceptance.mjs --verify-release`, which `run-batch.sh` requires. GAP: plan section 6 moves them to `node --test` files (`packages/release/gate.test.mjs`); until then they stay where they are.

| Check | Rule |
|---|---|
| NN01 | An unchanged commit runs no Class A row |
| NN02 | An unchanged commit runs no Class B row |
| NN03 | One edited .rs file reruns only the rows that read it |
| NN04 | One edited manifest reruns only its workspace's rows |
| NN05 | Keys are stable across identical runs |
| NN06 | Only executing rows carry the environment digest |
| NN07 | No identical child process twice in one run |
| NN08 | rustfmt is one process per bucket; no cargo fmt |
| NN09 | Non-compiling cargo never goes through the broker |
| NN10 | Inside a snapshot the resident gate answers cargo metadata |
| NN11 | A warm run proves the snapshot from the resident index, without re-hashing it |
| NN12 | Installed dependencies are identified by pnpm's install record and reused |
| NN13 | Class A takes no exclusion of its own: one snapshot lock, no build-ownership lease |
| NN14 | Preflight and app rules run once per tree |
| NN15 | Advice scans are cached per lock set and UTC day or hour |
| NN16 | A unit list shares one Class A and runs no per-unit format check |
| NN17 | Class A lanes equal the host's cores; Class B batches compatible rows |
| NN18 | Warm time stays inside the host's budget |
| NN19 | The gate under test is the installed sealed gate |
| NN20 | One edited crate recompiles only that crate and its dependents |
| NN21 | One edited lockfile reruns only the rows that read it |
| NN22 | A warm snapshot sync writes and removes nothing |
| NN23 | A gate upgrade that changes no key-relevant code keeps every hit |
| NN24 | The cache survives the resident gate's restart and idle exit |
| NN25 | A warm run starts no git-after-proof, version probe, pnpm, process probe or real cargo |
| NN26 | The gate asks the resident gate over its socket, in process |
| NN27 | Never two compiler passes over one crate, target and feature set |
| NN28 | Change detection comes from RightKit's file index and watcher |
| NN29 | Per-run identities never enter a key or a reuse record |
| NN30 | The releasing host passes (per host) |
| NN31 | No gate is released or installed as trusted without a passing contract test |
| NN32 | A killed run's snapshot lock is reclaimed by the next run on both hosts |
| NN33 | The contract and the test match |
| NN34 | Missing evidence fails; a partial run never passes |
| NN35 | The verdict shows coverage, and a release needs all of it |
| NN36 | The test is locked |
| NN37 | The test's author is not the implementer |
| NN38 | Cross-target rows built with stand-ins are limited evidence |
| NN39 | The parallelism is real, not configured |
| NN40 | Docs build as one command per workspace and feature round, on every compile |
| NN41 | A deterministic failure is cached like a pass |
| NN42 | Cross targets of one stage go in one broker command naming every --target |

Signing and installation, enforced by refusal codes in `@rightkit/release`: `RK_SUITE_UNSIGNED_BUILD`, `RK_SUITE_ADHOC_SIGNING`, `RK_DEV_UNSIGNED_INSTALL`, `RK_DEV_WIN_UNSIGNED_INSTALL`, `RK_NATIVE_SIGNING_FILE_UNSUPPORTED`, `RK_ADAPTER_ENV_SIGNING`, `RK_ADAPTER_ENV_SIGNING_FILE`; credentials `RK_CRED_PROVIDER_*`; a host upload `RK_HOST_UPLOAD_REFUSED`.

Enforced, test mapping not yet listed here (GAP: evidence, not an unimplemented obligation): notarization (`macos-signing-session.mjs` `MAC_SIGNING_AGENT_PROTOCOL` refuses incomplete notarization or stapling evidence; `install-app.mjs` `spctl-notarized` refuses an app not accepted as Notarized Developer ID) and the explicit `distribution.provider` (`admission-files.mjs` provider validation; `android-play-upload.mjs` `PLAY_CONFIG_INVALID`) and the explicit tier on upload (`release-state.mjs` "upload tier is required (patch|update)"; `addon-command.mjs` `--tier patch|update`). GAP: "upload no unsigned or test artifact" (RightKit `AGENTS.md`, locked invariants) has no test name or refusal code listed here yet; it enters with its check.

## 4. Decisions

| Date | Decision | Commit |
|---|---|---|
| 2026-11-02 | `cargo test` compiles in the broker with `--no-run` and runs outside it. | rustright 06a7dfd |
| 2026-11-07 | Live broker handover is killed; a cutover stops the queue and restarts. | rustright 809e057 |
| 2026-11-08 | A group's pass is stored only by the invocation that ran it; nothing is bisected. | rightkit 49af57af8 |
| 2026-11-08 | The serial test row reuses the plain row's executables. | rightkit 7123763c2 |
| 2026-11-08 | A row's key holds only what its tool reads. | rightkit 9842b0243 |
| 2026-11-09 | The seam is the gate's only route to the broker on a snapshot; stage children use it too. | rightkit 583adb06d |
| 2026-11-09 | CI admits seam receipts from their signed gate binding; a seam receipt binds no unit, stage or run label beyond its command. | rightkit 40708cad4, ce0201965 |
