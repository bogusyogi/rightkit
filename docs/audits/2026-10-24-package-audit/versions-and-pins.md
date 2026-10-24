# RightKit cross-cutting version and dependency audit

Read-only audit, 2026-10-24. RightKit HEAD `d9a5016` (shallow clone: 60 commits, graft root `bbdf6ab`, so history before 2026-10-23 is not available). Nine app repos at their checked-out HEADs, all clean.

Conventions: "published (recorded)" = `registry.cargo` / `registry.npm` in `packages/release/rightkit-versions.json` (the doc `docs/PLATFORM-MATRIX.md` and `rightkit-drift.mjs` define `registry` as the latest published version). "Source" = version in `crates/*/Cargo.toml` / `packages/*/package.json`. I also checked crates.io (index.crates.io) and npm (registry.npmjs.org) live: **every `registry` entry equals the live latest non-yanked version (110 names), and no source version other than the two named below is absent from the registries.** Live checks are marked LIVE; everything else comes from files. UNVERIFIED marks what I could not establish.

**Working-tree caveat.** RightKit is not clean: 15 modified and 3 untracked paths (`crates/rightkit-qa/{CHANGELOG.md,README.md,src/guard.rs,src/scenario.rs}`, `packages/release/{CHANGELOG.md,README.md,prebuild-gates.mjs,release-config.mjs,release.mjs,release.test.mjs,win-package-recipe.mjs,win-package-recipe.test.mjs}`, `docs/PIPELINE-ARCHITECTURE.md`, `scripts/preflight.mjs(+test)`, untracked `ci/app-check-baseline.json`, `packages/release/prebuild-gates.test.mjs`, `packages/release/release-config.test.mjs`). Commit-based statements below are about HEAD; anything published from this tree today would carry these uncommitted changes.

---

## 1. rightkit-versions.json: sections vs each other vs source

### 1.1 Section sizes and roles

| section | entries | what it holds |
|---|---|---|
| `npm` | 9 | stale adoption floors (e.g. `@rightkit/release` 0.2.47, `@rightkit/qa` 0.1.0); every one is below `registry` |
| `stagedNpm` | 24 | version of each live package source; equals source for all 24 |
| `registry.npm` | 28 | latest published (24 staged names + 4 retired names) |
| `cargo` | 15 | stale adoption floors (mostly 0.1.0 / 0.1.x); every one is below `registry` |
| `stagedCargo` | 67 | version of each live crate source; equals source for all 67 |
| `registry.cargo` | 82 | latest published (67 staged names + 15 retired names) |

All allow-list consumers (`release.mjs` `buildAllowedVersions`, `rightkit-registry-resolver.mjs` `recordedVersions`) take the union of `npm`, `stagedNpm`, `legacyNpm` and `registry.npm` (likewise cargo), so the stale `npm`/`cargo` floors only matter as the recorded minimum.

### 1.2 Disagreements

**staged vs registry (the only two units whose source is ahead of what is published):**

| unit | source | stagedX | registry | live | verdict |
|---|---|---|---|---|---|
| `rightkit-tauri-shell` (cargo) | 0.6.0 | 0.6.0 | 0.5.1 | 0.5.1 | unpublished, correctly staged |
| `@rightkit/release` (npm) | 0.6.4 | 0.6.4 | 0.6.3 | 0.6.3 | unpublished, correctly staged |

**In `registry` but with no source in this repo (retired or moved names; no staged entry):**
- npm: `@rightkit/app-shell` 0.6.2, `@rightkit/git` 0.2.42, `@rightkit/shell` 0.1.1, `@rightkit/updates` 0.2.5.
- cargo: `rightkit-control` 0.3.0, `-framed-sidecar` 0.2.0, `-jobs` 0.3.0, `-media` 0.2.6, `-ort` 0.2.3, `-process` 0.4.0, `-qts` 0.2.6, `-qts-ggml` 0.1.2, `-qts-ggml-sys` 0.1.2, `-secrets` 0.2.0, `-service` 0.1.4, `-settings` 0.2.0, `-shell` 0.3.0, `-tauri` 0.1.3, `-watch` 0.4.0.

**Source with no recorded version, by design (never published):** `rightkit-align` 0.1.0 and `rightkit-sync-keys` 0.1.0 (`publish = false`), `@rightkit/build-control-policy` 0.1.4-phase1 and `@rightkit/sync-worker` 0.1.0 (`private`). Absent from crates.io/npm (LIVE 404).

**Other record oddities:** none of the sections has a `revoked` map. `@rightkit/legal` etc. in `npm` (0.2.0) are far below `registry`; harmless because the policy minimum is the lowest recorded version across all sections including `legacyNpm`.

### 1.3 "Same version number, different source" (recorded published, source differs from the published artifact)

