# npm package publish-readiness audit (packages/*)

Repo /home/user/rightkit, HEAD d9a5016, branch claude/hopeful-albattani-loe9db. Read-only. Audited 2026-10-24.

## Method limits (read first)

- The clone is SHALLOW (`git rev-parse --is-shallow-repository` = true; root commit bbdf6ab, 2026-10-23). `git log -S'"version"'` therefore returns bbdf6ab for almost every package, so "commit that last changed the version line" is UNVERIFIED for them. I used the recorded registry version plus post-root commits that touch each package dir (excluding CHANGELOG.md) as the "unpublished changes" signal. Only packages with a post-root source commit are flagged as differing.
- No network, so the real npm registry was not queried. "Published" means `packages/release/rightkit-versions.json` `registry.npm`, as instructed.
- No package except `release` has node_modules and no package has a built `dist/`. Every `exports`/`bin` path under `dist/` is therefore "not on disk" here. I checked only that `files` covers each export path textually (it does for all). Whether the build emits those paths is UNVERIFIED.
- The working tree is dirty. `packages/release` has 8 modified files (CHANGELOG.md, README.md, prebuild-gates.mjs, release-config.mjs, release.mjs, release.test.mjs, win-package-recipe.mjs, win-package-recipe.test.mjs). I read them as-is. Nothing else under packages/ is modified.
- Tests: I ran `node --test` for hooks, issues-tracker, ax, ci and release (no install needed or already installed). The other packages use vitest or a build step and have no node_modules, so they are UNVERIFIED (not run).

## Registry facts
- 24 publishable npm packages are in packages/. `build-control-policy` and `sync-worker` have `"private": true` and are not publishable.
- `registry.npm` equals package.json for 23 of the 24. The one exception is `@rightkit/release` (registry 0.6.3, package.json 0.6.4).
- Every publishable package is covered: 12 have a `ci/consumer-recipes.json` npm recipe and 12 are in `ci/recipe-coverage.json` `uncovered.npm`. recipe-coverage says a unit about to publish must have a recipe "whatever this list says".

## Table

Legend. Recipe: R = reviewed recipe in ci/consumer-recipes.json, U = only listed in ci/recipe-coverage.json uncovered (no recipe). Deps "int" = internal @rightkit/*. All 24 have a README and a `test` script. "Tests exist" is the count of *.test.*/*.spec.* files.

