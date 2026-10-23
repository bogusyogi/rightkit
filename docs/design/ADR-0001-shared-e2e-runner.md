# ADR-0001: One RightKit runner for end-to-end proof. Build in the broker, run outside it.

```yaml
schema: architecture-decision.v1
decision_id: ADR-0001
record_worthiness: { hard_to_reverse: true, surprising_without_context: true, real_trade_off: true }
decision_status: proposed
revision: "rev 3, Sage sign-off conditions applied 2026-10-23"
realization_status: not_started
date: 2026-10-23T17:30:00+05:30
owner: RightKit (rightkit-qa execution; @rightkit/release admission)
decision_authority: Adrian (cutover dates, N-day rule, QA key distribution, Defender change); RightKit change gate (Sage + Oracle per change)
decision_question: >
  How does every app prove itself end-to-end through one RightKit owner, so that no app runs its own
  harness, compilation stays in the RustRight broker, tests run outside it, a full suite takes minutes
  not hours, and a release cannot ship without that proof?
alternatives:
  - { id: C-1, description: "Converge on right-release qa + rightkit-qa run; extend owners; retire the Node journeys orchestrator after equivalence", disposition: selected }
  - { id: C-2, description: "Node @rightkit/qa journeys as orchestrator over a Rust executor protocol", disposition: rejected }
  - { id: C-3, description: "Status quo harnesses plus flipping release admission from warn to refuse", disposition: rejected }
decision: C-1, as specified in the Decision section below
rationale: see Rationale
evidence_refs:
  # Committed under docs/design/adr-0001-evidence/ (with SHA256SUMS). Mac-only entries are not on the Dell; see Evidence retention.
  - adr-0001-evidence/test-arch-dell.md (committed; RK Dell Architect D2 frame, was D:\scratch\rk-plan\test-arch-dell.md; §2.4 carries the Sage correction)
  - adr-0001-evidence/e2e-speed-diagnosis.md (committed; rk Dell, sha 93ba0350; PostRight run 40f7ad68)
  - adr-0001-evidence/sage-verdict-adr-0001-rev2.md (committed; Sage C1-C11, rulings D-f and D-g; receipt ids in §7)
  - adr-0001-evidence/adr-0001-dispatch.md (committed; dispatch plan)
  - /Volumes/D/scratch/rk-plan/e2e-standard-amendment.md (Mac-only, not committed by rev 3; merged design)
  - /Volumes/D/scratch/rk-plan/test-arch-D2.md (Mac-only, not committed by rev 3; Architect D2 current-state frame)
  - /Volumes/D/scratch/test-inventory/REPORT.md (Mac-only, not committed by rev 3; 9-app test inventory)
  - /Volumes/D/scratch/rk-plan/astra-test-arch-D2.md (Mac-only, not committed by rev 3; adversarial review; its four critical findings are addressed below)
  - rightkit docs/E2E-TESTING-STANDARD.md (2026-10-21 standard, still "proposed")
consequences: see Consequences
residual_risks: see Residual risks
reversibility_and_exit: see Reversibility
review_triggers: see Review triggers
dependents: [all nine apps, @rightkit/release, rightkit-qa, @rightkit/qa, RustRight broker, Legion run close]
related_decisions: [rightkit#135 (broker test lane), rightkit#916 (Windows teardown latency), rightkit#931 (Windows CDP stall), rightkit#937 (release-switch presence rule)]
```

## Context

On 2026-10-21 Adrian asked for end-to-end journeys instead of thousands of small tests, owned by RightKit.
A standard was designed (`docs/E2E-TESTING-STANDARD.md`) and half built. It was never adopted. On
2026-10-23 builds stalled for hours, and every app inventoried its tests. Current state, cited from code:

| # | Fact | Evidence |
|---|---|---|
| 1 | Two QA systems that do not connect | `right-release qa` runs Rust `rightkit-qa run` (packages/release/release-qa-command.mjs:105) and writes an HMAC receipt. The Node journeys runner has an advisory gate that always exits 0 (packages/qa/src/journeys/gate.ts:137). It asserts `guard:'passed'` instead of checking it (run.ts:309), and the release pipeline never reads its receipts. |
| 2 | Release QA never blocks | `admit()` turns a refused receipt into a warning (packages/release/release-qa-audit.mjs:24-32). The CLI help says it "never blocks" (cli/right-release.mjs:273). |
| 3 | Strictly serial | One loop under an app-wide `native.lock` (crates/rightkit-qa/src/suite.rs:345-349, 417). PostRight wraps 212 scenarios in one `#[test]`. |
| 4 | Cold engine on every step | The `cli` step spawns a new engine each time (scenario.rs:690-693). On the Dell this is about 1.4 s × 1,531 steps, roughly 36 min, but that figure covers only 108 of the 212 scenarios (e2e-speed-diagnosis.md:7-8). The Mac's 0.2 s per step is derived from per-scenario time (inventories/postright.md:39), not measured per step. |
| 5 | Running a test holds a compile slot | `cargo test` is classified BUILD (rustright-policy command_policy.rs:330-331). PostRight compiled in 2 s, then held the slot for 55.7 min. |
| 6 | Apps run suites through `cargo test` | PostRight has 12 Rust step kinds in its hooks (postright crates/e2e/tests/scenarios.rs:2417-2526). Seven apps use Rust `Harness` closures. The CLI runner cannot load app code (`hooks: Default`, bin/rightkit-qa.rs:82-96). `Harness::with_run_lock` (harness.rs:313-317) serializes Harness apps across test binaries; the runner must take over that role. |
| 7 | Evidence based on trust | `kill_orphans` silently kills survivors (scenario.rs:430-438); it is opt-in (bin/rightkit-qa.rs:94) and PostRight opts in (postright scenarios.rs:2558). The Windows crash sweep never fires (process.rs:521-525). Six apps count a skip as a pass. |
| 8 | The runner still builds | `rightkit-qa` accepts `--build`/`--build-ui` and calls `build_engine` (engine.rs:757, suite.rs:272), so a runner run can compile outside the broker. |
| 9 | Little true e2e coverage | HeardRight 5/2,549, MailRight 1/1,395, CodeRight 4.4% of 11,782, ViewRight 2.1%, Pulse 5% (inventory REPORT.md). |

## Decision (C-1)

1. **Two lanes that meet only through a build record.**
   - **Build lane (broker), compile only.** The runner may issue only `rightkit cargo build` or
     `test --no-run --message-format=json` plus the declared frontend build. It writes a build record:
     binary paths, sha256 values computed by the runner, git tree, lockfile hashes, features, toolchain,
     broker request id, and an attested exit of 0. Binaries are copied to a run store that is safe from
     garbage collection.
   - **Unit tests stay in `cargo test`:** pure logic and contracts only, no process launches, under a
     short time limit in the broker.
   - **Runner lane:** `rightkit-qa run` uses no cargo. It re-hashes binaries at launch, refuses any
     binary without the QA marker (binary.rs:54, 167), and runs in the logged-in session. Today it can
     still build (`--build`, `--build-ui` → `build_engine`, engine.rs:757, suite.rs:272); qualifying
     runs refuse both flags, so the broker is the only compile path.
2. **One machine-wide pool.**
   - N = min(cores, free memory and commit ÷ measured peak per scenario). Start at Dell 2, Mac 4 until
     measured.
   - Lock shared resources, not apps: `focus`, `audio`, `gui-session`, `clipboard`. `native.lock` is
     dropped.
   - **Required journey IDs per platform** are fixed up front, before any scenario runs
     (suite.rs:420-468).
   - **Machine-wide state must be isolated or declared.** A scenario that touches machine-wide state
     must declare its token; `journeys check` refuses undeclared use. Cases verified in code:
     - **Single instance:** nothing to build. ViewRight's single-instance guard is already off under
       qa-native, bench-native, dev-native and tests (viewright src-tauri/src/lib.rs:76-99), through
       rightkit-tauri-shell (Cargo.toml:76-77). Pulse's docs mention single-instance, but its engine is
       unchecked.
     - **Keychain / Credential Manager:** `rightkit-credentials` has an in-memory backend compiled only
       for debug or test (crates/rightkit-credentials/src/store.rs:2, 57-63). It is process-wide and
       volatile, not per run, and it lives in the app or engine process, not in the runner's worker
       (store.rs:57-63). A worker subprocess therefore does not give a scenario its own store. Under QA
       isolation an app must trip the guard (`Deny`, or the memory store): PostRight's QA runs use
       `GuardMode::Deny` (postright engine runtime.rs:136-140). Only scenarios that need the real OS store
       take the `credentials` token, and sharing credentials across processes inside one scenario also
       needs that token. (Sage C2)
     - **Clipboard:** rightkit-automation's opt-in paste path uses the system clipboard
       (crates/rightkit-automation/src/delivery.rs:37, 514-580). Scenarios using it take a `clipboard`
       token, or type text as key events.