Method: for every crate with a published version equal to source I downloaded the `.crate` from static.crates.io and compared it with the working tree (CRLF-normalised; for `.rs`, also with `//` comments and whitespace stripped, to separate real code from doc-comment/rustfmt churn). npm tarballs ship `dist/` that is not in the tree, so for npm I compared `git log --since=<npm publish time> bbdf6ab..HEAD` file lists plus the shipped non-`dist` files. "Version commit" = last commit whose diff touches the `version` line, `git log -1 -G'^version…'`. For nearly every unit that commit is the graft root `bbdf6ab`, which only means the version line predates the 60-commit window (true version commit UNVERIFIED); the tarball comparison does not depend on it.

**A. Real behaviour/code drift (version must be bumped before the change can be published):**

| unit | published (LIVE time) | version commit | last commit touching dir | what differs from published |
|---|---|---|---|---|
| `rightkit-qa` 0.8.0 | 2026-10-23T02:21Z | `bbdf6ab` (graft; `versions.json` already says 0.8.0 published at `7d64724^`) | `b98c3e7`, plus uncommitted edits | 20 `src/` files with code changes (journeys run, worker/pool/supervisor, steps, step plugin v1, budgets, control token), `Cargo.toml` now requires `rightkit-child-process 0.5.5` (published 0.8.0 required 0.5.4), README/CHANGELOG. Introduced by `7d64724` ("ADR-0001 wave 2 ... (unpublished)", Oct 24 02:09 +05:30, i.e. after the publish) with no version bump; further uncommitted changes to `guard.rs`/`scenario.rs`. |
| `rightkit-child-process` 0.5.5 | before 7d64724 | `bbdf6ab` (graft) | `b98c3e7` | `src/owned.rs`, `src/process_windows.rs`, `src/windows_job.rs` (`WindowsJob::process_ids`, `OwnedChild::tree_members`), added by `7d64724` without bump. `rightkit-qa` (tree) uses these APIs, the published 0.5.5 does not have them. |
| `@rightkit/qa` 0.6.1 | 2026-10-23T05:27Z | `bbdf6ab` (graft) | `7d64724` | `src/evidence.ts`: `CheckStatus` gains `errored`/`missing` and a refuse-non-passing-receipt guard (`7d64724`, after publish, no bump). |
| `rightkit-tts` 0.2.3 | n/a | `bbdf6ab` (graft; origin of the manifest change UNVERIFIED) | `b98c3e7` | `Cargo.toml`: dependency `rightkit-transcript` `0.3.0` in published, `0.4.0` in tree (a semver-visible dependency change at the same version); `src/models.rs` doc text. |
| `rightkit-tts-qwen-sys` 0.1.2 | n/a | `bbdf6ab` (graft) | `b98c3e7` | `Cargo.toml`: `bindgen` 0.72 -> 0.73, `repository` URL `Orthic-Labs/rightkit` -> `bogusyogi/rightkit`; `VERSIONS.md`. |
| `@rightkit/outdated` 0.2.0 | 2026-10-14 | `bbdf6ab` (graft) | `b98c3e7` | `src/apply.ts` now runs `rustright cargo update ...` instead of `rightkit cargo update ...` (the `rightkit` client name is retired and "refused by name"); `update.schema.json` `$id` URL. The published 0.2.0 still invokes `rightkit`. |

**B. Shipped data/script text changed, behaviour unchanged (all from `b98c3e7`, the `rightkit` -> `rustright` rename, after the recorded publish):** `rightkit-ownership` 0.4.2 (`catalog/ownership-catalog.json` strings; recorded at `a3ae84c`, `b98c3e7` came after it), `rightkit-logs` 0.3.5 (`runtime-history/0.3.4/runtime-budget.json`), `rightkit-android` 0.3.0 (`scripts/build-android.{sh,ps1}`, `TOOLCHAIN.md`), `rightkit-automation` 0.6.1 (README, examples, e2e tests; recorded at `a3ae84c`, `b98c3e7` after it).

**C. Doc-comment or README only (tarball differs, code identical after stripping comments):** crates `rightkit-app-settings`, `-audio`, `-contracts`, `-credentials`, `-file-watch`, `-hooks`, `-hotkeys`, `-http`, `-ocr`, `-pdf`, `-search`, `-tts-qwen`, `-typeset`, `-vad` (rustdoc link and `argv[1]` backtick fixes) plus README/CHANGELOG-only drift in most others; npm `@rightkit/legal-ui`, `-hooks`, `-media-ui`, `-ci` (tests only), `@rightkit/license` (`package.json` line endings only, `a6a6076`). Not a publishing blocker.

**D. Checked and clean:** `rightkit-db` 0.5.2 and `rightkit-browser` 0.3.6 changed after their version commit, but those commits (`156c481`, `cee1b5a`) precede the feed commit that records them (`9300907`) and the tarballs match. `@rightkit/app-update` 0.4.2 (`d137a9c` precedes publish at 2026-10-23T19:21Z), `rightkit-license` 0.2.5 (README/CHANGELOG only). npm packages published 10-08..10-23 other than those above show no non-doc, non-test source change after their publish time inside the 60-commit window; changes before the graft root are UNVERIFIED for npm because `dist/` is not in the tree.

