# End-to-end testing standard and shared journey harness

Status: revision 3, 2026-11-03. Replaces the isolated-data model of revisions 1 and 2 with snapshot and restore of the owner's real data (rightkit#1437, GATE-CONTRACT GC-A10). Decision numbering D1 to D11 is unchanged and amended in place. Execution sections are governed by ADR-0001 (`docs/design/ADR-0001-shared-e2e-runner.md`) where section 8 says so.
Origin: Adrian, 2026-10-21: "for tests, each agent does it's own thing and we're getting thousands of small tests whereas we should have end to end tests, is this something that belongs in RK?" Tracking issue: bogusyogi/rightkit "End-to-end testing standard and shared journey harness (Adrian 2026-10-21)".

Adrian's rulings that shape this revision (2026-11-03):

> "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore."
> "End to end tests ONLY."
> "My decision was no unit tests. Only e2e. Unit tests are allowed only where e2e cannot cover something ... strictly with my written approval"
> "no debug assertions"

What follows from them, and what this standard no longer contains: there are **no throwaway worlds**, **no redirected `HOME`, `APPDATA`, `XDG_*`, temp or data-directory variables**, **no generated or seeded data**, **no dummy accounts or fixtures standing in for the owner's data**, **no test-mode build features** and **no `debug_assertions` switches**. A journey runs on the installed dev app and the owner's real data. What makes that safe is a snapshot taken before anything launches and a verified restore when the run ends.

## 1. Evidence (counts are `rg` line counts, 2026-10-21, primary checkouts, vendored `rk-snapshot` excluded)