3. **One supervised worker per scenario.**
   - The worker is a subprocess of the runner (D-a): one job, one tracker, one memory credential store
     per scenario. A per-scenario tracker replaces the shared one (suite.rs:358; process.rs:337-345).
   - Containment: a Windows no-breakaway Job Object. `WindowsJob` cannot list its pids today
     (rightkit-child-process/src/windows_job.rs:206-280), so recording survivors needs a new job-pid API
     in `rightkit-child-process`, published before the runner depends on it.
   - Breakaway is denied (windows_job.rs:84). A spawn with `CREATE_BREAKAWAY_FROM_JOB` therefore fails
     with access denied and the child never exists; the acceptance test is that the breakaway spawn
     fails. The job pid list must contain every live member, including grandchildren. A process created
     outside the job tree is not contained by it and is recorded under Residual risks. (Sage C7)
   - The runner/worker contract is ruling D-g in item 14. (Sage C10)
   - On Unix there is no `ctx.spawn`: `Ctx` (scenario.rs:291) has none and `Tracker::register`
     (process.rs:310) is manual. It is a new API, and every spawn must go through it, because
     `OwnedCommand` gives each child its own group. macOS `open` launches are pid-tracked by bundle path
     and start time. `run_owned` survivors are recorded too (process.rs:471-487), and the Windows
     fingerprint is fixed (process.rs:521-525).
   - Survivors are recorded before cleanup. Any survivor or unknown owner fails the scenario.
     `kill_orphans` is opt-in today (bin/rightkit-qa.rs:94; PostRight opts in); qualifying runs refuse
     `--kill-orphans`.
   - Hard step and scenario deadlines kill the tree and mark the scenario `errored`. Each worker gets a
     fresh sandbox: data, home, temp, ports and control socket.
4. **The runner owns the loop.**
   - Generic steps become rightkit-qa built-ins: http, engine_log, daemon lifecycle, mock_admin and
     secret_absent.
   - App-specific steps go in a per-app step plugin, built in the broker. It speaks the new step-plugin
     protocol v1 (D-b), not adapter protocol v1: that protocol is file-based and has the app report
     verdicts (journeys/adapter.rs:1-12, 22-24, 414). The plugin cannot choose journeys or verdicts.
   - **The plugin may not spawn processes itself.** On Unix its children would get their own process
     group and escape tracking. Anything long-lived becomes a built-in step. A one-off child goes
     through a `spawn` request in the protocol, so the runner starts and tracks it; spawn supports
     stdin, stdout, pid, wait and kill (postright scenarios.rs:2205-2225).
   - **Setup and teardown go through the plugin** (postright scenarios.rs:2472-2510).
   - Each app's `#[test]` suite wrapper is deleted after an equivalence run.
5. **One engine per scenario.**
   - `cli` steps are served by a persistent `--stdio` sidecar. The engine and client already exist
     (postright engine main.rs:170, :175; rightkit-qa engine.rs sidecar; scenario.rs:713-735).
   - Cold launches must be declared: `lifecycle = "cold"` with a reason (startup, restart, recovery,
     role change), plus explicit `cold_cli`, `restart` and `kill` steps.
   - The reasons must also cover PostRight's real needs: the exclusive store lock (`store.*` methods take
     the lock and open the store on every call), the CLI entry and args form, and non-method commands
     (sidecar_bridge.rs:18-20, 61-80). (Sage C3a)
   - The receipt counts engine starts per environment set (role), not per scenario. Each set may start once,
     plus its declared restarts; more starts fail the scenario. The W0 run made 411 engine starts for 212
     scenarios, so a limit of one start per scenario would fail the pilot as written. (Sage C3b)
   - A parity check runs each converted command both cold and in-session.
     Parity compares the JSON body and the failure class. The stdio protocol carries the CLI exit code; until
     it does, parity states that exit codes are not compared, because the bridge invents exit codes 0 and 1
     (sidecar_bridge.rs:136). (Sage C3c)
   - A one-shot engine needs a serve mode. That is the only product change an app must make.