---

## 2. App pins

Scope: every `package.json` (all dependency sections plus overrides/resolutions), every `pnpm-lock.yaml` (importers and `packages`), every `Cargo.toml` (dependencies, dev/build, target-specific, `workspace.dependencies`, `patch`, `replace`) and every `Cargo.lock`, excluding `node_modules`/`target`. Pin = lock-resolved version; in every case where a manifest pin exists it equals the lock (exceptions listed in 2.3). `*` = behind the latest published. `(t)` = lock-only (transitive, no manifest pin). `a+b` = two versions resolved in that app. `–` = not used. "Latest" is the recorded `registry` value (= LIVE).

Manifest style: all npm pins are exact (`0.6.3`, no `^`/`~`); all Cargo pins are exact (`=x.y.z`) or `workspace = true` inheriting the app's own `[workspace.dependencies]`, which is exact. No `engines`/`overrides` hold a RightKit spec.

### 2.1 npm `@rightkit/*` (pnpm-lock resolved)

| package | latest published | coderight | CutRight | genright | mailright | heardright | postright | pulse | scraperight | viewright |
|---|---|---|---|---|---|---|---|---|---|---|
| @rightkit/app-update | 0.4.2 | 0.4.1 * | 0.4.1 * | 0.4.0 * | 0.4.1 * | 0.4.1 * | 0.4.1 * | 0.4.1 * | 0.4.1 * | 0.4.1 * |
| @rightkit/doc-render | 0.2.1 | 0.2.1 | – | – | – | – | – | – | 0.2.1 | 0.2.1 |
| @rightkit/i18n | 0.2.0 | 0.2.0 | – | – | – | – | – | 0.2.0 | – | – |
| @rightkit/issues-tracker | 0.1.0 | – | – | – | – | – | – | 0.1.0 | – | – |
| @rightkit/legal | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.2 * | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 |
| @rightkit/legal-ui | 0.4.1 | 0.4.1 | 0.4.1 | 0.4.1 | 0.4.1 | 0.4.1 | 0.4.1 | 0.4.1 | 0.4.1 | 0.4.1 |
| @rightkit/license | 0.3.0 | 0.3.0 | 0.3.0 | 0.2.1 * | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 |
| @rightkit/logs | 0.1.6 | 0.1.6 | 0.1.6 | 0.1.6 | 0.1.6 | 0.1.6 | 0.1.6 | 0.1.6 | 0.1.6 | 0.1.6 |
| @rightkit/media-ui | 0.3.4 | – | 0.3.4 | 0.3.4 | – | – | – | – | 0.3.4 | 0.3.4 |
| @rightkit/onboarding | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 | 0.6.3 |
| @rightkit/pdf-ui | 0.2.0 | – | – | – | 0.2.0 | – | – | – | – | 0.2.0 |
| @rightkit/platform-ui | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 |
| @rightkit/qa | 0.6.1 | 0.6.1 | 0.6.1 | 0.6.1 | 0.6.1 | 0.6.1 | 0.6.1 | 0.6.1 | 0.6.1 | 0.6.1 |
| @rightkit/qa-chrome | 0.1.11 | 0.1.11 | 0.1.11 | 0.1.11 | 0.1.11 | 0.1.11 | 0.1.11 | 0.1.11 (t) | 0.1.11 | 0.1.11 |
| @rightkit/react-shell | 0.7.2 | 0.7.2 | 0.7.2 | 0.7.2 | 0.7.2 | 0.7.2 | 0.7.2 | 0.7.2 | 0.7.2 | 0.7.2 |
| @rightkit/release | 0.6.3 | 0.6.2 * | 0.6.2 * | 0.6.3 | 0.6.2 * | 0.6.3 | 0.6.2 * | 0.6.3 | 0.6.3 | 0.6.3 |
| @rightkit/settings-ui | 0.7.0 | 0.7.0 | 0.7.0 | 0.6.1 * | 0.7.0 | 0.7.0 | 0.7.0 | 0.7.0 | 0.7.0 | 0.7.0 |
| @rightkit/tauri | 0.3.0 | 0.3.0 | – | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | – |
| @rightkit/tauri-shell | 0.1.3 | 0.1.2 * | 0.1.2 * | 0.1.2 * | – | – | 0.1.2 * | 0.1.2 * | 0.1.2 * | 0.1.2 * |
| @rightkit/theme | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 | 0.3.1 |

Behind (npm): `@rightkit/app-update` 0.4.1 in 8 apps (0.4.0 in genright) vs 0.4.2; `@rightkit/release` 0.6.2 in coderight, CutRight, mailright, postright vs 0.6.3; `@rightkit/tauri-shell` 0.1.2 in coderight, CutRight, genright, postright, pulse, scraperight, viewright vs 0.1.3; genright also `legal` 0.6.2 (0.6.3), `license` 0.2.1 (0.3.0), `settings-ui` 0.6.1 (0.7.0). heardright is behind only on `app-update`; mailright on `app-update` and `release`.
No app pins `@rightkit/release` 0.6.4 or any version above the recorded latest.

