# Audit: crates m-z (rightkit-*), HEAD b4f4600

Read-only audit of 34 crates under `/home/user/rightkit/crates/` (`rightkit-mcp` .. `rightkit-workflow`; no non-`rightkit-` dirs exist). No cargo was run, nothing was edited or published. HEAD moved during the audit (b98c3e7 -> d9a5016 -> b4f4600, concurrent commits); all results are as of `b4f4600`. The working tree also holds unrelated uncommitted `packages/release/*` edits.

Method notes
- "Version set in" uses `git log -G'^version = ' -1 -- Cargo.toml`. The task's `-S'version = '` form misses edits that keep the count of matching lines, so `-G` was used. Almost every crate resolves to `bbdf6ab`, the repo-import baseline (6664 files), so for those it only means "unchanged since import".
- `registry.cargo` is `packages/release/rightkit-versions.json` -> `registry.cargo`. It is "recorded from the registries (rightkit-sync.mjs)" per commit a3ae84c. I did not query crates.io, so whether each recorded version is truly published is UNVERIFIED. A second top-level `cargo` map in the same file lists 0.1.0 for pdf, playback and search; its meaning is UNVERIFIED and it was ignored.
- Commit b98c3e7 ("Retire the rightkit client name") touched 13 of these crates; every hunk read is a comment, README, doc string or command-string rename (`rightkit cargo` -> `rustright cargo`). Reading it as non-functional is my judgement from the hunks I read; I read the qa, record, qa-driver, pdf, send, tauri-shell and ownership hunks, not all.
- Consumer recipe file hashes (`files` map, relative to `ci/consumers/<name>/`) were recomputed for all 19 m-z recipes and all match. The `aggregateSha256` values were not recomputed (UNVERIFIED).
- `cargo package` outcome cannot be proven without running cargo; the "likely fail" column below is from reading Cargo.toml only.

## Summary

| Crate | Ver | publish | Registry | Source changed since version set | CHANGELOG marker for current version | Recipe |
|---|---|---|---|---|---|---|
| mcp | 0.7.0 | default | 0.7.0 | rename text only | ok, duplicate heading | none (listed uncovered) |
| models | 0.5.1 | default | 0.5.1 | rename text only | WRONG: "unpublished staging" | yes |
| motion | 0.2.0 | default | 0.2.0 | no | WRONG: "unpublished staging" | yes |
| oauth | 0.10.0 | default | 0.10.0 | rename text only | WRONG: "unpublished staging" | yes |
| ocr | 0.2.3 | true | 0.2.3 | no | ok | none (uncovered) |
| onnx | 0.2.7 | default | 0.2.7 | no | WRONG: "unpublished staging" | yes |
| overlay | 0.2.0 | default | 0.2.0 | no | WRONG: "(unreleased)" | yes |
| overlay-win | 0.1.0 | default | 0.1.0 | no | WRONG: "(unreleased)" | yes |
| ownership | 0.4.2 | true | 0.4.2 | catalog strings (post-publish) | ok | yes |
| pdf | 0.3.3 | default | 0.3.3 | rename text only | ok | yes |
| platform-rules | 0.1.0 | default | 0.1.0 | no | WRONG: line 12 "0.1.0 - unreleased" duplicate | none (uncovered) |
| playback | 0.3.0 | default | 0.3.0 | rename text only | WRONG: "unpublished staging" | yes |
| **qa** | 0.8.0 | default | 0.8.0 | **YES, CODE (wave 2 + b4f4600)** | 0.8.0 unmarked; wave 2 under Unreleased | yes (pins =0.8.0) |
| qa-driver | 0.7.0 | default | 0.7.0 | rename text only | ok (stale 0.6.0 marker below it) | yes |
| record | 0.3.3 | true | 0.3.3 | rename text only | WRONG: "unpublished staging" | yes |
| review | 0.4.0 | default | 0.4.0 | no | ok, duplicate heading | none (uncovered) |
| sandbox | 0.6.1 | default | 0.6.1 | no | WRONG: "(unpublished staging)" | yes |
| search | 0.4.0 | default | 0.4.0 | no | ok, duplicate heading | none (uncovered) |
| send | 0.2.0 | default | 0.2.0 | rename text only | ok | none (uncovered) |
| sidecar | 0.3.0 | default | 0.3.0 | no | WRONG: "unpublished staging" | yes |
| speech-client | 0.4.0 | default | 0.4.0 | rename text only | WRONG: "unpublished staging" | yes |
| spend | 0.2.0 | default | 0.2.0 | no | WRONG: "(unreleased)" | yes |
| sync | 0.2.2 | default | 0.2.2 | rename text only | ok | none (uncovered) |
| sync-keys | 0.1.0 | **false** | none | no | n/a | yes (but unpublishable) |
| **tauri-shell** | 0.6.0 | default | **0.5.1** | doc comments only | heading unmarked while unpublished | yes (pins =0.6.0) |
| time | 0.1.0 | default | 0.1.0 | no | ok | none (uncovered) |
| transcript | 0.4.0 | true | 0.4.0 | no | ok | none (uncovered) |
| tts | 0.2.3 | true | 0.2.3 | rename text only | ok | none (uncovered) |
| tts-qwen | 0.2.7 | default | 0.2.7 | rename text only | ok | none (uncovered) |
| tts-qwen-sys | 0.1.2 | default | 0.1.2 | VERSIONS.md rename text | ok | none (uncovered) |
| typeset | 0.1.0 | default | 0.1.0 | no | ok, duplicate heading, stale `## Unreleased` | none (uncovered) |
| vad | 0.1.2 | true | 0.1.2 | no | ok | none (uncovered) |
| vault | 0.2.0 | default | 0.2.0 | no | WRONG: "unpublished staging" | yes |
| workflow | 0.2.0 | default | 0.2.0 | no | ok | none (uncovered) |