6. **Truthful evidence.**
   - Required journey IDs are selected per platform up front.
   - Each result is `passed | failed | skipped | errored | not_run`. Only `passed` counts. No
     retry-to-green. On the wire `not_run` keeps its existing name `missing` (journeys/mod.rs:114-120;
     adapter.rs:10); it is not renamed (D-d). `CheckStatus` (evidence.rs:12-16) gains the new states
     before other work depends on them. App wrappers must check `status != Passed`; PostRight's suite
     wrapper only fails on `Failed` today (postright scenarios.rs:2566).
     Interim rule until the runner lands: a wrapper fails on any status except `Passed`, and except a
     `Skipped` for another platform (suite.rs:420-447). W0-B adds a typed skip kind to `Outcome`, so that
     case is typed rather than inferred. (Sage C5)
   - Isolation is recorded from the actual launch environment and paths, not asserted.
   - Passing evidence comes only from committed scenario files with a clean tree. The control server
     accepts orders only with a per-run token from the runner (D-e).
   - `explore` mode produces no receipts. Its output is a recorded TOML scenario.
7. **Speed.**
   - Per-host budgets are warnings for about a week, then enforced at about 2× the p95 per step kind.
   - Runs select journeys from changed files (`journeys-map.toml`). A full run happens nightly and before
     release.
8. **Seven Harness apps: a time-boxed bridge.**
   - Bridge: the runner executes their built test binaries outside the broker, one worker per test
     function, with structured result lines. It takes no new scenarios and has a removal date per app.
   - Triage, not one-for-one conversion: real journeys move to TOML, disguised unit tests move down, and
     duplicates are deleted with owner sign-off.
9. **Guardrails at edit, push and done.**
   - At edit: test tags (`rk-test: pure | contract | pin #issue`) are checked. A `pure` lint applies,
     `pin` is checked via `gh`, and the untagged count may only fall from a frozen baseline. This applies
     to apps, not RightKit's own packages.
   - The runner refuses to launch apps from inside `cargo test`; this refusal ships with the worker
     supervision change (dispatch D), not the guardrails change. `journeys check` fails crates whose
     tests launch binaries.
     "Launched from inside `cargo test`" means a process that carries the world marker (D-c) and no run
     token. Bridge-run binaries (item 8) carry both, so they are not flagged; `journeys check` still exempts
     declared bridge entries until their removal date. (Sage C8)
   - At "done": a fresh receipt for the mapped journeys (Legion `run close` plus the push gate; Legion's
     owner must agree).
   - Make the right path easy: `rightkit-qa journeys new` and one shared journey skill.
10. **Release admission, last and earned.**
    - Hardening comes first:
      - local receipts must match platform and release id;
      - the qualifying runner is pinned by an approved digest;
      - every required journey ID must pass, not one matching check;
      - signing happens outside test execution.
    - Each app then switches `admit()` from warn to refuse automatically, after N consecutive days green
      and within budget on both hosts. N is Adrian's. The switch is recorded in protected policy, and
      only Adrian may move it back.
11. **Installed-app lane, last.**
    - A few smoke journeys run against the exact sealed build, driven externally through OS
      accessibility (rightkit-automation), because release builds have no QA control server.
    - They run on disposable machines Adrian owns: a Tart VM on the Mac Mini, and Windows Sandbox or
      Hyper-V on the Dell. Not GitHub.
    - They run before release, not daily.