### 2.2 Cargo `rightkit-*` (Cargo.lock resolved; every app lock source is crates.io)

| crate | latest published | coderight | CutRight | genright | mailright | heardright | postright | pulse | scraperight | viewright |
|---|---|---|---|---|---|---|---|---|---|---|
| rightkit-agent-tools | 0.3.0 | – | 0.3.0 | 0.3.0 | – | – | – | 0.3.0 | – | – |
| rightkit-agents | 0.2.0 | 0.2.0 | – | – | – | – | – | – | – | – |
| rightkit-app-settings | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 |
| rightkit-asr | 0.3.0 | – | 0.3.0 | – | – | 0.3.0 | – | – | 0.3.0 | – |
| rightkit-audio | 0.3.1 | – | 0.3.1 | 0.3.1 | – | 0.3.1 | – | – | 0.3.1 | – |
| rightkit-automation | 0.6.1 | 0.6.0 * (t) | 0.6.0 * | 0.6.0 * (t) | 0.6.0 * (t) | 0.6.1 | 0.6.0 * (t) | 0.6.0 * | 0.6.0 * (t) | 0.6.0 * (t) |
| rightkit-browser | 0.3.6 | 0.3.5 * | – | 0.3.6 | – | – | – | 0.3.6 | 0.3.5 * | – |
| rightkit-cache | 0.2.3 | 0.2.3 | 0.2.3 | – | 0.2.3 | – | 0.2.3 | – | – | – |
| rightkit-channel-memory | 0.2.1 | – | – | – | – | – | 0.2.1 | – | – | – |
| rightkit-child-process | 0.5.5 | 0.5.5 | 0.5.5 | 0.5.5 | 0.5.5 | 0.5.5 | 0.5.5 | 0.5.5 | 0.5.5 | 0.5.5 |
| rightkit-compose | 0.2.0 | – | 0.2.0 | – | – | – | – | – | – | – |
| rightkit-contracts | 0.1.7 | – | 0.1.7 | 0.1.7 | – | – | 0.1.7 | – | – | – |
| rightkit-crash | 0.3.6 | 0.3.6 | 0.3.6 | 0.3.6 | 0.3.6 | 0.3.6 | 0.3.6 | 0.3.6 | 0.3.6 | 0.3.6 |
| rightkit-credentials | 0.4.2 | 0.4.2 | 0.4.2 | 0.4.2 | 0.3.0+0.4.2 * | 0.4.2 | 0.4.2 | 0.4.2 | 0.4.2 | 0.4.2 |
| rightkit-crypto | 0.1.0 | 0.1.0 | – | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | – | 0.1.0 |
| rightkit-daemon | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 |
| rightkit-db | 0.5.2 | 0.5.1 * | 0.5.0 * (t) | 0.5.1 * | 0.5.1 * | 0.5.2 | 0.5.1 * | 0.5.1 * | 0.5.2 | 0.5.2 |
| rightkit-device | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 |
| rightkit-docs | 0.1.2 | – | – | – | – | – | – | 0.1.2 | – | – |
| rightkit-embed | 0.6.1 | 0.6.1 | – | – | 0.6.1 | – | – | 0.6.1 | – | – |
| rightkit-ffmpeg | 0.2.10 | 0.2.10 | 0.2.10 | 0.2.10 | 0.2.10 (t) | 0.2.10 | – | 0.2.10 | 0.2.10 | – |
| rightkit-file-watch | 0.4.1 | 0.4.1 | – | – | – | – | – | 0.4.1 | – | 0.4.1 |
| rightkit-fs | 0.4.7 | 0.4.6 * | 0.4.6 * | 0.4.7 | 0.4.6 * | 0.4.7 | 0.4.6 * | 0.4.6 * | 0.4.7 | 0.4.6 * |
| rightkit-fsindex | 0.5.0 | – | – | – | – | – | – | 0.5.0 | – | – |
| rightkit-hooks | 0.2.0 | – | – | – | – | – | – | 0.2.0 | – | – |
| rightkit-hotkeys | 0.1.2 | – | 0.1.2 | – | – | 0.1.2 | – | 0.1.2 | – | – |
| rightkit-http | 0.3.8 | 0.3.8 | 0.3.0+0.3.8 * (t) | 0.3.8 | 0.3.8 | 0.3.8 | 0.3.8 | 0.3.8 | 0.3.8 | 0.3.8 (t) |
| rightkit-job-queue | 0.5.1 | 0.5.0 * | 0.5.0 * | 0.5.0 * | 0.5.0 * | 0.5.1 | 0.5.0 * | 0.5.0 * | 0.5.0 * | – |
| rightkit-license | 0.2.5 | 0.2.4 * | 0.2.4 * | 0.2.3 * | 0.2.4 * | 0.2.4 * | 0.2.4 * | 0.2.4 * | 0.2.4 * | 0.2.4 * |
| rightkit-llm | 0.3.1 | 0.3.1 | 0.3.0 * | – | 0.3.0 * | 0.3.1 | – | 0.3.1 | – | – |
| rightkit-logs | 0.3.5 | 0.3.5 | 0.3.5 | 0.3.5 | 0.3.5 | 0.3.5 | 0.3.5 | 0.3.5 | 0.3.5 | 0.3.5 |
| rightkit-mcp | 0.7.0 | 0.7.0 | – | – | – | – | – | 0.7.0 | – | – |
| rightkit-models | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.1 | – | – | 0.5.1 | – |
| rightkit-motion | 0.2.0 | – | – | 0.2.0 | – | – | – | – | – | – |
| rightkit-oauth | 0.10.0 | 0.10.0 | – | – | 0.9.0 * | – | 0.10.0 (t) | – | – | – |
| rightkit-ocr | 0.2.3 | 0.2.3 | 0.2.3 | – | – | – | – | – | 0.2.3 | 0.2.3 |
| rightkit-onnx | 0.2.7 | 0.2.7 | 0.2.7 | 0.2.7 | 0.2.7 | 0.2.7 | – | 0.2.7 | 0.2.7 | – |
| rightkit-overlay | 0.2.0 | – | 0.2.0 | – | – | 0.2.0 | – | 0.2.0 | – | – |
| rightkit-overlay-win | 0.1.0 | – | – | – | – | 0.1.0 | – | – | – | – |
| rightkit-pdf | 0.3.3 | – | – | – | – | – | – | 0.3.3 (t) | – | 0.3.3 |
| rightkit-platform-rules | 0.1.0 | – | – | – | – | – | 0.1.0 | – | – | – |
| rightkit-playback | 0.3.0 | – | 0.3.0 | – | – | – | – | – | 0.3.0 | – |
| rightkit-qa | 0.8.0 | 0.8.0 | 0.8.0 | 0.8.0 | 0.8.0 | 0.8.0 | 0.8.0 | 0.8.0 | 0.8.0 | 0.8.0 |
| rightkit-record | 0.3.3 | – | 0.3.3 | – | – | – | – | – | – | – |
| rightkit-review | 0.4.0 | – | 0.4.0 | – | – | – | 0.4.0 | – | – | – |
| rightkit-sandbox | 0.6.1 | 0.6.1 | – | 0.6.1 (t) | – | – | – | 0.6.1 (t) | 0.6.1 (t) | – |
| rightkit-search | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | 0.4.0 | – | – | 0.4.0 | 0.4.0 | – |
| rightkit-send | 0.2.0 | – | – | – | – | – | – | 0.2.0 | – | – |
| rightkit-sidecar | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 | 0.3.0 (t) |
| rightkit-speech-client | 0.4.0 | – | 0.4.0 | – | – | 0.4.0 | – | – | 0.4.0 | – |
| rightkit-spend | 0.2.0 | – | – | 0.2.0 | – | – | 0.2.0 | 0.2.0 | – | – |
| rightkit-sync | 0.2.2 | – | – | – | – | 0.2.2 | – | 0.2.2 | 0.2.2 | – |
| rightkit-tauri-shell | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.1 | 0.5.0 * | 0.5.1 |
| rightkit-time | 0.1.0 | 0.1.0 | – | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | 0.1.0 | – |
| rightkit-transcript | 0.4.0 | – | 0.4.0 | 0.3.0+0.4.0 * | – | 0.4.0 | – | – | 0.4.0 | – |
| rightkit-tts | 0.2.3 | – | – | 0.2.3 | – | – | – | – | – | – |
| rightkit-tts-qwen | 0.2.7 | – | – | 0.2.7 (t) | – | – | – | – | – | – |
| rightkit-tts-qwen-sys | 0.1.2 | – | – | 0.1.2 (t) | – | – | – | – | – | – |
| rightkit-typeset | 0.1.0 | – | 0.1.0 | – | – | – | – | – | – | – |
| rightkit-vad | 0.1.2 | – | 0.1.2 | – | – | 0.1.2 | – | – | 0.1.1 * (t) | – |