| Dir / name | pkg ver | registry.npm | Source differs from published? | CHANGELOG for current version | Internal deps (exact? published?) | External deps | exports/files | Test script / tests exist / run result | Recipe |
|---|---|---|---|---|---|---|---|---|---|
| app-update `@rightkit/app-update` | 0.4.2 | 0.4.2 | No evidence. Last src commit d137a9c is an ancestor of feed commit a3ae84c, which records 0.4.2 as published. | NO 0.4.2 heading. Top is `## 0.4.1 — unpublished staging` (line 3). The 0.4.2 content is absent and 0.4.1 is mislabelled. | none | peer react `>=18 <21` (range) | 12 export paths, all under dist, covered by `files`. dist not built. | vitest run / 15 files / UNVERIFIED (no node_modules) | R |
| ax `@rightkit/ax` | 0.3.2 | 0.3.2 | No post-root change | `## 0.3.2` present (line 3), no published/unpublished label | none | none | 14 paths, covered. All files on disk. | node --test / 20 files / PASS 268/268 | U |
| build-control-policy | 0.1.4-phase1 | n/a (private) | n/a. 12 post-root commits touch it. | `## 0.1.4-phase1 (2026-10-19)` | none | none | no exports, no files | node --test / 8 files / UNVERIFIED | n/a |
| ci `@rightkit/ci` | 0.3.0 | 0.3.0 | No post-root change | `## 0.3.0 (2026-10-17)` present | none | none | 1 export, covered | node --test / 17 files / PASS 216, 0 fail, 5 skipped (221) | U |
| doc-render | 0.2.1 | 0.2.1 | No post-root change | `## 0.2.1 — unpublished staging` (line 3): present, but the label is stale because the registry says published | none (devDep `@rightkit/qa-chrome` is `workspace:*`) | 11 `^` ranges (@types/mdast, dompurify, github-slugger, remark-*, shiki, unified); parse5 exact 8.0.1; peer react range | 6 paths, covered | vitest run / 4 files / UNVERIFIED | R |
| hooks | 0.1.2 | 0.1.2 | No post-root change | NO CHANGELOG.md file | none | none | 3 bin/lib paths, all on disk, covered | node --test / 2 files / PASS 6/6 | U |
| i18n | 0.2.0 | 0.2.0 | No post-root change | `## 0.2.0 (2026-10-17)` present. Also `## 0.1.1 - unreleased` (line 12), but legacyNpm lists 0.1.1 as published. | none | peer react range | 4 paths, covered | vitest run / 1 file / UNVERIFIED | U |
| issues-tracker | 0.1.0 | 0.1.0 | No post-root change | `## 0.1.0 (unreleased)` (line 3). Wrong: registry says published. | none | none | 1 export, covered | node --test / 1 file / PASS 21/21 | R |
| legal | 0.6.3 | 0.6.3 | No evidence. Last src commit 1e3a4d4 is an ancestor of feed a3ae84c. | `## 0.6.3 (unpublished staging)` (line 3): present, label stale | peer `@rightkit/release ^0.4.1 \|\| ^0.5.0 \|\| ^0.6.0` (range, optional). Release 0.6.3 is published, so it resolves. | none | 11 paths, covered | `vitest run src && pnpm run test:tooling` / 5 files / UNVERIFIED | R |
| legal-ui | 0.4.1 | 0.4.1 | No post-root change | `## 0.4.1 (2026-10-19)` present | none | peer react range | 5 paths, covered | vitest run / 6 files / UNVERIFIED | U |
| license `@rightkit/license` | 0.3.0 | 0.3.0 | No functional change. Last commit a6a6076 re-indented package.json (`git show -w --stat` is empty). | PRESENT but malformed. Line 1 is `# 0.3.0` (no `# Changelog` header); the next heading at line 10 is `## 0.2.1 — unpublished staging`, and the 0.2.x section follows. Format differs from every other package. | none | none | 6 paths, covered. `files` lists dist files individually, so README is auto-included only. | vitest run / 6 files / UNVERIFIED | U |
| logs | 0.1.6 | 0.1.6 | No post-root change | `## 0.1.6 — unpublished staging` (line 3): present, label stale | none | none | 4 paths, covered | vitest run / 3 files / UNVERIFIED | R (old-style: no recipeId/scenarios/surface/closure; only pkId, id, files) |
| media-ui | 0.3.4 | 0.3.4 | No post-root change | `## 0.3.4` present | none (devDep qa-chrome `workspace:*`) | peers fabric `^7.4.0`, react-image-crop `^11.1.2`, react/react-dom range | 7 paths, covered | vitest run / 7 files / UNVERIFIED | U |
| onboarding | 0.6.3 | 0.6.3 | No post-root change | `## 0.6.3 — unpublished staging`: present, label stale | peers `@rightkit/react-shell >=0.2.0 <1`, `@rightkit/legal-ui >=0.4.1 <1` (ranges; published versions satisfy them). devDeps `workspace:*`. | peer react range | 5 paths, covered | vitest run / 5 files / UNVERIFIED | R |
| outdated | 0.2.0 | 0.2.0 | YES, unpublished and unversioned. b98c3e7 is NOT an ancestor of feed a3ae84c. It changes src/apply.ts (argv `"rightkit"` to `"rustright"` and the error text), README.md and 3 tests. | `## 0.2.0` present (line 7), but there is also `## Unreleased` (line 3) saying "Renamed from @rightkit/update; it was never published under the old name". The b98c3e7 change has no entry. | dep `@rightkit/ci workspace:^` (rewritten by pnpm at pack time; not an exact pin) | smol-toml exact 1.9.1 | 5 paths, covered (`dist/bin.js` is the bin) | vitest run / 11 files / UNVERIFIED | U |
| pdf-ui | 0.2.0 | 0.2.0 | No post-root change | `## 0.2.0 (2026-10-17)` present | none | dep pdfjs-dist `^6.4.299` (range); peer react range | 5 paths, covered | vitest run / 7 files / UNVERIFIED | U |
| platform-ui | 0.4.0 | 0.4.0 | No post-root change | `## 0.4.0 — unpublished staging`: present, label stale | none | peers `@tauri-apps/plugin-os >=2 <3`, react range | 10 paths, covered | vitest run / 16 files / UNVERIFIED | U |
| qa `@rightkit/qa` | 0.6.1 | 0.6.1 | YES, unpublished and unversioned. 7d64724 (src/evidence.ts, evidence.test.ts) adds `errored`/`missing` to `EvidenceCheckStatus`. `writeEvidenceBundle` now throws on a passing receipt that contains non-passing checks. Its commit message says "Not published". | `## 0.6.1` present (line 3, "qa-chrome 0.1.11" bump). It does NOT mention the 7d64724 type/behaviour change. Older sections `0.6.0` and `0.5.2` (lines 7, 20) are still labelled "unpublished staging", but legacyNpm lists both as published. | dep `@rightkit/qa-chrome 0.1.11`: exact, equals registry (published) | none | 12 paths, covered (`dist/journeys/cli.js` bin) | `pnpm --filter @rightkit/qa-chrome build && pnpm run build && vitest run && node --test ...` / 24 files / UNVERIFIED (needs build and install) | R |
| qa-chrome | 0.1.11 | 0.1.11 | No post-root change | `## 0.1.11` present. `0.1.10 (unreleased)` (line 7) and `0.1.9 (unreleased)` (line 11) are wrong: legacyNpm lists 0.1.10 as published and 0.1.9 is not in any list. | none | none | 5 paths, covered | vitest run / 4 files / UNVERIFIED | R |
| react-shell | 0.7.2 | 0.7.2 | No post-root change | `## 0.7.2 — unpublished staging`: present, label stale | peer `@rightkit/platform-ui >=0.2.8 <1` (range, ok) | peers `@tauri-apps/plugin-os >=2 <3`, react, react-dom ranges | 13 paths, covered | vitest run / 25 files / UNVERIFIED | U |
| release `@rightkit/release` | 0.6.4 | 0.6.3 | YES. 0.6.4 is the staged, unpublished bump (c61e314). 9 commits touch it after the bump commit, plus 8 uncommitted modified files in the working tree. | `## 0.6.4 (unpublished staging)` present (line 14). A new top section `## Unreleased (ADR-0001 item 10, W4-A)` (line 3) holds the uncommitted edits and is not yet under 0.6.4. | none | smol-toml exact 1.9.1 | bin only (cli, bin), no dangling paths. `files` excludes `*.test.mjs` and standalone-clone-evidence.json. | node --test (broker-guard, concurrency 1) / 214 files / FAIL 16 of 2962 here (2890 pass, 56 skipped). See Blocked. | R |
| settings-ui | 0.7.0 | 0.7.0 | No post-root change | `## 0.7.0 — unpublished staging`: present, label stale | peers `@rightkit/react-shell >=0.2.0 <1`, `license >=0.3.0 <0.4`, `logs >=0.1.0`, `theme >=0.2.0 <1`, `app-update >=0.2.0` (all ranges; the published versions satisfy them) | peer react range | 15 paths, covered | vitest run / 23 files / UNVERIFIED | R |
| sync-worker | 0.1.0 | n/a (private) | n/a | `## 0.1.0` | none | n/a | no exports, no files | vitest run / 3 files / UNVERIFIED | n/a |
| tauri `@rightkit/tauri` | 0.3.0 | 0.3.0 | No post-root change | `## 0.3.0 — unpublished staging`: present, label stale | dep `@rightkit/logs ^0.1.4` (RANGED, not exact; 0.1.6 is published and satisfies it); peer `@rightkit/app-update >=0.2.5 <0.5` (range; 0.4.2 satisfies it) | peer `@tauri-apps/plugin-dialog ^2.0.0` | 6 paths, covered | vitest run / 3 files / UNVERIFIED | R |
| tauri-shell | 0.1.3 | 0.1.3 | No evidence. Last src commit 7129a65 is an ancestor of feed a3ae84c. | NO 0.1.3 heading. Top is `## 0.1.2 — unpublished staging` (line 3), then `## Unreleased` (line 14), then `## 0.1.1`. | none | none | 4 paths, covered | `pnpm run build && node --test tests/shell.test.mjs` / 1 file / UNVERIFIED | R |
| theme | 0.3.1 | 0.3.1 | No post-root change | `## 0.3.1 — unpublished staging`: present, label stale | none (devDep qa-chrome `workspace:*`) | none | 7 paths, covered | `pnpm --filter "@rightkit/theme^..." run build && pnpm run build && vitest run` / 3 files / UNVERIFIED | U |