12. **Other test lanes, under the same pool and receipts:**
    - **React UI:** vitest with mocked IPC runs outside the broker (it is Node). It counts as unit-tier
      evidence, never as journey proof.
    - **iOS:** `xcodebuild build-for-testing` runs in the build lane, and `test-without-building`
      (XCUITest) in the runner lane.
    - **Android:** the Gradle/Rust build runs in the build lane, and the adb journeys in the runner lane.
    - **Deferred.** The vitest, iOS and Android lanes are explicitly deferred: they are not in dispatch
      waves 0-6 and get their own design pass and dispatch wave (Wave 7) after the PostRight pilot
      passes. This ADR fixes only their lane placement.
13. **Retire the Node journeys runner** once the replacement passes equivalence. Its gate, census and
    map logic remain as reports over rightkit-qa evidence.
14. **Settled rulings (Sage gate review, 2026-10-23).**
    - **D-a, worker per scenario.** Each scenario runs in its own worker subprocess: one job, one tracker,
      one memory credential store. The in-process `run_suite` stays serial under `native.lock` until the
      PostRight wrapper is deleted (W3-D).
    - **D-b, step-plugin protocol v1.** JSON lines over stdio with the messages hello, setup, step,
      teardown, spawn, spawn_result and shutdown. One persistent plugin process per scenario, inside the
      job. Verdict fields in plugin messages are ignored. Journeys declare it with `runner = "scenario"`.
    - **D-c, world marker.** Every scenario's processes carry an environment world marker. Survivors are
      found by a same-user scan (macOS KERN_PROCARGS2, Linux `/proc/<pid>/environ`) plus descendants of
      registered roots. The first acceptance item of the supervision change is that the marker is
      readable on the Mac. A child that scrubs its environment is a residual risk.
    - **D-d, `missing` stays on the wire.** `not_run` is its documented name; no wire rename.
    - **D-e, control token.** The token is a user-only file in the world directory, checked in the
      embedded control server (crates/rightkit-qa/src/embedded/mod.rs; line 13 says there is no bearer
      token today), not in control.rs. Threat model: it stops accidental ad-hoc driving, not a
      same-user process.
    - **D-f, CheckStatus wire names (Sage C9).** `CheckStatus::Errored` is added and serialises as `errored`.
      The `not_run` state (`CheckStatus::NotRun`) keeps the wire name `missing` (D-d). The Node mirror
      `packages/qa/src/evidence.ts:18` changes in the same release.
    - **D-g, runner and worker contract (Sage C10).** The worker is the same `rightkit-qa` executable, run as
      `rightkit-qa worker <world dir>`. The runner writes `scenario.json` (the resolved scenario, the world
      and the tokens) into the world directory. The worker writes `outcome.json` and exits. The runner holds
      the tokens, the Job Object (Windows) or process group (Unix) and the deadlines, and kills the whole
      tree when a deadline passes. W2-D0 lands the contract types in a new `worker.rs` before the pilot
      depends on them.

**Order:**
0. **Step 0, relief first, almost no new code:**
   - PostRight runs `rightkit cargo test --no-run` in the broker, then executes the test binary
     outside it. The 10-21 standard already describes this; it frees the compile slot immediately.
   - Unit lane must not run journeys (Sage C4). `test:rust` is `rightkit cargo test --workspace`
     (package.json:21) and includes `crates/e2e`, whose aggregate test is not ignored (scenarios.rs:2554-2555),
     so `pnpm test` still runs all 212 scenarios in the broker. Fix: `--exclude postright-e2e`. Remove the
     `POSTRIGHT_CARGO` override (run-e2e.sh:10, 18), which allows a plain cargo compile outside the broker.
   - PostRight's `cli` steps go through the existing `--stdio` sidecar, removing about 36 min of cold
     engine starts on the Dell.
   - The full pilot then lands behind these two.
1. PostRight pilot: runner, build record, engine per scenario, built-ins plus plugin, supervised
   workers. Rejection cases: wrong-platform receipt, ignored test, hook hang, detached descendant,
   concurrent state, dev-target substitution, stale build record.
2. Shared pool.
3. Harness bridge and triage.
4. Budgets, then guardrails as warnings, then enforced.
5. Admission hardening, then per-app refusal.
6. Installed lane.
7. Retire Node.

