# End-to-end testing standard and shared journey harness

Status: proposed 2026-10-21, revision 2 (Sage design; Architect charter D1, significant, objective `best_shape`). Revision 2 resolves Astra review findings 8 to 11 (adapter contract, macOS GUI execution, receipt proof, gate classification); decision numbering D1 to D11 is unchanged and amended in place. Not yet adopted; the lead commits after external adversarial review. Execution sections superseded by ADR-0001 (`docs/design/ADR-0001-shared-e2e-runner.md`, rev 3, Sage sign-off conditions applied 2026-10-23); section 8 maps D1 to D11 to what ADR-0001 keeps and changes.
Origin: Adrian, 2026-10-21: "for tests, each agent does it's own thing and we're getting thousands of small tests whereas we should have end to end tests, is this something that belongs in RK?" The lead answered yes; Adrian said do it now. Tracking issue: bogusyogi/rightkit (title "End-to-end testing standard and shared journey harness (Adrian 2026-10-21)").

## 1. Evidence (counts are `rg` line counts, 2026-10-21, primary checkouts, vendored `rk-snapshot` excluded)

| App | Rust `#[test]` | TS/JS `it(`/`test(` | Journey/e2e assets that exist today |
|---|---|---|---|
| heardright | 1699 | 468 | 3 `.scenario.toml` (all inside vendored `rk-snapshot` examples, none are the app's own); `tauri-app-next/scripts/run-contract-tests.mjs`; `qa:native:journey`, `qa:browser`, `qa:macos-installed` scripts (`heardright/docs/plans/2026-10-20-rightkit-adoption.md:119`) |
| mailright | 1224 | 220 | `scripts/qa-journeys.mjs` (16 hits of "journey"); it drives a Vite QA server with **mocked IPC** (`mailright/scripts/qa-journeys.mjs:3`), so it is not the real built app |
| scraperight | 544 | 114 | `crates/scraperight-ui-e2e/tests/native_journey.rs` (1 test); `scripts/sidecar-smoke/` incl. `installed-journeys.mjs` |
| postright | 303 | 0 | 221 `.scenario.toml` in `tests/scenarios/` run by `crates/e2e` through `suite::run_suite` (`postright/rightkit-qa.toml:1-9`). The only app already near the target shape. |

Reading: three of four apps carry 700 to 2100 narrow tests against at most a handful of real journeys. The harness to do better already exists and one app uses it; the missing pieces are a standard, a declared journey list, and a gate.

## 2. What RightKit already owns (extend, do not rebuild)

- `rightkit-qa` (Rust): declarative scenarios, engine and app control, run lock, isolated homes, mocks, hashed evidence, retention (`crates/rightkit-qa/README.md:1-3`, `:73-81`). Scenario TOML with `tier`, `target = "engine"|"ui"`, `[[step]]`, `[[step.expect]]` (`postright/tests/scenarios/accounts-store.scenario.toml:1-20`). Run kinds functional/visual (`README.md:84-92`). CLI `rightkit-qa run --build --build-ui` (`README.md:200-206`). Windows and macOS UI driving via the in-app control server (`README.md:162`, `:208`).
- Isolation is already runtime-gated: `userdata::Guard` runs before any launch, refuses real user folders, requires `RIGHTKIT_QA_DATA_DIR`, and engines need an isolated data dir (`README.md:111-129`). The control server is a compile-time feature (`control`) that never ships and is refused by `right-release` (`README.md:38`), but isolation is judged from the launch environment and paths, never from the build feature. This satisfies the workspace rule that installed dev builds are real user installs (`docs/agent-rules/workspace.md`, Releases section: strip QA code from release builds; dev builds run QA by runtime mode).
- `@rightkit/qa` + `@rightkit/qa-chrome` (Node): CDP page driver, dev-server launcher, evidence and ownership for browser surfaces (`packages/qa/README.md:1-12`, `crates/rightkit-qa/README.md:52-65`).
- `rightkit-qa-driver` is a deprecated shim; no new code goes there (`crates/rightkit-qa-driver/README.md:3`).

## 3. Decisions

**D1. The proof of an app is a declared list of user journeys run against the real built app in runtime QA mode on isolated data.**
Alternatives: (a) keep per-agent test discretion with a coverage percentage target; (b) mandate a test-count cap. Rejected: coverage rewards many small tests (the present problem) and a cap is arbitrary and gameable. A journey list is reviewable by a human and maps to what users do.

**D2. Extend `rightkit-qa` and `@rightkit/qa`; add no new crate or package.**
The journey manifest, `journeys` subcommands, the adapter protocol and the receipt live in `rightkit-qa` (new module `journeys`, with `journeys::adapter` for the app-side helper; new subcommands on the existing `rightkit-qa` binary). `@rightkit/qa` gains a `journeys` export with the same manifest reader, receipt validator and adapter helper, so Node runners emit and check byte-compatible records. Alternative: a new `rightkit-e2e` owner. Rejected: it would split run storage, isolation and evidence, which the two QA owners already share, and the ownership rule forbids a second owner for one concern. Run `rightkit owns <capability>` before coding to confirm no owner is missed (not run for this design; unknown). Verified today: no `journeys` module exists in `crates/rightkit-qa/src`.

**D3. One manifest per repo: `journeys.toml` at the app root, runner-agnostic.**
```toml
version = 1
app = "scraperight"
coverage_owners = ["bogusyogi"]    # logins whose issue comment may approve D8 coverage changes; baseline-protected

[[adapter]]                         # required only for runner = "adapter"
name = "postright-scenarios"
build = ["rightkit", "cargo", "test", "-p", "postright-e2e", "--test", "journeys_adapter", "--no-run", "--message-format=json"]
executable = { role = "adapter", cargo_target = "journeys_adapter" }   # test executable taken from cargo compiler-artifact messages
artifacts = [{ role = "engine", cargo_package = "postright-engine" }]   # other binaries the adapter launches; hashed and bound (D5)
args = []                           # extra argv after the executable; protocol env (D4) carries the rest

[[journey]]
id = "J-001"                      # immutable; never renumbered or reused
title = "User adds a source, runs a scrape, sees results"
platforms = ["macos", "windows"]  # where it must pass
surface = "native-ui"             # native-ui | web | engine | cli | installed
runner = "scenario"               # scenario (built-in adapter, default hooks only) | adapter (app-owned adapter, D4)
adapter = "postright-scenarios"   # required when runner = "adapter"
entry = "tests/scenarios/add-source-scrape.scenario.toml"   # path or selector the runner passes to the adapter; may be a directory or glob for a group
tier = "fast"                     # fast | full | paid (existing harness tiers)
data = "generated"                # generated | seeded (userdata::seed_from); never "real"
release_critical = true
```
Alternatives: extend `rightkit-qa.toml` (rejected: it is runner config, one per app, and journeys need identity and a verdict independent of which runner executes); a markdown list (rejected: not machine-checkable). Existing `.scenario.toml` files and Node journey modules are `entry` targets unchanged. An app whose scenarios run with default hooks uses `runner = "scenario"`; an app with its own step handlers, setup or teardown (PostRight) uses `runner = "adapter"`, because a manifest wrapper alone cannot reach those hooks (F8). A bare `cargo test <filter>` or `node script.mjs` is not a runner: it yields one aggregate exit code and no per-journey result, so it is removed from the `runner` set; it is only ever the inside of an adapter.

**D4. Runner and adapter contract.**
RightKit orchestrates: selection, isolation, build, launch, result collection and the receipt. The app's own suite runs its own handlers, setup and teardown, unchanged. The seam is one adapter contract (protocol version 1), implemented once in Rust (`rightkit_qa::journeys::adapter`) and once in Node (`@rightkit/qa/journeys`).

`rightkit-qa journeys check` validates the manifest, the mapping file (D8) and any adapter declaration only: schema, unique immutable IDs, entries exist, no `data = "real"`, every `adapter` reference resolves, every `native-ui` journey declares a real built-binary artifact, and no mocked-IPC dev server for `native-ui`. A Vite server with mocked IPC counts as `web`, not as proof of the app (mailright's current runner).

`rightkit-qa journeys run [--id J-001]... [--platform P] [--tier T] [--build-ui]` executes these steps in order; any failure ends the run with verdict `failed` or `incomplete` and a receipt (D5):
1. **Select.** Filter the manifest by id, platform and tier. Write `selection.json` into the managed run root (fresh `run_id` UUID and 128-bit hex `nonce`). Zero selected journeys is an error, never a pass.
2. **Source identity.** Record git `head`, `tree`, `dirty` (and `diff_sha256` when dirty) before the build.
3. **Build (broker).** Run the adapter's `build` argv through the broker (`rightkit cargo ...`). For Rust, parse `--message-format=json` `compiler-artifact` records for the declared `executable` and `artifacts`; no text scraping. Re-check the source tree after the build; a changed tree fails the run. This step never launches the app.
4. **Launch (D11).** Launch the built executable by the shared interactive-session launcher (never inside the broker), after re-hashing every artifact.
5. **Collect.** Read `results.jsonl`, validate it against the selection (D5 rules), write the receipt.

**Adapter invocation.** The runner starts `<executable> <args...>` with this environment, on top of the isolation environment produced by the existing guard (`RIGHTKIT_QA_DATA_DIR` etc., `README.md:111-129`) and runtime QA mode:

| Variable | Meaning |
|---|---|
| `RIGHTKIT_JOURNEYS_PROTOCOL` | `1` |
| `RIGHTKIT_JOURNEYS_SELECTION` | absolute path of `selection.json` (read-only to the adapter) |
| `RIGHTKIT_JOURNEYS_RESULTS` | absolute path of `results.jsonl`; the runner pre-creates it empty; only the adapter appends |

`selection.json`:
```json
{"protocol":1,"run_id":"<uuid>","nonce":"<32 hex>","app":"postright","platform":"windows","tier":"fast",
 "manifest_sha256":"<hex>","journeys":[{"id":"J-001","entry":"tests/scenarios/accounts-store.scenario.toml"}]}
```

The adapter must run exactly the selected journeys through the app's existing suite (for PostRight, `suite::run_suite` with the app's `SuiteHooks`: `SuiteOptions.only`/`files` select scenarios, `hooks()` installs the steps, setup and teardown at `postright/crates/e2e/tests/scenarios.rs:2528`), and append JSON lines (one object per line, UTF-8, flushed per line) to `RIGHTKIT_JOURNEYS_RESULTS`. stdout and stderr are app logs and carry no protocol. Every record repeats `run_id` and `nonce`; the runner discards the whole file if any record disagrees.

| Record | Fields (type) | When |
|---|---|---|
| `start` | `type:"start"`, `protocol:1`, `run_id`, `nonce`, `adapter` (string), `adapter_version` (string), `started_at` (RFC 3339), `artifacts` (array of `{role, path, sha256}` for every binary the adapter will launch, hashed at that moment), `isolation` (`{data_dir, guard:"passed", qa_mode:"runtime"}`) | first line, before any journey |
| `journey` | `type:"journey"`, `run_id`, `nonce`, `id` (selected journey id), `status` (`passed` \| `failed` \| `skipped` \| `errored`), `started_at`, `duration_ms` (integer), `units` (`{total, passed, failed, skipped}`, integers; scenarios or tests inside the journey), `evidence` (array of `{path, sha256, kind}`, `kind` = `log` \| `screenshot` \| `report` \| `trace`; paths relative to the run root), `reason` (string or null; required unless `passed`) | exactly once per selected id, after its teardown |
| `end` | `type:"end"`, `run_id`, `nonce`, `selected` (int), `reported` (int), `ended_at` | last line |

A journey `passed` only if `units.total >= 1`, `units.failed = 0`, `units.skipped = 0` and its own status mapping is 1:1 from the app's result (`CheckStatus` to `status`); a group entry with zero matching scenarios is `errored`, not `passed`. The Rust helper offers `adapter::Selection::read()`, `adapter::Reporter::{start, journey, end}` and `adapter::journey_from_outcomes(id, &[Outcome])`; the Node helper offers the same three calls. The adapter's exit code must be 0 when it completed the protocol, whether or not journeys failed; the runner derives pass or fail from records only.

**No-alteration rule.** The adapter wraps; it never rewrites. (a) It may not edit scenario files, hooks, steps, expectations or tolerances. (b) It may not retry, re-order to hide a failure, or convert `failed` to `skipped` or `passed`. (c) It may not filter by test name pattern; selection is by manifest `entry` only. (d) `hooks()` stays the single source of the app's handlers: if the existing test file cannot export it, the only permitted change is a move-only extraction into a shared module, reviewed by Oracle as byte-identical in body. (e) The app's existing aggregate test (PostRight `every_scenario_passes_against_the_built_engine`) keeps running unchanged beside the adapter, and the acceptance run compares adapter verdicts to it on the same tree.

**D5. Evidence and receipt (proof, not process).**
`journeys run` writes `journeys-receipt.json` into the run's managed evidence directory (existing run root, `README.md:81`). Schema `rightkit.journeys.receipt/1`; all hashes are lowercase hex SHA-256 unless stated.

| Field | Type | Meaning |
|---|---|---|
| `schema` | string | `"rightkit.journeys.receipt/1"` |
| `run_id`, `nonce` | string | echo of `selection.json`; every adapter record must match |
| `app` | string | manifest `app` |
| `kind` | `"functional"` \| `"visual"` | existing run kind |
| `verdict` | `"passed"` \| `"failed"` \| `"incomplete"` | computed only by the rules below |
| `started_at`, `ended_at` | string | RFC 3339 |
| `host` | object | `{os: "windows"\|"macos"\|"linux", arch: string, session: "interactive"\|"noninteractive"}` |
| `source` | object | `{head: 40-hex, tree: 40-hex (git tree of head), dirty: bool, diff_sha256: string\|null}`; `diff_sha256` is required when `dirty`; release signoff requires `dirty=false` |
| `manifest` | object | `{path, sha256}` |
| `mapping` | object\|null | `{path, sha256}` of the D8 mapping file; null while the gate is advisory |
| `build` | object | `{command: string[], source_tree: 40-hex, tree_after_build: 40-hex, started_at, ended_at, broker_receipt: {id, path, sha256}\|null, artifacts: [{role, path, size: int, sha256, produced_by: "broker_receipt"\|"build_output"}]}` |
| `launched` | array | `[{role, path, size: int, sha256}]`, the digests the runner measured immediately before launch AND the digests the adapter reported in its `start` record |
| `isolation` | object | `{data_dir, guard: "passed", qa_mode: "runtime", real_data_touched: false}` |
| `adapter` | object | `{name, version, protocol: 1, argv: string[], exit_code: int, results_sha256}` |
| `selection` | object | `{ids: string[], platform, tier}`; ids frozen before execution |
| `journeys` | array | one entry per selected id: `{id, status: "passed"\|"failed"\|"skipped"\|"errored"\|"missing", release_critical: bool, started_at, duration_ms: int, units: {total, passed, failed, skipped}, evidence: [{path, sha256, kind}], reason: string\|null}`; `missing` is runner-synthesized and never emitted by an adapter |
| `counts` | object | `{selected, executed, passed, failed, skipped, errored, missing, duplicate}`, all ints |
| `problems` | array | `[{code, id: string\|null, detail}]`; codes below |

**Provenance rule.** The claimed binary is bound to the claimed source, never merely found beside HEAD. The runner always builds in the run (step 3 of D4): `build.source_tree` must equal `source.tree` and `build.tree_after_build`, and `build.artifacts` must come from this run's cargo `compiler-artifact` output (`produced_by: "build_output"`) or from a broker build receipt whose own recorded source tree and artifact digests equal those values (`produced_by: "broker_receipt"`; the receipt is accepted only after those fields are checked, and where the broker receipt does not record tree and digests the field is `unknown` and the receipt is not accepted). Every `launched[*].sha256` must equal a `build.artifacts[*].sha256` for the same `role`; a launched binary that was not produced by the claimed tree fails with `artifact_not_from_build`.

**Verdict rule.** `verdict = "passed"` only if all hold; otherwise `failed` (a journey or binding failed) or `incomplete` (no usable result):
1. `counts.selected >= 1` and `counts.executed == counts.selected` (zero executed is never a pass; code `zero_executed`);
2. every selected id has exactly one `journey` record; none `missing` (`missing_result`), none duplicated (`duplicate_result`), none outside the selection (`unselected_result`);
3. every journey is `passed`; `skipped` is not a pass (`skipped`), `errored` and `failed` fail; a journey a platform does not declare is not selected, so skip is never a way to meet a platform requirement;
4. the `start` and `end` records exist, `end.selected == end.reported == counts.selected`, and all records carry the right `run_id` and `nonce` (`protocol_violation`, `stale_results`);
5. `launched` digests match `build.artifacts` and the adapter's reported digests (`artifact_mismatch`, `artifact_not_from_build`); `source`, `build` and `manifest` bindings agree (`source_mismatch`);
6. isolation was `passed` and no real-data path was touched (`isolation`);
7. `adapter.exit_code == 0` and `results_sha256` matches the file read (`adapter_exit`).

`journeys validate-receipt <file> [--tree <40-hex>] [--manifest <path>]` re-checks all rules offline against the receipt, the manifest and the evidence hashes, so the gate trusts neither the runner's prose nor the verdict field. A gate accepts a receipt only if it validates and its `source.tree`, `manifest.sha256` and artifact digests match the commit being claimed, except for carry-forward under D8. For release signoff every `release_critical` journey must be `passed` on every declared platform; a platform with no valid receipt is `unknown`.
Alternative: trust the process exit code. Rejected: no proof of which journeys ran, against which build.

**D6. CI versus local.** Public repos: CI runs journeys with `surface` engine, cli, web on Linux/Windows/macOS runners (compiler is CI only per workspace rules). Native-UI journeys need an interactive session (`README.md:162`, `:249`), so they run locally per host (private apps: Dell and Mac, broker-built dev profile, one build per app per host) and the receipt is the artifact the gate checks. A platform with no receipt is `unknown`, not pass. No journey ever runs against `/Applications` or Program Files unless the installed stage is explicitly selected (`README.md:186`, existing refusal). Alternative: run everything in CI. Rejected: not possible for hidden-window native UI on the hosts we have.

**D7. Unit tests: allowed classes.** A unit/integration test is accepted only if tagged with a one-line class comment `// rk-test: <class>`:
- `pure`: deterministic pure logic with no I/O (parsers, math, state machines) where enumerating inputs through a journey would be impractical;
- `contract`: a RightKit owner contract or a protocol/spec-bound format, run at the owner or at the app's adapter boundary;
- `pin #<issue>`: a regression pin for a bug that no journey can reach, naming the issue and why a journey cannot reach it.
Reviewers reject: a test per fix that restates a journey step; tests of private helpers; tests asserting call sequences or mocks; snapshot/golden tests with no named behavior; new tests with no tag; a feature or fix with no new or amended journey when the user-visible behavior changed. A bug fix for user-visible behavior adds or extends a journey first, then at most one `pin`.
Alternative: forbid unit tests. Rejected: pure logic and spec-bound formats are cheaper and sharper below the journey.

**D8. Census, then gate (advisory before blocking); the behavior map; coverage ownership.**
`rightkit-qa journeys census` counts tagged and untagged tests (Rust `#[test]`, JS `it/test`), journeys, and the ratio, and diffs against a base ref. It reports; it never classifies a change as user-visible (test counts cannot).

**Reviewed behavior-to-journey mapping.** `journeys-map.toml` at the app root (checked by `journeys check`, reviewed by the app owner and Oracle):
```toml
version = 1
[[behavior]]
id = "B-sources"                    # immutable
title = "Source management and scraping"
paths = ["crates/engine/src/sources/**", "ui/src/sources/**"]   # globs from the app root; a changed file matching one is a user-visible change
journeys = ["J-001", "J-004"]       # journeys that cover it; at least one, all must exist in journeys.toml
[[exempt]]
paths = ["docs/**", "**/*.md", "**/tests/**", "scripts/**"]     # never user-visible
reason = "docs and test-only"
[global]
paths = ["Cargo.lock", "pnpm-lock.yaml", "rust-toolchain.toml", "rightkit-qa.toml", "journeys.toml"]   # a change invalidates every carried receipt
```
Classification: a changed product-source file that matches a `behavior` is user-visible and requires its journeys; a file matching `exempt` is not; a product-source file matching neither is `unmapped` and fails the gate until the map is amended (this forces the map to stay complete). Shrinking a mapping (removing paths, removing a journey from a behavior, adding exempt globs over product source) is a coverage change and takes the approval below.

**Phase 1 (pilot):** census and an advisory `journeys gate` (always exit 0, prints what phase 2 would fail). **Phase 2** (after the pilot is accepted and one more app migrated; separate decision) fails a diff when any of these holds: it adds an untagged test; a changed product file is `unmapped`; a user-visible behavior's mapped journeys lack a valid receipt (below); a coverage change lacks approval; release signoff lacks valid receipts.

**Fresh matching receipts instead of mandatory journey edits.** A behavior change does not require editing a journey when an existing journey already covers it: it requires that every mapped journey has a valid receipt (D5) for the claimed tree on each declared platform. A receipt for an older tree may be carried forward for a journey only if `git diff --name-only <receipt.source.tree> <claimed tree>` touches no path of any behavior mapping that journey, no `global` path, no file of the journey's `entry`, and no adapter source; otherwise it is stale and the journey reruns. A new behavior with no existing journey needs a new journey and a mapping entry.

**Coverage-owner validation.** Deleting a `release_critical` journey, setting `release_critical = false`, removing a platform from `platforms`, changing `surface`, `entry` or `data` to something weaker, shrinking the map, or changing `coverage_owners`, compared with the baseline (the manifest and map at the base ref, or at the last release tag for signoff), requires a tombstone in `journeys.toml`:
```toml
[[change]]
id = "J-007"
kind = "removed"                   # removed | downgraded | platform-reduced | weakened | map-shrunk
before_after_sha256 = "<hex>"      # sha256 of canonical JSON {id, before, after}; binds the approval to this exact change
approval = "https://github.com/bogusyogi/<repo>/issues/<n>#issuecomment-<id>"
replacement = "J-012"              # a passing journey, or null with risk_accepted = true
risk_accepted = false
```
The comment must be authored by a login in the baseline `coverage_owners` and contain the line `rk-coverage-approve <id> <kind> <before_after_sha256>`; the gate fetches it (`gh api`), checks author and line, and checks the replacement journey has a valid receipt. A flaky journey is fixed or goes through this path; it is never silently demoted or retried to green. Limit, stated plainly: agents share one GitHub identity, so authorship proves the account, not the person; for a live app (HeardRight) the owner login is Adrian's and Oracle reviews the approval, and until phase 2 this check is advisory.
Alternative: block from day one. Rejected: the tag convention does not exist yet and a blocking gate on 2000 untagged tests would only produce mass bulk-tagging.

**D9. Migration of existing small tests.** Nothing is deleted without the owner's validation. Per app, in order: (1) author the journey list from product behavior (the app's pitch and feature list, existing `qa:*` scripts); (2) run the census, classifying every existing test as `keep` (fits a D7 class), `consolidate` (several tests exercise one journey path), or `delete-candidate`; (3) a candidate may be deleted only when a passing journey covers its asserted behavior, the app owner chat validates the list in writing (issue comment), and the deletion lands in a commit separate from the journey addition; (4) Oracle reviews the delete commit against the candidate list. Untagged tests stay until classified; no bulk deletion by count.