## Ready to publish now

None unconditionally.

Only `@rightkit/release` meets the version test (0.6.4 bumped, registry 0.6.3, changelog entry, no internal deps, recipe present). It is blocked below. Every other package is at its registry version with no post-root source change.

## Blocked

- `@rightkit/release` 0.6.4. Not publishable from this tree as it stands:
  1. Uncommitted edits in 8 shipped or test files (list above). A publish must come from a clean commit, and the 0.6.4 content is not fixed until they are committed.
  2. Their changelog text sits under `## Unreleased` (packages/release/CHANGELOG.md:3), not under `## 0.6.4`. Move it under 0.6.4 if it ships in 0.6.4, or bump the version.
  3. Tests: 16 of 2962 fail in this sandbox. The ones I checked look environmental:
     - `scraperight`/`heardright`/`coderight` contract tests: sibling repos at /home/user/{scraperight,heardright,coderight} cannot resolve `@rightkit/release`.
     - `public dev dry-run ...`: "development target mac requires its native host".
     - `doctor without --gates keeps its output (golden)`: the extra `[WARN] sccache` line (sccache not on PATH).
     - Still UNVERIFIED as environmental or real: `ownership fixtures match the current implementation (generate.mjs --check)` reports 2 drifted fixtures (possibly caused by the other agent's in-flight edits), `local-runtime.test.mjs`, the two `elevation`/`checkout elevation` tests, `committed scanner verifies pinned key log`, `release worker injects PATH ...`, `build runner injects shim ...`, `right-release dev passes installed shim ...`, `install command failure is surfaced ...`, `pnpm subprocess preserves JSON arguments ...`, `HeardRight and ScrapeRight expose one locked ASR promotion adapter contract`.
     - A clean run on the real CI host is needed.
  4. Registry record oddity: `legacyNpm["@rightkit/release"]` already contains "0.6.3", which is also `registry.npm`. It may be intentional once 0.6.4 ships, but it should be confirmed.
- `@rightkit/qa` 0.6.1. Source changed (evidence.ts, 7d64724) with no version bump. Bump to 0.6.2 (or 0.7.0, since `EvidenceCheckStatus` widens and `writeEvidenceBundle` now rejects previously accepted input), add a changelog entry, re-hash the recipe if its files changed, and run the tests (needs build and install).
- `@rightkit/outdated` 0.2.0. Source changed (apply.ts argv and error text, b98c3e7) with no version bump and no changelog entry. Bump to 0.2.1 and add an entry. It has no reviewed recipe (listed in uncovered), and publication refuses that unit until one is written.
- No reviewed consumer recipe, so these cannot be published (recipe-coverage says publication refuses them): ax, ci, hooks, i18n, legal-ui, license, media-ui, outdated, pdf-ui, platform-ui, react-shell, theme.
- hooks: no CHANGELOG.md. Nothing is wrong with the recorded version, but a changelog entry is needed before the next publish.

## Defects (concrete)

1. packages/app-update/CHANGELOG.md:3. The top section is `## 0.4.1 — unpublished staging`, but package.json:3 is 0.4.2, the registry has 0.4.2 and 0.4.1 is in legacyNpm. There is no 0.4.2 heading. The staged content is mislabelled and the 0.4.2 entry is missing.
2. packages/tauri-shell/CHANGELOG.md:3. The same pattern: `## 0.1.2 — unpublished staging` while package.json:3 is 0.1.3 (published). There is no 0.1.3 heading and a stray `## Unreleased` at line 14. packages/tauri-shell/package.json also has no `publishConfig`. Every other public package sets `{"registry":"https://registry.npmjs.org","access":"public"}`, so a scoped publish needs an explicit `--access public`.
3. Stale "unpublished staging" labels on versions the registry records as published:
   - doc-render/CHANGELOG.md:3
   - legal/CHANGELOG.md:3
   - logs/CHANGELOG.md:3
   - onboarding/CHANGELOG.md:3
   - platform-ui/CHANGELOG.md:3
   - react-shell/CHANGELOG.md:3
   - settings-ui/CHANGELOG.md:3
   - tauri/CHANGELOG.md:3
   - theme/CHANGELOG.md:3
   - qa/CHANGELOG.md:7 and :20 (0.6.0 and 0.5.2, both in legacyNpm)
   - license/CHANGELOG.md:10 (0.2.1, in legacyNpm)
4. Stale "unreleased" labels on published versions: issues-tracker/CHANGELOG.md:3 (`0.1.0 (unreleased)`), i18n/CHANGELOG.md:12 (`0.1.1 - unreleased`, in legacyNpm), qa-chrome/CHANGELOG.md:7 (`0.1.10 (unreleased)`, in legacyNpm). Also qa-chrome/CHANGELOG.md:11, `0.1.9 (unreleased)`, which is in no published list (UNVERIFIED whether it was skipped or published).
5. packages/license/CHANGELOG.md:1. The file starts with `# 0.3.0` instead of `# Changelog`, and the history below is under `## 0.2.1 — unpublished staging` at line 10. The format is inconsistent and tooling that expects a `## <version>` heading may miss the 0.3.0 entry.
6. packages/qa: unversioned source change. src/evidence.ts:18 (widened `EvidenceCheckStatus`) and src/evidence.ts:153-157 (new throw in `writeEvidenceBundle`) were added after the recorded 0.6.1 release without a bump or changelog entry.
7. packages/outdated: unversioned source change. src/apply.ts:173 (argv) and the error text at about line 187 changed `rightkit` to `rustright` without a bump or changelog entry. The CHANGELOG `## Unreleased` section at line 3 is also a leftover.
8. packages/ax, packages/ci, packages/hooks: package.json declares `"license": "MIT OR Apache-2.0"` but the directories contain no LICENSE files and `files` does not list any (ax: package.json `files` array; ci: `files` array; hooks: `files` array). The published tarballs carry no license text. All other public packages ship LICENSE-MIT and LICENSE-APACHE.
9. packages/tauri: `license` is `MIT OR Apache-2.0` (package.json:5), but only a single `LICENSE` file exists and `files` lists only `LICENSE`. This is inconsistent with the dual license and with the sibling packages.
10. packages/tauri/package.json:34. `@rightkit/logs` is `^0.1.4` (a range) in `dependencies`. The task asked for exact internal pins. It resolves to the published 0.1.6 today but would float on a future 0.1.x.
11. packages/outdated/package.json (dependencies). `@rightkit/ci` is `workspace:^`, so the published range is derived from the workspace at pack time. A publish outside pnpm would leave an unresolvable protocol. The package also has an explicit `"private": false`.
12. Peer ranges on internal packages are unbounded or loose: settings-ui `@rightkit/logs >=0.1.0` and `@rightkit/app-update >=0.2.0` (no upper bound), react-shell `@rightkit/platform-ui >=0.2.8 <1`, onboarding `>=0.2.0 <1`. They are not pins, but the current published versions satisfy them. I did not test the lower bounds against the code (UNVERIFIED).
13. ci/consumer-recipes.json, `@rightkit/logs` entry: an old-style recipe with only `pkId`/`id`/`files`. It has no `recipeId`, `scenarios`, `surface` or `closure` fields, unlike the other 11, so it may not meet the current review contract (UNVERIFIED against scripts/preflight.mjs).
14. packages/release/rightkit-versions.json: `legacyNpm["@rightkit/release"]` includes "0.6.3" while `registry.npm["@rightkit/release"]` is "0.6.3". Entries should be disjoint if legacy means "superseded".
15. All packages: `exports` and `bin` paths point into `dist/`, which is not present in this checkout. Whether the build emits them is UNVERIFIED. `license` lists dist files individually (`dist/index.js`, `dist/index.d.ts`, `dist/testing.js`, `dist/testing.d.ts`), so any extra emitted chunk would be missed.
16. UNVERIFIED overall: vitest-based packages (about 17), qa and tauri-shell build-chained tests, and build-control-policy / sync-worker tests were not run.

## Test results actually run

| package | command | result |
|---|---|---|
| hooks | `node --test tests/*.test.mjs` | 6 pass, 0 fail |
| issues-tracker | `node --test *.test.mjs` | 21 pass, 0 fail |
| ax | `node --test --test-concurrency=1 *.test.mjs` | 268 pass, 0 fail |
| ci | `node --test *.test.mjs` | 216 pass, 5 skipped, 0 fail |
| release | `node --import ./test-support/broker-guard.mjs --test --test-concurrency=1 --test-force-exit *.test.mjs` | 2890 pass, 56 skipped, 16 fail (see Blocked) |
