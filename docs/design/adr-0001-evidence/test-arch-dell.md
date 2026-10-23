# Right Suite testing architecture: compile, test, qualify (RK Dell, Architect D2, 2026-10-23)

Charter: **significant** (nine apps, durable contract, platform, trust: a false green ships a broken release). Depth **D2**, objective **best_shape** (Adrian: "design and tell me what this whole thing should look like, cargo test, all of it"), rigor **standard**. Read-only; nothing was changed or run except reading receipts and evidence folders. Every number is measured on the Dell unless marked *inferred* or *unknown*.

Baseline it extends: `rightkit/docs/E2E-TESTING-STANDARD.md` (Sage, 2026-10-21, rev 2, **not adopted**; decisions D1–D11; `rightkit-qa journeys check|validate-receipt` exist, `run|census|gate` do not). This record keeps D1–D11 and adds D12–D17 for what the standard does not cover: speed, parallelism, the execution model, budgets, and enforcement of the gate.

## 1. Frame

| | |
|---|---|
| Decision question | One shape by which every app compiles, runs its tests, and proves itself on both hosts, in minutes, with no app running its own harness. |
| System of interest | The qualification plane: `rightkit-qa` (Rust runner), `@rightkit/qa` + `@rightkit/qa-chrome` (Node/browser), `@rightkit/release` (`right-release test`, `right-release qa`), the RustRight broker, each app's test crates and scripts, hosted CI. |
| In scope | Unit tier (`cargo test`), journey tier (end-to-end), build/run split, parallelism and isolation, speed budgets, evidence and receipts, the release gate, migration order, the Dell/Mac split. |
| Out of scope | Defender settings (Adrian's call, #916), hosted-runner cost policy, the #925/#916 code fixes, Android/iOS journey runners (named as surfaces only). |
| Authority | Adrian decides; Sage + Oracle approve any change (his rule of 2026-10-23). RK Mac integrates `main`. |
| Shared-platform index (`rustright owns`) | desktop-qa → **rightkit-qa** (extend); qa-evidence → **@rightkit/qa** (extend); browser-qa → @rightkit/qa-chrome (reuse); release-tooling → **@rightkit/release** (extend); owned-processes → rightkit-child-process (reuse); framed-sidecar → rightkit-sidecar (reuse); local-services → rightkit-daemon test-mode (reuse). **No new crate or package. No new third-party test runner on the critical path** (cargo-nextest: see C-3). |

## 2. Current state (inventory, 2026-10-23)

### 2.1 Per app

| App | Journey assets today | How they run | Unit tier | Measured (Dell broker, 72 h) |
|---|---|---|---|---|
| **PostRight** | 212 `.scenario.toml` (+3 pending, 5 UI, 1 UI pending), `rightkit-qa.toml`, `crates/e2e` with `SuiteHooks` | ONE `#[test]` loops all 212 through `suite::run_suite`, sequential; 3,079 `cli` steps = one cold `postright-engine.exe` per step; 1 scenario uses the sidecar | `test:rust` = `--workspace` with `test-mode` | 16 roots, 51.8 unit-h; e2e 3,345 s cancelled at 108/212 (compile 2.26 s); `postright-engine` 8,269 s of which **8,267 s compile** |
| **GenRight** | 178 `#[test]` journeys in `genright-e2e` (fast/local/paid/ui/windows), own `scenario()` over `Harness` | `--test-threads=1`; one engine process per command (`lib.rs:456`) | `test:rust`; 39 node tests | 22 roots, 36.7 unit-h, **35 % compile** (run-bound): 2,987 / 2,696 / 1,726 s serial |
| **CutRight** | 32 UI flows in `cutright-ui-e2e` (Harness + control, launches the real app bundle); 163 engine tests, 62 of them drive ffmpeg (`finishing_e2e` ran ffmpeg at 7 cores today) | plain `cargo test` **outside the broker** (today); no `rightkit-qa.toml`, no package scripts | same crates | no broker receipts: unmeasured |
| **HeardRight** | `qa-e2e` (installed_app_ax, native_journey, native_pill_regression); 23 node contract suites; `qa:browser` | `rightkit cargo test --manifest-path qa-e2e` | 56 test files in `src-tauri/tests`; vendored `rk-snapshot` excluded | 120 roots, 80.8 unit-h, **95 % compile**; `heardright_next` 8,864 s FAILED |
| **ViewRight** | `src-tauri/qa-e2e` (formats, pdf_edit, shell, upgrade) on Harness + control, qa-native debug build | `rightkit storage run --app viewright -- right-release test`; vitest unit | `verify:rust` `--workspace` | 1 root |
| **MailRight** | `qa/native-e2e` (1 native flow + surface guard); `scripts/qa-journeys.mjs` drives a Vite server with **mocked IPC** (not the app) | Harness | 59 engine/core test files; 40 tsx tests | 10 roots, 42.6 unit-h, **99 % compile** (2,701 s compile) |
| **ScrapeRight** | `scraperight-ui-e2e` 3 files; `native_journey` is `#[ignore]` by default; `native_shell_real` needs 6 env vars | Harness; 20 of 24 roots `--test-threads=1` | crates/* ; 31 node tests, each its own script | 24 roots, 23.7 unit-h, 94 % compile |
| **CodeRight** | `qa-e2e` with `rightkit-qa.toml` + 5 TOML scenarios + visual | `rightkit-qa` config exists; engine tests via `--workspace` | 121 test files under `engine/bins/coderight` (vendored `repos/` excluded) | 17 roots, 44.5 unit-h, 96 % compile |
| **Pulse** | `hub/qa-e2e/tests/ui.rs` (Harness + control, functional/visual, inventory.json); `qa/release/{source,installed}` TOML via `right-release qa`; `scripts/gate.sh` with its own `PULSE_TEST_TARGET` | gate.sh | 53 test files | 72 roots, 33.4 unit-h, 59 % compile |

Nine apps, five different ways to run a journey (TOML suite in one `#[test]`; Harness `#[test]` per journey; `right-release qa`; `gate.sh`; plain `cargo test` outside the broker), two apps with no runner config at all, one app whose "journeys" never touch the app.

### 2.2 Shared machinery

- **rightkit-qa 0.8.0**: scenario TOML (`cli`, `sidecar_start/command`, `mock_admin`, `http`, UI control, media validators), `suite::run_suite` ("run scenarios **sequentially**", `suite.rs:1`), `Harness::scenario` with optional cross-process run lock, `userdata::Guard`, orphan sweep, hashed evidence, retention, `rightkit-qa-interactive` (Windows session-0 relaunch), `journeys` module (manifest + receipt, no runner).
- **@rightkit/release**: `right-release test` (suites/runners, vitest); `right-release qa --stage source|installed` runs rightkit-qa and writes a sealed, MAC'd QA receipt against `release-qa-floor.json` (`smoke.launch`, `journey`); **"a missing receipt warns and is recorded; it never blocks"** (`right-release.mjs:273`).
- **RustRight broker (6ffcfb7, Dell config)**: `compileBudgetUnits 50`, `buildCostUnits 20`, `cargoBuildJobs 20`, `buildConcurrency 2`, target cache cap 150 GB (310 GB in `ws/` today). `cargo test` is BUILD-lane, costed 10 units, and holds them from compile until the last test exits (rustright#135). `cargo nextest` is also BUILD. Jobs run `env_clear()` (rustright#116: QA variables never reach the test). Windows reap misses reparented descendants (#134). Load-sensitive tests fail under broker load (#136).
- **Hosts**: Dell i9-13900H 14c/20t, 63.7 GB RAM, commit headroom seen at 8–10 GB, Defender RTP on (process teardown 2–69 s measured in #916). Mac: *unknown* here.

### 2.3 Where the time goes (the breakdown Adrian asked for)

| # | Multiplier | Evidence | Share of PostRight's 55 min |
|---|---|---|---|
| 1 | **Serial by design**: the suite runner is sequential, and 212 scenarios sit inside one `#[test]`, so `--test-threads` is moot | `suite.rs:1-2`, `scenarios.rs` | 1× instead of 8–12× |
| 2 | **A cold process per step**: every `cli` step spawns the engine; each start does daemon-mode init, crash hook, shared store lock, store open, then exits; on Windows add process creation and a Defender scan | `postright/crates/engine/src/main.rs:146-176`; 1,531 steps, p50 1.38 s | **36 min** |
| 3 | **No budget**: step and scenario limit 600 s; 132 s, 93 s, 73 s stalls go unnoticed | evidence timestamps | 12.5 min |
| 4 | **Executed inside the broker inside `cargo test`**: holds 10 compile units for the whole run; blocks every other compile | receipt 096d6843 | blocks the lanes |
| 5 | **Cold compiles** for the compile-bound apps: 95–99 % of their test permit time is compiling; several roots show a 45–140 min compile with "warm cache" claimed | receipts (cause: see §2.4) | not PostRight's e2e, but 80 h of the 300 unit-h |

The same four shapes appear in GenRight (178 serial `#[test]`s, one process per command) and ScrapeRight (serial). The tools to remove #2 already exist: the engine has `--stdio` and rightkit-qa has a sidecar client; PostRight uses it in one scenario.

### 2.4 Why the compile-bound apps are slow (it is mostly not compiling)

> **Correction (Sage gate, 2026-10-23, verified in code by RK Dell):** the "broker lock wait after the permit" row below is misattributed. `lockWaitMs` counts **Cargo's own file-lock waits** ("Blocking waiting for file lock on package cache / build directory", `rustright-broker/src/job_service/service.rs:6330-6341`), not broker workspace/repository exclusivity, which the scheduler already checks **before** granting units (`rustright-scheduler/src/scheduler.rs:254-266`, grant at `:502-523`). The symptom (units held while no compile happens) and the numbers stand; the cause is contention on the single shared `cargo-home` package cache, and the fix is "a root blocked on a Cargo cache lock must not hold units" (package cache as a scheduler class, or return units while waiting), not "locks before permit". D18's first bullet and the dispatch's W1-A are re-scoped accordingly. The 8,268 s PostRight root (`b3ceaed6`) started with an **empty** target (`unitsAtStart 0`, 0/251 fresh), not a feature-set eviction; why it was empty is open.

107 broker roots ran ≥ 10 min in the last 72 h. Only **5** were cold starts (2 `target-empty-at-start`, 3 `target-nonempty-but-no-fresh-artifacts`); **94** started with fresh artifacts. Where the time went in the twenty longest:

| Cause | Evidence | Examples (run s / wait s) |
|---|---|---|
| **Broker lock wait after the permit** (`lockWaitMs`: workspace / repository exclusivity) | 50–95 % of the run in ~half of the top 20; counted as run time while holding 10–20 compile units | GenRight check 3,821 / 3,694; CutRight check 3,778 / 3,689; ViewRight check 6,234 / 3,119; MailRight test 6,667 / 2,873; HeardRight check 3,148 / 2,996; ScrapeRight check 3,619 / 3,612 → `RK_LOCK_WAIT_TIMEOUT` |
| **Cargo's package-cache lock** inside the one shared `global/cargo-home` ("Blocking waiting for file lock on package cache") | 4–10 waits in every long root; cargo's own `Finished in Ns` includes this wait, which is why the "compile share" looked like 95–99 % | all of the above |
| **Real cold compile on a feature flip or a fresh generation** | the two biggest roots only | HeardRight `heardright_next` 8,864 s (gen 023 empty); PostRight `postright-engine --features test-mode` 8,269 s (406+ crates recompiled: the `test-mode` feature set differs from the dev build, so the warm dev target is useless for it) |
| Stalls and tool exits | `stalled` cancellations, `tool-exit` failures (CodeRight check 8,265 s FAILED) | several |

Not a cause: target-generation churn (420 generation dirs across 421 workspace keys, one each; `rightkit` has 6 target hashes for its scratch checkouts). The "warm cache" is warm; the roots spend their permit waiting for locks, or rebuilding the world because the test feature set is a different world.

Consequences for the design: (a) executing tests outside the broker (D12) removes the largest avoidable permit-holder, but it does not fix lock-wait-under-permit, which is a broker defect to file (rustright: acquire workspace/repository locks **before** taking compile units, or release units while waiting; and one package cache lock per concurrent root class). (b) Each app needs **one** test-mode feature set with its own warm target (D18), or every switch between dev and test builds is a 2-hour compile.

## 3. Context, constraints, unknowns

Stakeholders: Adrian (owner; speed, no ad-hoc harnesses, strict proof); RK Mac (integration owner, publication); RK Dell (Windows leg); the nine app chats; Rustright Win/Mac (broker); Sage/Oracle (approval).

Constraints: tests are stopped until Adrian's go; compile only through the broker; no worktrees/clones; release builds strip QA, dev builds run QA by runtime mode (`workspace.md`); native-UI journeys need an interactive session on each host; the broker scrubs the environment (#116); Windows reap (#134); Defender unmeasured; two GitHub-hosted OS runners for public surfaces only.

Unknowns: Mac per-step cost; why GenRight/ScrapeRight chose `--test-threads=1` (ports? model servers? starvation?); engine stdio stability over thousands of commands; Windows interactive-session execution from a chat (session 0).

## 4. Drivers as measurable quality scenarios

Thresholds come from Adrian's statement ("two, three, five minutes" for ~1,200 scripts; "100–500× real time") and from measured data; none is invented.

| ID | Scenario (stimulus → response → measure) |
|---|---|
| QS-1 Journey speed | PostRight's 212 engine journeys (≈2,300 engine commands) on the Dell, warm binaries: **≤ 5 min wall** with parallel cold-process steps (1,531 × 1.4 s ÷ 8 workers ≈ 4.5 min: parallelism alone meets Adrian's number); **≤ 1 min** with a persistent engine (≈ 2,300 × 20–50 ms ÷ 8). Per engine command over the sidecar ≤ 50 ms p50 (*to be measured; VOI gate for the budget numbers, not the shape*). |
| QS-2 Compile | After a one-file change, rebuilding an app's test-mode engine + test binaries ≤ 2 min warm; a full rebuild happens only when `Cargo.lock`, toolchain, profile or feature set change, and then once per (app, host, feature set), never per chat or per generation. |
| QS-3 Broker | No test execution ever holds a compile permit; compiles keep flowing while 8 journey workers run; a hung journey is killed in ≤ 10 min with the last step named. |
| QS-4 Parallel = serial | N journeys run concurrently with per-journey worlds; the verdict set equals the serial verdict set in 3 of 3 equivalence runs per app before parallel is the default for that app. |
| QS-5 Proof | `verdict = passed` only when executed == selected, zero skipped, binaries bound to the built tree (sha256 + git tree), no retry altered a verdict; a misspelled filter that runs nothing fails (standard D5 §5, kept). |
| QS-6 Uniformity | An app with no `journeys.toml` or a non-conforming adapter cannot obtain a `right-release qa` receipt; an app-owned aggregate `#[test]` loop over scenarios is refused by `journeys check`; the release gate **blocks** on release-critical journeys (today it never blocks). |
| QS-7 Hosts | Each journey declares platforms; a platform without a valid receipt is `unknown`, never pass; native-UI runs in the interactive session of each host; CI covers engine/cli/web only. |
| QS-8 Unit tier | An app's D7 unit tests compile in the broker and **execute outside it**, parallel by default, with no engine/app launch (that is a journey); ≤ 3 min warm for any app (*derived from compile share data, calibrate*). |
| QS-9 Observability | Every run reports p50/p95/max per step kind and the ten slowest steps, inside the receipt. |
| QS-10 Hygiene | Zero leaked processes after each journey (sweep per world, not per run); evidence retention bounded per run. |

## 5. Boundaries, data authority, invariants

Planes and owners:

| Plane | Owner | Writes | Reads |
|---|---|---|---|
| Build | RustRight broker | artifacts, build receipt (source tree, artifact sha256) | source |
| Run | `rightkit-qa` runner (outside the broker), `@rightkit/qa` for Node/browser | run root, per-journey evidence, results, timing | artifacts (read-only) |
| Evidence | `rightkit-qa` (`rightkit.journeys.receipt/1`) | receipt | results, build receipt |
| Gate | `@rightkit/release` (`right-release qa`), `rightkit-qa journeys gate`, hosted publish | sealed QA receipt, release decision | journeys receipt, manifest, map |
| App | app repo | `journeys.toml`, `journeys-map.toml`, scenarios/adapters, test-mode engine, D7-tagged unit tests | shared runner |

Invariants:
- **I1** Compiling happens only in the broker; **executing** a test never holds a compile permit (the runner is the only executor).
- **I2** One journey = one world: its own data dir, ports, mock, env; concurrent journeys share only read-only binaries.
- **I3** The verdict comes from evidence only; `skipped` never passes; a retry never changes a verdict (it may record flakiness).
- **I4** Launched sha256 == built sha256 == claimed git tree (standard D5 provenance rule).
- **I5** Release builds carry no QA control surface; QA runs on dev builds in runtime QA mode (existing).
- **I6** An app's proof is its `journeys.toml`; `cargo test` holds only D7 classes (`pure`, `contract`, `pin #issue`).
- **I7** Budgets are declared per journey kind and enforced: exceeding one fails the journey (after a calibration window, see D15).

## 6. Risk and uncertainty

| Class | Item | Handling |
|---|---|---|
| Epistemic | Mac per-step cost; sidecar per-command cost; cold-compile cause | Measure before fixing budgets (VOI high, cost ≈ 30 min each). Stop rule: budgets are published only after both hosts are measured. |
| Requirements | Which journeys are `release_critical` per app | App owner + Oracle, in `journeys.toml` (standard D8). |
| Technical | Engine stdio over thousands of commands; parallel port/mock allocation; orphan sweep under concurrency; Windows session-0 | Each is a named acceptance test in §10. |
| Operational | Two hosts, broker stop/go, chats editing concurrently | One coordinator per host serialises runs of the same app; worlds isolate everything else. |
| Consequence | False green at release | Highest; I3–I4 and the gate (D17) exist for this. No `retry-to-green`, no advisory floor at release. |

## 7. Candidates

**C-1 Standard as written + rustright#135.** Implement `journeys run|census|gate`; the broker splits `cargo test` into compile (BUILD) and run (new RUN lane). Adapters stay serial. Result: compile lanes unblock, but PostRight still takes 1 h 50 (fails QS-1).

**C-2 Runner-owned execution.** The standard's D1–D11 stay. `rightkit-qa journeys run` becomes the only executor: it builds in the broker (`--no-run --message-format=json`, existing D4 step 3), then executes **outside** the broker with a worker pool, per-journey worlds, a persistent engine per journey, budgets, and strict evidence. The unit tier uses the same split (build in broker, execute outside). The broker never executes a test. Passes all gates; speed from measured data.

**C-3 cargo-nextest as the runner.** Process-per-test, groups, timeouts, JUnit, archives. Astra (2026-10-23) verified: the broker injects `--target-dir`, which nextest's `--archive-file` refuses; `nextest` is classified BUILD (no run lane); JUnit is not understood by the receipt/gate; no doctests; archives skip a path-relocation test silently. Fails I4/QS-5 today; usable later for the unit tier only.

**C-4 App-local freedom with a speed SLO.** Each app optimises its own suite. Fails QS-6 and I6 (Adrian's explicit requirement) and duplicates isolation/evidence nine times.

Gates: C-4 fails I6; C-3 fails I4/QS-5 without receipt work and conflicts with the broker; C-1 fails QS-1. **C-2 passes.** Dominance: C-2 dominates C-1 on every scenario at the same ownership cost; C-3 is kept as a later unit-tier optimisation, not a dependency.

## 8. Selected shape (C-2): decisions D12–D17 (D1–D11 of the standard unchanged unless amended)

**D12. The broker compiles; the runner executes. Nothing else executes tests.**
- Journey tier: `rightkit-qa journeys run` (D4 steps 1–5) builds via `rightkit cargo … --no-run --message-format=json`, takes artifacts from `compiler-artifact` records, re-hashes them, then launches **outside** the broker in the host's interactive session (D11). The broker job ends at the artifact.
- Unit tier: `rightkit-qa unit run -p <crate>…` does the same for `cargo test --no-run` binaries: build in broker, execute the libtest binaries outside it (one binary = one process; libtest parallel inside; the runner caps concurrent binaries). This is `scripts/run-ui-tests.sh` made the rule.
- Consequence: rustright#135's RUN lane is **not needed**; #135 part 3 (fast lane for fmt/metadata) stays. #116 (env scrub) stops mattering for tests because the test process is not a broker child; it still matters for builds that read env. #134 (reap) shrinks to build children.
- Hosted CI (no broker) runs the same runner binary; the receipt records `host.session`.
- Exit: if attestation ever requires the broker to witness execution, add a RUN lane that admits only pre-built artifacts (cost 0 compile units) — the receipt format does not change.

**D13. Journeys are parallel by default; serial is declared, never assumed.**
- `journeys run` schedules selected journeys on a pool of `workers = physical_cores − 2` (Dell 12; configurable per host in `rightkit-qa.toml [host]`), each in its own world: data dir under the run root, an allocated port range, its own mock process, its own env. Journeys that need exclusivity declare `serial_group = "ui" | "audio" | "gpu" | "<name>"`; one journey per group at a time; groups default to none.
- `suite::run_suite` gains the pool (the loop at `suite.rs:417` becomes a work queue); `Harness::scenario` journeys (GenRight, CutRight UI, Mail, View, Scrape, Pulse) are executed by the runner as **separate processes** of the test binary (`<bin> --exact <journey> --test-threads=1`), N at a time, so no app changes its code to get parallelism.
- **The run lock moves from per-app to per-world.** Today `run_suite` takes `<managed app root>/native.lock` for the whole run (`suite.rs:345`), and five apps opt into `Harness::with_run_lock` (GenRight, CutRight UI, HeardRight qa-e2e, MailRight native-e2e, ScrapeRight ui-e2e), which "serialize[s] scenarios across threads and test binaries" (`harness.rs:313`). That lock, not `--test-threads=1`, is what makes those suites serial; dropping the flag alone changes nothing. The lock's job (no two runs on the same real resources) is kept by the world: the runner holds one lock per `serial_group` and none for ungrouped journeys. (No app documents a reason for `--test-threads=1`; *unknown*, likely the same lock.)
- Promotion rule (QS-4): an app runs serial and parallel three times each with identical verdict sets before parallel is its default; until then its manifest carries `parallel = false`.

**D14. One engine per journey, not one per step.**
- A `cli` step in a scenario is executed against a persistent `--stdio` engine started once per journey (rightkit-qa's existing sidecar client; the engine's existing `--stdio` mode), unless the scenario sets `process = "fresh"`. The adapter for code journeys gets the same: `Sc::run` routes to the sidecar by default.
- Each app keeps a **named, small set** of journeys on `process = "fresh"` to prove the CLI entry, startup, store locking and crash hook (PostRight: the `start_daemon/stop_daemon` and `backup::hold_shared` scenarios).
- Acceptance: PostRight's 212 run once in each mode with identical verdicts; the sidecar mode is then the default.
- Engines without a stdio mode: *unknown* for CutRight/GenRight/ScrapeRight engine binaries; the inventory shows GenRight has `fast_sidecar.rs`, so it has one. Each app's migration issue states it.

**D15. Budgets are declared, measured, and enforced.**
- Three numbers per journey kind in `journeys.toml` `[budget]`: `step_ms`, `journey_ms`, `suite_ms`; defaults: engine journeys step 2 000 / journey 30 000; UI journeys step 10 000 / journey 60 000; suite 300 000 (Adrian's 5 min). Numbers are calibrated per app during a two-week window in which exceeding reports `SLOW` without failing; after the window, exceeding **fails** the journey (I7). The receipt gains `timing: {per_kind: {p50,p95,max}, slowest: [10]}` (QS-9).
- A `SLOW` or budget failure names the step label, so the 132 s `dispatch_stored_brief` stall becomes a bug with a name.

**D16. The unit tier is small by rule and fast by construction.**
- D7 stays: a unit test needs `// rk-test: pure|contract|pin #n`. The census (D8) reports the untagged count per app; it is the migration backlog, not a gate, until phase 2.
- Unit tests never launch an engine or app (`userdata::Guard` already refuses real folders; the census flags `Command::new(<engine>)` in a tagged `pure` test as misclassified).
- They run through D12's unit path, parallel, with no `--test-threads=1` unless a `serial_group` is declared in the crate's `Cargo.toml` metadata (`[package.metadata.rk-test] serial = "audio"`).

**D17. The gate blocks, and one receipt feeds it.**
- `right-release qa` stops "never blocks": for `--stage source` and `installed` it requires a **valid** `rightkit.journeys.receipt/1` for every `release_critical` journey on the platform being sealed, validated offline (`journeys validate-receipt`), tree-bound to the sealed source. The sealed QA receipt embeds the journeys receipt hash; `release-qa-floor.json` maps `journey` to "all release-critical journeys passed".
- `journeys check` gains conformance: an app whose test crate runs scenarios from its own aggregate `#[test]` (the PostRight shape) or whose package scripts call `cargo test` on a journey crate is non-conforming; the app's only journey entry points are `journeys.toml` + `rightkit-qa journeys run`. Phase 1 advisory, phase 2 blocking per D8, with a date set by Adrian.
- The hosted publish pipeline for RightKit crates is unchanged; apps' private native-UI proof is the per-host receipt (D6).

**D18. One test-mode world per app, kept warm; locks never burn budget.**
- Each app declares in `journeys.toml` `[build]` the single feature set its QA binaries are built with (e.g. PostRight `postright-engine/test-mode`; HeardRight `qa-native,test-mode`), and every journey and unit build uses exactly that set. The broker keys the target generation on (workspace, profile, feature set) so the QA world stays warm beside the dev world instead of replacing it; a feature flip then costs nothing after the first build. (Today the flip recompiles 400+ crates: §2.4.)
- Broker (rustright, separate issue): a root takes workspace/repository locks before it is granted compile units, or returns the units while it waits; `lockWaitMs` is reported as queue time, not run time. A `RK_LOCK_WAIT_TIMEOUT` after 60 minutes of holding units is the symptom.
- Package cache: one `cargo-home` per concurrent root class, or `--offline`/`--locked` for QA builds whose lockfile is unchanged, so QA builds never wait on another root's dependency download.

## 9. Views

**9.1 Execution topology (each host)**
```
chat / CI ──► rightkit-qa journeys run --app X [--id J-…] [--platform …]
               │ 1 select (journeys.toml, platform, tier) → selection.json (run_id, nonce)
               │ 2 source identity (head, tree, dirty)
               │ 3 BUILD in broker: rightkit cargo … --no-run --message-format=json  ──► artifacts + sha256 + broker receipt
               │ 4 EXECUTE outside broker (interactive session; Windows: rightkit-qa-interactive)
               │      worker pool (N) ── world per journey ── persistent engine / hidden app ── steps ── asserts ── sweep
               │ 5 collect results.jsonl → receipt (verdict, counts, timing, bindings) → managed runs root
               └─► right-release qa (blocking) / journeys gate / hosted publish
```
**9.2 Journey lifecycle**: allocate world → `userdata::Guard` → start engine (`--stdio`) or app (control server, hidden) → steps over the sidecar / control → expectations → teardown → orphan sweep (this world only) → evidence hashed → `journey` record → world removed (or kept with `--keep-data`).

**9.3 Who owns what**: §5 table. The app never writes a receipt; the runner never edits a scenario; the gate never runs anything.

**9.4 Migration per app** (what changes; nothing is deleted without the owner's validation, D9)

| App | Steps (in order) | Why this position |
|---|---|---|
| PostRight | `journeys.toml` + adapter over the existing 212 (standard step 0); sidecar default + `fresh` set (D14); parallel promotion (D13); budgets (D15). No scenario file changes. | Already the target shape; proves D12–D15 against a mature suite; the measured 55-min case. |
| GenRight | `journeys.toml` listing its 178 `#[test]` journeys as `runner = "adapter"` entries; runner executes them per-process in parallel; `Sc::run` → sidecar (`fast_sidecar.rs` proves the mode exists); find the real reason for `--test-threads=1`. | Run-bound (35 % compile): the second-largest speed win. |
| CodeRight | Adopt the existing `qa-e2e` config into `journeys.toml`; engine tests classified by D7. | Config and 5 scenarios already exist. |
| Pulse | `right-release qa` already runs rightkit-qa; map `qa/release/*` and `ui.rs` journeys into `journeys.toml`; drop `gate.sh`'s private target dir once D12 builds through the broker. | Nearest to the gate shape. |
| ScrapeRight | Un-ignore `native_journey` under the runner (it needs the interactive session the runner provides); wrap `sidecar-smoke` scripts as `web`/`cli` journeys. | Standard's pilot; cheap. |
| MailRight | Replace the mocked-IPC `qa-journeys.mjs` with native journeys or declare them `web` (standard D4); 7,554 unit tests → census. | 99 % compile: fix compile churn (§2.4) first or it stays slow regardless. |
| ViewRight | Map `qa-e2e` to `journeys.toml`; keep `right-release test` for vitest. | Small. |
| CutRight | Bring under the broker (today runs plain `cargo test`); the 62 ffmpeg engine tests become engine journeys with `serial_group = "media"` and budgets; the 32 UI flows need a built app bundle per host (manifest `artifacts`). | Media and bundle logistics make it late. |
| HeardRight | Last: the only live app; 95 % compile with 2.5-h roots; fix compile churn first, then journeys (`qa-e2e` + 23 node contract suites as `contract` class). | Risk and compile cost. |

Deviation from the standard's D10 (pilot ScrapeRight first): the measured speed problem is in the run-bound apps (PostRight, GenRight), so they go first; ScrapeRight stays the cheap pilot for the gate.

## 10. Assurance (proportional)

1. Rejection fixtures (standard §5 list) stay the acceptance for `journeys run`.
2. **Sidecar equivalence**: PostRight 212 × (cli, sidecar), identical verdicts, 3 runs.
3. **Parallel equivalence**: per app, 3 × serial vs 3 × parallel, identical verdict sets (QS-4).
4. **Broker isolation**: during a 12-worker journey run, a `cargo check` on another repo is admitted within its normal queue time (QS-3).
5. **Budget enforcement**: a scenario with a deliberate 3 s sleep fails after the calibration window with the step named.
6. **Gate**: a release with one release-critical journey `skipped` is refused by `right-release qa` (QS-6).
7. **Session**: on the Dell, a run started from session 0 ends `unknown`, not pass; from the desktop it passes (D11).
8. Oracle reviews each app's migration diff against its candidate list (D9); Sage reviews D12–D17 (this record) before any code.

## 11. Governance

- **ADRs** (record-worthy: hard to reverse, surprising without context, real trade-off): ADR-1 D12 (execute outside the broker); ADR-2 D13 (parallel by default, declared serial); ADR-3 D14 (persistent engine, fresh set); ADR-4 D15 (enforced budgets); ADR-5 D17 (blocking gate on one receipt). Alternatives and exits are in §7–8.
- **Ownership**: rightkit-qa (runner, pool, sidecar default, budgets, receipt timing, conformance check); @rightkit/qa (Node parity for web journeys); @rightkit/release (`right-release qa` blocking, floor mapping); rustright (#135 part 3, #116, #134 as independent fixes; no RUN lane); each app (manifest, adapters, test-mode engines, D7 tags).
- **Order of work**: (1) measure sidecar and Mac per-step cost; (2) rightkit-qa: pool + worlds + sidecar default + budgets + `journeys run` (one release, Sage/Oracle); (3) PostRight adoption + equivalence runs; (4) `right-release qa` blocking behind a date; (5) GenRight, CodeRight, Pulse, ScrapeRight, MailRight, ViewRight, CutRight, HeardRight.
- **Review triggers**: host change (re-calibrate budgets); attestation needs execution witnessed (add RUN lane); unit-tier needs per-test isolation (revisit nextest, C-3); Mac measurement contradicts the Dell numbers.
- **Debt carried**: `right-release qa` never-blocks until D17 lands; PostRight's aggregate `#[test]` kept beside the adapter until equivalence (standard D4 no-alteration rule); ~~`receipt.rs:1163` discards `unchecked`~~ **Retracted 2026-10-23:** `check_evidence` adds `EVIDENCE_MISMATCH` for an unreadable or mismatched evidence file and `RESULTS_UNVERIFIED` when no evidence root is supplied, and `Validation::passed()` requires zero problems, so such a receipt fails. The discarded `unchecked` list affects reporting only. No defect.
- **Terminal state**: proposed; nothing implemented; awaiting Adrian, then Sage + Oracle.
