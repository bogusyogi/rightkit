# Class A inventory

**Status:** PROPOSAL from joint-plan step 3 (rightkit, 2026-10-29). Observed placements are what the code does at the commit that carries this file; PROPOSED placements are not accepted until a decision is recorded in `decisions.md` (Sage/Astra may adjudicate; Adrian reserves trust-policy changes). Nothing here claims final contract acceptance. Source of the requirement: [GATE-CONTRACT.md](GATE-CONTRACT.md) §1 "Class A first": Class A is the agreed deterministic, mechanical checks, classified by observed command behaviour (what each reads and launches), run before the first local compile; placement changes in either direction need dependency/launch evidence; compiler-backed commands such as `cargo check` and `clippy` stay B.

**How this file is held to the code.** `packages/release/gate-contract.test.mjs` checks, as source-drift coverage only:
- the Scans table names exactly `CLASS_A_SCAN_PREFIXES`, `CLASS_B_SCAN_PREFIXES` and `TEST_SCAN_CLASS` (`deterministic-scans.mjs`), with the class the code gives each, and every scan the planner emits for a repository of every kind has a row with its class;
- the Rules table names exactly the rule ids of `app-check.mjs` `RULES` and `scripts/preflight.mjs` `RULES`, with the tier;
- the Entries table names exactly `STAGES` (`gate-stages.mjs`), `EXECUTOR_KINDS` (`gate-executor.mjs`) and the `@rightkit/release` bin entries.
A new scan, rule, stage, executor kind or bin without a row here fails that test. Behaviour (ordering, routing, refusal) is tested separately in the same file with real subprocesses; this table is not runtime proof.

Legend for the "Proposed" column: **keep** (observed placement stands on the evidence given), **PROPOSED A/B** (a move, not yet decided), **PENDING** (evidence missing; what would settle it is named).

## Scans

Columns: id prefix | class in code | proposed | command (tool, argv, root) | reads | compiles or emits | launches a later step | network | evidence. File references are `packages/release/deterministic-scans.mjs` (DS) at this commit unless named.

