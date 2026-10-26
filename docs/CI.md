# RightKit CI

The pipeline this document implements (check → build → qualify → release) and its invariants are in
[`PIPELINE-ARCHITECTURE.md`](PIPELINE-ARCHITECTURE.md); this file is the mechanics.

Pushes to `main` & pull requests run static validation. Native Windows & macOS qualification
starts through broker-admitted `workflow_dispatch` requests. `publish.yml` qualifies sealed archives;
`publish-direct.yml` uploads every green qualification run's sealed bytes as soon as it completes;
`publish-reviewed.yml` promotes committed, reviewed candidates (locally qualified bytes).
The public-repository guard rejects private forks.

## Layout

| File | Role |
|---|---|
| `.rightgit.json` | right-git manifest: `rust-hybrid` profile, `ci` lane, runners `xcode-27` and `windows-2025` |
| `.github/workflows/ci.yml` | Hand-owned sharded workflow (see "Sharded CI"); no longer rendered by `right-git sync` |
| `.github/actions/rk-setup/action.yml` | Shared job setup: Node, pnpm install, Rust toolchain and per-shard Rust cache |
| `scripts/gate.sh` | Thin caller for the gate (runs the best-effort dependency graphs first) |
| `scripts/ci/gate.mjs` | Selects the units a push touched (or runs the units a shard job names), runs them, writes the summary |
| `scripts/ci/select.mjs` | Pure selection logic, covered by `scripts/ci/select.test.mjs` |
| `scripts/ci/ci-plan.mjs`, `scripts/ci/shards.mjs` | The `plan` job: per-OS selection cut into jobs; pure logic covered by `scripts/ci/shards.test.mjs` |
| `scripts/ci/ci-summary.mjs`, `scripts/ci/secret-scan.mjs` | The final `summary` verdict and the secret crate scan of the `hygiene` job |
| `ci/shards.json` | Shard sizes & units that always get a job of their own |
| `ci/gate.config.json` | Per-crate feature sets, extra paths, serial runs, host limits |
| `ci/known-red.json` | Units that are red today, per host, each with a reason |
| `.github/workflows/publish.yml` | Manual qualification: pack once, verify sealed archives on both native hosts |
| `.github/workflows/publish-direct.yml` | Upload a green `publish.yml` run's sealed bytes directly (`workflow_run`, no candidate or pin) |
| `.github/workflows/publish-reviewed.yml` | Promote a committed candidate without rebuilding |
| `scripts/ci/publish.mjs` | Qualification planning, packing, verification & receipt-ledger import |
| `scripts/ci/reviewed-candidate.mjs`, `scripts/ci/upload.mjs` | Authenticate candidate provenance, enforce approvals & upload sealed bytes |

## Deterministic scans come first

What is implemented today:

- `packages/release/deterministic-scans.mjs` is the one scan module. Class A scans compile nothing
  and run no repository code; class B scans (clippy, check, doc, tsc, tests, the repository's docs
  generator) start only after class A passes. Content scans run on the exported HEAD. A required
  tool that is missing fails the scan.
- `scripts/preflight.mjs` runs first: in the pre-push hook, as the first step of `ci.yml`'s `plan` job (every
  other job needs `plan`), at the start of `rk-pipeline preflight` and `loop` (before any canary, compile or dispatch)
  and as the first step of `publish.yml`'s `scans` job (every other job needs `scans`). It reads only the committed files of HEAD: no network, cargo, pnpm, broker or compile, a few
  seconds on any host. Its rules are the predicates the later stages already refuse with (consumer recipe hashes,
  npm surface & import closures, version copies, dispatcher helper digests, consumer locks, CRLF vs `.gitattributes`,
  file-URL paths, generated docs), run for every unit rather than only the one being published. Two more rules catch what
  only the hosted Windows and macOS runners used to show: `eol-pinned` (G25) refuses a tracked text file whose line ending
  `.gitattributes` leaves to `core.autocrlf` (the repository pins `* text=auto eol=lf` once, so a byte-compared or hashed
  file cannot gain CRLF on a Windows checkout), and `tmpdir-realpath` (G26) refuses a test that passes its temp directory to
  `realpath` yet compares the unresolved path (macOS answers `/private/var/...` for `/var/...`). Each `FAIL` line names
  the file and the change. `node scripts/preflight.mjs --list` lists the rules; `--commit <rev>` checks another commit, `--repo <dir>` another repository and `--worktree` what a compile would see.