Behind (crates), by app: mailright 8, CutRight 7, coderight 6, scraperight 6, genright 5, postright 5, pulse 5, viewright 3, heardright 1. Recurring: `rightkit-license` 0.2.4 -> 0.2.5 (all 9 apps; genright is on 0.2.3), `rightkit-automation` 0.6.0 -> 0.6.1, `rightkit-fs` 0.4.6 -> 0.4.7, `rightkit-job-queue` 0.5.0 -> 0.5.1, `rightkit-db` 0.5.1 -> 0.5.2. Also: coderight/scraperight `rightkit-browser` 0.3.5 -> 0.3.6, CutRight/mailright `rightkit-llm` 0.3.0 -> 0.3.1, mailright `rightkit-oauth` 0.9.0 -> 0.10.0, scraperight `rightkit-tauri-shell` 0.5.0 -> 0.5.1 and `rightkit-vad` 0.1.1 -> 0.1.2 (t), genright `rightkit-transcript` 0.3.0 -> 0.4.0.
Every app pins `rightkit-tauri-shell` at 0.5.x (or none); none can be "behind 0.6.0" until it is published.

### 2.3 Flags

**Forbidden specs (path / git / workspace / link / file to RightKit): none found.**
- No `path`, `git`, `[patch]`, `[replace]`, `workspace:`, `link:`, `file:` or `git+` spec resolves to RightKit in any Cargo.toml / package.json / pnpm override. No `.cargo/config.toml` source replacement (coderight and heardright configs set only linker flags and `git-fetch-with-cli`).
- False positive: pulse's `pulse-mesh = { path = "../rightkit-mesh" }` (3 manifests) and the `rightkit-mesh` / `rightkit-mesh-ffi` directories are app-local code that merely carries the prefix; `cargo-contract.mjs` also skips app path crates RightKit does not publish.
- Reference worth a look: `CutRight/apps/cutright/legal-snapshot.config.json` has `"sourceRoot": "../../../rightkit/legal"`, a relative path into a sibling RightKit checkout (a build input, not a dependency spec; whether the doctor or a CI check rejects it is UNVERIFIED).
- Swift (outside the npm/crates scope, but same family): `exact` pins to the RightKit Swift package are 0.8.1 (coderight, pulse x2, scraperight) and 0.8.0 (heardright x3), while the versions file records only `stagedSwift` 0.7.0 (and a retired 0.4.0). Tags 0.7.0, 0.8.0, 0.8.1 exist on GitHub (LIVE `git ls-remote`), so the pins are real but are NOT recorded in `rightkit-versions.json`. pulse's two manifests use the `bogusyogi/rightkit` URL while `stagedSwift` records `Orthic-Labs/rightkit`. Whether the doctor checks Swift pins: UNVERIFIED.