| `id` | class | proposed | command | reads | compiles/emits | later-step launch | network | evidence |
|---|---|---|---|---|---|---|---|---|
| `path-policy` | A | keep | internal refusal row | nothing (always fails with its reason) | no | no | no | DS `add` for refused paths and unit names |
| `rustfmt` | A | keep | `rustfmt --check --edition E <files>`, content root | listed .rs files, crate editions | no | no | no | parse/format check only |
| `node-check` | A | keep | `node --check <file>`, content root | the file | no | no | no | parse only |
| `json-parse` | A | keep | internal | changed .json (JSONC for tsconfig/.vscode) | no | no | no | INTERNAL map |
| `toml-parse` | A | keep | internal (`smol-toml`) | changed .toml | no | no | no | INTERNAL map |
| `yaml-parse` | A | keep | internal (`workflow-yaml.mjs`) | changed workflow YAML | no | no | no | INTERNAL map |
| `ownership-catalog` | A | keep | internal byte equality | two ownership catalogs | no | no | no | INTERNAL map |
| `actionlint` | A | keep | `actionlint [-config-file] <workflows>`, content root | workflows, config | no | launches `shellcheck` (release tier) | no | pinned analyzers |
| `shellcheck` | A | keep | `shellcheck --format=gcc --external-sources <files>` (pinned 0.11.0), release tier | shell files, .shellcheckrc | no | no | no | `PINNED_SCAN_TOOLS` |
| `pnpm-lockfile` | A | keep | `pnpm install --frozen-lockfile --lockfile-only --ignore-pnpmfile --ignore-scripts` | package.json files, lock, workspace file, .npmrc | no | no (hooks and scripts off) | yes: re-verifies entries against the registry on a cold cache | `REGISTRY_SCAN_IDS`: a red from it is never a reusable deterministic verdict |
| `pnpm-audit` | A | keep | `pnpm audit <declared flags>` | lock, root scripts | no | no | yes, live | `LIVE_SCAN_IDS`: re-run before any verdict reuse (`live-scans`) |
| `cargo-metadata` | A | keep | `<executor> cargo metadata --no-deps --format-version 1 --locked` | manifests, lock, .cargo | no | no | no (`--locked`, `--no-deps`) | metadata only |
| `cargo-deny` | A | keep | `cargo deny --frozen --metadata-path <m> --config <overlay> check`; the runner first runs `cargo fetch --locked` and a full `cargo metadata --locked` | deny.toml or the default policy, pinned advisory DB, metadata | no | no | yes: `cargo fetch` and a cold advisory-DB fetch | auxiliary metadata failure is `AUX_METADATA`, never deterministic |
| `cargo-audit` | A | keep | `cargo audit --db <db> --no-fetch` | Cargo.lock, .cargo/audit.toml, pinned DB | no | no | cold DB fetch only | pinned DB |
| `cargo-package-list` | A | keep | `cargo package -p <crate> --list --allow-dirty --locked`, release tier | crate files, manifest | no (lists files; no verify build) | no | no | `--list` builds nothing |
| `changelog-version` | A | keep | internal | Cargo.toml, CHANGELOG.md | no | no | no | INTERNAL map |
| `license` | A | keep | internal | in-scope manifests | no | no | no | INTERNAL map |
| `secret-scan` | A | keep | internal (`scanCrateForSecrets`) | every in-scope file; launches `git` only | no | no | no | INTERNAL map |
| `advisory-exceptions` | A | keep | internal | advisory ledgers, deny.toml, audit.toml, Cargo.lock; depends on today's date | no | no | no | date-dependent: never cached (`rightkit-gate.mjs`); not a remote scan |
| `suite-api` | A | keep; PENDING snapshot coverage | internal, imports `scripts/gen-suite-api.mjs` from the trusted gate home | contract and projections | no | runs the gate home's own generator code | no | in snapshot mode the home is the snapshot, so a gate run from elsewhere drops it to a note: coverage gap, owner step 4 |
| `tsc` | A | PROPOSED B (Astra 2026-10-29; conflicts with the typecheck-in-A attribution to Adrian, flagged to him) | `pnpm exec tsc --noEmit -p tsconfig.json` | TS project, node_modules types | type-checks, never emits | no | no | Adrian 2026-10-27 placed typecheck in A ("everything mechanical and deterministic that does not compile"); the contract keeps compiler-backed commands such as `cargo check` in B. `tsc --noEmit` is the TypeScript analogue of `cargo check`: Astra: `--noEmit` suppresses output, not compiler analysis, so it is compiler-backed like `cargo check`: B, full coverage kept. Not applied: the code comment attributes typecheck-in-A to Adrian (2026-10-27); his call (decisions.md) |
| `eslint` | A | keep (not planned in RightKit) | `pnpm exec eslint .` | repository ESLint config and plugins (code) | no | runs repository config/plugin code in-process | no | RightKit has no ESLint or Biome config at this commit (zero `eslint.config.*`, `.eslintrc*`, `biome.json*` outside node_modules), so the row is never planned here. In an app it loads that app's config and plugins in-process and runs without the launch guard (only `kind: "test"` rows get it): since step 4 every `executes` class A row runs under the launch guard (`runScans`), so a config or plugin that launches a compiler fails the row |
| `biome` | A | keep | `pnpm exec biome lint .` | biome.json(c) | no | no | no | native linter, no plugin code |
| `generated-docs` | A | keep (evidence: zero launches) | `node scripts/generate-index.mjs --check --root <content>` (repository code), deferred without a trusted root | generator inputs | no (`--check` compares, writes nothing) | none: zero child launches observed | none observed | `node scripts/generate-index.mjs --check --root <export>` run on a clean export under the launch guard plus a launch observer: exit 0, zero launches (`/Volumes/D/scratch/rk-step3/obs/generated-docs.*`) |
| `clippy` | B | keep | `cargo clippy -p <c> --no-deps --all-targets <set> --locked -- -D warnings` | crate sources | compiles | n/a | no | compiler-backed (contract) |
| `check` | B | keep | `cargo check -p <c> --all-targets --locked` | crate sources | compiles | n/a | no | compiler-backed (contract) |
| `check-all-features` | B | keep (dead prefix) | no producer at this commit | n/a | n/a | n/a | n/a | listed in `CLASS_B_SCAN_PREFIXES` but nothing plans it; remove or reuse (owner: gate maintenance) |
| `check-features` | B | keep | `cargo check ... <features> --locked` | crate sources | compiles | n/a | no | compiler-backed |
| `check-dependent` | B | keep | `cargo check` of a reverse dependent | crate sources | compiles | n/a | no | compiler-backed |
| `doc` | B | keep | `cargo rustdoc -p <c> --lib ... -- -D warnings` | crate sources | compiles docs | n/a | no | compiler-backed |
| `swift-build` | B | keep | `swift build` (darwin) | Swift package | compiles | n/a | no | compiler |
| `test:rust` | B | keep | `cargo test -p <c> <set> --locked --no-fail-fast` | crate sources | compiles tests | runs them | no | compiler-backed |
| `test:rust-serial` | B | keep | the same, `--test-threads=1` | crate sources | compiles tests | runs them | no | compiler-backed |
| `test:npm` | B (A per unit with `npm.<unit>.classATests.evidence`) | per unit (see Test units) | `pnpm --dir <unit> run test` | the unit and its dependencies | per unit | per unit | per unit | B by default (`TEST_SCAN_CLASS`); since step 4 a unit whose launch evidence is recorded in `ci/gate.config.json` `npm.<unit>.classATests.evidence` runs in class A under the launch guard, never when its script builds, reaches the broker or is declared afterClassA (the planner then notes why) |
| `test:scripts-ci` | B | PENDING (see Test units) | `node --test scripts/ci/*.test.mjs` (list from the content root, run at the live root) | scripts/ci and what it imports | per evidence | per evidence | per evidence | reads one tree and runs in another: fix before any move (owner step 4, snapshot mode) |