**D10. Pilot and order.** Pilot: **scraperight**, because it is an early internal tool, already has a native journey test and `sidecar-smoke` journey scripts to wrap, and a mid-size test count; failure costs least. Step 0, cheap: postright gets a `journeys.toml` plus a `journeys_adapter` test target (D4, `runner = "adapter"`) over its 221 scenarios to prove the adapter protocol, receipt and census against a mature suite with custom hooks; it changes no scenario file and no assertion. Then mailright (replace the mocked-IPC web journeys with native or declare them `web`), then heardright (the only live app, so after the gate is proven; its journeys protect it most). Remaining apps follow; each is its own issue linked from the RightKit issue.

**D11. Windows and macOS: compile through the broker, execute through the session launcher.**
Manifest `platforms` per journey; the receipt records host. Build and execution are two separate steps on both hosts (D4 steps 3 and 4). The broker compiles; it never runs a native-UI journey, because under `rightkit cargo test` the test binary runs in the broker's context where macOS `open` has no GUI session and the environment is scrubbed (`crates/rightkit-qa/README.md:164`; `scripts/run-ui-tests.sh` is the existing two-step precedent).
- **macOS.** The runner builds with `rightkit cargo ... --no-run --message-format=json`, takes the executable from the cargo artifact record, and executes it from the user's login session with the runner's own environment plus the protocol and isolation variables (the same shape as `scripts/run-ui-tests.sh`). The app is then launched by `control::launch`, which stages the binary or `.app` under `<appRoot>/apps/<launch-id>/` and passes every home, temp and profile path through `open --env` (`README.md:164`), so runtime QA mode and the isolated data survive the `open` boundary. The runner verifies the adapter's `start.isolation` and the guard result. A macOS run started from a non-GUI context (plain SSH) is `unknown`, not pass; the existing launcher does not cover that case off Windows (`README.md:266`: "off Windows, it just runs the program"), and adding it is future work, not part of this design.
- **Windows.** Same two steps. Execution goes through `rightkit-qa-interactive` (`crates/rightkit-qa/src/interactive.rs`, `src/bin/rightkit-qa-interactive.rs`; `README.md:266`): `RIGHTKIT_QA_INTERACTIVE=1 rightkit-qa-interactive --timeout-secs N --pass-env <protocol and isolation vars> -- <executable> <args>`, so harness and app share the interactive session and the control server over a named pipe. From an interactive desktop the runner executes it directly.
- The launcher passes the `RIGHTKIT_JOURNEYS_*` variables and the guard's isolation variables explicitly; the runner refuses a run whose `start.isolation.guard` is not `passed`.
No new platform code beyond the runner's call into the existing launchers; gaps surface as `unknown` receipts.