**Pins to versions not recorded as published:** none for npm; for crates one lock-only pin: CutRight `rightkit-db` 0.5.0 (transitive, pulled by `rightkit-job-queue` 0.5.0; no manifest pin). 0.5.0 exists on crates.io (LIVE) but is absent from every section of `rightkit-versions.json` (it sits inside min..max, so the minimum rule does not refuse it; exact-membership checks would). All other manifest and lock versions in all nine apps are recorded and live.

**Inconsistent pins of the same package inside one app:**
- genright: `rightkit-transcript` manifest `=0.3.0` (`crates/genright-studio`) but `Cargo.lock` also resolves 0.4.0 (via `rightkit-motion` 0.2.0). Two copies; bump studio to 0.4.0.
- mailright: `rightkit-credentials` manifest `=0.4.2` (2 manifests) but `src-tauri/Cargo.lock` also resolves 0.3.0, pulled by `rightkit-oauth` 0.9.0. Fixed by moving oauth to 0.10.0.
- CutRight: `rightkit-http` 0.3.0 in the root `Cargo.lock` (via `rightkit-llm` 0.3.0 / `rightkit-models` 0.5.1; the root lock also holds `rightkit-db` 0.5.0 via `job-queue` 0.5.0) vs 0.3.8 in `apps/cutright/src-tauri/Cargo.lock` (via `rightkit-license` 0.2.4). Fixed by repinning llm/models/license.
- No app has differing manifest pins for the same package across its own manifests or package.json files, and no npm importer/lock mismatch.

**Registry-source check:** every RightKit `Cargo.lock` entry has `source = registry+https://github.com/rust-lang/crates.io-index`; none is yanked or missing (LIVE).

**Install-time note (UNVERIFIED how each setting is applied):** every app's `pnpm-workspace.yaml` carries a `minimumReleaseAgeExclude` list of exact `@rightkit/*` versions (pulse: "each version Pulse resolves is excluded exactly"). None lists `@rightkit/tauri-shell@0.1.3` or `@rightkit/app-update@0.4.2`; `@rightkit/release@0.6.3` is listed only in genright, heardright, pulse and scraperight/viewright, and `0.6.4` in none; viewright sets `minimumReleaseAge: 10080`. Each repin needs the matching exclude entry (or to wait out the age).

---

## 3. Publication order

### 3.1 Units whose source is not (yet) what is published

