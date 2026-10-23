# RightKit pipeline architecture

The one path from a commit to a shipped package or app, for every repository that uses RightKit. Changed only by Adrian.
ADR-0001, ADR-0002 and ADR-0003 are decisions under this document: each serves invariants below and none may contradict
them. An invariant without an enforcing test is a gap, listed as one, never claimed as done.
`scripts/ci/pipeline-contract.test.mjs` fails if a row below names a test that does not exist, or names neither a test
nor a gap.

## 1. The pipeline

```
commit
  │
  ① CHECK     seconds · committed files only · no network, compile, install, broker or environment
  │            scripts/preflight.mjs (RightKit) → rightkit-gate check in @rightkit/release (every repo, 0.6.5)
  ▼
  ② BUILD     always local: the Mac and the Dell, warm cache, through the RustRight broker, public or private
  │            compile everything, compile the tests, run the fast unit tests; machine checks of this host
  ▼
  ③ QUALIFY   the exact artifact that ships, on both OSes
  │            public repository  → GitHub (publish.yml), dispatched only after ② is green for that commit
  │            private repository → locally, on the Mac and the Dell
  │            e2e journeys run here, built in the broker and run outside it
  ▼
  ④ RELEASE   upload exactly the qualified bytes; nothing is rebuilt
               RightKit crates and npm packages → registries · apps → right-release upload / publish-update
```

The only difference between a public and a private repository is where ③ runs. Every step reruns ① first: it costs
seconds and proves the rules held for exactly the bytes in hand, so no receipt has to carry that proof between steps.

## 2. The steps

**① Check.** One rule list. Each rule reads the committed files of one commit (or, for a compile, the tree on disk:
`--worktree`) and reports every finding in one run, each with rule, file, reason and fix. A rule that throws is a
FAIL; a question it cannot decide from files is "needs step 2", never a pass. Rules switch on by what the repository
contains: every repository (line endings vs `.gitattributes`, lockfile path versions, file-URL paths); RightKit
(consumer recipes G01, npm surface G02, version copies G03, dispatcher pins G05, consumer locks G08, generated docs
G10); apps (release config, `switches.json`, DevTools declaration, relative paths, no secrets or environment at load,
project type vs package root, declared host inputs, ownership fingerprints; 0.6.5). The one place it runs repository
code is RightKit checking itself with its own scripts.

**② Build.** Every compile goes through the broker (`rustright cargo|rustc|rustdoc`; plain cargo is refused). The
broker owns build slots per host. `cargo test` compiles the tests and runs only fast tests, under the test-run cap; a
Cargo lock wait lends its slots (ADR-0002). Machine checks run here, on each host, because one host cannot vouch for
another: broker attestation, protected trust and elevation, signing identity, notary credentials, host inputs with
their hashes, disk, feature closure (`cargo tree`), live registry freshness.

**③ Qualify.** RightKit crates and packages: `rk-pipeline dispatch` → `publish.yml`: `scans` (① again, then the
class A scans) → pack once → verify the sealed archive on Windows and macOS (Linux where `ci/linux-required.json` says)
→ consumer install → `qualified`. Locally: `right-release qualify cargo` on both hosts → `publish-reviewed.yml`. Apps:
`right-release build` on both hosts (signed, sealed) → `right-release qa` (journeys through `rightkit-qa journeys
run`) → admission. `@rightkit/release` also needs the canary: the old and new doctor against every app on both hosts.

**④ Release.** `publish-direct.yml` uploads a green qualification run's own sealed bytes; apps upload with
`right-release upload --release <id> --tier patch|update`. Approvals (recipe hashes, exceptions, `allowIgnored`,
broker enrollment) are reviewed by a person; no tool writes them.

## 3. Owners

| Box | Owner | Where |
|---|---|---|
| ① check, RightKit rules | RightKit | `scripts/preflight.mjs` |
| ① check, app rules; doctor; build, sign, package, QA admission, upload | `@rightkit/release` | `packages/release` (`right-release`, `rightkit-gate`) |
| ② compile slots, test-run cap, lock-wait lending, attestation | RustRight broker | `bogusyogi/rustright` |
| ③ e2e journeys outside the broker | `rightkit-qa` runner | `crates/rightkit-qa` (`journeys run`) |
| RightKit publication loop (check → compile → canary → dispatch → watch) | RightKit | `scripts/ci/rk-pipeline.mjs` |
| ③/④ hosted qualification and upload | RightKit | `.github/workflows/publish.yml`, `publish-direct.yml`, `publish-reviewed.yml` |
| Hook installation on the Mac and the Dell | workspace | `tools/setup-workspace.py`, `workspace-doctor` |

## 4. Interfaces