Across all 34: no `=` exact pins on any internal dependency in any Cargo.toml; no internal dependency floor exceeds its recorded published version; no non-dev path-only internal dependency (the only path-only internal deps are dev-dependencies: `rightkit-mcp` self-reference at mcp/Cargo.toml, `rightkit-http` at sync/Cargo.toml:46 - cargo strips those on package); every crate has description, license, repository and README; `ci/known-red.json` is empty (`macos: {}`, `windows: {}`), so no crate is listed; every crate without a recipe (15: mcp, ocr, platform-rules, review, search, send, sync, time, transcript, tts, tts-qwen, tts-qwen-sys, typeset, vad, workflow) is listed in `ci/recipe-coverage.json` -> `uncovered.cargo` (the coverage file says a unit about to publish must have a recipe).

## rightkit-qa 0.8.0: recorded published, source has later unpublished wave 2

Evidence:
1. `registry.cargo["rightkit-qa"] = "0.8.0"`; `legacyCargo` for qa ends at 0.7.1, so 0.8.0 is the recorded current published version. The record entered the file in the import commit `bbdf6ab` (so the real publish date is UNVERIFIED).
2. `crates/rightkit-qa/Cargo.toml:3` is still `version = "0.8.0"`. Commit `7d64724` (2026-10-24 02:09 +0530, titled "rightkit-qa 0.8.0 + rightkit-child-process 0.5.5: ADR-0001 wave 2 ... (unpublished)", body "Not published: RightKit releases are on hold (Adrian)") added the whole wave-2 runner under that same number: `pool.rs`, `plugin.rs`, `steps/*`, `journeys/run.rs` (4275 lines), `journeys/build_record.rs`, `journeys/lint.rs`, `journeys/bridge.rs`, worker/supervise/budget, `lib.rs` public modules 35 -> 42, and the child-process requirement 0.5.4 -> 0.5.5 (`Cargo.toml:37`). Feed commit `a3ae84c` (02:11 +0530) does not list qa as newly published.
3. `b4f4600` (HEAD, 2026-10-24 00:50 UTC) adds more code (`src/guard.rs` +86, `src/scenario.rs` +125) recorded only under `## Unreleased`.
4. CHANGELOG: `## Unreleased` is line 3; it holds the wave-2 entries, including a BREAKING change (the app-wide `native.lock` run lock is gone, line 24) and `CheckStatus` gaining `errored`/`missing`. `## 0.8.0` is line 31 with no unpublished marker, consistent with the registry. So the CHANGELOG correctly separates released and unreleased, but the Cargo version was never bumped: the published 0.8.0 tarball and the tree differ under one number.
5. Consequence: a `0.8.0` publish would be refused by crates.io (version exists); a breaking change plus new public API needs 0.9.0. The recipe `ci/consumer-recipes.json` and `ci/consumers/rightkit-qa/Cargo.toml` pin `=0.8.0` (they verify the published bits, not wave 2) and `rightkit-qa-driver` depends on `rightkit-qa` 0.8.0 (optional).
6. Stale statements in the CHANGELOG: lines 8 and 20 still say wave 2 is "not yet compiled or tested" / "written without compiling or running Cargo", while the 7d64724 body reports Windows and macOS clippy clean and lib tests 659/0 (Windows) and 728/0 (Mac). Which is current is UNVERIFIED (I did not run anything).
7. Mirror: `@rightkit/qa` is 0.6.1 in `packages/qa/package.json`, `stagedNpm` and `registry.npm`, yet 7d64724 says the Node mirror changed ("CheckStatus errored/missing") in the same release. Same-number drift on the npm side is likely; UNVERIFIED (outside m-z crates).

Verdict: not publishable as 0.8.0. Needs a version bump (0.9.0), the Unreleased block turned into a dated `## 0.9.0` section, `rightkit-child-process` 0.5.5 recorded published (registry already shows 0.5.5), the recipe re-pinned, and the npm mirror bumped.