| App | Rust `#[test]` | TS/JS `it(`/`test(` | Journey/e2e assets that exist today |
|---|---|---|---|
| heardright | 1699 | 468 | 3 `.scenario.toml` (all inside vendored `rk-snapshot` examples, none are the app's own); `tauri-app-next/scripts/run-contract-tests.mjs`; `qa:native:journey`, `qa:browser`, `qa:macos-installed` scripts |
| mailright | 1224 | 220 | `scripts/qa-journeys.mjs`; it drives a Vite QA server with **mocked IPC**, so it is not the real built app |
| scraperight | 544 | 114 | `crates/scraperight-ui-e2e/tests/native_journey.rs` (1 test); `scripts/sidecar-smoke/` incl. `installed-journeys.mjs` |
| postright | (see the 2026-10-21 census) | | 221 `.scenario.toml` scenarios with app-owned hooks |

Reading: three of four apps carry 700 to 2100 narrow tests against at most a handful of real journeys. The harness to do better already exists; the missing pieces are a standard, a declared journey list, and a gate.

## 2. What RightKit already owns (extend, do not rebuild)

- `rightkit-qa` (Rust): declarative scenarios, engine and app control, run lock, hashed evidence, retention, the in-app control server, and (0.10.3) the `snapshot` module that snapshots and restores the owner's real data. Scenario TOML with `tier`, `target = "engine"|"ui"`, `[[step]]`, `[[step.expect]]`. Run kinds functional/visual. Windows and macOS UI driving through the in-app control server.
- The control server is a compile-time feature (`control`) that is part of the dev build only (the release profile plus `qa-native`). It never ships: `right-release` scans every sealed binary for `QA_CONTROL_MARKER` and refuses a release or update build that carries it. The dev build is the owner's installed app; QA drives it as it is.
- `@rightkit/qa` + `@rightkit/qa-chrome` (Node): CDP page driver, dev-server launcher, evidence and ownership for browser surfaces; the journeys manifest reader, receipt validator (`rightkit.journeys.receipt/2`), adapter protocol 2 helper, census and advisory gate. It has no runner and no throwaway workspace: the run, and with it the snapshot, belongs to the Rust runner.
- `rightkit-qa-driver` is a deprecated shim; no new code goes there.

## 3. Decisions

**D1. The proof of an app is a declared list of user journeys run against the real built, installed dev app on the owner's real data, under a snapshot and restore.**
Alternatives: (a) keep per-agent test discretion with a coverage percentage target; (b) mandate a test-count cap; (c) run journeys on generated or isolated data. Rejected: coverage rewards many small tests (the present problem), a cap is arbitrary and gameable, and (c) is the dummy data Adrian ruled out: a journey on made-up data proves the app works on made-up data. A journey list is reviewable by a human and maps to what users do.

**D2. Extend `rightkit-qa` and `@rightkit/qa`; add no new crate or package.**
The journey manifest, `journeys` subcommands, the adapter protocol, the snapshot and the receipt live in `rightkit-qa` (modules `journeys`, `journeys::adapter` for the app-side helper, `snapshot`). `@rightkit/qa` gains a `journeys` export with the same manifest reader and receipt validator, so Node tooling checks byte-compatible records. Alternative: a new `rightkit-e2e` owner. Rejected: it would split run storage, snapshot and evidence, which the two QA owners already share, and the ownership rule forbids a second owner for one concern.

**D3. One manifest per repo: `journeys.toml` at the app root, runner-agnostic.**
```toml
version = 1
app = "scraperight"
coverage_owners = ["bogusyogi"]    # logins whose issue comment may approve D8 coverage changes; baseline-protected

[[adapter]]                         # required only for runner = "adapter"
name = "postright-scenarios"
build = ["rustright", "cargo", "test", "-p", "postright-e2e", "--test", "journeys_adapter", "--no-run", "--message-format=json"]
executable = { role = "adapter", cargo_target = "journeys_adapter" }   # test executable taken from cargo compiler-artifact messages
artifacts = [{ role = "engine", cargo_package = "postright-engine" }]   # other binaries the adapter launches; hashed and bound (D5)
args = []

[[journey]]
id = "J-001"                      # immutable; never renumbered or reused
title = "User adds a source, runs a scrape, sees results"
platforms = ["macos", "windows"]  # where it must pass
surface = "native-ui"             # native-ui | web | engine | cli | installed
runner = "scenario"               # scenario (built-in runner; app steps come from a step plugin) | adapter (app-owned adapter, D4)
adapter = "postright-scenarios"   # required when runner = "adapter"
entry = "tests/scenarios/add-source-scrape.scenario.toml"
tier = "fast"                     # fast | full | paid
release_critical = true
```
There is no `data` key to choose: every journey runs on the owner's real data. A manifest may carry `data = "real"` (redundant); `generated`, `seeded` and every other value are a check error. Alternatives: extend `rightkit-qa.toml` (rejected: it is runner config, one per app, and journeys need identity and a verdict independent of which runner executes); a markdown list (rejected: not machine-checkable). A bare `cargo test <filter>` or `node script.mjs` is not a runner: it yields one aggregate exit code and no per-journey result; it is only ever the inside of an adapter.

**D3a. Snapshot configuration is mandatory: `[snapshot]` in `rightkit-qa.toml`.**
An app that has journeys declares what the run snapshots and restores. `rightkit-qa run`, `run --qualifying` and `journeys run` refuse a config with no `[snapshot]`, and every launch of an app or engine refuses to start unless a snapshot is armed (`snapshot::require_armed`). Rust tests arm one with `Harness::with_snapshot`.
```toml
[snapshot]
app_name = "ViewRight"           # data, config and local folders, from rightkit-fs AppDirs (the account home, no environment)
process_names = ["ViewRight"]    # required: the run refuses to start while one of these is running
# suite_shared = true            # also the suite shared folder, "Orthic Labs/shared" (the account's per-user suite root)
# flat = true, include_cache = true, extra_paths = ["/abs/folder"]

[[snapshot.credentials]]         # real OS store items (Keychain / Credential Manager), by the app's namespace
service = "app.orthic.viewright"
accounts = ["license-device-id"]

[snapshot.android]               # Android journeys: the app's data on the device or emulator (debuggable dev build, run-as)
package = "app.orthic.viewright.dev"   # a test package: last segment dev, qa, test or debug
serial = "emulator-5554"               # required: the snapshot never picks "the only attached device"
```
Covered, in one manifest with sha256 per file: the app's data, config and local folders; `extra_paths`; the suite shared folder when declared; the declared credential items; and, for an Android app, its package data dir on the device (copied as a tar through `adb exec-out run-as`, restored through `adb shell run-as`, verified against a path and sha256 listing; the top-level `lib` entry is left out because it can be a symlink into the installed APK). The suite shared folder is shared by every app of the suite, so declaring it also requires every suite app named in `process_names` to be stopped; the restore puts back exactly the files that were there.

**D4. Runner and adapter contract (protocol 2).**
RightKit orchestrates: selection, build, snapshot, launch, result collection, restore and the receipt. The app's own suite runs its own handlers, setup and teardown, unchanged. The seam is one adapter contract (protocol version 2), implemented once in Rust (`rightkit_qa::journeys::adapter`) and once in Node (`@rightkit/qa/journeys`).

`rightkit-qa journeys check` validates the manifest, the mapping file (D8) and any adapter declaration only: schema, unique immutable IDs, entries exist, no `data` other than `real`, every `adapter` reference resolves, every `native-ui` journey declares a real built-binary artifact, and no mocked-IPC dev server for `native-ui`. A Vite server with mocked IPC counts as `web`, not as proof of the app.

`rightkit-qa journeys run [--id J-001]... [--platform P] [--tier T]` executes these steps in order; any failure ends the run with verdict `failed` or `incomplete` and a receipt (D5):
1. **Select.** Filter the manifest by id, platform and tier. Write `selection.json` into the managed run root (fresh `run_id` UUID and 128-bit hex `nonce`). Zero selected journeys is an error, never a pass.
2. **Source identity.** Record git `head`, `tree`, `dirty` (and `diff_sha256` when dirty) before the build.
3. **Build (broker).** Run the adapter's `build` argv through the broker (`rustright cargo ...`). Parse `--message-format=json` `compiler-artifact` records for the declared `executable` and `artifacts`; no text scraping. Re-check the source tree after the build; a changed tree fails the run. This step never launches the app.
4. **Snapshot.** After the build and before anything launches: start-up recovery of any leftover restore marker, refusal while a `process_names` process runs, then the snapshot (README, "Snapshot and restore of the real data"). Nothing live is touched before the snapshot is verified.
5. **Launch (D11).** Launch the built executable by the shared interactive-session launcher (never inside the broker), after re-hashing every artifact. The app runs with its real environment: nothing is redirected.
6. **Collect.** Read `results.jsonl`, validate it against the selection (D5 rules).
7. **Restore.** Every app is stopped; put the owner's real data back and verify it against the manifest, however the run ended (pass, fail, error, panic; `Drop` covers an early return). A kill leaves the marker and the next `rightkit-qa` start restores it. A restore that fails fails the run and keeps the marker. Only then is the receipt written, recording the snapshot and that verification.

**Adapter invocation.** The runner starts `<executable> <args...>` with this environment, on the real environment of the run:

| Variable | Meaning |
|---|---|
| `RIGHTKIT_JOURNEYS_PROTOCOL` | `2` |
| `RIGHTKIT_JOURNEYS_SELECTION` | absolute path of `selection.json` (read-only to the adapter) |
| `RIGHTKIT_JOURNEYS_RESULTS` | absolute path of `results.jsonl`; the runner pre-creates it empty; only the adapter appends |
| `RIGHTKIT_JOURNEYS_SNAPSHOT_ID` | the id of the snapshot the run is made under; the adapter echoes it in `start.snapshot_id` |
| `RIGHTKIT_QA_SNAPSHOT_MARKER` | path of the restore marker, so a bridged process knows a snapshot is armed |

`selection.json`:
```json
{"protocol":2,"run_id":"<uuid>","nonce":"<32 hex>","app":"postright","platform":"windows","tier":"fast",
 "manifest_sha256":"<hex>","journeys":[{"id":"J-001","entry":"tests/scenarios/accounts-store.scenario.toml"}]}
```

The adapter must run exactly the selected journeys through the app's existing suite and append JSON lines (one object per line, UTF-8, flushed per line) to `RIGHTKIT_JOURNEYS_RESULTS`. stdout and stderr are app logs and carry no protocol. Every record repeats `run_id` and `nonce`; the runner discards the whole file if any record disagrees.

| Record | Fields (type) | When |
|---|---|---|
| `start` | `type:"start"`, `protocol:2`, `run_id`, `nonce`, `adapter` (string), `adapter_version` (string), `started_at` (RFC 3339), `artifacts` (array of `{role, path, sha256}` for every binary the adapter will launch, hashed at that moment), `snapshot_id` (string, from `RIGHTKIT_JOURNEYS_SNAPSHOT_ID`) | first line, before any journey |
| `journey` | `type:"journey"`, `run_id`, `nonce`, `id` (selected journey id), `status` (`passed` \| `failed` \| `skipped` \| `errored`), `started_at`, `duration_ms` (integer), `units` (`{total, passed, failed, skipped}`, integers), `evidence` (array of `{path, sha256, kind}`, `kind` = `log` \| `screenshot` \| `report` \| `trace`; paths relative to the run root), `reason` (string or null; required unless `passed`) | exactly once per selected id, after its teardown |
| `end` | `type:"end"`, `run_id`, `nonce`, `selected` (int), `reported` (int), `ended_at` | last line |

An adapter refuses to report `start` with no snapshot id: a process not started by a journeys runner under a snapshot cannot produce a result. A journey `passed` only if `units.total >= 1`, `units.failed = 0`, `units.skipped = 0` and its status mapping is 1:1 from the app's result; a group entry with zero matching scenarios is `errored`, not `passed`. The adapter's exit code must be 0 when it completed the protocol, whether or not journeys failed; the runner derives pass or fail from records only.

**No-alteration rule.** The adapter wraps; it never rewrites. (a) It may not edit scenario files, hooks, steps, expectations or tolerances. (b) It may not retry, re-order to hide a failure, or convert `failed` to `skipped` or `passed`. (c) It may not filter by test name pattern; selection is by manifest `entry` only. (d) `hooks()` stays the single source of the app's handlers. (e) It may not substitute data: a journey that needs an account, a document, a library or a recording uses what the owner has, and a journey the owner's data cannot support is `skipped` with a reason, which is not a pass.

**D5. Evidence and receipt (proof, not process).**
`journeys run` writes `journeys-receipt.json` into the run's managed evidence directory. Schema `rightkit.journeys.receipt/2`; all hashes are lowercase hex SHA-256 unless stated.

| Field | Type | Meaning |
|---|---|---|
| `schema` | string | `"rightkit.journeys.receipt/2"` |
| `run_id`, `nonce` | string | echo of `selection.json`; every adapter record must match |
| `app` | string | manifest `app` |
| `kind` | `"functional"` \| `"visual"` | existing run kind |
| `verdict` | `"passed"` \| `"failed"` \| `"incomplete"` | computed only by the rules below |
| `started_at`, `ended_at` | string | RFC 3339 |
| `host` | object | `{os, arch, session: "interactive"\|"noninteractive"}` |
| `source` | object | `{head, tree, dirty, diff_sha256}`; `diff_sha256` is required when `dirty` |
| `manifest` | object | `{path, sha256}` |
| `mapping` | object\|null | `{path, sha256}` of the D8 mapping file |
| `build` | object | `{command, source_tree, tree_after_build, started_at, ended_at, broker_receipt, artifacts: [{role, path, size, sha256, produced_by}]}` |
| `launched` | array | `[{role, path, size, sha256}]`, the digests the runner measured immediately before launch AND the digests the adapter reported in its `start` record |
| `snapshot` | object | `{snapshot_id, manifest_sha256, credentials_sha256?, roots: int, credentials: int, restore_verified: bool}`: the snapshot the run was made under, the sha256 of its `manifest.json`, the data folders and credential items restored, and whether the restore matched the manifest |
| `adapter` | object | `{name, version, protocol: 2, argv, exit_code, results_sha256}` |
| `selection` | object | `{ids, platform, tier}`; ids frozen before execution |
| `journeys` | array | one entry per selected id, with a per-journey `launch` record; `missing` is runner-synthesized and never emitted by an adapter |
| `counts` | object | `{selected, executed, passed, failed, skipped, errored, missing, duplicate}` |
| `problems` | array | `[{code, id, detail}]` |

**Provenance rule.** The claimed binary is bound to the claimed source, never merely found beside HEAD. `build.source_tree` must equal `source.tree` and `build.tree_after_build`, and `build.artifacts` must come from this run's cargo `compiler-artifact` output or a broker build receipt that records the same tree and digests. Every `launched[*].sha256` must equal a `build.artifacts[*].sha256` for the same `role`; otherwise `artifact_not_from_build`.

**Verdict rule.** `verdict = "passed"` only if all hold; otherwise `failed` or `incomplete`:
1. `counts.selected >= 1` and `counts.executed == counts.selected` (`zero_executed`);
2. every selected id has exactly one `journey` record; none `missing`, none duplicated, none outside the selection;
3. every journey is `passed`; `skipped` is not a pass, `errored` and `failed` fail;
4. the `start` and `end` records exist, `end.selected == end.reported == counts.selected`, and all records carry the right `run_id` and `nonce` (`protocol_violation`, `stale_results`);
5. `launched` digests match `build.artifacts` and the adapter's reported digests (`artifact_mismatch`, `artifact_not_from_build`); `source`, `build` and `manifest` bindings agree (`source_mismatch`);
6. the snapshot exists and its restore was verified: `snapshot.snapshot_id` is non-empty and equals `start.snapshot_id`, `snapshot.manifest_sha256` is a sha256, `snapshot.roots >= 1` (or `snapshot.android` for an Android-only app), `snapshot.restore_verified` is true (`snapshot`);
7. `adapter.exit_code == 0` and `results_sha256` matches the file read (`adapter_exit`).

`journeys validate-receipt <file> [--tree <40-hex>] [--manifest <path>]` re-checks all rules offline against the receipt, the manifest and the evidence hashes, so the gate trusts neither the runner's prose nor the verdict field. The validator proves the receipt says the restore was verified and ties it to the snapshot id the adapter ran under; the restore itself is proven by the run that did it (the manifest hash in the receipt names the manifest it was checked against). A gate accepts a receipt only if it validates and its `source.tree`, `manifest.sha256` and artifact digests match the commit being claimed, except for carry-forward under D8. For release signoff every `release_critical` journey must be `passed` on every declared platform; a platform with no valid receipt is `unknown`.

**D6. CI versus local.** Public repos: CI runs journeys with `surface` engine, cli, web on Linux/Windows/macOS runners. A CI runner is a disposable machine with no owner data; the snapshot and restore run there as everywhere, and cover whatever the runner holds. Native-UI journeys need an interactive session, so they run locally per host (private apps: Dell and Mac, broker-built dev profile, one build per app per host) on the owner's real data, and the receipt is the artifact the gate checks. A platform with no receipt is `unknown`, not pass. A journey runs against the installed dev app, never a release; the installed-app stage is explicit.

**D7. Unit tests: none by default.** Adrian's ruling (2026-11-03): only end-to-end tests. A unit or integration test exists only where an end-to-end journey cannot cover the behavior, and only with Adrian's written approval, which the test names. The census (D8) keeps counting tests so each one can be classified and removed once a journey covers it. A bug fix for user-visible behavior adds or extends a journey first. Reviewers reject: any new unit test without the written approval; tests of private helpers; tests asserting call sequences or mocks; snapshot/golden tests with no named behavior; and any test that builds a fake world, a fake account or dummy data. `debug_assert!` and `cfg(debug_assertions)` switches are not used for QA or anything else.

**D8. Census, then gate (advisory before blocking); the behavior map; coverage ownership.**
`rightkit-qa journeys census` counts tests (Rust `#[test]`, JS `it/test`), journeys, and the ratio, and diffs against a base ref. It reports; it never classifies a change as user-visible (test counts cannot).

**Reviewed behavior-to-journey mapping.** `journeys-map.toml` at the app root (checked by `journeys check`, reviewed by the app owner and Oracle):
```toml
version = 1
[[behavior]]
id = "B-sources"                    # immutable
title = "Source management and scraping"
paths = ["crates/engine/src/sources/**", "ui/src/sources/**"]   # globs from the app root; a changed file matching one is a user-visible change
journeys = ["J-001", "J-004"]       # journeys that cover it; at least one, all must exist in journeys.toml
[[exempt]]
paths = ["docs/**", "**/*.md", "scripts/**"]     # never user-visible
reason = "docs"
[global]
paths = ["Cargo.lock", "pnpm-lock.yaml", "rust-toolchain.toml", "rightkit-qa.toml", "journeys.toml"]   # a change invalidates every carried receipt
```
Classification: a changed product-source file that matches a `behavior` is user-visible and requires its journeys; a file matching `exempt` is not; a product-source file matching neither is `unmapped` and fails the gate until the map is amended. Shrinking a mapping is a coverage change and takes the approval below.

**Phase 1 (pilot):** census and an advisory `journeys gate` (always exit 0, prints what phase 2 would fail). **Phase 2** (separate decision) fails a diff when: a changed product file is `unmapped`; a user-visible behavior's mapped journeys lack a valid receipt; a coverage change lacks approval; release signoff lacks valid receipts; or a test is added without Adrian's written approval (D7).

**Fresh matching receipts instead of mandatory journey edits.** A behavior change does not require editing a journey when an existing journey already covers it: it requires that every mapped journey has a valid receipt (D5) for the claimed tree on each declared platform. A receipt for an older tree may be carried forward for a journey only if `git diff --name-only <receipt.source.tree> <claimed tree>` touches no path of any behavior mapping that journey, no `global` path, no file of the journey's `entry`, and no adapter source; otherwise it is stale and the journey reruns.

**Coverage-owner validation.** Deleting a `release_critical` journey, setting `release_critical = false`, removing a platform from `platforms`, changing `surface` or `entry` to something weaker, shrinking the map, or changing `coverage_owners`, compared with the baseline, requires a tombstone in `journeys.toml`:
```toml
[[change]]
id = "J-007"
kind = "removed"                   # removed | downgraded | platform-reduced | weakened | map-shrunk
before_after_sha256 = "<hex>"
approval = "https://github.com/bogusyogi/<repo>/issues/<n>#issuecomment-<id>"
replacement = "J-012"
risk_accepted = false
```
The comment must be authored by a login in the baseline `coverage_owners` and contain the line `rk-coverage-approve <id> <kind> <before_after_sha256>`; the gate fetches it, checks author and line, and checks the replacement journey has a valid receipt. A flaky journey is fixed or goes through this path; it is never silently demoted or retried to green. Until phase 2 this check is advisory.

**D9. Migration of existing small tests.** Nothing is deleted without the owner's validation. Per app, in order: (1) author the journey list from product behavior; (2) run the census and classify every existing test as `keep` (Adrian-approved in writing, D7), `consolidate` (several tests exercise one journey path), or `delete-candidate`; (3) a candidate may be deleted only when a passing journey covers its asserted behavior, the app owner chat validates the list in writing, and the deletion lands in a commit separate from the journey addition; (4) Oracle reviews the delete commit against the candidate list. No bulk deletion by count.

**D10. Pilot and order.** ADR-0001 sets PostRight as the runner pilot. Then mailright (replace the mocked-IPC web journeys with native or declare them `web`), then heardright (the only live app, so after the gate is proven), then the remaining apps; each is its own issue linked from the RightKit issue.

**D11. Windows, macOS and Android: compile through the broker, execute through the session launcher.**
Manifest `platforms` per journey; the receipt records host. Build and execution are two separate steps (D4 steps 3 and 5). The broker compiles; it never runs a native-UI journey, because under `rustright cargo test` the test binary runs in the broker's context where macOS `open` has no GUI session and the environment is scrubbed.
- **macOS.** The runner builds with `rustright cargo ... --no-run --message-format=json`, takes the executable from the cargo artifact record, and executes it from the user's login session with the runner's own environment plus the protocol variables. The app is launched by `control::launch` as the owner's installed dev app with its real data. A macOS run started from a non-GUI context (plain SSH) is `unknown`, not pass.
- **Windows.** Same two steps. Execution goes through `rightkit-qa-interactive` (`RIGHTKIT_QA_INTERACTIVE=1 rightkit-qa-interactive --timeout-secs N --pass-env <protocol vars> -- <executable> <args>`), so harness and app share the interactive session and the control server over a named pipe. From an interactive desktop the runner executes it directly.
- **Android.** The `android` driver installs and launches the dev package on the emulator or the device by serial and collects pid-filtered logcat. A launch never uninstalls the package (`uninstall_at_teardown` is `false` by default): an uninstall deletes the app's real data. The `[snapshot.android]` table (D3a) makes the package data part of the snapshot, and the restore puts it back.
The launcher passes the `RIGHTKIT_JOURNEYS_*` variables explicitly; the runner refuses a run whose adapter's `start.snapshot_id` is not the run's snapshot. Gaps surface as `unknown` receipts.

## 4. Doctrine text

Proposed single line for `docs/agent-rules/workspace.md`, under "Scope & completion":

> Prove each app by its `journeys.toml`: end-to-end user journeys run on the installed dev app and the owner's real data, under a snapshot and restore (`[snapshot]` in `rightkit-qa.toml`, mandatory), through `rightkit-qa journeys`; no worlds, no redirected data, no dummy data, no unit tests without Adrian's written approval (`rightkit/docs/E2E-TESTING-STANDARD.md`).

## 5. Acceptance criteria

1. `rightkit-qa journeys check|run|census|validate-receipt|gate` exist (`gate` advisory), documented in the `rightkit-qa` README; `journeys run` refuses a config without `[snapshot]`, snapshots after the build, restores and verifies when the run ends in any way, and records the snapshot in the receipt. This is proven by running journeys on the real dev app and reading the receipt, not by a fixture adapter.
2. `@rightkit/qa` ships the Node manifest reader, receipt validator and adapter helper; a receipt written by the Rust runner (`rightkit.journeys.receipt/2`) validates in Node, and the Node `validate-receipt` gives the same verdict as the Rust one.
3. The adapter protocol (D4, protocol 2) is documented with the Rust and Node helpers.
4. App pilots: an app has a `journeys.toml` and `journeys-map.toml` listing its release-critical journeys, each passing on both hosts with a validating receipt whose snapshot restore is verified; the census report is published; the delete-candidate list awaits owner validation.
5. Phase 2 gate is switched on only by a separate decision after criteria 1 to 4.
6. Public APIs and README text are reviewed by Oracle; RightKit publishing follows the existing dual-signed, both-host gate.

## 6. Risks and limits

- Journeys run on the owner's real data. The snapshot copies whole folders (models and caches included, so `include_cache` is off by default); the credential read needs the OS store to answer without a prompt; and the owner must not use the app while a run is in progress, because the restore puts back the snapshot. A crash leaves the marker and the next `rightkit-qa` start restores it.
- A restore cannot undo an effect outside the snapshot: mail sent, a file written outside a declared root, a network call. A journey that causes such an effect is a journey the owner decides to run; the receipt does not claim it undone. Declare every root the app writes (`extra_paths`, `suite_shared`, credentials, Android package) so the restore covers them.
- Native journeys are slower and host-bound; they can flake. A flaky journey is fixed or goes through the D8 coverage-owner approval, never silently demoted and never retried to green.
- Counts in section 1 are line-level `rg` counts, not parsed test inventories; they size the problem, they are not exact.

## 7. Implementation status

**Implemented in `rightkit-qa` 0.10.3 and `@rightkit/qa` 0.7.0:** `journeys check`, `census`, `validate-receipt`, `run`, an advisory `gate`; receipt `rightkit.journeys.receipt/2` and adapter protocol 2 in Rust and Node; the `snapshot` module (data folders, credentials, the suite shared folder and Android package data); removal of the worlds, the redirected environment and the dummy-data guard; removal of the Node runner and fixture adapter.

**Follows:** app pilots (D10); phase 2 gate enforcement by separate decision; the doctrine line in section 4 after approval; macOS over non-GUI SSH launch (stays `unknown`).

## 8. Relation to ADR-0001 (D1 to D11)

ADR-0001 (`docs/design/ADR-0001-shared-e2e-runner.md`, chosen option C-1) makes `rightkit-qa` the single runner and retires the Node journeys orchestrator, which this revision has done (the Node package keeps the validator, census and gate). Where this section and ADR-0001 differ, ADR-0001 governs, except that the isolated-world model in ADR-0001 (worlds, `worker <world dir>`, per-world data directories and the dummy-data guard) is superseded by GC-A10 and Adrian's 2026-11-03 ruling: scenario runs are serial on the real data under one snapshot.

| Decision | Under ADR-0001 and this revision |
|---|---|
| D1 journeys are the proof | Kept; the data is the owner's real data under a snapshot and restore. Required journey IDs are fixed per platform before any scenario runs. |
| D2 extend `rightkit-qa`, no new crate | Kept; `rightkit-qa` is the one orchestrator and owns the snapshot. |
| D3 `journeys.toml` manifest | Journey identity, immutable IDs, `platforms`, `tier` and `release_critical` are kept; `data` is gone (optional `real` only); `runner = "scenario"` with app steps from a step plugin (ADR item 4); `[snapshot]` in `rightkit-qa.toml` is mandatory (D3a). |
| D4 runner and adapter contract | Changed: adapter protocol 2 (`start.snapshot_id`); the step-plugin protocol (ADR item 4) carries the app's steps; the worker has no world. The no-alteration rule is kept. |
| D5 receipt and verdict rules | Receipt `/2`: `snapshot` replaces `isolation`; only `passed` counts, no retry to green. |
| D6 CI versus local | Kept. |
| D7 unit-test classes | Replaced: no unit tests without Adrian's written approval (2026-11-03). |
| D8 census, gate, behavior map, coverage owners | Kept as reports. |
| D9 migration of existing tests | Kept; the equivalence run and owner sign-off gate every deletion. |
| D10 pilot and order | PostRight first (ADR-0001). |
| D11 build through the broker, execute through the session launcher | Kept; Android added. |