| unit | state | internal deps | all deps published? |
|---|---|---|---|
| `rightkit-tauri-shell` 0.6.0 | never published (latest 0.5.1) | `rightkit-daemon` 0.4.0, `-job-queue` 0.5.0 (opt), `-child-process` 0.5.0, `-app-settings` 0.3.0, `-logs` 0.3.4, `-crash` 0.3.4, `-license` 0.2.5, `-overlay` 0.2.0, `-pdf` 0.3.3, `-models` 0.5.0, `-credentials` 0.4.0 (all optional except daemon and child-process); no `rightkit-*` dev-dependency | yes |
| `@rightkit/release` 0.6.4 | never published (latest 0.6.3) | no `@rightkit/*` dependency (only `smol-toml` 1.9.1) | yes (nothing to wait for) |
| `rightkit-child-process` 0.5.5 | changed after publish, no bump | `rightkit-*`: none of its own unpublished | needs a new version |
| `rightkit-qa` 0.8.0 | changed after publish, no bump | `child-process` (tree needs the unpublished APIs), `fs` 0.4.4, `sidecar`, `daemon`, `browser`(opt), `automation` 0.6.0 | needs new `child-process` first |
| `@rightkit/qa` 0.6.1 | changed after publish, no bump | `@rightkit/qa-chrome` 0.1.11 (published) | yes |
| `rightkit-tts` 0.2.3, `rightkit-tts-qwen-sys` 0.1.2, `@rightkit/outdated` 0.2.0 | changed after publish, no bump | `transcript` 0.4.0 (published); `outdated` -> `@rightkit/ci` `workspace:^` (0.3.0 published) | yes |

### 3.2 Tracing the two named releases

**rightkit-tauri-shell 0.6.0.** Direct deps: every required minimum is satisfied by a published version, and the feature names it enables (`job-queue/supervisor`, `crash/window-errors`, `license/update-auth`, `pdf/host`) exist in the published index entries (LIVE). Transitive closure (all non-dev edges, all features) is 17 crates besides itself, all at their recorded `registry` version: `app-settings` 0.3.0, `child-process` 0.5.5, `contracts` 0.1.7, `crash` 0.3.6, `credentials` 0.4.2, `daemon` 0.4.0, `db` 0.5.2, `fs` 0.4.7, `http` 0.3.8, `job-queue` 0.5.1, `license` 0.2.5, `logs` 0.3.5, `models` 0.5.1, `oauth` 0.10.0, `overlay` 0.2.0, `pdf` 0.3.3, `sidecar` 0.3.0. **Nothing has to publish before it; it does not depend on any unpublished unit.** It does not use `WindowsJob::process_ids` / `tree_members` (grep), so the unpublished `child-process` change is not a dependency. Its own tree changes after the version commit (`CHANGELOG.md`, `src/harden.rs`, `src/update.rs`) are fine because it is unpublished. Caveat from commit `7d64724`: "RightKit releases are on hold (Adrian)"; that is policy, not a technical dependency.

**@rightkit/release 0.6.4.** Zero RightKit package dependencies in `package.json`, so nothing must publish before it. It publishes from a dirty tree today (8 modified + 2 untracked files under `packages/release`; 55 files changed since its version commit `c61e314`). Soft runtime coupling, not a manifest dependency (UNVERIFIED how hard it is): 0.6.4's QA admission code (`qa.journeys`, `release-qa-hardening.mjs`) shells out to `rightkit-qa journeys run|validate-receipt` and reads `@rightkit/qa`-style receipts; those exist only in the unpublished `rightkit-qa` / `@rightkit/qa` source (3.1). Apps that enable `qa.journeys` need the new `rightkit-qa` build; apps that do not are unaffected. 0.6.4 bundles `rightkit-versions.json` with `tauri-shell` 0.6.0 and `@rightkit/release` 0.6.4 as staged, so it is the tool version that accepts the new pins offline.
The two are independent of each other.

### 3.3 Topological order (units needing publication)

```
Layer 0 (no unpublished internal deps, can go in any order / in parallel):
  rightkit-tauri-shell 0.6.0      @rightkit/release 0.6.4
  rightkit-child-process <bump>   @rightkit/qa <bump>   rightkit-tts <bump>
  rightkit-tts-qwen-sys <bump>    @rightkit/outdated <bump>
Layer 1:
  rightkit-qa <bump>              (needs child-process >= new version; tree already requires 0.5.5 APIs)
  rightkit-tts-qwen               only if its own source changes (depends on tts-qwen-sys req 0.1.0, satisfied by 0.1.2)
Layer 2:
  rightkit-qa-driver              only if it changes (optional dep on qa 0.8.0; req must be raised if driver needs the new qa)
```

Dependents of `rightkit-child-process` (agents, browser, cache, daemon, ffmpeg, file-watch, job-queue, llm, logs, oauth, playback, record, sandbox, send, sync, tts, tauri-shell) all use caret minimums (0.5.0 to 0.5.4), so a `child-process` bump forces no republish of them. Bump numbers are the owner's choice (crates.io and npm refuse a republish of the same version).

---

## 4. Release package pinned tools vs current app pins