## rightkit-tauri-shell 0.6.0: source-only, pending publication

State: `Cargo.toml:3` = 0.6.0; `registry.cargo` = 0.5.1 (`legacyCargo` ends 0.5.1); `stagedCargo` = 0.6.0. Version set in `7129a65`. Later commits touch only `CHANGELOG.md` (110b814) and two doc-comment lines (`src/update.rs`, 22e592d; `src/harden.rs`, b98c3e7). No code changed after the version was set.

What clears it (verified from files):
- Every internal dependency floor is at or below the recorded published version, no `=` pins, no path-only non-dev deps (Cargo.toml lines for daemon 0.4.0, job-queue 0.5.0, child-process 0.5.0, app-settings 0.3.0, logs 0.3.4, crash 0.3.4, license 0.2.5, overlay 0.2.0, pdf 0.3.3, models 0.5.0, credentials 0.4.0; registry has 0.4.0, 0.5.1, 0.5.5, 0.3.0, 0.3.5, 0.3.6, 0.2.5, 0.2.0, 0.3.3, 0.5.1, 0.4.2).
- `rightkit-license` 0.2.5, which carries `suite_api` that 0.6.0's `update-license` additions need (license CHANGELOG.md:3-5), is recorded published (feed a3ae84c, 2026-10-24).
- description, license (`MIT OR Apache-2.0`), repository, readme present; LICENSE-MIT, LICENSE-APACHE, README.md, CHANGELOG.md, build.rs all exist; `include` covers `build.rs`, `src/**`, `tests/**`.
- Consumer recipe present: `ci/consumer-recipes.json` cargo."rightkit-tauri-shell" (PK08), file hashes match, `ci/consumers/rightkit-tauri-shell/Cargo.toml` pins `=0.6.0`. Not in `uncovered`, not in known-red.
- `ci/gate.config.json` has a row (5 feature sets, all with `wv2-0_38`; `.rightkit-local-build.json` exists for local publication).
- Version bump is semver-appropriate (0.5.1 -> 0.6.0, BREAKING source field `release_status_url`, CHANGELOG.md:5).

What blocks or still gates it:
1. Publication has not happened (registry 0.5.1). Commit 7d64724's body states "RightKit releases are on hold (Adrian)": a policy hold, not a file defect.
2. Qualification owed and not evidenced for this tree: CHANGELOG.md `0.5.0` section states nothing was compiled, tested or run and that `rustfmt --check`, `cargo test`/`cargo check` for `wv2-0_38` and `wv2-0_39`, and a signed-fixture updater test on macOS were still owed. No later entry records them. The publish gate also needs the unit's `ci/gate.config.json` commands to run with zero failures on both hosts; the evidence for the current commit is UNVERIFIED.
3. Gate coverage gap: the `featureSets` rows do not include a `wv2-0_39` (the new default feature) set (`checkFeatureSets` was not examined); the default-feature build is covered only through the PK08 consumer recipe (pins `=0.6.0`, so it can only resolve after publication - circular for pre-publish evidence). UNVERIFIED whether the pipeline stages a path patch for it (22e592d says "consumer stage patches path siblings").
4. CHANGELOG hygiene: heading `## 0.6.0 (2026-10-23)` (CHANGELOG.md:3) carries no "unpublished" marker although the version is unpublished; `## 0.5.1 (unpublished staging)` (line 9) is wrong, since 0.5.1 is the registry version.
5. `tauri-plugin-updater` is `>=2.10.1, <3` (Cargo.toml) while the workspace lock stays on 2.10.1; runtime behaviour on 2.14 is stated in the CHANGELOG to be the app's qualification (accepted, not a blocker). Advisory exceptions in `ci/advisory-exceptions.json` (lines 101, 120, 139) reference 2.10.1.
6. Possible `cargo package` note (UNVERIFIED): `[[bench]] budget` and `budget_native` and `example-app/` are not in `include`; recent cargo only warns and drops excluded bench targets, it does not fail.

Net: nothing in the source blocks it; it is clear to publish once the hold lifts and gate evidence for HEAD exists, then record 0.6.0 in `registry.cargo` and correct the CHANGELOG markers.

## Ready to publish now

- None are ready by the registry record alone: for 33 of 34 crates the current version already equals `registry.cargo`, so there is nothing new to publish (a republish of the same number is impossible).
- `rightkit-tauri-shell` 0.6.0 is the only crate with an unpublished current version and no source-level blocker; it is "ready" only subject to the hold and the gate evidence in its section above.
- Excluded: `rightkit-sync-keys` (`publish = false`), `rightkit-qa` (needs a bump).

## Blocked