## Rationale

- **C-1 over C-2.** Rust `rightkit-qa` already owns launch, process tracking, isolation, evidence and the
  runner that `right-release` invokes, and every app already depends on it. C-2 is not wrong in
  principle; the adapter protocol exists. But it would keep two orchestrators until one is retired, and
  the release path already consumes C-1's output.
- **C-1 over C-3.** Refusing releases on today's serial, trust-based harnesses would block releases
  without truthful coverage, leave runs slow, and teach everyone to bypass the gate.
- **Measured W0 result (Sage C1).** PostRight at `ca1839f` (serial, sidecar bridge, Dell, 2026-10-23):
  211 passed, 1 skipped, 304 s, 411 engine starts for 3,066 cli calls, against 4,323 s with one process
  per step. The persistent engine was the dominant term. Serial PostRight on the Dell already meets the
  2 to 5 minute target. The pool (item 2, W2-B) is justified only by the sub-1-minute warm target or by the
  other serial apps (GenRight 2,987, 2,696 and 1,726 s). Choosing between those two targets is Adrian's
  decision.
- **Speed comes from structure, not tuning.** The 30-110 minute runs come from one cold engine per step,
  a serial loop, and runtime held inside a compile slot. Removing all three addresses the measured
  causes. The estimate for PostRight's suite is minutes, which the pilot must confirm.
- **Targets, defined and set per machine.**
  - *Warm* = runner time only, from `rightkit-qa run` start to receipt, with a valid build record already
    present.
  - *Cold* = warm plus the broker build from a warm cargo cache, including queue time.
  - PostRight targets: warm under 1 min, cold under 5 min, for each of the Mac and the Dell separately.
  - Both are measured from the receipt's compile, queue, execution and teardown durations.
- **Unit-test time limit:** a starting cap of 600 s per `cargo test` invocation in the broker.
  - Today's offenders hold slots for 3,000+ s.
  - Real unit suites in the inventories run in 14-59 s (ScrapeRight) and about 21-39 s (HeardRight's
    whole workspace).
  - The cap is retuned to about 3× the measured p95 of unit-tier runs per host after the first week.
- **Enforcement is mechanical.** Instructions alone have not held; the 10-21 standard stalled as
  "advisory". Checks at edit, push and "done", plus the receipt's start count and token-gated control,
  make the right path the only passing one.

## Consequences

- **Positive:**
  - one owner;
  - test runtime no longer blocks compilation;
  - suites are expected to take minutes;
  - false greens are removed;
  - releases are eventually gated on real proof;
  - agents cannot go back to ad hoc clicking or tiny tests.
- **Negative:**
  - each app with a one-shot engine must add a serve mode;
  - PostRight's hooks need a plugin binary;
  - the seven Harness apps need triage work;
  - new RightKit code: worker supervision, build record, pool, step built-ins, plugin protocol, start
    counting, run tokens, `journeys new`/`record`;
  - a RustRight change (test time limit; later, test execution leaves BUILD and the commit probe is added).
- **Neutral:** RightKit's own crates keep their contract and integration tests; the journey rules apply
  to apps.

## Residual risks

- Windows per-step cost (about 1.7 s vs 0.2 s on the Mac) may be Defender scanning unsigned binaries.
  This is unproven. Measure with and without exclusions, or on a Dev Drive; the change is Adrian's.
- The macOS GUI-session executor for the Tart guest does not exist yet: `tart.rs` runs commands over SSH
  only.