- `scripts/local-gate.mjs` (the pre-push hook, after preflight) invokes `rightkit-gate --pre-push`: all applicable
  class A scans, focused `scripts/ci` tests & deterministic `generated-docs` checks before each push
  (Adrian 2026-10-22, #911). Native compile, clippy, rustdoc & package tests remain mandatory in
  publication qualification. Direct `rightkit-gate` invocation retains full class A & B coverage;
  generated app hooks also retain that default. Pre-push mode needs the committed local
  compile grant (`.rightkit-local-build.json`). Cargo runs only through the broker: `rustright cargo`, or
  the managed `cargo` shim (`cargo-guard.mjs`); plain cargo is refused. A deferred scan fails the gate,
  and so does a checkout that differs from HEAD where the broker scans read it. A change to a root
  workspace file (`Cargo.lock`, `Cargo.toml`, toolchain, `.cargo/`, pnpm lock or workspace) puts every
  crate or package in scope.
- The scans are always on in CI: there is no enable switch and no `deterministicScans` config key.
  A missing tool, a deferred scan or a failed scan fails the job; none of them is ever scan success.
- `publish.yml` job 0, `scans`, runs `scripts/ci/run-deterministic-scans.mjs` (class A only, the
  full requested units & shared qualification inputs, never a diff; every planned class A scan must pass) before `plan` and
  every build lane, with `cargo-deny` 0.20.2 and `cargo-audit` 0.22.2 (SHA-pinned
  `taiki-e/install-action`) and a checksum-verified `actionlint` 1.7.12. It runs on Ubuntu, where
  native crates do not compile, so class B (compile, lint) runs in the native `verify` jobs on their own
  hosts. `verify` and `verify-linux` `needs: scans` and pass `needs.scans.result` to `gate.mjs` as
  `RK_SCANS_JOB_RESULT`; `scans` is not a default gate step and those jobs install no scan tools.
  Selected content includes complete requested package directories, crate consumers, referenced external Rust inputs,
  shared CI/release tooling, workflows & root configuration. All Cargo manifests remain metadata inputs; unrelated
  crate/template/vendor Rust is excluded from formatting. Unresolved external source inputs refuse qualification
  (`scripts/ci/run-deterministic-scans.mjs`, `publicationContent`). The receipt binds registry & requested units.
  It uploads the scan receipt (classes `["A"]`), which `publish-reviewed.yml` verifies against the original run (commit,
  `Cargo.lock` digest, tool versions, every scan passed) before any upload.
- `ci.yml` runs the class A scans in the `hygiene` job (`gate.mjs --steps scans`) with the same pinned
  tools, before any compile or test step, for every change: Rust, npm, workflow, scripts and docs. The
  plan job passes `scan_base`, the oldest changed-path base of the runners; without a base (a full
  run) every tracked file is scanned. Shard and closure jobs `needs: hygiene`, so they start only after
  it succeeds, and receive its result as `RK_SCANS_JOB_RESULT`. A gate job that neither ran the scans
  step nor received that success runs no class B work and fails.
  `scripts/ci/ci-scans-contract.mjs` asserts these rules on the parsed `ci.yml` and proves them by mutation.
- Native CI & publication qualification run `cargo doc --no-deps` through `scripts/ci/gate.mjs`
  for configured feature sets, with rustdoc warnings treated as errors. Class B in shard jobs does not
  run through the shared runner (clippy and check command lines are shared, execution is not).
- Class A evidence for class B binds to the input content (`inputDigest` of the changed files), the class A
  scan set and a run that included class A; excluding every class A scan, or a plan with none, needs explicit
  prior evidence. Changed paths are checked lexically and by real path (symlinks). Class A pnpm scans run with
  `--ignore-pnpmfile` and `--ignore-scripts` (flags on `install`, the same settings through the environment for `audit`).
- A scan refuses, and never runs, a changed path that is absolute or escapes the content root, and a
  crate or package directory name that is not a plain name. Class B refuses to run without class A
  passing in the same call or valid prior class A evidence for the same scan set. Parsers load from
  the runner's own package; the docs generator runs only from a trusted checkout, with the exported
  commit passed as data.
- `packages/release/pipeline-receipts.mjs` verifies stage receipts (ADMISSION to PUBLISHED); the
  broker signs them. Nothing in CI or the gate issues or checks them yet.
- `local-publish.mjs` does not use the module yet; it follows once its pin is updated. `feed-legacy` (G27) refuses a staged version whose published predecessor is missing from `legacyNpm`/`legacyCargo` (rightkit#1188 dropped qa-chrome 0.1.11; right-suite-contract only saw it minutes into a native verify). After the dependency install and before any compile, signing or pack, `scripts/ci/exports-import-effects.mjs` imports every exported module of every package in a fresh process and refuses one that does anything at import (publish.yml `scans`, ci.yml `hygiene`); the sealed-byte import check in `verify` found this 27 minutes in (run 40168412670). It fails closed: a wildcard export (`./*`) is expanded against the source tree and every match is probed, a target that is missing and not under `./dist/` is a finding, a wildcard that matches nothing is a finding, and every `dist/` target it leaves to the sealed-byte check is printed as `not checked here` (the isolated simulation of 2026-10-25 found the earlier silent skips, A5.06/A5.07). Each probe child runs under Node's permission model (`--permission --allow-fs-read=*`), so an import that writes a file, starts a child process or a worker fails in the runtime and is a finding even when it prints nothing (A5.07b). The pre-push gate's Rust deferral on non-protected branches (`rightkit-gate`, `RIGHTKIT_GATE_NO_DEFER`) is a deferral, not evidence: compile evidence for such a push is produced only by CI (public repositories) or by the host broker's admitted compile and its HOST RECEIPT (private apps), and a merge requires one of them (A3.02). `feed-legacy` likewise fails when the feed is missing or unparseable instead of passing with nothing to check (A4.05). It probes an exported copy of `HEAD` (`git archive`), never the working directory, with the checkout's installed dependencies linked in and bound by the lockfile and pnpm's installed-state digest, and records the checkout identity before and after (a HEAD that moves during the probe makes the report unusable); untracked, ignored or locally modified files never reach it. Its `dist/` exports are INCOMPLETE, not green: `--report` writes them (`rightkit.import-effects/1`), the scans job uploads the report, and `publish.mjs verify` refuses to run without it (`RK_IMPORT_EFFECTS_REPORT`) and refuses any unit whose INCOMPLETE export the sealed-tarball import check did not import (`consume.mjs` closure: exact set per package, duplicate or conflicting rows and rows for undeclared build output refused); verify also compares the report's probe digest and Node version with its own trusted copies; without `--report` the probe exits 3. `ci.yml` has no sealed-byte stage, so its hygiene report is kept as an artifact and closed only by a publish run. Prior class A evidence handed to `runScans` (`priorClassA`) must carry the same `checker` as the runner judging it: `@rightkit/release@<version>#<digest of deterministic-scans.mjs + scan-digest.mjs>`, so another version, the same version with changed bytes, a bare version or an unknown identity never unlocks class B. Contract rule (C-1, from the same simulation): class A evidence must carry exactly the required rule set, one row per rule id, and an exact-id exception may cover only a rule id in that set, for one tree id, until its expiry; a skipped row whose id is not in the set is a refusal, never an exception candidate.

### The gate in other repositories (`rightkit-gate`)

`@rightkit/release` ships `rightkit-gate`, the same scan module as the pre-push gate above for any repository. It reads the layout from
the repository itself and covers all of it; what it cannot scan it names, never skips quietly.

- **Rust.** Every tracked `Cargo.toml` that is a workspace root, or a standalone package not inside another workspace, is its own Cargo
  workspace, read with its own `cargo metadata --no-deps --locked` through the broker (`rustright cargo` or the managed shim). A manifest below
  a workspace that is not one of its members is tried on its own (an `exclude`d crate is a real workspace); when cargo refuses it there it
  is a fixture or vendored crate, and the run prints it as skipped. A workspace member whose manifest is untracked or ignored, and an untracked
  `Cargo.toml`, are skipped with a note too. A manifest outside every workspace that cargo refuses stops the gate.
- **Names.** A unit of the root workspace keeps its crate name. A unit of a workspace below the root is `<cargoDir>:<crate>`
  (`hub/src-tauri:app`): that is its name in scan ids (`clippy:hub/src-tauri:app`), its scans run with that directory as cwd and `-p <crate>`,
  and `ci/gate.config.json` `rust` may key its feature sets by `<cargoDir>:<crate>` (wins) or by the plain crate name. The per-workspace scans
  are `cargo-metadata`, `cargo-deny` and `cargo-audit` (suffixed `:<cargoDir>` below the root). Reverse dependents, the lock scope of a
  lock-only change and the result-cache keys come from each workspace's own `Cargo.lock`; a change to one workspace's root files puts only
  that workspace's units in scope.
- **npm.** The `pnpm-workspace.yaml` globs over tracked `package.json` files. The root `package.json` is a unit as well when that file is
  absent, has no `packages:` or declares `.`; with a `packages:` list that does not name the root, the root is only the workspace manifest.
  A file inside a Rust unit below the root does not put a root package in scope.
- **Hooks.** `rightkit-gate install-hook` sets `core.hooksPath=.githooks`, which makes git ignore `.git/hooks`. The generated `.githooks/pre-push`
  therefore runs `.git/hooks/pre-push` first when it is executable (git's stdin is buffered and given to it and to the gate; its failure stops
  the push), and every other hook found in `.git/hooks` when `install-hook` ran (`pre-commit`, `commit-msg`, ...) gets a `.githooks/<name>`
  trampoline that execs the own hook when present. Nothing absolute is written, so the hooks are committed (`git add --chmod=+x .githooks/*`);
  `install-hook` lists the hooks it chained and replaces a differing `.githooks/<name>` only with `--force`.
- **Finding the gate.** The hook runs the root `node_modules/.bin/rightkit-gate`; else the first workspace package's (one to three levels
  below the root: `tauri-app-next`, `apps/*`, `packages/*`); else `pnpm exec rightkit-gate`; else node with a package-relative path. It prints which
  it used. `install-hook` warns when no `package.json` in the repository depends on `@rightkit/release`.

#### Release evidence for an app

`right-release qualify cargo` qualifies crates for a registry upload (below). A product repository, an app, publishes no crate through it:
the pin's evidence "a release qualified locally on both hosts" is the app's own `right-release` release QA, run on both hosts (the sealed
`right-release qa` source and installed receipts of the release for macOS and Windows, which `right-release qa audit` checks), not crate
qualification.

## What runs

A push to `main` is compared with the last commit the same runner passed on. With no such commit,
everything runs.

- **Rust.** A crate is tested and linted when its directory, a third-party package it compiles, or
  its gate config changed. Crates that depend on it are compiled with `cargo check --all-targets`.
  `Cargo.lock` is refreshed first and `cargo fmt` is applied; a difference fails the `lock` or
  `fmt` unit and the patch is printed in the log and uploaded as evidence.
- **npm.** A changed package & its dependents are tested after selected packages’ workspace
  dependency closure builds in dependency order.
- **Swift.** `swift/RightKitSwift` and `swift/RightKitCloudKitHelper` build and test on the macOS
  runner when their directory changed. The repository-root `Package.swift` (the manifest apps resolve by
  URL and tag) is checked with `swift package describe --type json`, which validates every target path
  without compiling; `scripts/swift-root-manifest.test.mjs` guards it against drift from
  `swift/RightKitSwift/Package.swift`.
- **Root suites.** `ownership-approval`, `characterization`, `infrastructure`, `swift/contracts`
  and `scripts` run when their directory changed.

Not covered yet: `swift/ios-app-template`, `spikes`. `desktop-app-template` is no fixture any more (rightkit#906): it is its own locked Cargo workspace with its own `deny.toml` and `.cargo/audit.toml`, which `rightkit-gate` discovers and scans like an app; this runner-side gate does not build it.

## Sharded CI

`ci.yml` retains static validation on push/PR; native shard & closure jobs run only on dispatch.
Both `ci.yml` & `publish.yml` require `request_id` & full `expected_sha`; first guard checks ID syntax
& SHA equality before planning or native work. Run names carry request correlation. These inputs
are correlation & commit binding, not signed authority; repository access remains unchanged.
Android compilation is dispatch-only with identical correlation checks; broker adapter allowlisting
for Android remains pending. Sources: `.github/workflows/ci.yml:4`, `publish.yml:4`, `android.yml:5`.

`ci.yml` runs independent jobs:

| Job | Runs on | What it does |
|---|---|---|
| `private-repo-guard` | Linux | Refuses a private repository |
| `plan` | Linux | `scripts/ci/ci-plan.mjs`: asks `gate.mjs --plan --host <os>` what Windows and macOS each have to verify (each OS against its own last-green marker; a pull request against its merge base), then cuts that into the jobs below |
| `shards-windows`, `shards-macos` | one job per shard | `gate.mjs --select "<tokens>" --steps test`: clippy, feature checks & rustdoc before selected crate tests; failed preflight skips that unit’s tests & consumers while independent units continue; npm package tests; root suites; each Swift package. `fail-fast: false`, so one red shard never hides another |
| `closure-windows`, `closure-macos` | up to 4 jobs per OS | `--steps closure`: every dependent of a changed crate is compiled with default features and with its `checkFeatureSets` rows (all features unless the config says otherwise) |
| `hygiene` | Linux | Required `--steps lock,fmt` for selected crates & shared secret scan of selected crate/package sources; installs frozen release dependency closure with lifecycle scripts disabled |
| `ownership` | Linux | Independent required ownership validation; advisory dependency graphs follow without delaying native qualification |
| `mark-green-windows`, `mark-green-macos` | Linux | Dispatch only: advance that OS's compilation baseline when guard, plan, hygiene & exact host command proof pass; overall ownership remains required independently |
| `summary` | Linux | Required check: push/PR proves static jobs passed & native jobs stayed skipped; dispatch additionally requires planned native reports |

How units are cut: a crate is never split, so all of its feature rows share one job and one target directory. Crates are packed
into groups of about 12 build commands (`rustWeight` in `shards.mjs`: test rows, doubled for a serial rerun, plus clippy, rustdoc &
feature-check rows); a crate that weighs that much alone runs alone. Groups are computed from every crate that applies on the OS,
never from the selection, so a crate always lands in the same job and the same Rust cache key (`rk-<runner>-rust-<anchor crate>`).
npm packages are grouped six to a job; each job builds only its packages' workspace dependency closure. The root suites and the
Swift root-manifest check share the `root` job. `ci/shards.json` can change the sizes (`rustTargetWeight`, `npmTargetWeight`,
`closureChunkSize`, `closureMaxJobs`) or pin units to their own job (`solo`).

Concurrency: Windows matrices cap shard jobs at 12 & closure jobs at 4; macOS caps them at 5 & 2.
GitHub admits jobs under account-wide runner limits; these caps do not reserve capacity for other workflows.

Each OS uses authenticated successful `mark-green-<os>` job history, resolved by `last-green.mjs`.
Resolver checks repository, workflow, main dispatch (or earlier push with successful native marker), exact attempt, full commit ancestry & compatible
shared harness/toolchain inputs. Missing or uncertain history selects full coverage; cache entries
never prove green. A red run repeats required units until they pass. Known-red labels remain visible;
aggregate command proof still requires successful results.
Optional toolchain-file absence is fingerprinted; adding, removing or changing either supported file invalidates reuse.
Lockfile changes use dependency selection instead of invalidating every host's compilation baseline.
An ownership refusal keeps overall CI red while valid compilation baselines can advance, so unrelated ownership work does not repeat successful native commands.
The aggregate check is `summary`; branch protection must select it when replacing former per-OS `ci` checks.
Summary requires every prerequisite, accepts matrix skips only for an explicit zero plan count, & checks every planned
shard report against commit, host, selection & steps. Plan freezes every required command; reports must contain
exact successful command coverage, complete result fields & consistent aggregate rows. Empty, failed or incomplete
proof fails. Shard artifacts overwrite their own prior attempt, so retrying failed jobs preserves exact coverage. Each host marker advances only from its
proved aggregate output, even when opposite host fails.

Preview what a shard would run: `node scripts/ci/gate.mjs --plan --select "rust:rightkit-fs"`. Compiling and testing still
happen only in Actions. Tokens: `rust:<crate>`, `rust-check:<crate>`, `npm:<dir>`, `root:<suite id>`, `swift:<package id>`; steps:
`lock`, `fmt`, `test`, `closure`.

## Reading a run

Each job writes a table to its job summary and uploads `qa-evidence-<runner>-<job id>` (`qa-evidence-hygiene-<attempt>` for the hygiene job):
`summary.md`, `summary.json`, one log per unit, and `fmt.patch` or `Cargo.lock.patch` when produced.

```bash
gh run list --workflow ci.yml --limit 5
```

```bash
gh run download <run-id> --dir /tmp/rightkit-ci
```

## Known red

A unit in `ci/known-red.json` still runs & retains its label in gate output. Aggregate shard proof
rejects failed commands, including labelled failures, before advancing last-green markers. An entry
records a defect; a crate or package with an entry on either host cannot publish. When it passes,
gate report names it under "Now green" for removal in next commit.

## Working method

1. Edit locally. Before pushing CI scripts or workflows, run `node --test "scripts/ci/*.test.mjs"` (Adrian, 2026-10-20). This Node-only preflight is local; Cargo, package test suites, installs & builds remain CI-only.
2. Preview selection with `node scripts/ci/gate.mjs --plan --base <sha>`.
3. Batch reviewed fixes into one push. Active `main` validation finishes; its single pending slot coalesces newer pushes to latest pending commit. Superseded PR validation cancels. Independent qualification runs remain concurrent; promotion uses a separate global queue. Avoid micro-pushes that discard pending validation.
4. Apply `fmt.patch` and `Cargo.lock.patch` from the evidence with `git apply`, then commit.

## Two hosts, one owner

- One integration owner owns HEAD, index, commits & pushes to `main`; workers edit disjoint paths in primary checkout.
- Each CI run qualifies its exact commit on both native runners. Owner reports commit & observed result.
- Green CI proves hosted qualification. Installed-build verification separately names installed commit.

## Publishing

Two routes, one uploader (`scripts/ci/upload.mjs`), and registry uploads exist only in Actions:

- **GitHub qualification publishes directly** (Adrian, 2026-10-22: "As soon as the qualification passes on GitHub, GitHub can
  directly push it to npm and crates"). One dispatch of `publish.yml` from `main` of public `bogusyogi/rightkit`; when that run
  completes green, `publish-direct.yml` (`workflow_run`) uploads the run's own sealed bytes. No candidate, no approval commit,
  no second dispatch. The uploader (`reviewed-candidate.mjs run-qualified`) re-checks the run through the API (dispatch of
  `publish.yml` on `main`, `guard`/`plan`/`pack`/`qualified` and both native `verify` jobs successful in the completed attempt,
  `verify-linux` exactly when `ci/linux-required.json` requires it), downloads `publish-packed` and the verify receipts from
  that run's artifact listing, re-hashes them, reads registry, units and allowsets from the sealed manifest and receipts, and
  publishes exactly those bytes with the run URL recorded as the review reference. Its checkout is the qualified commit.
  Runtime-proof owners (`RUNTIME_PROOF_WORKLOADS` in `scripts/ci/upload.mjs`: logs, job-queue, qa, daemon, tauri-shell)
  publish on this route too (Adrian 2026-10-22: "those can have exceptions"); `publish.yml` carries no runtime workload, so
  their resource budgets are a separate, non-blocking report (local-runtime or `qualify-runtime.yml`), named in the
  publication receipt's candidate as `runtimeBudgetUnproven`.
  Hosted qualification enters through RustRight broker `rightkit-dispatch` for `publish.yml`; local warm-cache
  feedback runs separately. `--with-ci` is retired & refuses before effects. A green hosted run publishes its own
  sealed bytes directly; a local candidate cannot replace bytes of an already published version.
- **Local qualification promotes reviewed candidates** (see "Local qualification", below: both native hosts through the
  RustRight broker, an approval commit, then `publish-reviewed.yml`). This is the route for bytes GitHub did not qualify
  and for every runtime-proof owner.

Dispatch `publish.yml` through broker adapter with registry, units, broker request ID & admitted full SHA.
Guard rejects missing correlation or mismatched commit before scans; direct sealed-byte publication stays unchanged.

`units` is a space-separated, dependency-first list; one registry per qualification run.
Native publication checks cover exactly those units & their configured standalone consumers; reverse workspace
dependents remain ordinary CI obligations (`scripts/ci/select.mjs`, `selectUnits`).
`publish.yml` has no `dry_run` dispatch input & never uploads to a registry. It packs once,
then Windows (`windows-2025`) & macOS (`xcode-27`) verify those sealed archives.
Its per-run concurrency group permits independent qualifications concurrently; `publish-direct.yml` uploads
serially in the `publish` concurrency group, so dependency-ordered dispatches publish in order.

The rest of this section is the reviewed route. After R8 Astra signoff on exact diff, commit both approval layers in `ci/approved-artifacts.json` (schema 2):

- `artifacts`: every unit's registry, name, version & archive SHA-256, with reviewer & review reference.
- `candidates`: immutable candidate ID, original source SHA, qualification run ID/attempt/workflow ID,
  sealed manifest SHA-256, registry, ordered units & exact allowsets; packed artifact ID/digest &
  both native receipt artifact IDs/digests plus raw receipt SHA-256 values.

Archive approval hashes & candidate provenance hashes are both required. One cannot replace
another. R8 review must cover original source, sealed archives, manifest & native evidence.
Collect authenticated pins with `node scripts/ci/candidate-pins.mjs <run-id> <candidate-id> <new-scratch-directory>`.
Collector emits an unsigned proposal after verifying source identity, artifact digests & native receipts.
It never edits approvals, assigns reviewers or publishes; independent R8 review remains required.

Commit approval after qualification source commit; promotion commit must descend from it.
Dispatch only committed candidate ID:

```bash
gh workflow run publish-reviewed.yml -f candidate_id="<committed-candidate-id>" -f dry_run=false
```

Promotion defaults to `dry_run=true`: admission & authenticated transfer run without registry upload.
Set `dry_run=false` explicitly for publication. Secretless `seal-check` internally runs
`reviewed-candidate.mjs run --dry-run`; credentialed `upload` authenticates its same-run receipt &
uploads exact approved archives without rebuilding. Dispatch cannot supply paths, URLs, units or wider allowsets.

### Local qualification (default)

Adrian's design (2026-10-21/22): every crate is qualified on the two native hosts through the RustRight broker, the exact
bytes are approved by hash in a commit, and `publish-reviewed.yml` uploads exactly those bytes. No GitHub qualification run
and no rebuild stand between the approval and the upload. `publish.yml` qualification stays as the fallback (below).

```bash
# Dell, D:/Claude/rightkit. Clean tree, HEAD pushed to origin/main, the Mac checkout (/Volumes/D/claude/rightkit) at the same HEAD.
right-release qualify cargo --crates rightkit-fs,rightkit-child-process
# without right-release on PATH: node packages/release/cli/right-release.mjs qualify cargo --crates ...
```

`--hosts win,mac` is the default and the only set that cuts a candidate (one host alone writes its bundle and stops);
`--id`, `--allow-ignored-units`, `--jobs`, `--concurrency`, `--runtime-repeats`, `--wait-idle-min`, `--out`, `--win-root`,
`--mac-root` & `--no-release` are the other options (the two runtime options: "Runtime proof (local)", below). Crates only: `publish cargo --local --qualify-only` is cargo only, so npm packages use `publish.yml`.

Any repository that publishes crates this way, not only RightKit: the repository (owner/name) is the `origin` remote, and the host
checkout roots, the approvals file and the promotion workflow come from a committed `.rightkit-gate.json`:

```json
{ "hosts": { "win": { "root": "D:/work/widgets" }, "mac": { "root": "/Volumes/D/work/widgets" } },
  "approvedArtifacts": "ci/approved-artifacts.json", "promotionWorkflow": "publish-reviewed.yml" }
```

RightKit's own values (`D:/Claude/rightkit`, `/Volumes/D/claude/rightkit`, `ci/approved-artifacts.json`, `publish-reviewed.yml`) are the
defaults only when the origin is `bogusyogi/rightkit`. For any other repository an absent file, an unknown key or a missing value refuses by
name before anything runs (a host root is needed only for a host in `--hosts`; `--win-root` and `--mac-root` still override), and the
repository must keep crates under `crates/` and ship the CI scripts qualification loads from its own checkout (`scripts/ci/scan-receipt.mjs`,
`reviewed-candidate.mjs`, `upload.mjs`, `publish.mjs`, `archive-secret-scan.mjs`): a missing one refuses by file name. The other host runs
this script from the same layout, or from `node_modules/@rightkit/release` when the repository installs the package. Those scripts' promotion
side (`reviewed-candidate.mjs`, `upload.mjs`) check the repository name they were written for; a repository carrying its own copy keeps
that check in step with its `origin`.

What runs, at once on both hosts (Windows on the Dell; macOS on the Mac through `ssh mac "<cmd>"`, or from the Mac through
`win "<cmd>"`; set `RK_LOCALQ_REMOTE_NODE` when `node` is not on the non-interactive PATH of the other host):

1. `packages/release/local-qualify.mjs host`: the deterministic scans of the units' scope (class A then class B, through the
   broker, none deferred), then per crate `right-release publish cargo --crate X --local --qualify-only --bundle <dir>` (tests,
   clippy, fmt, secret scan, package verify, publication gate), the archive secret scan of the packed `.crate`, and a hashed
   bundle. The macOS host also writes the crates.io upload metadata: its archive is the canonical one, as on CI's macOS packer.
2. The driver pulls each remote bundle as one hash-checked JSON document (`export`), refuses unless every host packed the SAME
   archive bytes for every unit, seals `packed-<id>.zip` (manifest, `.crate` files, upload metadata) and one `local-verify`
   receipt per host, and runs the same `loadSealed` and `checkLocalReceipts` the promotion runs.
3. `gh release create candidate-<id> --prerelease --target <HEAD>` attaches the three assets, and the driver reads the release
   back. It prints the candidate and the approval rows; it never edits `ci/approved-artifacts.json`.

The printed candidate (`source: "local"`) has this shape; every key is required and nothing else is allowed:

```json
{ "id": "local-fs-032-1a2b3c", "source": "local", "sourceSha": "<HEAD>", "registry": "crates", "units": ["rightkit-fs"],
  "allow": { "hostExclusion": [], "noTest": [], "ignored": [], "missingAssets": [] }, "release": { "tag": "candidate-local-fs-032-1a2b3c" },
  "assets": { "packed": { "name": "packed-<id>.zip", "sha256": "<64 hex>" },
              "windows": { "name": "local-receipt-windows-<id>.json", "sha256": "<64 hex>" },
              "macos": { "name": "local-receipt-macos-<id>.json", "sha256": "<64 hex>" } } }
```

Compatibility only: older combined candidates may contain `ci`; new local qualification does not dispatch hosted work.

The `artifacts` rows come with `reviewer` and `reviewRef` set to `null`, which the approval loader refuses until a reviewer
fills them. After the R8 review, one commit adds the candidate to `candidates`, the filled rows to `artifacts` and the two
sorted lines to `scripts/ci/publish.test.mjs`; the promotion commit must descend from `sourceSha`. Then:

```bash
gh workflow run publish-reviewed.yml -f candidate_id="<candidate-id>" -f dry_run=true
gh workflow run publish-reviewed.yml -f candidate_id="<candidate-id>" -f dry_run=false
```

Promotion of a local candidate, with the workflow file unchanged: `identify` reads the committed candidate, its prerelease, the
tag (a lightweight tag at `sourceSha`) and the compare against the promotion commit; `run --dry-run` downloads each asset by
release asset ID through the API, requires its SHA-256 to equal the committed pin, rebuilds the `packed`/`receipts` layout,
and runs `local-admission.mjs` over the source as data; `scan-receipt.mjs verify` re-checks every host's scan record against
the candidate's `Cargo.lock`; `upload` repeats every pin, approval and receipt check in the credential job.

Binding: a `local-verify` receipt names host and OS, the RustRight client (path and SHA-256), commit and tree (equal to the
candidate's source), the manifest hash, the archive SHA-256 of every unit (equal on both hosts), the gate evidence of every
unit, the archive secret scan, a scan record (classes A and B, nothing deferred, bound to `Cargo.lock`) and, for a runtime
owner, its runtime proof (below). Admission also requires each packed file to equal the blob at `sourceSha` and
`.cargo_vcs_info.json` to name it.

**Separate local feedback & hosted qualification.** `--with-ci` is retired: argument parsing, driver & exported
legacy dispatch helpers refuse before effects (source: `packages/release/local-qualify.mjs:150`, `:618`, `:628`).
Run local qualification without that option for warm-cache feedback; submit hosted `publish.yml` qualification
through RustRight broker `rightkit-dispatch`, carrying broker request ID & admitted full SHA. Local feedback
& hosted qualification produce separate evidence; hosted direct publication uploads its own sealed archives.

Compatibility only: existing candidates may retain
`ci: { workflowId, runId, sourceSha, units, dispatchedAt }`, with matching source & units. Existing admission
continues validating that pinned run & writing `ciProof`; historical `allow.ciFailure` cannot waive required Linux
proof. This schema support does not restore combined dispatch or create new candidates with hosted proof.

Use broker-dispatched `publish.yml` when local qualification cannot produce required Linux evidence
(`ci/linux-required.json`), hosts pack different bytes, or required local scan tools are unavailable.
Runtime owners without trusted workload coverage remain unproven locally; hosted qualification does not
supply runtime proof. Direct publication records that separate runtime-budget status as described above.

Not covered by a local candidate: the R2a sealed-registry consumer recipe and the original-run QA artifacts.
`reviewed-registry-readback.yml` and `supplement-archive-scan.yml` read Actions-run fields and do not accept a local
candidate. The R8 review is the compensating control, and it must read the receipts, scan records and runtime receipts.

#### Runtime proof (local)

Runtime-budget owners (`logs`, `job-queue`, `qa`, `daemon`, `tauri-shell`) no longer need `qualify-runtime.yml`. After a
host's units are qualified, and only then (a quiet host is the precondition), `packages/release/local-runtime.mjs` measures
each runtime owner on that host and writes the proof into the host's `local-verify` receipt as `runtime.crates[<crate>]`.
The receipt is pinned by hash in the committed candidate, so the proof is pinned with it; no new asset, no new candidate key.

What runs, per crate, per host:

- **The trusted workloads.** `workloadsFor(crate)` from `scripts/ci/runtime-qualify.mjs` (the table of pinned `cargo test
  --locked --profile release-iterate ... --bench ...` commands, features, trailing arguments, env names, profiles), never the
  candidate's list: `crates/<crate>/benches/workloads.json` must equal that table or the run refuses. Each command runs
  through the broker (`rustright cargo`), after a warm `--no-run` build that is never part of a window.
- **The measured source.** The checkout is clean and at the candidate commit before and after every run (HEAD is on
  `origin/main`, as for the rest of local qualification). Every file the measurement depends on is hashed from the git
  blobs of that commit into `bindings`, as CI's `bindSource` does, and the whole harness helper closure
  (`trustedRuntimeHelperPaths`) is hashed into `harness.helpers`; admission requires it to equal the promotion checkout's.
- **The same measurement code.** The bench's own `RESULT budget` line is recorded by the owner's `recordMeasurement` and
  judged by `measurementProblems` against the frozen budget (top-level or profile ceilings, delta basis, required scale).
- **Three runs, the worst submitted.** Each workload runs `--runtime-repeats` times (3 to 9, default 3), each in its own
  window; every run must pass the owner gate and the submitted record is the worst by `value / ceiling`. A lucky quiet run
  cannot hide a noisy workload.

The quiet window (`scripts/ci/local-runtime-proof.mjs`, policy `local-runtime-window/1`), recorded for every run and
**recomputed by admission from the samples** (the receipt's own `quiet` flag is never trusted):

1. before: the broker's `health.json` is fresh, `READY`, not paused, with `inFlight`, `queued` and `admitting` all 0, and
   the host CPU (OS tick counters, all processes) was at least 90% idle over a 10 s sample;
2. during: every broker snapshot shows at most the measured job (`inFlight` <= 1, `queued` 0), with no gap over 15 s;
3. after: a snapshot written after the run ended shows the broker idle again, and CPU was at least 90% idle over 5 s;
4. the broker's job listing (`rustright list`) shows exactly one root job in the measured checkout and no other root job
   overlapping the span from the first CPU sample to the last (this catches a short job that two 5 s snapshots could miss).

A window that fails any of these refuses the run with the reason; it is never retried silently. `--wait-idle-min` (0 to 240,
default 30) bounds how long the host waits for quiet before the first sample; it never waits during or after a run.

**Broker exclusivity: a gap, not a feature.** The broker (`D:/Claude/rustright`) has no client-held exclusive or drain lease.
`pause-admission` / `resume-admission` (`DataRootGate`) is a global, persisted switch that rejects, not queues, every new
root job, including the bench itself (only NESTED jobs pass), and a crashed holder leaves the whole host paused across broker
restarts. `RIGHTKIT_EXCLUSIVE` classes only serialize jobs that declare the same class. The cutover `DRAIN_STATUS`, hold fence
and activation fence belong to the installer. So the proof DETECTS a disturbed window; it does not prevent one. A broker lease
(`EXCLUSIVE_WINDOW` with a TTL, queueing other roots instead of rejecting, the holder's nested jobs admitted) would turn
detection into prevention; ask for it in `rustright` if disturbed windows turn out common.

Admission (`local-admission.mjs`, `runtime-supplement.mjs verifyLocalRuntimeProof`) applies the Actions path's validators to
the receipts of both hosts: trusted workload ids and order, prescription equality, frozen budget (ceilings, scale, profiles),
`validateWorkloadReceipts`, `measurementProblems` and the full owner record gate (`validateRuntimeRecords`) once per repeat
index and once for the submitted records, every source binding against the candidate's blobs, and the window recomputation.
The result is the same opaque handle `runtimeFeatureProofPolicy` accepts, so the unit reads `qualified` with the trusted
workload ids and a proof hash over the pinned runtime evidence, which the credentialed job recomputes from the receipts it
downloaded. `checkLocalReceipts` repeats only the shape checks (both hosts present, workload ids, repeat bounds, raw hashes,
selected record, no runtime claim for a unit without coverage). `rightkit-tauri-shell` and `rightkit-daemon` need the
broker to forward the harness env (`RIGHTKIT_RUNTIME_SCRATCH_ROOT`, TLS fixture paths); if it does not, those workloads fail
closed with the bench's own error.

### Qualification batch & scheduling

Until per-unit candidate/receipt slicing exists, one qualification batch must contain only
review-clean units with compatible dependency closure. Isolate any host-held or asset-held unit
in its own batch; never promote a clean subset from a mixed batch. Schema currently binds entire
ordered unit list, sealed manifest & both native receipts.

Promotion is serialized across registries by global concurrency group `publish`, `queue: max`
(up to 100 pending runs), `cancel-in-progress: false`. Queue order is not a dependency scheduler:
operator dispatches only DAG-ready candidates whose dependencies are available. Preserve every
active promotion; qualification runs need no promotion lock.

Qualification artifacts, native receipts, admission receipts & publication receipts retain 90 days.
Promotion rejects expired or missing artifacts, mismatched digests, wrong source/run/attempt,
failed or incomplete native jobs & altered manifests. Requalify & review after evidence expires;
never rebuild silently under an old approval.

### Qualification jobs

| Job | Host | Evidence / authority |
|---|---|---|
| `guard` | Ubuntu | Requires `main` & public repository |
| `plan` | Ubuntu | Parsed workflow contracts & focused CI contract tests; validates unit names, versions, dependency order, known-red status & explicit allowsets |
| `build-swap` | `xcode-27`, when `@rightkit/release` is selected | Compiles helper without secrets; seals unsigned bytes & source/run/host provenance |
| `sign-swap` | Fresh `xcode-27` runner | Validates same-run unsigned artifact before credential import; signs/verifies without source checkout, dependency install or compilation; only qualification job using `release` environment & signing secrets |
| `pack` | Ubuntu for npm; `xcode-27` for crates | Builds selected dependency closure, packs once & seals name/version/size/SHA-256 in `manifest.json`; npm also seals SHA-1/integrity; crates seal packaged upload metadata |
| `verify` | `xcode-27` & `windows-2025` | Re-hashes archives, runs selected unit gates, consumes sealed packages & writes host receipt bound to source, manifest & run/attempt |
| `qualified` | Ubuntu | Requires successful guard, plan, pack & both-host verification with nonempty plan |

`pack` stays secretless. For `@rightkit/release`, both helper jobs must succeed; a skipped signer
cannot satisfy packing. Pack downloads signed helper, verifies its hash & includes it before sealing.
Unsigned transfer binds source, run/attempt, workflow, native host & bytes; signing checks that binding
before importing identity. `sign-swap` uses `APPLE_CERTIFICATE_BASE64`,
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_KEYCHAIN_PASSWORD` & `vars.APPLE_DEVELOPER_ID` in `release`.

Four optional qualification inputs name requested units explicitly:

- `allow_host_exclusion`: crates excluded from one native host by gate config.
- `allow_no_test`: npm packages without a test script.
- `allow_ignored`: units permitting recorded ignored tests.
- `allow_missing_assets`: units listed in `ci/asset-gated.json` permitting recorded asset/platform skips;
  these run with `RIGHTKIT_REQUIRE_ASSETS=0`. Other units run strict.

Unapproved exclusions, missing scripts, ignored tests & skips fail qualification. These allowsets
record exceptions; they do not establish contract closure or waive native runtime requirements.

Sealed npm consumers install requested tarballs together into scratch projects, resolve declared
entry points, import supported JS entries & run declared safe `binSmoke` commands. Sealed crate
bootstrap checks may patch unpublished request siblings only to acquire upstream dependencies.
Named Cargo R2a examples then build & run in isolation through a directory registry of original
sealed crates, without path patches. Generic npm import smoke alone cannot qualify a named PK recipe.
These checks do not substitute for every release qualification requirement.

### Manual holds & registry equality

Locked [package qualification requirements](plans/2026-10-20-rightkit-implementation/01-packages.md)
remain controlling. Missing external-policy evidence, R7 native runtime-budget evidence for published version/features,
dependency-closure proof or R2a automation stays manual **HOLD**. Workflow success alone does not close these requirements.
R8 review records actual evidence & unresolved holds before candidate approval.

R2a requires isolated consumers resolving through a directory registry of sealed archives before
upload. Existing sealed-consumer checks are evidence, not a claim that complete R2a directory-registry
automation exists. R2b consumes actual registry packages after upload. Record R2b evidence separately;
a successful upload or checksum readback alone does not prove consumer behavior.

Registry equality is mandatory. npm publication sends sealed `.tgz` with `--ignore-scripts`, public
access & provenance; readback checks sealed SHA-1/integrity. Crates upload uses direct registry API
PUT of sealed `.crate` & packaged metadata, avoiding token-bearing compilation; sparse-index checksum
must match sealed SHA-256. Registry errors, unreadable responses or mismatched bytes fail publication.

### Admission & credentials

`reviewed-candidate.mjs` authenticates original `publish.yml` run/attempt, source repository/commit,
workflow identity, successful required jobs & exact artifact IDs/digests through Actions API. It checks
raw native receipt hashes, sealed manifest hash & every per-unit archive approval before promotion.
Secretless admission installs trusted verifier dependencies without lifecycle scripts, then checks out
original source as data outside verifier. Full Git history, clean source tree, original native raw QA,
actual command rows/counts, named PK consumer recipe & runtime owner checks bind admission to bytes.
A same-run hashed admission artifact transfers this verdict into a fresh sparse credential job.
Both jobs use trusted scripts/approval data from promotion commit; source package code is never
checked out or built in credential job. Dry-run executes admission & transfer with neither registry upload.

Historical native receipts remain immutable. If an original receipt lacks its own archive-scan field,
`supplement-archive-scan.yml` can scan retained original archives without rebuilding them. Separate
committed pins in `ci/supplemental-proofs.json` bind successful producer provenance, policy revision,
original candidate/manifest/archive & supplemental proof hashes. Admission authenticates those pins;
failed, null or malformed existing scans remain failures. Runtime, closure & consumer requirements
still need their own evidence. Missing or expired original artifacts refuse reuse.

Npm provenance names promotion workflow & promotion commit; candidate records separately retain
original producing commit & archive identity. Rerunning only failed qualification jobs cannot supply
complete same-attempt native pins; preserve original valid evidence or run complete qualification.

`workflow-contract.mjs` enforces qualification structure, one pack & both native hosts with no registry
privilege. `reviewed-workflow-contract.mjs` pins promotion's candidate ID/dry-run inputs, global queue,
secretless admission, sparse checkout, token scope & upload dependency. Contracts reject weakened
workflow structure; they cannot establish external repository policy or defend against a malicious
committer who changes both workflow & contract.

Trusted `main` writers own workflow, verifier & approval data. Approval records bind exact archive
bytes to independently observed human review; reviewer/reference strings are not cryptographic
signatures. Authenticated Actions provenance establishes artifact origin, not reviewer identity.
The `release` environment has a `main` branch restriction & no required reviewers. Registry secret
names were verified there; repository-level `CARGO_REGISTRY_TOKEN` was removed to prevent unrelated
branch workflows from receiving it. Branch protection would add a separate trust boundary; it is
not claimed by this pipeline or required by existing R8 review policy.
Registry tokens & OIDC authority belong only to promotion `upload`; read-only Actions authentication
is used for artifact admission. Child-process environments are allow-listed; only npm upload child
receives npm token. Secretless admission refuses registry/OIDC credentials. Credential-free transfer receives OIDC
capability from its upload job but never requests an OIDC token.

Current semantic capabilities fail closed: only reviewed PK01/PK11 Cargo recipes are inventoried;
npm named recipes & authenticated isolated runtime feature/workload proof still require producer
integration. Always-on runtime classes remain HOLD until that proof exists. Historical receipts
without canonical source-gate/publication fields cannot be repaired by synthesizing missing metadata.

### Chrome for Testing

Qualification provisions pinned Chrome through `scripts/ci/provision-chrome.mjs` using
`ci/chrome-for-testing.json`, verifies download SHA-256 & caches by pin-file hash. Empty pins fail;
there is no dispatch dry-run pin-discovery route. Windows headless-shell pin supports
`rightkit-browser` AppContainer fixtures. Workspace moving-Stable installer is not qualification evidence.

Ordinary CI gate uses same provisioner for selected Chrome-dependent units, exports
`RIGHTKIT_CHROME_FOR_TESTING_ROOT` & `RIGHTKIT_CHROME_FOR_TESTING`, & reuses an installation only
when recorded digest matches pin. Provisioning failure is red `chrome:provision` evidence.

### Retries & ledger

Every real promotion reconciles all planned versions against registry before sending bytes. Exact
existing bytes are recorded as `published-by-earlier-attempt`; different bytes or unreadable registry
state refuse publication. Current reviewed workflow does not download previous-attempt receipts;
retain those artifacts separately rather than claiming automatic receipt-history carry-forward.
Ambiguous upload outcomes require registry reconciliation, never a guessed success.

`publication-receipt` records source commit, promotion identity/candidate, versions, archive hashes &
publication/readback outcomes. After actual publication:

```bash
gh run download <promotion-run-id> --name publication-receipt --dir D:/scratch/rk-receipt
node scripts/ci/publish.mjs ledger D:/scratch/rk-receipt/publication-receipt.json
```

`ledger` rejects dry-run receipts & records only published, registry-verified rows through strict
`recordPublished` into `packages/release/rightkit-versions.json` & consumer copies, then reads them
back. Commit resulting ledger files through integration owner. Registry consumer receipts & contract
closure evidence remain bound separately to exact published versions & hashes.