| Crate | Exact blocker |
|---|---|
| rightkit-qa | Version 0.8.0 is recorded published; tree has code changes (7d64724, b4f4600) and a BREAKING change under the same number. Needs bump to 0.9.0 (`crates/rightkit-qa/Cargo.toml:3`), `## Unreleased` (CHANGELOG.md:3) converted to a dated 0.9.0 section, recipe pin `=0.8.0` updated (`ci/consumers/rightkit-qa/Cargo.toml`), release hold lifted. |
| rightkit-tauri-shell | Release hold; no recorded qualification for the 0.6.0 tree (see list above). |
| rightkit-sync-keys | `publish = false` (`crates/rightkit-sync-keys/Cargo.toml:13`, "Parked 2026-10-16 ... Kept, not published"). |
| 15 crates with no recipe: mcp, ocr, platform-rules, review, search, send, sync, time, transcript, tts, tts-qwen, tts-qwen-sys, typeset, vad, workflow | Any future version bump is refused until a reviewed consumer recipe is added to `ci/consumer-recipes.json` (`ci/recipe-coverage.json` "about": "a unit about to publish must have one whatever this list says"). No current bump is pending for them. |
| rightkit-ownership, pdf, record, tts, send, sync, mcp, playback, oauth, models, qa-driver, speech-client, tts-qwen, tts-qwen-sys | No blocker today, but the tree differs from the published tarball in README/doc text (b98c3e7); ownership additionally differs in `catalog/ownership-catalog.json`. A republish is impossible without a bump; whether that drift matters is a policy call. |

## Defects

CHANGELOG markers (current version vs registry record that says published):
- `crates/rightkit-models/CHANGELOG.md:3` "0.5.1 - unpublished staging" (and :15 for 0.5.0) but registry 0.5.1.
- `crates/rightkit-motion/CHANGELOG.md:3` "0.2.0 - unpublished staging".
- `crates/rightkit-oauth/CHANGELOG.md:3` "0.10.0 - unpublished staging"; also :14 and :26 are two `## 0.8.0` headings, :26 and :35 still say "(unpublished)" for 0.8.0/0.7.0, which are long published.
- `crates/rightkit-onnx/CHANGELOG.md:3` "0.2.7 - unpublished staging".
- `crates/rightkit-overlay/CHANGELOG.md:3` "0.2.0 (unreleased)"; duplicate `## 0.1.2` at :9 and :17.
- `crates/rightkit-overlay-win/CHANGELOG.md:3` "0.1.0 (unreleased)".
- `crates/rightkit-platform-rules/CHANGELOG.md:3` and :12 are two `## 0.1.0` headings, :12 says "unreleased".
- `crates/rightkit-playback/CHANGELOG.md:3` "0.3.0 - unpublished staging".
- `crates/rightkit-record/CHANGELOG.md:3` "0.3.3 - unpublished staging".
- `crates/rightkit-sandbox/CHANGELOG.md:3` "0.6.1 (unpublished staging)"; :17 "0.5.1 (unpublished staging)" is superseded.
- `crates/rightkit-sidecar/CHANGELOG.md:3` "0.3.0 - unpublished staging".
- `crates/rightkit-speech-client/CHANGELOG.md:6` "0.4.0 - unpublished staging".
- `crates/rightkit-spend/CHANGELOG.md:3` "0.2.0 (unreleased)"; :33 "0.1.0 - unreleased".
- `crates/rightkit-vault/CHANGELOG.md:3` "0.2.0 - unpublished staging".
- `crates/rightkit-qa-driver/CHANGELOG.md:7` "0.6.0 - unpublished staging" under a published 0.7.0.
- `crates/rightkit-tauri-shell/CHANGELOG.md:3` unmarked unpublished 0.6.0; :9 "0.5.1 (unpublished staging)" is the published version.
- Duplicate current-version headings (two `## X` sections for one version): mcp :3/:21 (0.7.0), review :3/:24 (0.4.0), search :3/:11 (0.4.0), sync :7/:12 (0.2.1), sync-keys :3/:5 (0.1.0), typeset :7/:11 (0.1.0, plus stale `## Unreleased` at :3).
- `crates/rightkit-qa/CHANGELOG.md:8` and `:20`: wave 2 "not yet compiled or tested" contradicts the 7d64724 commit body (UNVERIFIED which is current).

Recipes and registry:
- `ci/consumer-recipes.json:964` has a recipe for `rightkit-sync-keys`, which is `publish = false`; a registry-consumer recipe for an unpublishable crate cannot resolve (`ci/consumers/rightkit-sync-keys/Cargo.toml` pins `=0.1.0`). `registry.cargo` has no entry for it.
- `rightkit-motion` recipe pins `rightkit-ffmpeg =0.2.9` (registry 0.2.10); matches the crate floor, so fine.