- The `pure` lint is heuristic. Tests that use temp directories need an honest class (`contract`).
- Hosted Windows CI flakiness (#931) can still redden qualification until it is resolved.
- Under-scoped journey maps can under-select. Nightly and pre-release full runs bound the exposure.
- A scenario process that scrubs its environment escapes the world-marker scan (D-c); it is still caught
  if it descends from a registered root.
- The control token (D-e) is not protection against a same-user process.
- A process created outside the job tree is not contained by the Job Object and is not in its pid list.
  The breakaway spawn fails (Decision item 3), but a process the app starts by other means is a residual
  risk and is recorded only if it descends from a registered root. (Sage C7)
- **Disk.** Outside-broker test target directories under /Volumes/D/scratch pushed free space below the
  broker's 200 GB storage floor (RK_STORAGE_FLOOR, 15:27Z). Target directories outside the broker are a
  violation, not a capped resource: under item 1 the runner has no target directories, and any found is a
  failure. The cap applies to the run store and evidence (workstream W2-A), with a cleanup-after-run
  fixture and a numeric cap set by the RightKit owner. (Sage C6)

## Reversibility and exit

- **Reversible per app:** the cutover is policy-recorded, and Adrian can move it back.
- **Additive:** the bridge and the Node runner stay available until equivalence is shown, so rolling back
  means re-enabling the old path for that app.
- **Not freely reversible:** deleting the suite wrappers and duplicate tests. Each deletion is gated on
  an equivalence run and owner sign-off.

## Review triggers

- The PostRight pilot misses its time target (under 5 min cold, under 1 min warm) or its equivalence check.
- Pool sizing causes memory or commit pressure on the Dell.
- More than one app needs a cold-start exemption outside the four allowed reasons.
- The guardrail false-positive rate leads to repeated bypass attempts.
- A Harness app's removal date is missed.
- RustRight cannot classify test execution out of BUILD.

## Evidence retention

The scratch copies are disposable. The evidence files are committed next to this ADR in
`docs/design/adr-0001-evidence/`, with a `SHA256SUMS` file (sha256 of each committed file):
- committed by rev 3: the RK Dell Architect D2 frame (`test-arch-dell.md`, with the Sage correction at §2.4),
  rk Dell's speed diagnosis (`e2e-speed-diagnosis.md`), the Sage verdict on rev 2
  (`sage-verdict-adr-0001-rev2.md`), and the dispatch plan (`adr-0001-dispatch.md`);
- not on the Dell, so not committed by rev 3: the merged amendment, the Architect D2 frame, the 9 app
  inventories plus REPORT.md, and the Astra D2 review. The RK Mac integration owner commits them from the
  Mac checkout and adds their hashes to `SHA256SUMS`.

The cited file:line table is pinned to rightkit `9300907b`, postright `f67c4aa`, heardright `9487ef4d`
and rustright `d3f50c6`, so it can be re-checked at those commits.

## Status

Proposed.

**Revision 3 (rev 3, Sage sign-off conditions applied 2026-10-23).** Sage's verdict on rev 2 is
APPROVE-WITH-CONDITIONS, conditions C1 to C11 (saved as `docs/design/adr-0001-evidence/sage-verdict-adr-0001-rev2.md`).
They are folded into the sections they touch and tagged "(Sage Cn)": C1 Rationale; C2 Decision 2;
C3 Decision 5; C4 Order step 0; C5 Decision 6; C6 Residual risks; C7 Decision 3 and Residual risks; C8
Decision 9; C9 and C10 Decision 14; C11 evidence_refs. Rulings D-f (CheckStatus wire names) and D-g
(runner and worker contract) are recorded as Decision item 14. Still proposed. Per the RightKit change
gate, each implementation change still needs Sage and Oracle review.

**Revision 2026-10-23 (Sage gate).** Sage's gate review found the decision holds against the code and
required these changes, applied in this revision. Corrected facts: the step plugin needs a new protocol,
not adapter v1; `ctx.spawn` is a new API; the Windows job needs a pid API; the ViewRight single-instance
item is dropped; the credential store is process-wide and volatile; `kill_orphans` is opt-in; the Dell
timing covers 108 of 212 scenarios and the Mac figure is derived; `admit()` is at lines 24-32;
`Harness::with_run_lock` is recognized; the runner's `--build` flags are refused. Rulings D-a to D-e are
recorded as Decision item 14. The vitest, iOS and Android lanes are explicitly deferred. Still proposed.

The design merges the RightKit Mac lead's design, a second architect's proposal and its
enforcement layer, and rk Dell's speed diagnosis. The merged amendment has not yet had an independent
adversarial review: Astra ran out of credits. Per the RightKit change gate, each implementation change
still needs Sage and Oracle. Adrian approves the cutover rule (N), the QA key distribution, and any
Defender change.