`rightkit-versions.json` has exactly one tool pin: `"packageManager": "pnpm@11.24.0"`. `release.mjs:1149` refuses an app whose root `package.json` `packageManager` differs. Result: all nine app roots (`coderight/apps/*`, `CutRight/apps/cutright`, `genright`, `mailright`, `heardright` and `tauri-app-next`, `postright`, `pulse`, `scraperight`, `viewright`) carry `pnpm@11.24.0`; the only missing one is `pulse/hub/package.json` (a sub-package; the doctor reads the root, UNVERIFIED for sub-packages). Rust: crates declare `rust-version = 1.98.0` (68 of 69; `rightkit-send` declares 1.90); apps with a `rust-toolchain.toml` pin `1.98.0`; genright, mailright, viewright have none (installed rustc UNVERIFIED).

The real "allow-list" is the recorded-version set (`rightkit-version-policy.mjs`, `cargo-contract.mjs`):
- npm: an exact pin is allowed if recorded in any section, or between the lowest and highest recorded version for that package; a spec like `github`, `git:`, `file:`, `link:`, `workspace:` is refused outright.
- cargo: manifest requirement must be `=X` with X in the recorded set (exact membership, no range), source crates.io, no `path`/`registry`; no `[patch]`.
- Both are widened by exact versions the registry resolver proves live at admission (`rightKitVerifiedNpm`/cargo `verified`), and a pin below the recorded minimum is refused.

Outcome with the HEAD file (what 0.6.4 will bundle): **no current app pin is refused** (npm and cargo manifests, all nine apps).

Outcome with the file each app's installed `@rightkit/release` actually bundles (downloaded LIVE: 0.6.2 and 0.6.3 tarballs):
- On 0.6.2 (coderight, CutRight, mailright, postright): nothing refused.
- On 0.6.3: genright `rightkit-fs =0.4.7`; scraperight `rightkit-fs =0.4.7`; heardright `rightkit-automation =0.6.1`, `rightkit-fs =0.4.7`, `rightkit-job-queue =0.5.1`. These crate versions were published after 0.6.3's feed and are not in its list, so an offline doctor on 0.6.3 would refuse them; with live registry resolution they are admitted as verified exact versions (which path the doctor takes is UNVERIFIED). npm pins on 0.6.3 are all inside min..max.
- Repinning anything to a version newer than the installed tool's feed (e.g. `rightkit-tauri-shell =0.6.0`, `@rightkit/release 0.6.4`) is refused by an installed 0.6.2/0.6.3 on the same terms. That is why `@rightkit/release` must be repinned first (it bundles the newer feed). Note the feed lags by one release by design (`release.mjs` comment), so after publishing 0.6.4 and recording `registry`, the entries for 0.6.4 and `tauri-shell` 0.6.0 reach apps only in the next release unless 0.6.4 is the one carrying them (it is: both are in `stagedNpm`/`stagedCargo` at HEAD).

---

## Recommended publish + repin order

1. Commit (or discard) the RightKit working-tree changes so what is published equals a commit (`packages/release` and `crates/rightkit-qa` are dirty).
2. Decide the same-version drift in 1.3-A: bump `rightkit-child-process`, `rightkit-qa`, `@rightkit/qa`, `rightkit-tts`, `rightkit-tts-qwen-sys`, `@rightkit/outdated` (their source cannot ship at the current version numbers). Optional: leave 1.3-B/C as is.
3. Publish `rightkit-tauri-shell` 0.6.0 (all deps already published).
4. Publish `@rightkit/release` 0.6.4 (no internal deps). Steps 3 and 4 are independent; do both before any app repin.
5. Publish bumped `rightkit-child-process`, then bumped `rightkit-qa` (then `@rightkit/qa`, `tts`, `tts-qwen-sys`, `outdated` any time). Required before enabling `qa.journeys`.
6. Commit the feed update (`registry` entries) for everything published.
7. Repin each app, in this order:
   a. `@rightkit/release` -> 0.6.4 (and add the `minimumReleaseAgeExclude` entries for each new exact version).
   b. npm: `@rightkit/app-update` -> 0.4.2, `@rightkit/tauri-shell` -> 0.1.3, then genright `legal` 0.6.3, `license` 0.3.0, `settings-ui` 0.7.0.
   c. crates (published, safe now): `rightkit-license` =0.2.5, `-automation` =0.6.1, `-fs` =0.4.7, `-job-queue` =0.5.1, `-db` =0.5.2, `-browser` =0.3.6 (coderight, scraperight), `-llm` =0.3.1 (CutRight, mailright), `-oauth` =0.10.0 (mailright; removes the duplicate `credentials` 0.3.0), `-vad` 0.1.2 (scraperight), `-transcript` =0.4.0 (genright), and in CutRight repin so `rightkit-http` and `rightkit-db` resolve to one version across both lock files.
   d. `rightkit-tauri-shell` =0.6.0 in every app that uses it, after step 4 so the installed doctor knows it.
8. Swift pins: record 0.8.x (or the intended tag) in `stagedSwift`, and align the pulse URL, before trusting a doctor run on those apps (UNVERIFIED that it checks them).