## 4. Proposed doctrine text (do not apply until approved)

Proposed single line for `docs/agent-rules/workspace.md`, under "Scope & completion":

> Prove each app by its `journeys.toml`: end-to-end user journeys run on the real built app in runtime QA mode with isolated data through `rightkit-qa journeys`; add unit tests only for tagged `rk-test: pure|contract|pin #issue` cases, and delete none without the owner's validation (`rightkit/docs/E2E-TESTING-STANDARD.md`).

## 5. Acceptance criteria

1. `rightkit-qa journeys check|run|census|validate-receipt|gate` exist (`gate` advisory), documented in the `rightkit-qa` README, with focused RightKit-owned tests of: manifest and mapping validation; receipt binding; census; and a fixture adapter that proves each rejection: zero selected, zero executed, missing result, duplicate result, unselected result, skipped result, stale `nonce`/`run_id`, wrong artifact digest, binary not from the claimed tree, dirty tree, adapter exit nonzero with all-passed records, and a misspelled filter that runs nothing.
2. `@rightkit/qa` ships the Node manifest reader, receipt validator and adapter helper, and its fixture produces a receipt that the Rust `validate-receipt` accepts (and the reverse), byte-compatible per `rightkit.journeys.receipt/1`.
3. The adapter protocol (D4) is documented with the Rust and Node helpers and tested on both hosts per D11 using a RightKit-owned fixture app.
4. After publication, app pilots: scraperight has a `journeys.toml` and `journeys-map.toml` listing its release-critical journeys, each passing on both hosts with a validating receipt; census report published; delete-candidate list awaiting owner validation. postright has a `journeys.toml` and `journeys_adapter` target that run its existing scenarios through its own hooks without changing any scenario file or assertion, with adapter verdicts equal to the unchanged aggregate test on the same tree.
5. Phase 2 gate is switched on only by a separate decision after criteria 1 to 4.
6. Public APIs and README text are reviewed by Oracle; RightKit publishing follows the existing dual-signed, both-host gate.