| Command | Step | Contract |
|---|---|---|
| `node scripts/preflight.mjs [--repo <dir>] [--commit <rev> \| --worktree] [--only <rule,...>] [--list]` | ① | Exit 0 pass, 1 findings, 2 could not run. Each finding is `FAIL <rule> (<Gnn>): <problem>`, then `     fix: <change>`; last line `preflight: …`. Never writes to the repository. |
| `.githooks/pre-push` | ① | preflight, then `scripts/local-gate.mjs`; a FAIL stops the push. Early feedback only: a hook can be skipped. |
| `node scripts/ci/rk-pipeline.mjs preflight <unit...>` / `loop` | ①→② | Check of HEAD first; red starts no canary, compile or dispatch. Then `rk-preflight` / `rk-npm-preflight` per unit through the broker; receipts per unit and SHA. |
| `node scripts/ci/rk-pipeline.mjs dispatch <unit...>` | ③ | Only with a green local receipt for HEAD, HEAD on origin; through the digest-pinned `rightkit-dispatch` adapter. |
| `publish.yml` | ①→③ | `guard` → `scans` (preflight first) → `plan` → build lanes → `pack` → `verify` (windows-2025, xcode-27) → `verify-linux` → `qualified`. Every job needs `scans`. |
| `ci.yml` (push, pull request) | ① | `plan` runs preflight first; every other job needs `plan`; native jobs run only on a broker dispatch. |
| `right-release doctor [--platform mac\|win] [--gates]` | ①+② | The build's own checks for one app, reported as rows. |
| `right-release build` / `dev` / `qa` / `upload` | ②–④ | Build is upload-free, upload is build-free; upload needs an explicit tier. |
| `rightkit-qa journeys run` | ③ | Builds through the broker, runs outside it; a qualifying run refuses `--build`, `--build-ui`, `--kill-orphans`. |

## 5. Invariants