Packaging risks (UNVERIFIED without cargo):
- Missing license files in the package dir while `license = "MIT OR Apache-2.0"` is declared: `rightkit-ownership`, `rightkit-record` and `rightkit-tts` ship only README/CHANGELOG (their `include` omits LICENSE-*); cargo accepts this, crates.io legal completeness is a policy call.
- `rightkit-qa` `include` (Cargo.toml `include = [...]`) omits `examples/**` and `benches/**`; the wave-2 tests rely on `examples/step_plugin_fixture.rs`, so tests run from the packaged tarball would fail; `cargo package` verify builds only lib and bins, so packaging itself is probably fine.
- `rightkit-tts-qwen-sys` vendors 7.8 MB (`vendor/**`); crates.io's 10 MB compressed limit is likely fine but not verified.
- `rightkit-sync/Cargo.toml:40` (optional normal dep) and `:46` (path-only dev-dep) name `rightkit-http` twice with different features; cargo strips the dev-dep when packaging; fine.

Not defects but notable: `ci/gate.config.json` `rust` has no row for ocr, ownership, platform-rules, review, search, send, sidecar, speech-client, spend, sync, sync-keys, time, transcript, typeset, vad, vault, workflow; whether a missing row means "default gate" or "ungated" is UNVERIFIED.

---

# Per-crate tables (generated from Cargo.toml, git and ci/*.json)

### rightkit-mcp

| Field | Value |
|---|---|
| version | 0.7.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.7.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.7.0 (2026-10-17)`; CHANGELOG.md:21 `## 0.7.0` - DUPLICATE heading x2 |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-fs` 0.4.0 [dependencies] reg 0.4.7 floor<=published<br>`rightkit-http` 0.3.0 [dependencies] reg 0.3.8 floor<=published<br>`rightkit-mcp` - [dev-dependencies] reg 0.7.0 path-only (dev-dep: stripped at publish, ok) |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'examples/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-models

| Field | Value |
|---|---|
| version | 0.5.1 |
| publish | not set (publishable by default) |
| registry.cargo | 0.5.1 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.5.1 — unpublished staging` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.5.1 as published |
| internal rightkit-* deps | `rightkit-fs` 0.4.3 [dependencies] reg 0.4.7 floor<=published<br>`rightkit-daemon` 0.4.0 [dependencies] reg 0.4.0 floor<=published<br>`rightkit-http` 0.3.0 [dependencies] reg 0.3.8 floor<=published<br>`rightkit-http` 0.3.8 [dev-dependencies] reg 0.3.8 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-motion

| Field | Value |
|---|---|
| version | 0.2.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.0 — unpublished staging (2026-10-18)` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.2.0 as published |
| internal rightkit-* deps | `rightkit-transcript` 0.4.0 [dependencies] reg 0.4.0 floor<=published<br>`rightkit-ffmpeg` 0.2.9 [dependencies, optional] reg 0.2.10 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-oauth

| Field | Value |
|---|---|
| version | 0.10.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.10.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.10.0 — unpublished staging` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.10.0 as published |
| internal rightkit-* deps | `rightkit-http` 0.3.0 [dependencies] reg 0.3.8 floor<=published<br>`rightkit-credentials` 0.4.1 [dependencies, optional] reg 0.4.2 floor<=published<br>`rightkit-child-process` 0.5.0 [dependencies, optional] reg 0.5.5 floor<=published<br>`rightkit-http` 0.3.0 [dev-dependencies] reg 0.3.8 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'examples/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-ocr

| Field | Value |
|---|---|
| version | 0.2.3 |
| publish | `publish = true` |
| registry.cargo | 0.2.3 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:4 `## 0.2.3` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-onnx` 0.2.6 [dependencies, optional] reg 0.2.7 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'PROVENANCE.toml']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-onnx

| Field | Value |
|---|---|
| version | 0.2.7 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.7 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.7 — unpublished staging` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.2.7 as published |
| internal rightkit-* deps | `rightkit-ffmpeg` 0.2.10 [dependencies, optional] reg 0.2.10 floor<=published<br>`rightkit-models` 0.5.0 [dependencies, optional] reg 0.5.1 floor<=published<br>`rightkit-ffmpeg` 0.2.10 [dev-dependencies] reg 0.2.10 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-overlay

| Field | Value |
|---|---|
| version | 0.2.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.0 (unreleased)` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.2.0 as published |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-overlay-win