Issue #878 stays open until criteria 1 to 4 have evidence.

## 6. Risks and limits

- Native journeys are slower and host-bound; they can flake. Mitigation: tiers, one scenario dir per run, existing orphan checks; a flaky journey is fixed or goes through the D8 coverage-owner approval, never silently demoted and never retried to green.
- The tag convention is honor-system until the gate; reviewers (Oracle) enforce meanwhile.
- Counts above are line-level `rg` counts, not parsed test inventories; they size the problem, they are not exact.
- Not verified: whether `rightkit owns` already lists a journey/manifest capability; whether `right-release qa` should consume the receipt directly (decide during implementation, within D5).

## 7. Implementation status

**RightKit implements now (rightkit-qa and @rightkit/qa, before any app work):**
- `journeys check`: manifest, mapping and adapter-declaration validation (D3, D8).
- `journeys census`: tagged/untagged tests, journeys, ratio, base-ref diff (D8).
- Receipt schema `rightkit.journeys.receipt/1` and `journeys validate-receipt` (D5), including provenance, completeness and zero-execution rules.
- The adapter protocol v1 (D4): runner side (selection, nonce, results validation), Rust helper `journeys::adapter`, Node helper in `@rightkit/qa`.
- The Node reader in `@rightkit/qa` (manifest, receipt validator), cross-checked against the Rust implementation.
- `journeys run` orchestration (select, source identity, broker build, interactive launch, collect, receipt), exercised against a RightKit-owned fixture adapter on both hosts.
- An advisory `journeys gate` (always exit 0).

