# Why the e2e scenarios take hours, and the fix (RK Dell, 2026-10-23)

Adrian: "1,212 scripts should take 2-5 minutes, not an hour and a half. Where is the breakdown?"

## Evidence: PostRight run 40f7ad68 (Dell, broker req 096d6843)
- `cargo test -p postright-e2e --test scenarios -- --test-threads=1`: compile **2.26 s**, then a single `#[test]` (`every_scenario_passes_against_the_built_engine`) that loops over **212 scenarios**.
- The broker cancelled it at **3,344 s**, after **108/212** scenarios (a full run ≈ 1 h 50 min), holding 10 build units the whole time.
- Evidence dir `D:\.rightkit-managed\runs\postright\40f7ad68-…`: 1,531 engine CLI step logs in 3,338 s.
  - Steps per scenario: p50 11, max 46.
  - Gap between steps: p50 1.38 s, p90 2.98 s.
  - Quick steps total **2,324 s**; stalls over 10 s total **750 s**.
  - Top stalls: 132 s in `dispatch_stored_brief_to_editor_engine` step 23→24; 93 s mock-platform start; 73 s in `inbox_state` step 26→27; 50 s; 42 s.

## The four multipliers (each measured or read in code)

| # | Breakdown | Where | Cost here |
|---|---|---|---|
| 1 | **Strictly serial.** rightkit-qa's suite runner "runs scenarios sequentially" by design. PostRight wraps all 212 in one `#[test]`, so `--test-threads` can't help either. Each scenario already gets its own data dir and an ephemeral mock port, so they're independent. | `rightkit-qa/src/suite.rs:1-2`, `:417` loop; `postright/crates/e2e/tests/scenarios.rs` | 1× instead of N× cores |
| 2 | **A cold process per step.** Every `cli` step starts `postright-engine.exe` from zero: daemon mode init, crash hook, store lock (`backup::hold_shared`), store open, command, exit. On Windows, add process creation + a Defender scan of every launch. | `postright/crates/engine/src/main.rs:146-176` | ≈1.4 s × 1,531 = 36 min |
| 3 | **No speed budget.** A step may take up to the scenario limit (600 s default). Nothing fails a suite for being slow, so 132 s stalls go unnoticed. | `suite.rs` (`timeout_s` default 600) | 12.5 min of stalls |
| 4 | **Run inside `cargo test` inside the broker.** Runtime counts as build time and blocks other compiles (rustright#135). | broker receipt | blocks the lanes for the whole run |

The tools to avoid #2 already exist: the engine has a `--stdio` mode (`main.rs:170`), and rightkit-qa has a framed-stdio sidecar client (`engine.rs` sidecar, `command()` per request). The scenarios just use `cli` steps (a new process each) instead.

## The fix (shared, in rightkit-qa; every app gets it)
1. **Build once, in the broker:** `rightkit cargo build -p <engine> -p <mock> --features test-mode`. Record the binary sha256, source SHA, lock hash, features and toolchain in a build manifest. The broker's job ends there.
2. **Run outside the broker** with one entry point, `rightkit-qa run --app <app> --manifest <build manifest>` (the CLI already exists; make it the only path). No cargo and no build slot.
3. **Parallel scenarios by default:** a worker pool of N (default: physical cores − 2). Each scenario keeps its own data dir, port and mock process. Scenarios that need exclusivity (UI focus, Chrome, a real GUI session) declare `serial_group = "ui"`, and only those serialise.
4. **A persistent engine per scenario:** a `cli` step goes to a long-lived `--stdio` sidecar started once per scenario. That removes process start, store open and the Defender scan per step: milliseconds instead of ~1.4 s. A small, named set of scenarios keeps true process-per-call to cover the CLI entry itself.
5. **Hard speed budgets, enforced:** a step over 2 s fails as `SLOW` with its name (configurable per step, with a reason); the scenario budget is 30 s; the suite budget is 5 min wall. Failing the budget fails the gate. Every run reports p50/p95 per step kind.
6. **Evidence stays strict:** expected vs executed scenario IDs; a skip never counts as a pass; no retry-to-green; per-scenario evidence as today.
7. **Enforcement:** the release gate accepts an app's e2e result only as a rightkit-qa run receipt bound to the build manifest. An app crate that runs scenarios from its own `#[test]` loop (as `postright-e2e/tests/scenarios.rs` does now) fails a conformance check.

## Expected time (PostRight, Dell)
About 2,300 engine commands. Over a sidecar at a 20–50 ms round trip that's 1–2 min serial; with 8 workers, ≈15–30 s plus per-scenario setup (data dir, mock start). The 132 s and 93 s stalls must be fixed or they fail the new budgets. Target: **the whole suite in under 5 min, under 1 min warm.**

## Open questions for RK Mac
- Per-step cost on the Mac for the same suite: is the 1.4 s Windows-specific (Defender/process creation) or engine startup everywhere?
- The status of the per-app script inventory Adrian asked for: counts, run shape (process per step? serial?), measured wall time per app.
- Which apps' engines lack a stdio/sidecar mode.