| Field | Value |
|---|---|
| version | 0.1.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.1.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.1.0 (unreleased)` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.1.0 as published |
| internal rightkit-* deps | `rightkit-overlay` 0.2.0 [dependencies] reg 0.2.0 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'examples/**', 'README.md', 'CHANGELOG.md', 'NOTICE', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-ownership

| Field | Value |
|---|---|
| version | 0.4.2 |
| publish | `publish = true` |
| registry.cargo | 0.4.2 |
| version set in | `7129a65` |
| last commit touching dir | `b98c3e7` |
| source changed since version set | Version set in 7129a65. Since then: 110b814 (CHANGELOG only), b98c3e7 edits 5 descriptive strings in catalog/ownership-catalog.json (the file is in `include`; b98c3e7 is later than the 0.4.2 publish feed a3ae84c). Whether the embedded-catalog digest changes is UNVERIFIED. |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.4.2 (2026-10-23)` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: none (README/CHANGELOG only); include: `['src/**', 'tests/**', 'CHANGELOG.md', 'README.md', 'rightkit-ownership.json', 'catalog/**']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-pdf

| Field | Value |
|---|---|
| version | 0.3.3 |
| publish | not set (publishable by default) |
| registry.cargo | 0.3.3 |
| top-level `cargo` map (older record) | 0.1.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.3.3` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-fs` 0.4.0 [dependencies, optional] reg 0.4.7 floor<=published<br>`rightkit-fs` 0.4.0 [dev-dependencies] reg 0.4.7 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'assets/**', 'tests/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-platform-rules

| Field | Value |
|---|---|
| version | 0.1.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.1.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.1.0 (2026-10-17)`; CHANGELOG.md:12 `## 0.1.0 - unreleased` - DUPLICATE heading x2 |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.1.0 as published |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'assets/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-playback