**Follows with app pilots, after RightKit publication:**
- ScrapeRight pilot: `journeys.toml`, `journeys-map.toml`, native journeys with receipts on both hosts, census and delete-candidate list.
- PostRight wrapper: `journeys.toml` plus `journeys_adapter` over the 221 scenarios and its `hooks()`, with the move-only extraction if needed.
- Then mailright and heardright (D10), each its own issue.
- Phase 2 gate enforcement, by separate decision (criterion 5).
- Doctrine line in section 4, only after approval.
- macOS over non-GUI SSH launch (stays `unknown`) and the installed-package lane (gap ledger row 56) stay out of scope.

## 8. Relation to ADR-0001 (D1 to D11)

ADR-0001 (`docs/design/ADR-0001-shared-e2e-runner.md`, chosen option C-1) makes `rightkit-qa` the single
runner and retires the Node journeys orchestrator after an equivalence run. This section records, for each
decision in section 3, whether ADR-0001 keeps it or changes it. Where this section and ADR-0001 differ,
ADR-0001 governs. Sections 1 to 7 are unchanged.

| Decision | Under ADR-0001 | What ADR-0001 keeps or changes |
|---|---|---|
| D1 journeys are the proof | Kept | A declared journey list run on the real built app in runtime QA mode with isolated data. Required journey IDs are fixed per platform before any scenario runs (ADR item 2). The runner owns the loop (item 4). |
| D2 extend `rightkit-qa`, no new crate | Kept, owner emphasis | `rightkit-qa` is the one orchestrator (C-1 over C-2). The Node `@rightkit/qa` journeys orchestrator is retired after equivalence (item 13). Its gate, census and map logic remain as reports over `rightkit-qa` evidence. `@rightkit/qa` keeps its CheckStatus mirror, updated for `errored` (ruling D-f). |
| D3 `journeys.toml` manifest | Partly changed | Journey identity, immutable IDs, `platforms`, `tier`, `data` and `release_critical` are kept. The `runner` and `adapter` fields change: journeys declare `runner = "scenario"`, and app-specific steps are supplied by a per-app step plugin (ADR item 4, ruling D-b). The plugin cannot choose journeys or set verdicts. |
| D4 runner and adapter contract | Changed | The file-based adapter protocol v1 (the app appends verdict records to `results.jsonl`; `journeys/adapter.rs`) is not the runner-to-app contract. ADR-0001 uses the step-plugin protocol v1: JSON lines over stdio (`hello`, `setup`, `step`, `teardown`, `spawn`, `spawn_result`, `shutdown`), one persistent plugin per scenario inside the job. Worker contract (ruling D-g): the same `rightkit-qa` executable run as `worker <world dir>`. The no-alteration rule (no retry, no name filter, no softened verdict) is kept. |
| D5 receipt and verdict rules | Kept in substance, with changes | Only `passed` counts, there is no retry to green, and a missing result is `missing` (ruling D-d). `CheckStatus` gains `errored` on the wire (ruling D-f). The build record in ADR item 1 (runner-computed sha256, git tree, lockfile hashes, features, toolchain, broker request id, attested exit) extends the `build` object in D5. Its exact schema is set by the implementation and its review. |
| D6 CI versus local | Kept | Native-UI journeys still run per host in the logged-in session, and CI runs the other surfaces. ADR-0001 adds that the runner lane never runs inside the broker or `cargo test` (items 1 and 9). The vitest, iOS and Android lanes are deferred to a later wave (item 12). |
| D7 unit-test classes | Kept and extended | `pure`, `contract` and `pin #issue` stay. ADR item 9 checks tags at edit time, lints `pure` and freezes the untagged count. Unit tests run in the broker under a time cap (item 1). The unit lane must not run journeys (Sage C4). |
| D8 census, gate, behavior map, coverage owners | Kept as reports | Census, `journeys-map.toml` and the tombstone rules are kept as written here. ADR-0001 adds selection from changed files and a nightly and pre-release full run (item 7), and checks at edit, push and done (item 9). It does not restate the Phase 2 coverage-owner rules, which stay as written until their own decision. |
| D9 migration of existing tests | Kept, with a bridge | Nothing is deleted without owner validation. ADR item 8 adds a time-boxed bridge for the seven Harness apps: their built test binaries run outside the broker, one worker per test function, with a removal date per app. Deleting wrappers and duplicates is gated on an equivalence run and owner sign-off (Reversibility). |
| D10 pilot and order | Superseded for the runner | ADR-0001 sets PostRight as the runner pilot (Order steps 0 and 1), not scraperight. The scraperight-first order is not carried into ADR-0001. ADR-0001 does not set app order beyond PostRight. |
| D11 build through the broker, execute through the session launcher | Kept, made mechanical | Compile in the broker, execute outside it. Qualifying runs refuse `--build` and `--build-ui`, so the broker is the only compile path (ADR item 1). The installed-app lane (item 11) is separate and runs last, on disposable machines. |
