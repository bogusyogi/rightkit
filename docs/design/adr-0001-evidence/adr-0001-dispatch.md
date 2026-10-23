# Dispatch plan: ADR-0001 (shared e2e runner) + ADR-0002 (broker locks and QA target)

Plan only, 2026-10-23. Nothing here is started. Rules that bind every assignment:
- **Governance:** each change ships as an uncommitted patch to its repository's integration owner, with Sage design sign-off before coding and Oracle review before commit. No release without Adrian. Tests stay stopped until Adrian's go; workers compile only, never run tests.
- **Workers:** fresh context, edit-only, `fork_turns: "none"`. No cargo/pnpm runs, no builds, no commits, no branches, no worktrees. Reads may overlap; writes never do. Each reports changed paths, source-level result and blockers; no command-based claims.
- **Integration owners (sole checkers, one verification batch per repo after a wave's coding ends):** rightkit → RK Mac (`main`); rustright → Rustright Mac (Mac broker) with Rustright Win for the Dell install; each app → its app chat; Dell legs → RK Dell.
- **Evidence pointers:** `D:\scratch\rk-plan\test-arch-dell.md` (D2 record, Dell), `D:\Downloads\ADR-0001-shared-e2e-runner.md`, `/Volumes/D/scratch/rk-plan/e2e-standard-amendment.md` (Mac), `rightkit/docs/E2E-TESTING-STANDARD.md`, PostRight run `40f7ad68` evidence, Dell receipt scripts in the Dell scratchpad.

Tier markers: **[S]** = `legion:alchemist-specialist` (security, process containment, privileged or OS code, spec-bound protocol, concurrency); unmarked or **[M]** = `legion:alchemist`.

Dependencies below are derived from consumed outputs and shared writes only. Work in different repositories with no consumed output runs in parallel.

## Wave 0: relief (almost no new code; ADR order step 0)

| ID | Repo / owner | Assignment | OWN | READ | FORBIDDEN | Done when |
|---|---|---|---|---|---|---|
| W0-A | postright / PostRight chat | Package scripts build the e2e binary in the broker (`rightkit cargo test -p postright-e2e --no-run --message-format=json`) and run it outside (`scripts/run-e2e.mjs`: parse `compiler-artifact`, exec the binary from the desktop session, Windows via `rightkit-qa-interactive`). The aggregate `#[test]` stays. | `package.json` scripts, new `scripts/run-e2e.mjs` | `crates/e2e/tests/scenarios.rs`, `rightkit/scripts/run-ui-tests.sh` (precedent) | any scenario file, engine source | `pnpm test:e2e` never holds a compile permit past `Finished` |
| W0-B | postright / PostRight chat | `cli` steps served by the existing `--stdio` sidecar: `SuiteHooks` setup starts one sidecar per scenario (`rightkit_qa::engine::Sidecar`), the `cli` handler routes commands to it; scenarios listed in a `fresh` allowlist (daemon start/stop, `backup::hold_shared`, crash hook) keep process-per-call. No scenario TOML changes. | `crates/e2e/tests/scenarios.rs` hooks section (~L2417–2526), new `crates/e2e/src/sidecar_bridge.rs` | `rightkit-qa/src/engine.rs`, `crates/engine/src/main.rs:146-176` | `tests/scenarios/*.toml`, engine code | equivalence ready: same 212 verdicts cold vs sidecar (run by owner after the go) |
| W0-C **[S]** | rustright / **Rustright Win** (authored; Rustright Mac integrates `main`) | ADR-0002 part 1: `cargo test` roots get a run cap (`testRunCapMs`, measured from the end of compile); exceeding → kill tree, typed `RK_TEST_RUN_CAP`, exit 124, slot freed; `testRun` receipt field incl. `startDetected`. Sage W0-C verdict (`D:\scratch\rk-plan\w0c-sage-verdict.md`, APPROVE WITH CHANGES) applied to the patch: cap check above the `stall_quiet == 0` early return; ANSI-safe, backtick-independent `Finished` detection plus JSON `build-finished`; fail-open cases documented (`-q`, json, locale, nextest, `cargo --manifest-path X test` subcommand parse); tests for cap 0 → no `testRun`, under-cap → `capped:false`. | `crates/rustright-config/src/config.rs` + `config_tests.rs`, `crates/rustright-broker/src/job_service/{service.rs, job.rs, mod.rs, test_run.rs (new), tests.rs}`, `docs/using-the-broker.md` (patch `D:\scratch\rk-plan\w0c-testRunCap.patch`) | `admission.rs` | scheduler lanes; `rustright-policy` | Oracle on the patch; **rollout:** W1-D sets `testRunCapMs` explicitly per repo, or the cap lands only after the `--workspace` test runs have left the broker (GenRight's in-broker e2e would be killed by a 600 s default) |

W0-A and W0-B share `scenarios.rs` → **same worker or sequential** (B after A). W0-C independent, specialist tier (`legion:alchemist-specialist`, broker process control); W0-A/B are `legion:alchemist`. PostRight's suite wrapper must also check `status != Passed` (it fails only on `Failed` today, postright scenarios.rs:2566); that edit goes with W0-B in `scenarios.rs`.

## Wave 1: ADR-0002 broker (Rustright), parallel with Wave 2

| ID | Owner | Assignment | OWN | FORBIDDEN | Done when |
|---|---|---|---|---|---|
| W1-A | Rustright Mac | Locks before permit: a root acquires workspace + repository exclusivity **before** compile units are granted; `lockWaitMs` reported under queue, not run. | `rustright-scheduler/src/scheduler.rs` (~L157–254), `job_service/service.rs` admission path (~L3290) | lane enum, receipts schema beyond the new field | scheduler tests: a root blocked on a lock holds 0 units; receipt `lockWaitMs` is queue time |
| W1-B | Rustright Mac | QA target world: target generation keyed on (workspace, profile, **feature set**) so `--features <qa set>` never evicts the dev generation; cap accounting unchanged. | target-dir / generation code (`service.rs` ~L3972 `--target-dir` injection, generation retention) | admission costs | a feature-set flip reuses its own warm generation (receipt `freshness`) |
| W1-C | Rustright Mac | Package cache: QA builds whose lockfile is unchanged run `--locked --offline` after one fetch per lockfile hash; measure "Blocking waiting for file lock on package cache" before/after. | fetch/offline handling in the cargo adapter | `cargo-home` layout | before/after counts in two receipts |
| W1-D | Rustright Win | Dell install of the resulting broker + `config.json` keys (`testRunCapMs`, generation key); verify `rustright status` shows the new fields. | `D:\.rightkit-managed\config.json`, runtime install | source | Dell receipt shows the fields |

W1-D consumes W0-C/W1-A..C outputs. A–C are separate files: parallel if the owner splits workers; one integration batch.

## Wave 2: rightkit-qa core (one release; Sage gate on ADR-0001 passed 2026-10-23, rulings D-a..D-e frozen in the ADR)

Repository `rightkit`, integration owner RK Mac. All in `crates/rightkit-qa` unless stated; writes partitioned by file. Tier is named per assignment: **[S]** = `legion:alchemist-specialist` (a mistake would still compile or pass but be wrong or unsafe), **[M]** = `legion:alchemist`.

**Shared-file owners (one owner each, no exceptions):**
- `src/bin/rightkit-qa.rs`, `src/lib.rs`, `src/journeys/mod.rs` → **A**. A's first deliverable is the wiring for the whole wave: empty module declarations (`pool`, `worker`, `steps`, `plugin`, `bridge`, `lint`, `budget`) and subcommand/flag stubs for B..I, so no other assignment edits these three files. Wire names stay as they are on the wire: `Missing` (journeys/mod.rs:114-120; adapter.rs:10) is not renamed (D-d).
- `src/suite.rs` (including `Outcome`, suite.rs:104) → **B**. B adds the Outcome fields C, D, G and F need in its first pass (`engine_starts`, `survivors`, `timing`, `tokens_held`); C/D/G/F fill them from their own files and never edit `suite.rs`.
- `src/evidence.rs` `CheckStatus` (evidence.rs:12-16) → **F1**, which lands before B, C and D start consuming it.

**Fixed orders:**
- `scenario.rs`: **B → C → D → E → G**, strictly sequential (E's `exec` hook at scenario.rs:684-688; D and E rely on C's sidecar routing at scenario.rs:690-693, 713-735).
- `manifest.rs`: **B → G → H → I**. B adds E's fields (plugin declaration, `runner = "scenario"`) in its manifest pass, so E never edits `manifest.rs`.
- `CheckStatus` (F1) before B, C and D.
- `rightkit-child-process` job-pid API (W2-0) published before D starts.

| ID | Assignment | OWN | FORBIDDEN | Done when |
|---|---|---|---|---|
| W2-0 **job-pid API** [S] (repo `rightkit`, crate `rightkit-child-process`) | Add pid enumeration to `WindowsJob` (rightkit-child-process/src/windows_job.rs:206-280 has none) so a worker can record job members as survivors before cleanup. Published as a new crate version first; D consumes the published version. | `crates/rightkit-child-process/src/windows_job.rs` and its public API | everything in `rightkit-qa` | fixture on Windows: members of a job, including a descendant started with `CREATE_BREAKAWAY_FROM_JOB` denied, are listed; version published before D starts |
| W2-F1 **CheckStatus** [S] | Extend `CheckStatus` (evidence.rs:12-16) with `errored` and `not_run` (wire name stays `missing`, D-d). Only `passed` counts. | `src/evidence.rs` | any other file | existing receipt fixtures still validate; `errored`/`not_run`/`skipped` never count as passing |
| W2-A **build record, launch checks, wiring** [S] | `journeys run` steps 1-3: select → source identity → broker build via `rightkit cargo … --no-run --message-format=json`; parse `compiler-artifact`; sha256 + copy binaries into the run store; write `build-record.json` (paths, sha256, tree, lock hashes, features, toolchain, broker request id, attested exit 0). At launch: re-hash and refuse a missing QA marker (binary.rs:54, 167). Qualifying runs refuse `--build` and `--build-ui` (engine.rs:757, suite.rs:272). Required journey IDs per platform are selected up front (suite.rs:420-468). Owns the shared wiring files above. | `src/journeys/run.rs` (new), `src/journeys/build_record.rs` (new), `src/bin/rightkit-qa.rs`, `src/lib.rs`, `src/journeys/mod.rs`, `src/binary.rs` launch checks | `suite.rs`, `scenario.rs`, `receipt.rs` | fixtures: real broker passthrough; broker id and attested exit recorded; record goes stale on a feature or lockfile change; tamper after build refused at launch; missing marker refused; tree change after build → `source_mismatch`. **Open:** the source of the broker request id is unknown; A reports it as a blocker if the broker output does not expose it. |
| W2-B **pool + worlds** [S] | Replace the sequential loop (`suite.rs:417`) with a work queue; N from `[host] workers` else min(cores, commit headroom ÷ configured per-scenario peak); per-scenario world (data dir, port range, env, control socket, credentials via D-a); resource tokens `focus|audio|gui-session|clipboard|credentials` as locks; `journeys check` refuses undeclared machine-wide state. The in-process `run_suite` stays serial under `native.lock` until W3-D (D-a); the pool runs scenarios through D's worker subprocess. Owns the `Outcome` fields and the manifest fields for E. | `src/suite.rs`, new `src/pool.rs`, `src/scenario.rs` world fields, `src/journeys/manifest.rs` token and plugin fields | `engine.rs`, `receipt.rs` | fixtures: 8 no-op scenarios complete in parallel with distinct worlds; two `focus` scenarios never overlap; teardown of one world never touches another running concurrently; worlds are read from the child's env; a panic releases its tokens |
| W2-C **engine per scenario** [M] | `cli` steps route to a per-scenario `--stdio` sidecar; `lifecycle = "cold"` with reason, plus `cold_cli`, `restart`, `kill` steps; engine-start counter in the scenario outcome; parity helper that runs a command cold and in-session. | `src/engine.rs`, `src/scenario.rs` (step dispatch ~L690–735) | `suite.rs` | fixture engine: start count = 1 + declared restarts; >1 undeclared fails; negative parity (a differing cold vs in-session result fails); a sidecar crash → `errored`; a role change without restart is refused |
| W2-D **supervised worker** [S] | A worker subprocess per scenario (D-a): one job, one per-scenario tracker (suite.rs:358; process.rs:337-345), one memory credential store. Windows no-breakaway Job Object using W2-0's pid API; Unix: a new `ctx.spawn` API (`Ctx` scenario.rs:291 has none; `Tracker::register` process.rs:310 is manual) through which every spawn passes; macOS `open` pid-tracked by bundle + start time; world marker env plus same-user scan plus descendants of registered roots (D-c); `run_owned` survivors recorded (process.rs:471-487); Windows fingerprint fixed (process.rs:521-525); survivors recorded before cleanup, any survivor fails; qualifying runs refuse `--kill-orphans`; hard step/scenario deadlines → `errored`; the runner refuses to launch apps from inside `cargo test` (moved here from H). | `src/process.rs`, `src/guard.rs`, new `src/worker.rs`, `src/scenario.rs` teardown (~L428–440) | pool scheduling, `suite.rs` | **first item: marker readability probe on the Mac (D-c)**, run by the root owner before the rest. Fixtures on real OS processes: double-fork/`setsid` descendant and (Windows) `CREATE_BREAKAWAY_FROM_JOB` are recorded and fail the scenario; a deadline overrun → `errored`; `--kill-orphans` refused; a launch from inside `cargo test` refused |
| W2-E **built-in steps + plugin** [S] | Generic steps become built-ins (`http`, `engine_log`, `start_daemon`/`stop_daemon`, `mock_admin`, `secret_absent`); per-app step plugin over the new step-plugin protocol v1 (D-b: JSON lines over stdio, hello/setup/step/teardown/spawn/spawn_result/shutdown; one persistent process per scenario inside the job; verdict fields ignored; `runner = "scenario"`). Setup and teardown run through the plugin (postright scenarios.rs:2472-2510). `spawn` requests are started and tracked by the runner (stdin/stdout/pid/wait/kill, as postright scenarios.rs:2205-2225). The exec hook at scenario.rs:684-688 is E's insertion point. | new `src/steps/*.rs`, new `src/plugin.rs`, `src/scenario.rs` exec hook (after D) | `suite.rs`, `engine.rs`, `manifest.rs`, `journeys/adapter.rs` (v1 stays as is) | fixture plugin: setup/step/teardown round-trip; verdict fields in plugin messages ignored; plugin crash → `errored`; a plugin `spawn` request is tracked by the runner and supports stdin, stdout, pid, wait and kill |
| W2-F2 **truthful receipt + control token** [S] | Receipt gains `timing {per_kind p50/p95/max, slowest[10]}`, `engine_starts`, `survivors`, `tokens_held`; isolation recorded from the actual launch env; run token (D-e: user-only file in the world dir) checked in the embedded server, not `control.rs`; `explore` mode emits a recorded TOML, never a receipt. | `src/journeys/receipt.rs`, `src/embedded/mod.rs`, `src/in_app.rs`, `src/control.rs` (client side) | `suite.rs` | validate-receipt fixtures per new field; tested against the embedded server: token-less control order refused; `errored`/`not_run`/`skipped` never count; a missing required journey ID fails. **Re-pin note:** the embedded server is compiled into each app, so the token check reaches an app only after that app re-pins the new `rightkit-qa`; apps stay on the old behavior until they do. |
| W2-G **budgets** [M] | `[budget]` per journey kind in `journeys.toml`; warn window (date in manifest), then enforce at 2× p95 per step kind per host; `SLOW` names the step. | new `src/journeys/budget.rs` (starts now); `src/journeys/manifest.rs` budget types and `src/scenario.rs` step timing applied at G's slot in the orders | receipt layout (W2-F2 owns) | fixture: a 3 s step warns in window, fails after |
| W2-H **guardrails** [M] | `journeys check`: refuses aggregate `#[test]` suite loops and undeclared tokens, and fails crates whose tests launch binaries; test-tag lint (`rk-test: pure|contract|pin #n`, `pure` heuristic, `pin` checked via `gh`, untagged count only falls from a frozen baseline); `journeys new`. The refusal to launch from inside `cargo test` is **not** H's; it is D's. | new `src/journeys/lint.rs` (starts now), `src/journeys/manifest.rs` check applied at H's slot | runtime code, `bin/rightkit-qa.rs`, `lib.rs`, `journeys/mod.rs` (A owns the wiring) | fixtures per refusal; `gh` unavailable fails closed; an untagged-count increase is refused |
| W2-I **Harness bridge** [M] | Runner executes a built test binary one worker per test function (`--list` → `--exact <name> --test-threads=1`) with structured result lines; per-app removal date in manifest. Owns the Harness run lock: the bridge takes over what `Harness::with_run_lock` does (harness.rs:313-317), serializing Harness apps across binaries. | new `src/journeys/bridge.rs`, `src/journeys/manifest.rs` removal-date field at I's slot | `harness.rs` semantics | fixtures: 4 tests run as 4 workers, results collected; an `--exact` filter matching zero tests and exiting 0 is `not_run`; an ignored test is not a pass; a panic is `failed`; two bridged binaries are serialized by the run lock |
| W2-J **docs** [M] | README, `docs/E2E-TESTING-STANDARD.md` amendment (ADR text incl. D-a..D-e), CHANGELOG, version bump proposal. | docs only | code | Oracle reads it against the code |

**Start status (after the Sage gate):**
- **Can start now:** A; G (`budget.rs` first); H (`lint.rs` first); J; the **B → C** chain (B first, then C on B's files); W2-0 (job-pid API; it is a prerequisite D needs, and independent of everything else) and W2-F1 (it must land before B/C/D consume `CheckStatus`).
- **D** waits for: D-b and D-c frozen (done by the ADR revision), W2-0 published, B and C landed (`scenario.rs` order), and D's own marker-readability probe on the Mac.
- **E** waits for D (`scenario.rs` order) and the frozen D-b protocol (done).
- **I** waits for H (`manifest.rs` order); the Harness run-lock ruling is settled (it is I's).
- **F2** waits for F1 and the frozen D-e; the app re-pin note applies at rollout.

Suggested worker split: {W2-0}, {F1, then F2}, {A}, {B, C, D, E, G in order}, {H}, {I}, {J}. One integration batch by RK Mac; Oracle on the whole release.

## Wave 3: PostRight pilot (consumes Wave 2)

| ID | Owner | Assignment | OWN | Done when |
|---|---|---|---|---|
| W3-A | PostRight chat | `journeys.toml` + `journeys-map.toml` over the 212 scenarios (`runner = "scenario"` with a step plugin, per D-b; not `adapter`), `[build]` = `postright-engine/test-mode` + mock platform, `[budget]`, `fresh` allowlist, tokens declared. | app root manifests | `journeys check` clean |
| W3-B | PostRight chat | Step plugin binary from the 12 hook step kinds (`scenarios.rs:2417–2526`), speaking step-plugin protocol v1 (D-b), built in the broker; no direct spawn (spawn requests go to the runner); setup/teardown from `scenarios.rs:2472-2510`. | new `crates/e2e/src/bin/step-plugin.rs`, hooks moved verbatim | round-trip fixture |
| W3-C | RK Mac + RK Dell | Equivalence and rejection runs (after Adrian's go): 3× serial vs 3× parallel; cold vs sidecar parity; rejection cases: wrong-platform receipt, ignored test, hook hang, detached descendant, concurrent state, dev-target substitution, stale build record. Targets: warm < 1 min, cold < 5 min, per host. | run evidence only | targets met on both hosts, or the review trigger fires |
| W3-D | PostRight chat | Delete the aggregate `#[test]` wrapper after W3-C passes (separate commit, Oracle). Until then the wrapper checks `status != Passed`, and the in-process `run_suite` stays serial under `native.lock` (D-a). | `crates/e2e/tests/scenarios.rs` | removal recorded |

## Wave 4: release admission and other owners (parallel with Wave 3 after Wave 2)

| ID | Repo / owner | Assignment | OWN | Done when |
|---|---|---|---|---|
| W4-A | rightkit / RK Mac | `right-release qa` hardening: receipt must match platform + release id; qualifying runner pinned by approved digest; every required journey id must pass (not one matching check); signing outside test execution; `admit()` gets a per-app policy switch warn→refuse recorded in protected policy (default warn). | `packages/release/release-qa-{command,receipt,audit}.mjs`, `release-qa-floor.json`, policy file | fixtures for each rule; refuse path tested |
| W4-B | rightkit / RK Mac | `@rightkit/qa` Node side: gate/census/map become reports over rightkit-qa evidence; `journeys run` orchestration marked deprecated with a removal date; byte-compatible receipt reader kept. | `packages/qa/src/journeys/*` | parity fixture Rust↔Node |
| W4-C | rightkit / RK Mac | Legion `run close` + push gate hook: fresh receipt for mapped journeys required (needs Legion owner's agreement first). | hook spec only until agreed | spec reviewed |
| W4-D | rightkit / RK Mac | Shared journey skill + `rightkit-qa journeys new` template docs. | `skills/`, README | exists, reviewed |

## Wave 5: app migrations (each after Wave 2 + its own Sage pass; order by measured gain and risk)

| Order | App / owner | Bridge or convert | App-specific work | Removal date |
|---|---|---|---|---|
| 1 | GenRight | bridge 178 `#[test]` journeys, then triage to TOML | `Sc::run` → sidecar (`fast_sidecar.rs` proves mode); find the `--test-threads=1` reason; engine serve mode exists | set at pilot |
| 2 | CodeRight | convert: existing `qa-e2e` config + 5 scenarios | engine tests tagged (D7); check `coderight` engine serve mode | — |
| 3 | Pulse | convert `qa/release/*` + `ui.rs`; `gate.sh` builds through the broker | drop private `PULSE_TEST_TARGET` | — |
| 4 | ScrapeRight | bridge `ui-e2e`; un-ignore `native_journey` under the runner; wrap `sidecar-smoke` as `web`/`cli` | — | — |
| 5 | MailRight | replace mocked-IPC `qa-journeys.mjs` (declare `web`) ; bridge `native-e2e` | 7,554 unit tests → census; compile churn first (W1) | — |
| 6 | ViewRight | convert `qa-e2e`; `right-release test` keeps vitest as unit tier | — (single-instance is already off in QA modes, viewright src-tauri/src/lib.rs:76-99) | — |
| 7 | CutRight | bring under the broker; 62 ffmpeg engine tests → engine journeys with `media` token; 32 UI flows need a built bundle per host | ffmpeg via built-in media step | — |
| 8 | HeardRight | last (live app): bridge `qa-e2e`; 23 node contract suites → `contract`; compile churn first | — | — |

Per app, one worker: manifests + bridge entries + tags; the app chat integrates; Oracle reviews the delete commit against the candidate list (standard D9). Apps run in parallel across chats; none touches another repo.

## Wave 6: cutover and installed lane

| ID | Owner | Assignment | Done when |
|---|---|---|---|
| W6-A | Adrian decides N; RK Mac records | Per-app `admit()` warn→refuse after N consecutive green, in-budget days on both hosts; only Adrian moves it back. | policy entry per app |
| W6-B | RK Mac / RK Dell | Installed-app smoke journeys against the sealed build via `rightkit-automation` on Tart (Mac) / Windows Sandbox or Hyper-V (Dell); before release only. Blocker: no Tart GUI-session executor yet (`tart.rs` is SSH-only). | first installed receipt on each host |
| W6-C | RK Mac | Retire the Node journeys runner after equivalence. | removed, CHANGELOG |

## Wave 7: other lanes (deferred, not started)

The vitest (React UI), iOS (`xcodebuild build-for-testing` / `test-without-building`) and Android (Gradle/Rust build, adb journeys) lanes of ADR-0001 item 12 are **explicitly deferred**: no assignment exists until the PostRight pilot (Wave 3) passes, then each lane gets its own Sage design pass and dispatch. They are not in Waves 0-6.

## Open items that gate waves (named, not assumed)
- Sage gate on ADR-0001: reviewed 2026-10-23; the ADR is revised and rulings D-a..D-e are recorded. Remaining before Wave 2 coding on a given assignment: Sage design sign-off per change (rightkit change gate) and, for D, the Mac marker-readability probe.
- **Disk.** Outside-broker test target dirs under /Volumes/D/scratch pushed free space below the broker's 200 GB storage floor (RK_STORAGE_FLOOR, 15:27Z). The runner's target dirs need a size cap or a cleanup after each run, managed by the runner (not ad hoc); decide the cap and cleanup rule before the first runner run (W2-A/W2-D).
- Unknown: the source of the broker request id that W2-A records.
- Adrian: the test go; N for W6-A; QA key distribution for `right-release qa`; any Defender change (measure first: Dell per-step 1.4–1.7 s vs Mac 0.2 s).
- Legion owner: W4-C hook.
- Measurements before budgets are enforced: sidecar per-command cost on both hosts; per-scenario commit peak for pool sizing (Dell headroom ~12 GB now, ~34 GB after reboot).

## Hosts
- **Mac:** Waves 1–2 integration, W3-C Mac leg, W6-B Tart.
- **Dell:** W1-D, W3-C Windows leg (Job Object containment, `rightkit-qa-interactive`, Windows reap #134 interplay), W6-B Sandbox, Windows legs of Wave 5 on request.