| Field | Value |
|---|---|
| version | 0.3.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.3.0 |
| top-level `cargo` map (older record) | 0.1.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.3.0 — unpublished staging (2026-10-18)` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.3.0 as published |
| internal rightkit-* deps | `rightkit-ffmpeg` 0.2.3 [dependencies] reg 0.2.10 floor<=published<br>`rightkit-child-process` 0.5.0 [dependencies] reg 0.5.5 floor<=published<br>`rightkit-fs` 0.4.0 [dependencies] reg 0.4.7 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-qa

| Field | Value |
|---|---|
| version | 0.8.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.8.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b4f4600` |
| source changed since version set | YES, CODE. 7d64724 (2026-10-24 02:09 +0530, "rightkit-qa 0.8.0 ... ADR-0001 wave 2 ... (unpublished)") adds ~35k lines (pool, plugin, steps/*, journeys/run|build_record|lint|bridge, worker, supervise, budget; lib.rs `pub mod` 35 -> 42) on top of the 0.8.0 set in bbdf6ab, plus b98c3e7 (rename text) and b4f4600 (2026-10-24 00:50 UTC, HEAD at audit time: `platform:` alias + gated-block guard fix, code in src/guard.rs and src/scenario.rs, recorded only under `## Unreleased`). |
| CHANGELOG, current version | CHANGELOG.md:31 `## 0.8.0` |
| CHANGELOG status check | 0.8.0 section (line 31) has no marker (consistent with registry published); wave-2 work correctly sits under `## Unreleased` (line 3) but the 0.8.0 number is not bumped |
| internal rightkit-* deps | `rightkit-fs` 0.4.4 [dependencies] reg 0.4.7 floor<=published<br>`rightkit-sidecar` 0.3.0 [dependencies] reg 0.3.0 floor<=published<br>`rightkit-daemon` 0.4.0 [dependencies] reg 0.4.0 floor<=published<br>`rightkit-child-process` 0.5.5 [dependencies] reg 0.5.5 floor<=published<br>`rightkit-browser` 0.3.0 [dependencies, optional] reg 0.3.6 floor<=published<br>`rightkit-automation` 0.6.0 [dependencies] reg 0.6.1 floor<=published<br>`rightkit-fs` 0.4.4 [dev-dependencies] reg 0.4.7 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'docs/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-qa-driver

| Field | Value |
|---|---|
| version | 0.7.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.7.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.7.0` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-automation` 0.6.0 [dependencies] reg 0.6.1 floor<=published<br>`rightkit-qa` 0.8.0 [dependencies, optional] reg 0.8.0 floor<=published<br>`rightkit-daemon` 0.4.0 [dev-dependencies] reg 0.4.0 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-record

| Field | Value |
|---|---|
| version | 0.3.3 |
| publish | `publish = true` |
| registry.cargo | 0.3.3 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.3.3 — unpublished staging` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.3.3 as published |
| internal rightkit-* deps | `rightkit-contracts` 0.1.5 [dependencies] reg 0.1.7 floor<=published<br>`rightkit-sidecar` 0.3.0 [dependencies] reg 0.3.0 floor<=published<br>`rightkit-credentials` 0.4.0 [dependencies] reg 0.4.2 floor<=published<br>`rightkit-child-process` 0.5.0 [dependencies cfg(target_os = "macos")] reg 0.5.5 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: none (README/CHANGELOG only); include: `['src/**', 'tests/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'rightkit-ownership.json', 'helpers/macos/Package.swift', 'helpers/macos/build.sh', 'helpers/macos/Sources/**', 'helpers/macos/Tests/**']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-review

| Field | Value |
|---|---|
| version | 0.4.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.4.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.4.0 (2026-10-17)`; CHANGELOG.md:24 `## 0.4.0` - DUPLICATE heading x2 |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-sandbox

| Field | Value |
|---|---|
| version | 0.6.1 |
| publish | not set (publishable by default) |
| registry.cargo | 0.6.1 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.6.1 (unpublished staging)` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.6.1 as published |
| internal rightkit-* deps | `rightkit-child-process` 0.5.0 [dependencies] reg 0.5.5 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-search

| Field | Value |
|---|---|
| version | 0.4.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.4.0 |
| top-level `cargo` map (older record) | 0.1.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.4.0 (2026-10-17)`; CHANGELOG.md:11 `## 0.4.0` - DUPLICATE heading x2 |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'Cargo.toml', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-send

| Field | Value |
|---|---|
| version | 0.2.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.0 (2026-10-17)` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-child-process` 0.5.0 [dependencies] reg 0.5.5 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'benches/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-sidecar

| Field | Value |
|---|---|
| version | 0.3.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.3.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.3.0 — unpublished staging` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.3.0 as published |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-speech-client

| Field | Value |
|---|---|
| version | 0.4.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.4.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:6 `## 0.4.0 — unpublished staging` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.4.0 as published |
| internal rightkit-* deps | `rightkit-daemon` 0.4.0 [dependencies] reg 0.4.0 floor<=published<br>`rightkit-sidecar` 0.3.0 [dependencies] reg 0.3.0 floor<=published<br>`rightkit-transcript` 0.4.0 [dependencies] reg 0.4.0 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'API.md', 'INDEX.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-spend

| Field | Value |
|---|---|
| version | 0.2.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.0 (unreleased)` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.2.0 as published |
| internal rightkit-* deps | `rightkit-db` 0.5.0 [dependencies] reg 0.5.2 floor<=published<br>`rightkit-db` 0.5.0 [dev-dependencies] reg 0.5.2 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-sync

| Field | Value |
|---|---|
| version | 0.2.2 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.2 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.2 (2026-10-17)` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-child-process` 0.5.0 [dependencies, optional] reg 0.5.5 floor<=published<br>`rightkit-fs` 0.4.0 [dependencies] reg 0.4.7 floor<=published<br>`rightkit-http` 0.3.0 [dependencies, optional] reg 0.3.8 floor<=published<br>`rightkit-http` - [dev-dependencies] reg 0.3.8 path-only (dev-dep: stripped at publish, ok) |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', '!src/bin/**', 'tests/**', '!tests/helper_e2e.rs', 'benches/*.rs', 'README.md', 'API.md', 'HTTP-PROTOCOL.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-sync-keys

| Field | Value |
|---|---|
| version | 0.1.0 |
| publish | `publish = false` |
| registry.cargo | none |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.1.0 (2026-10-17)`; CHANGELOG.md:5 `## 0.1.0` - DUPLICATE heading x2 |
| CHANGELOG status check | n/a (publish = false); heading dated, no registry record |
| internal rightkit-* deps | `rightkit-credentials` 0.4.0 [dependencies] reg 0.4.2 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-tauri-shell

| Field | Value |
|---|---|
| version | 0.6.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.5.1 |
| version set in | `7129a65` |
| last commit touching dir | `b98c3e7` |
| source changed since version set | Version set in 7129a65. Since then: 110b814 (CHANGELOG only), 22e592d (1 doc-comment line in src/update.rs), b98c3e7 (1 doc-comment line in src/harden.rs). No code change. |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.6.0 (2026-10-23)` |
| CHANGELOG status check | Marker WRONG/absent: version is unpublished (registry 0.5.1) but heading carries no unpublished marker; 0.5.1 heading (line 9) says unpublished though 0.5.1 is the registry version |
| internal rightkit-* deps | `rightkit-daemon` 0.4.0 [dependencies] reg 0.4.0 floor<=published<br>`rightkit-job-queue` 0.5.0 [dependencies, optional] reg 0.5.1 floor<=published<br>`rightkit-child-process` 0.5.0 [dependencies] reg 0.5.5 floor<=published<br>`rightkit-app-settings` 0.3.0 [dependencies, optional] reg 0.3.0 floor<=published<br>`rightkit-logs` 0.3.4 [dependencies, optional] reg 0.3.5 floor<=published<br>`rightkit-crash` 0.3.4 [dependencies, optional] reg 0.3.6 floor<=published<br>`rightkit-license` 0.2.5 [dependencies, optional] reg 0.2.5 floor<=published<br>`rightkit-overlay` 0.2.0 [dependencies, optional] reg 0.2.0 floor<=published<br>`rightkit-pdf` 0.3.3 [dependencies, optional] reg 0.3.3 floor<=published<br>`rightkit-models` 0.5.0 [dependencies, optional] reg 0.5.1 floor<=published<br>`rightkit-credentials` 0.4.0 [dependencies, optional] reg 0.4.2 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['build.rs', 'src/**', 'tests/**', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'CHANGELOG.md']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-time

| Field | Value |
|---|---|
| version | 0.1.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.1.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.1.0 (2026-10-17)` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-transcript

| Field | Value |
|---|---|
| version | 0.4.0 |
| publish | `publish = true` |
| registry.cargo | 0.4.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:6 `## 0.4.0` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | none |
| metadata | license `MIT`; missing fields: none; license files in dir: ['LICENSE']; include: `['src/**', 'tests/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-tts

| Field | Value |
|---|---|
| version | 0.2.3 |
| publish | `publish = true` |
| registry.cargo | 0.2.3 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:6 `## 0.2.3 (2026-10-17)` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-fs` 0.4.0 [dependencies, optional] reg 0.4.7 floor<=published<br>`rightkit-onnx` 0.2.5 [dependencies, optional] reg 0.2.7 floor<=published<br>`rightkit-child-process` 0.5.0 [dependencies, optional] reg 0.5.5 floor<=published<br>`rightkit-transcript` 0.4.0 [dependencies, optional] reg 0.4.0 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: none (README/CHANGELOG only); include: `['src/**', 'tests/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-tts-qwen

| Field | Value |
|---|---|
| version | 0.2.7 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.7 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.7` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-tts-qwen-sys` 0.1.0 [dependencies] reg 0.1.2 floor<=published<br>`rightkit-onnx` 0.2.6 [dependencies] reg 0.2.7 floor<=published |
| metadata | license `Apache-2.0`; missing fields: none; license files in dir: ['LICENSE']; include: `['src/**', 'proto/**', 'Cargo.toml', 'README.md', 'LICENSE', 'NOTICE', 'PINNED.md', 'CHANGELOG.md', 'testdata/**']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-tts-qwen-sys

| Field | Value |
|---|---|
| version | 0.1.2 |
| publish | not set (publishable by default) |
| registry.cargo | 0.1.2 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `b98c3e7` |
| source changed since version set | `b98c3e7`: b98c3e7 is the `rightkit` -> `rustright` rename (comments, READMEs, doc strings; no behaviour change read) |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.1.2 - 2026-10-10` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | none |
| metadata | license `Apache-2.0 AND MIT`; missing fields: none; license files in dir: ['LICENSE']; include: `['src/**', 'build.rs', 'CHANGELOG.md', 'bindings_stamp.rs', 'wrapper.h', 'bindings-*.rs', 'vendor/**', 'Cargo.toml', 'README.md', 'VERSIONS.md', 'LICENSE', 'NOTICE', 'PINNED.md']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-typeset

| Field | Value |
|---|---|
| version | 0.1.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.1.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:7 `## 0.1.0 (2026-10-17)`; CHANGELOG.md:11 `## 0.1.0 - 2026-10-17` - DUPLICATE heading x2 |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD-PARTY.md']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-vad

| Field | Value |
|---|---|
| version | 0.1.2 |
| publish | `publish = true` |
| registry.cargo | 0.1.2 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:6 `## 0.1.2` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | `rightkit-onnx` 0.2.6 [dependencies, optional] reg 0.2.7 floor<=published<br>`rightkit-ffmpeg` 0.2.3 [dev-dependencies] reg 0.2.10 floor<=published |
| metadata | license `MIT`; missing fields: none; license files in dir: ['LICENSE']; include: `['src/**', 'tests/*.rs', 'Cargo.toml', 'README.md', 'CHANGELOG.md', 'LICENSE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-vault

| Field | Value |
|---|---|
| version | 0.2.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.0 — unpublished staging` |
| CHANGELOG status check | INCONSISTENT: heading says unpublished/unreleased but registry.cargo records 0.2.0 as published |
| internal rightkit-* deps | `rightkit-credentials` 0.4.0 [dependencies] reg 0.4.2 floor<=published<br>`rightkit-http` 0.3.1 [dependencies] reg 0.3.8 floor<=published |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'README.md', 'API.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: True; ci/consumers dir: True; recipe-coverage uncovered: False |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |

### rightkit-workflow

| Field | Value |
|---|---|
| version | 0.2.0 |
| publish | not set (publishable by default) |
| registry.cargo | 0.2.0 |
| version set in | `bbdf6ab` (note: bbdf6ab is the repo-import baseline, 6664 files, so it only means "unchanged since import") |
| last commit touching dir | `bbdf6ab` |
| source changed since version set | NO |
| CHANGELOG, current version | CHANGELOG.md:3 `## 0.2.0` |
| CHANGELOG status check | ok (no unpublished marker; registry records published) |
| internal rightkit-* deps | none |
| metadata | license `MIT OR Apache-2.0`; missing fields: none; license files in dir: ['LICENSE-APACHE', 'LICENSE-MIT']; include: `['src/**', 'tests/**', 'README.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE']` |
| consumer recipe | consumer-recipes.json cargo entry: False; ci/consumers dir: False; recipe-coverage uncovered: True |
| known-red | no (ci/known-red.json is empty: `macos: {}`, `windows: {}`) |
| README | present |