## Test units

Observed on a clean export of the commit (an evidence method GATE-CONTRACT does not allow; rerun on the gate snapshot) (lockfile-only offline install, no build output), each unit's `test` script run under the class A launch guard (the environment class A test rows get: refuses any launch of cargo, rustc, rustright, rightkit, tauri, tsup, esbuild, swc, rollup, webpack, `tsc` without `--noEmit`, `vite build`, `next build`, or a package-manager build script). Raw results: `/Volumes/D/scratch/rk-step3/launch-evidence.log`, per-unit logs in `/Volumes/D/scratch/rk-step3/launch/` (Mac, node v26.12.1, rightkit 63387ea9). Every unit is class B in code today (`TEST_SCAN_CLASS`).

| unit | runner | observed under the guard | dependency evidence | proposed |
|---|---|---|---|---|
| packages/legal | node --test after `pnpm run build` | refused: `pnpm build` | runs a build | B (keep) |
| packages/onboarding | vitest, pretest builds dependencies | refused: `pnpm --filter ... run build` | runs a build | B (keep) |
| packages/qa | vitest + node --test after builds | refused: `pnpm --filter @rightkit/qa-chrome build` | runs a build | B (keep) |
| packages/react-shell | vitest, pretest builds | refused: `pnpm build` | runs a build | B (keep) |
| packages/settings-ui | vitest, pretest builds | refused: `pnpm build` | runs a build | B (keep) |
| packages/tauri-shell | node --test after `pnpm run build` | refused: `pnpm build` | runs a build | B (keep) |
| packages/theme | vitest after builds | refused: `pnpm build` | runs a build | B (keep) |
| packages/release | node --test (stub compilers) | refused: `rustright cargo metadata`, `rustright run ... build`, `cargo --version` | launches compiler names (stubs); `afterClassA` declared | B (keep) |
| packages/license | vitest | failed, no refusal | reads `dist/` build output (ENOENT `dist/index.js`) | B (keep: needs a prior build) |
| packages/platform-ui | vitest | failed, no refusal | reads `dist/headless.js` | B (keep: needs a prior build) |
| packages/tauri | vitest | failed, no refusal | resolves a built workspace dependency (`@rightkit/logs` entry) | B (keep: needs a prior build) |
| packages/app-update, doc-render, i18n, legal-ui, logs, media-ui, outdated, pdf-ui, qa-chrome | vitest | green, zero refusals | vite transforms TypeScript in memory (compiler work the guard cannot see) | B (keep; Astra: in-memory compile is compiler-backed under the `tsc` reading). Revisit only if Adrian rules `tsc --noEmit` A |
| packages/ax, build-control-policy, ci, hooks, issues-tracker | node --test | green, zero refusals; rerun under the guard plus a launch observer that logs every launch of every Node descendant: ax 268/268 pass, build-control-policy 414/414, hooks 6/6, issues-tracker 21/21, ci 241/244 (the 3 failures are observer artifacts: a CLI child reports unsettled top-level await under the extra import; ci passes under the guard alone) | **imports:** no compiler API (typescript, esbuild, swc, vite, rollup, webpack, tsx, ts-node, babel, sucrase, oxc, rolldown) and no loader or `module.register` in any of their 116 source files; no runtime dependencies. **launches** (all Node descendants): ax git x108, node x180, npm pack x1 (no lifecycle child), tar, sh, one deliberately invalid command; build-control-policy git x137, sh; ci git, node, pnpm pack x1 (no lifecycle child), tar, actionlint, gh, bash (fixture workflow step scripts), ps; hooks node, sh; issues-tracker node, gh. No compiler, build script or emit. **Limit:** children of non-Node programs (bash step scripts, gh, tar, actionlint) are not traced past their own launch; the bash scripts are test fixture content. Logs `/Volumes/D/scratch/rk-step3/obs/` | **A** (settled on this evidence, per Astra's stated criterion), applied since step 4: `ci/gate.config.json` `npm.rightkit-{ax,build-control-policy,ci,hooks,issues-tracker}.classATests.evidence`; the rows run in class A under the launch guard, which refuses any compile at run time |
| scripts/ci (`test:scripts-ci`) | node --test | 873 tests, 11 failed, zero refusals | the 11 need a git checkout (`exports-import-effects`, tracked-file checks); a snapshot has no git | PENDING: B (keep) until it runs against snapshot-mode inputs (owner step 4) |
| packages/sync-worker | vitest | not a unit: outside the pnpm workspace (no node_modules) | n/a | n/a (not planned) |

No placement changes in code at this step; every move above is a proposal for a recorded decision.

## Launches before Class A that are not plan rows

These run before the first scan and are outside the plan. None is a compile; each is listed so "Class A first" names everything that runs first.
- Unit discovery: `<executor> cargo metadata --no-deps --format-version 1 --locked` per workspace (`workspaceMetadata`, `rightkit-gate.mjs`).
- Snapshot mode: the offline dependency install (`pnpm install --offline --frozen-lockfile --ignore-scripts --ignore-pnpmfile`) and the skipped-build usability probe under the launch guard (`snapshot-check.mjs`); a compile tool such as esbuild is never launched there (deferred, not proven, until its first permitted post-Class-A launch).
- Environment identity: `pnpm --version` in the checked root (`classAEnvironmentDigest`).
- Door toolchain probes: `rustright rustc -V`/`rustc -V`, `cargo-deny/cargo-audit/actionlint/shellcheck --version` (`rightkit-gate.mjs` binding).
- Snapshot mode on a broker host: cargo's member list in the primary checkout (`cargo metadata`), compared with the commit's.

## Rules

App rules (`packages/release/app-check.mjs` `RULES`) and RightKit's own preflight rules (`scripts/preflight.mjs` `RULES`). All are Class A: they read the commit's files (exported by `git ls-tree`/`cat-file`) and launch only `git`, except preflight's `generated-docs`, which runs `node scripts/generate-index.mjs --check` (repository code, same PENDING note as the `generated-docs` scan). Tier: compile for the 0.6.6 set (`COMPILE_TIER_IDS`), release for every rule added since.

| `id` | class | gap |
|---|---|---|
| `eol-policy` | A (app, compile tier) | G04 |
| `file-url` | A (app, compile tier) | G12 |
| `retired-alias` | A (app, compile tier) | P11 |
| `qa-platform` | A (app, compile tier) | G13 |
| `test-harness` | A (app, compile tier) | G14 |
| `scenario-skip` | A (app, compile tier) | G15 |
| `release-config` | A (app, compile tier) | G16 |
| `skip-checks` | A (app, compile tier) | G17 |
| `qa-dev-server` | A (app, compile tier) | G18 |
| `ffmpeg-release-escape` | A (app, compile tier) | G19 |
| `switches-inventory` | A (app, compile tier) | G20 |
| `speech-link` | A (app, compile tier) | G21 |
| `e2e-under-cargo-test` | A (app, compile tier) | G22 |
| `suite-key-literal` | A (app, release tier) | C3-04 |
| `suite-updater-key` | A (app, release tier) | C3-14 |
| `engine-record-store` | A (app, release tier) | C3-05 |
| `shell-window-hidden` | A (app, release tier) | SC-R1 |
| `shell-plugin-order` | A (app, release tier) | SC-R2 |
| `shell-handler` | A (app, release tier) | SC-R3 |
| `shell-csp` | A (app, release tier) | SC-R4 |
| `shell-grants` | A (app, release tier) | SC-R5 |
| `qa-window-label` | A (app, release tier) | SC-R6 |
| `devtools-declaration` | A (app, release tier) | SC-R7 |
| `shell-lifecycle-external` | A (app, release tier) | SC-R8 |
| `legal-receipt-native` | A (app, release tier) | SC-R9 |
| `release-config-identity` | A (app, release tier) | CA-01 |
| `pnpm-node-floor` | A (app, release tier) | CA-05 |
| `rust-dependency-msrv` | A (app, release tier) | CA-05 |
| `release-age` | A (app, release tier) | CA-06 |
| `notice-lock-digest` | A (app, release tier) | CA-14 |
| `doc-comment-bare-cr` | A (app, release tier) | CA-15 |
| `checkout-step-input` | A (app, release tier) | CA-07 |
| `source-relation` | A (app, release tier) | CA-09 |
| `checkout-longpaths` | A (app, release tier) | CA-11 |
| `workflow-action-pair` | A (app, release tier) | CA-04 |
| `referenced-input` | A (app, release tier) | CA-07 |
| `caddy-global-options` | A (app, release tier) | CA-08 |
| `legion-schema-projection` | A (app, release tier) | LG-02 |
| `legion-legacy-tokens` | A (app, release tier) | LG-03 |
| `legion-retirement-ledger` | A (app, release tier) | LG-04 |
| `legion-authority-body` | A (app, release tier) | LG-05 |
| `retired-token` | A (app, release tier) | CA-13 |
| `route-deny-policy` | A (app, release tier) | CA-13 |
| `layer-reach` | A (app, release tier) | CA-13 |
| `spec-route-parity` | A (app, release tier) | CA-13 |
| `source-closure` | A (app, release tier) | SF-01 |
| `mandatory-foundation` | A (app, release tier) | OW-05 |
| `ownership-exceptions` | A (app, release tier) | OW-06 |
| `eol-pinned` | A (RightKit preflight) | G25 |
| `consumer-recipe` | A (RightKit preflight) | G01 |
| `npm-recipe` | A (RightKit preflight) | G01 |
| `npm-surface` | A (RightKit preflight) | G02 |
| `version-pins` | A (RightKit preflight) | G03 |
| `feed-legacy` | A (RightKit preflight) | G27 |
| `dispatch-pins` | A (RightKit preflight) | G05 |
| `consumer-locks` | A (RightKit preflight) | G08 |
| `generated-docs` | A (RightKit preflight) | G10 |
| `tmpdir-realpath` | A (RightKit preflight) | G26 |

## Entries, stages and executors

| `id` | class | what it is |
|---|---|---|
| `stage:check` | door | class A only: app rules, then every class A scan; nothing compiles |
| `stage:compile` | door | after a green check; local compile through the executor |
| `stage:package` | door | after compile |
| `stage:qualify` | door | after package; CI for RightKit/public, Mac/Dell for private |
| `stage:release` | door | trusted publisher only |
| `executor:broker` | route | Mac and Dell: RustRight on PATH, its cargo shim, or the managed agent-bin cargo |
| `executor:refuse` | route | a broker-managed host whose broker is unreachable: the gate never compiles around it |
| `executor:direct` | route | every Linux (and non-Mac, non-Windows) host always, whatever is on PATH or in the environment (since joint plan step 4: `resolveExecutor` returns direct before any broker check), and any unmanaged Mac or Windows host |
| `bin:right-release` | entry | `cli/right-release.mjs`: builds, stages, qualifies; launched by the door after a green check |
| `bin:rightkit-gate` | entry | `bin/rightkit-gate.mjs`: the one door |
| `bin:rightkit-agent-hook` | entry | `bin/rightkit-agent-hook.mjs`: the managed-agent PreToolUse decision |
| `policy:github-actions-only` | host policy | `execution-policy.mjs` `assertReleaseExecutionAllowed`: an app whose `.rightgit.json` says `compile: github-actions-only` (required for Orthic-Labs public repositories) may compile only in GitHub Actions, unless granted below; refusal code `RK_GITHUB_ACTIONS_ONLY`. Enforced by `cli/right-release.mjs` (which the door launches for dev, test and package) and `development.mjs`; the build-control hook mirrors it (`githubActionsOnlyViolation`, `build-control-policy`). Product-dependent: private apps have no `.rightgit.json` and compile locally. PENDING for the public-app cells A-M, A-D, A-L: the contract requires a local compile on the origin host, which needs a grant per app (Adrian's policy call) |
| `policy:.rightkit-local-build.json` | host policy | the local-build grant (`localBuildGranted`): `allow: build-test-publish` (local publish is not permitted: RightKit publishes from git through CI) and a `repository` equal to the checkout's origin; RightKit's own grant names `bogusyogi/rightkit`. The grant's recorded reason (pre-push checks, 2026-10-22) is disputed by Adrian and needs correcting (GATE-CONTRACT §1) |

The door also accepts `--to dev|test` (not in `STAGES`): today it runs full class A on the WORKING TREE and then `right-release dev|test`; under the commit-only build it must check and build a commit's snapshot (owner: step 4).

## Gaps found (code against contract)

1. PARTLY FIXED (step 4: per-unit `classATests` evidence). Every test was class B by name (`TEST_SCAN_CLASS`; `test:npm` and `test:scripts-ci` hard-coded B); the class A test-row machinery (launch guard, test lane) is reachable only from hand-built plans. The contract: neither all-tests-A nor all-tests-B is presumed. Settled per unit by the evidence in Test units.
2. `deterministic-scans.mjs` header says class A runs no repository plugin or generator and lists tsc/eslint/biome/docs as B; the constants make them A. The header is stale.
3. FIXED (step 4): `eslint` and `generated-docs` executed repository code in class A without the launch guard; every `executes` class A row now runs under it.
4. FIXED (step 4): Linux was not always direct (see `executor:direct`).
5. The door's `--to dev|test` builds the working tree, not a commit snapshot.
6. `suite-api` coverage is lost in snapshot mode for a gate run from outside the repository.
7. `prePushPlan` treats `generated-docs` as class B; it is A.
8. `rightkit-gate.mjs` header says class A scans are never cached; they are, keyed on bytes and environment.
9. `check-all-features` is a class B prefix with no producer.
10. `test:scripts-ci` lists files from the content root and runs at the live root.
11. FIXED (step 4): `scriptBuildCall` and `compileLaunch` treated pnpm's `-w` as taking a value, so `pnpm -w build` was missed; options are now per package manager (npm's `-w` still takes a name).
12. No exported enumeration of internal scan ids, `executes` scans or pre-plan launches; the inventory test derives them from `planScans` output.
13. The public-app cells (A-M, A-D, A-L) need a local compile on the origin host; `policy:github-actions-only` refuses it without a per-app local-build grant (Adrian's policy call).
Each is a finding for the owning step, not changed by step 3.
