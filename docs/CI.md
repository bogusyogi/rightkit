# RightKit CI

RightKit CI runs in GitHub Actions on Windows & macOS. Pushes to `main` & pull requests run
selected package qualification in independent jobs. `publish.yml` qualifies sealed archives;
`publish-reviewed.yml` promotes committed, reviewed candidates.
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
| `.github/workflows/publish-reviewed.yml` | Promote a committed candidate without rebuilding |
| `scripts/ci/publish.mjs` | Qualification planning, packing, verification & receipt-ledger import |
| `scripts/ci/reviewed-candidate.mjs`, `scripts/ci/upload.mjs` | Authenticate candidate provenance, enforce approvals & upload sealed bytes |

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

Not covered yet: `desktop-app-template`, `swift/ios-app-template`, `spikes`.

## Sharded CI

`ci.yml` runs independent jobs instead of one serial gate per OS:

| Job | Runs on | What it does |
|---|---|---|
| `private-repo-guard` | Linux | Refuses a private repository |
| `plan` | Linux | `scripts/ci/ci-plan.mjs`: asks `gate.mjs --plan --host <os>` what Windows and macOS each have to verify (each OS against its own last-green marker; a pull request against its merge base), then cuts that into the jobs below |
| `shards-windows`, `shards-macos` | one job per shard | `gate.mjs --select "<tokens>" --steps test`: tests, clippy and every feature row of the selected crates; npm package tests; root suites; each Swift package. `fail-fast: false`, so one red shard never hides another |
| `closure-windows`, `closure-macos` | up to 4 jobs per OS | `--steps closure`: every dependent of a changed crate is compiled with default features and with its `checkFeatureSets` rows (all features unless the config says otherwise) |
| `hygiene` | Linux | Required `--steps lock,fmt` for selected crates & shared secret scan of selected crate/package sources; installs frozen release dependency closure with lifecycle scripts disabled |
| `ownership` | Linux | Independent required ownership validation; advisory dependency graphs follow without delaying native qualification |
| `mark-green-windows`, `mark-green-macos` | Linux | Advance that OS's compilation baseline when guard, plan, hygiene & exact host command proof pass; overall ownership remains required independently |
| `summary` | Linux | The one required check: fails unless guard, plan, hygiene & ownership pass and every other job passed or was skipped because the plan needed none of it |

How units are cut: a crate is never split, so all of its feature rows share one job and one target directory. Crates are packed
into groups of about 12 build commands (`rustWeight` in `shards.mjs`: test rows, doubled for a serial rerun, plus clippy and the
feature-check rows); a crate that weighs that much alone runs alone. Groups are computed from every crate that applies on the OS,
never from the selection, so a crate always lands in the same job and the same Rust cache key (`rk-<runner>-rust-<anchor crate>`).
npm packages are grouped six to a job; each job builds only its packages' workspace dependency closure. The root suites and the
Swift root-manifest check share the `root` job. `ci/shards.json` can change the sizes (`rustTargetWeight`, `npmTargetWeight`,
`closureChunkSize`, `closureMaxJobs`) or pin units to their own job (`solo`).

Concurrency: Windows matrices cap shard jobs at 12 & closure jobs at 4; macOS caps them at 5 & 2.
GitHub admits jobs under account-wide runner limits; these caps do not reserve capacity for other workflows.

Each OS uses authenticated successful `mark-green-<os>` job history, resolved by `last-green.mjs`.
Resolver checks repository, workflow, main push, exact attempt, full commit ancestry & compatible
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

Publication has two dispatches from `main` of public `bogusyogi/rightkit`: qualify once,
then promote approved original bytes. Builds, package tests & publication stay in Actions;
local-build exceptions do not authorize local execution under current CI-only instruction.

```bash
gh workflow run publish.yml -f registry=npm -f units="@rightkit/logs @rightkit/qa"
gh workflow run publish.yml -f registry=crates -f units="rightkit-fs rightkit-child-process"
```

`units` is a space-separated, dependency-first list; one registry per qualification run.
`publish.yml` has no `dry_run` dispatch input & never uploads to a registry. It packs once,
then Windows (`windows-2025`) & macOS (`xcode-27`) verify those sealed archives.
Its per-run concurrency group permits independent qualifications concurrently.

After R8 Astra signoff on exact diff, commit both approval layers in `ci/approved-artifacts.json` (schema 2):

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