| ID | Invariant | Enforced by | Status | Gap |
|---|---|---|---|---|
| P1 | (hard) Nothing compiles, canaries or qualifies before the check passes for exactly those bytes. | `scripts/ci/pipeline-contract.test.mjs` "P1: the pre-push hook runs preflight before the gate"; `scripts/ci/pipeline-contract.test.mjs` "P1: ci.yml runs preflight first in plan, and every other job needs plan"; `scripts/ci/pipeline-contract.test.mjs` "P1: publish.yml runs preflight in scans before installing any tool"; `scripts/ci/rk-pipeline.test.mjs` "P1: a red check stops the loop before any canary, pre-flight or dispatch"; `scripts/ci/rk-pipeline.test.mjs` "P1: pre-flight runs the check first and compiles nothing when it is red"; `scripts/ci/rk-pipeline.test.mjs` "P1: a direct canary runs the check first and starts no canary when it is red"; `scripts/ci/rk-pipeline.test.mjs` "P1: a direct dispatch runs the check first and dispatches nothing when it is red"; `scripts/ci/rk-pipeline.test.mjs` "P1: the check reads the captured commit, and HEAD moving while it runs starts nothing"; `scripts/ci/rk-release-canary.test.mjs` "runCanary --sha: a HEAD other than the checked commit is refused" | partial | The broker does not yet refuse a tree that has not passed the check (RustRight: run a pinned check of the request's tree, `--worktree`, cached per tree id), so a direct `rustright cargo` call skips it. Apps: `right-release build`, `dev` and `doctor` run no check yet (app rules, 0.6.5). The hook runs only where `core.hooksPath` is set. |
| P2 | The check reads committed files only: no network, environment, install, compile or broker. | `scripts/preflight.test.mjs` "P2: preflight on this checkout needs no environment or install and never touches the network"; `scripts/preflight.test.mjs` "P2: --repo checks another repository's committed files, not its working tree" | enforced | — |
| P3 | Every finding in one run, with file and fix; a check that throws is a finding, never a pass and never a crash. | `scripts/preflight.test.mjs` "a rule that throws is a FAIL, never a pass" | partial | `right-release doctor` and the build stop at the first problem (37 early exits in `packages/release/release.mjs` before the doctor's rows; `evaluatePrebuildGates` returns at its first finding) and crash on some inputs (HeardRight, Pulse, ScrapeRight canary cells). 0.6.4. |
| P4 | Native compile happens locally first; GitHub compiles natively only when ③ dispatches a commit with a green local receipt. | `scripts/ci/pipeline-contract.test.mjs` "P4: ci.yml compiles natively only on a broker dispatch"; `scripts/ci/rightkit-dispatch.test.mjs` "default admission refuses direct adapter use before POST when no valid local receipt exists" | enforced | — |
| P5 | Qualification tests the exact artifact that ships, on both OSes. | `scripts/ci/publish.test.mjs` "both hosts require strict passing scan proof for exact sealed bytes" | partial | Apps: release QA admission warns and never refuses; the refuse switch is in `@rightkit/release` "Unreleased" (ADR-0001 W4-A), unpublished. Only PostRight's journeys run on the shared runner (pilot). |
| P6 | Only qualified bytes ship; nothing is rebuilt after qualification. | `scripts/ci/reviewed-candidate.test.mjs` "a green qualification run publishes its own sealed bytes directly: no candidate, no approval pin, no reviewed dispatch"; `scripts/ci/publish.test.mjs` "upload refuses when the sealed bytes, the commit or the receipts do not bind"; `scripts/ci/publish.test.mjs` "upload refuses a registry that serves different bytes than the sealed tarball" | partial | Apps: the `right-release upload` sealed-bytes refusals are not yet linked here. |
| P7 | (hard) No e2e through `cargo test`; tests never hold a broker slot beyond the test-run cap; journeys build in the broker and run outside it. | `crates/rightkit-qa/src/journeys/run.rs` "fn qualifying_mode_refuses_build_build_ui_and_kill_orphans" | partial | The test-run cap ships off and lists no repository (ADR-0002 D-5/D-8; switch on per repository once its journeys run on the runner). No check rule yet refuses an app test target that drives `rightkit_qa` (PostRight `crates/e2e/tests/scenarios.rs` does today). `rightkit-qa` 0.8.0 is unpublished; ADR-0002's Unix suites have not run on the Mac. |
| P8 | One release tool and one config shape for every product: a data config valid against one schema; no app-level release, signing or packaging code. | `packages/release/right-suite-contract.test.mjs` "follows the signed tiered Right Release contract" | partial | Release configs are code: 11 `.mjs` configs compute versions, absolute paths, environment reads and secrets; there is no schema; `desktop-app-template` carries the same patterns. 0.6.5. |
| P9 | Copies are derived and drift fails the check; approvals are reviewed by a person; the check never writes. | `scripts/preflight.test.mjs` "version-pins refuses a crate's dispatch exception left at the old version"; `scripts/preflight.test.mjs` "consumer-locks refuses a consumer lock behind its path crate and ignores ungated locks"; `scripts/preflight.test.mjs` "dispatch-pins refuses a changed helper and a pin it cannot map to a file"; `scripts/preflight.test.mjs` "G01: a changed consumer recipe fails with the file and its committed hash"; `scripts/preflight.test.mjs` "G02: an export missing from importClosures fails"; `scripts/preflight.test.mjs` "P9: it never writes to the checkout"; `scripts/preflight.test.mjs` "G01: a unit about to publish is checked even when its inventory entry and recipe are deleted"; `scripts/preflight.test.mjs` "G01: recipe coverage is explicit" | partial | No bump command: the version feed, session copy, consumer pins, lock path entries and `INDEX.md` are edited by hand. Broker enrollment (`ci/rk-pipeline.json` `attestation`) is a separate manual commit, not part of installing a broker. Recipe coverage is explicit, not complete: 38 publishable units (26 crates, 12 npm packages) have no reviewed consumer recipe and are listed in `ci/recipe-coverage.json`; publication refuses each until its recipe lands (work item 13). |
| P10 | `@rightkit/release` ships last: only after the canary is green for that exact SHA and version, on both hosts, across every app; compatibility, readiness and coverage reported separately. | `scripts/ci/rk-pipeline.test.mjs` "loop: the canary runs before pre-flight for @rightkit/release only"; `scripts/ci/rk-npm-preflight.test.mjs` "@rightkit/release pre-flight: a green canary for this SHA, version, scripts and expected file passes" | partial | The verdict mixes compatibility and readiness (`scripts/ci/rk-release-canary.mjs` `compareCells`: a baseline that cannot run `--gates` turns every candidate gate row into a block; a same-signature fatal is only reported). Coverage is not a separate result. |
| P11 | One client name: every compile, status, storage and owns call names `rustright`; the retired `rightkit` alias (rustright#91) appears nowhere outside history, and is refused where it would run. | `scripts/preflight.test.mjs` "P11: retired-alias finds every invocation form and spares history, generated files and look-alikes"; `scripts/preflight.test.mjs` "P11: retireAliasText rewrites to rustright and is idempotent"; `packages/build-control-policy/src/index.test.mjs` "rustright is the only sanctioned managed form; the retired rightkit name is refused by name"; `crates/rightkit-qa/src/journeys/build_record.rs` "retired name of the RustRight client"; `scripts/preflight.test.mjs` "P11: retired-alias finds the forms the first sweep missed" | partial | The published `@rightkit/release` (0.6.2 and 0.6.3, in every app's `node_modules`) still runs `rightkit` itself: progress control, `storage xcode-args`, sidecar staging, the drift check and the gate. Retiring the alias therefore waits for a published release that names `rustright` and every app pinned to it, not only for the source branches; GenRight's release config moves with that bump. The installed broker forwards `rightkit` until then. Generated `AGENTS.md` files keep the old name until the workspace source `docs/agent-rules/workspace.md` is edited and `manage.py sync` runs. |

## 6. What an app never does

An app declares; RightKit does. An app has no release, signing, packaging, installer or updater code; no build path that
does not go through the broker; no e2e harness under `cargo test`; no copy of a RightKit version, hash or generated file
written by hand; no environment read, secret or absolute path in its release config; no QA, test, mock or dev-control
code in a release build, behind a flag or otherwise.

## 7. Decisions under this document

| Decision | Serves | Where |
|---|---|---|
| ADR-0001: one RightKit runner for end-to-end proof; build in the broker, run outside it | P5, P7 | `docs/design/ADR-0001-shared-e2e-runner.md` |
| ADR-0002: broker lock-wait lending and per-repository test-run cap | P7 | rustright `origin/main` `dc95191..78ad8c5`; the ADR text is not committed (Dell `D:\scratch\rk-plan`, Mac `~/scratch/rk-dell-handoff/adr-0002`) |
| ADR-0003: release readiness (one definition of releasable, derived copies, enforcement) | P1, P3, P8, P9, P10 | not committed (Mac draft, corrected by Oracle) |
| Scan-gaps report, 2026-10-23 (G01–G13, the 54 later-stage refusals) | P1, P2, P9 | `/Volumes/D/scratch/scan-gaps/astra-report.md` |
| Release-readiness review, 2026-10-24 (app findings) | P3, P8, P10 | `/Volumes/D/scratch/release-readiness-review-2026-10-24.md` |

## 8. Work, in order

Each item closes a gap in section 5; file each as a GitHub issue in its owning repository.

1. Merge the preflight branch to `main` (P1, P2, P9 for RightKit).
2. `right-release doctor` and the build: every finding in one run, never a crash (P3). In 0.6.4.
3. Apps fix their full findings from the release-readiness review; repair the Dell's shared elevation path once (P3, P10).
4. Canary: compatibility, readiness and coverage as separate results (P10); one clean run; publish 0.6.4.
5. RustRight: the broker refuses a compile of a tree that has not passed the check (P1). An ADR-0002 addendum.
6. Broker installs carry their own re-enrollment (P9), before any host installs the ADR-0002 broker.
7. Publish `rightkit-qa` 0.8.0 and `rightkit-child-process` 0.5.5; move each app's journeys to the runner; then switch
   the test-run cap on per repository; add the check rule that refuses e2e under `cargo test` (P7).
8. `rightkit-gate check` with the app rules; data release configs, the schema and `right-release config export`;
   `desktop-app-template` first (P1, P8). 0.6.5.
9. `right-release bump` writes every copy (P9).
10. Hooks through `setup-workspace` and `workspace-doctor`, and each repository's cloud-session start (P1, early feedback).
11. Commit ADR-0002 and ADR-0003 next to ADR-0001 so section 7 points into the repository.
12. Land the `rightkit` retirement (P11), in this order: every repository's branch (callers say `rustright`; the alias still
    forwards, so these are safe first); the workspace `docs/agent-rules/workspace.md` edit and `manage.py sync`; a green canary
    and a published `@rightkit/release` whose own commands name `rustright`; every app's pin moved to it (GenRight's stage
    `cmd` with it), verified on the Mac and the Dell; only then rustright's alias removal, a reinstall and re-enrollment
    (`ci/rk-pipeline.json` `attestation`) on both hosts.
13. Write a reviewed consumer recipe for each unit in `ci/recipe-coverage.json` (26 crates, 12 npm packages), each in the
    commit that takes it off the list (G01). A unit about to publish needs its recipe first, whatever the list says.

## 9. Open decisions (Adrian)

1. Split the release hold: publish `rightkit-qa` 0.8.0 and `rightkit-child-process` 0.5.5 ahead of `@rightkit/release`.
2. 0.6.4 scope: keep ADR-0001's QA admission (now in `@rightkit/release` "Unreleased" on `main`) in 0.6.4, or move it
   to 0.6.5. Either way the canary reruns on exactly what publishes.
3. When each repository's test-run cap switches on.

## Changing this document

Only Adrian changes an invariant. An ADR that changes one edits section 5 in the same commit, and the change must keep
`scripts/ci/pipeline-contract.test.mjs` green: every row names an existing test, or a gap.
