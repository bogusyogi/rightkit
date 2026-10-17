# RightKit CI

RightKit is a public repository. Every compile and every test runs in GitHub Actions; a developer
host only edits, reads and runs checks that cannot compile or execute project code. `.rightgit.json`
declares `"compile": "github-actions-only"`.

## Layout

| File | Role |
|---|---|
| `.rightgit.json` | right-git manifest: `rust-hybrid` profile, `ci` lane, runners `xcode-27` and `windows-2025` |
| `.github/workflows/ci.yml` | Rendered by `right-git sync .`; never edited by hand |
| `scripts/gate.sh` | Thin caller for the gate |
| `scripts/ci/gate.mjs` | Selects the units a push touched, runs them, writes the summary |
| `scripts/ci/select.mjs` | Pure selection logic, covered by `scripts/ci/select.test.mjs` |
| `ci/gate.config.json` | Per-crate feature sets, extra paths, serial runs, host limits |
| `ci/known-red.json` | Units that are red today, per host, each with a reason |
| `.github/workflows/publish.yml` | Manual-dispatch publication of npm packages and crates; see "Publishing" |
| `scripts/ci/publish.mjs` | Plans, packs and publishes for `publish.yml`; records receipts in the ledger |

## What runs

A push to `main` is compared with the last commit the same runner passed on. With no such commit,
everything runs.

- **Rust.** A crate is tested and linted when its directory, a third-party package it compiles, or
  its gate config changed. Crates that depend on it are compiled with `cargo check --all-targets`.
  `Cargo.lock` is refreshed first and `cargo fmt` is applied; a difference fails the `lock` or
  `fmt` unit and the patch is printed in the log and uploaded as evidence.
- **npm.** A changed package and every package that depends on it are tested after the whole
  workspace is built in dependency order.
- **Swift.** `swift/RightKitSwift` and `swift/RightKitCloudKitHelper` build and test on the macOS
  runner when their directory changed. The repository-root `Package.swift` (the manifest apps resolve by
  URL and tag) is checked with `swift package describe --type json`, which validates every target path
  without compiling; `scripts/swift-root-manifest.test.mjs` guards it against drift from
  `swift/RightKitSwift/Package.swift`.
- **Root suites.** `ownership-approval`, `characterization`, `infrastructure`, `swift/contracts`
  and `scripts` run when their directory changed.

Not covered yet: `desktop-app-template`, `swift/ios-app-template`, `spikes`.

## Reading a run

Each runner writes a table to the job summary and uploads `qa-evidence-<runner>-<attempt>`:
`summary.md`, `summary.json`, one log per unit, and `fmt.patch` or `Cargo.lock.patch` when produced.

```bash
gh run list --workflow ci.yml --limit 5
```

```bash
gh run download <run-id> --dir /tmp/rightkit-ci
```

## Known red

A unit in `ci/known-red.json` still runs and still shows red, but does not fail the gate, so the
last-green commit can advance and later pushes stay incremental. An entry is a recorded defect:
a crate or package with an entry on either host is not published. When such a unit passes, the
summary names it under "Now green" and the entry is removed in the next commit.

## Working method

1. Edit locally. Run nothing that compiles or tests.
2. Preview the selection with `node scripts/ci/gate.mjs --plan --base <sha>`.
3. Push to `main` in batches. A push that arrives while a run is in progress waits; of several
   waiting pushes only the newest runs, and it covers everything since the last green commit.
4. Apply `fmt.patch` and `Cargo.lock.patch` from the evidence with `git apply`, then commit.

## Two hosts, one owner

- One integration owner (the Mac lead) commits to `main` and merges pull requests. Nobody else pushes to `main`.
- The Windows side works on `win/<topic>` branches, one small pull request per change, and reports "PR N green, merge it" with the order when pull requests depend on each other. The owner squash-merges.
- A pull request run builds the branch merged with `main`'s tip on both runners, so a red `main` fails every pull request. Whoever breaks `main` fixes `main` first.
- The owner states `main`'s condition in plain words after each run: "main is green at <sha>" or "main is red at <sha>, cause, fix pushed". Branches merge `main` in only after "green".
- Docs-only commits carry `[skip ci]`.
- Green CI is not "seen working": a report of behaviour on an installed build is separate and names the commit.

## Publishing

RightKit cannot publish from a local machine. `publish.yml` is the only route: manual dispatch, from
`main` of the public repository, one registry per run. Bytes are built once, then only moved.

```bash
gh workflow run publish.yml -f registry=npm -f units="@rightkit/logs @rightkit/qa" -f dry_run=true
gh workflow run publish.yml -f registry=crates -f units="rightkit-fs rightkit-child-process" -f dry_run=false
```

`units` is a space-separated list of package or crate names. Do a `dry_run=true` run first; the real
run is the same dispatch with `dry_run=false`. **Nothing waits for approval**: the `release`
environment has no required reviewers, so a real run uploads as soon as its checks pass (see "What
protects a publication" below).

**Local publish (Mac, under the local-build grant).** Adrian's 2026-10-14 decision lets agents publish one
`@rightkit/*` package or one crate from the Mac, without `publish.yml`:
`right-release publish npm --package @rightkit/<name> --local` or
`right-release publish cargo --crate <name> --local`. `--local` refuses unless the committed
`.rightkit-local-build.json` (repository `Orthic-Labs/rightkit`, `allow: "build-test-publish"`) is present, the host is
macOS, the unit's directory is clean at a resolvable HEAD, and the version is not yet on the registry. It then runs the
unit's own CI commands from `ci/gate.config.json` (a unit in `ci/asset-gated.json` runs with
`RIGHTKIT_REQUIRE_ASSETS=0` and its skipped count is recorded; any other unit runs strict), packs the artifact
(`pnpm pack` / `cargo package --locked`), and writes the normal gate evidence to `release-evidence/gate/`: one darwin
receipt (operator `agent (local-build grant)`) with `hostPolicy: "darwin-only"` and the grant as the reason. The existing
`enforcePublicationGate` and the existing publisher (`publishGatedTarball`, or the `publish cargo` flow) then run on that
evidence. No test or evidence step can be skipped, and Windows is covered by the next CI run.

**Signed helper.** An npm run that names `@rightkit/release` first runs the `sign-swap` job (`xcode-27`, `environment: release`):
it builds `rightkit-swap` with no secret in scope, then installs the Developer ID identity from `APPLE_CERTIFICATE_BASE64`,
`APPLE_CERTIFICATE_PASSWORD`, `APPLE_KEYCHAIN_PASSWORD` and `vars.APPLE_DEVELOPER_ID`, signs and verifies it, and records its sha256.
`pack` stays secretless: it downloads the `rightkit-swap-signed` artifact, re-checks the hash and places it at
`packages/release/bin/darwin/rightkit-swap` before sealing. The `release` environment must hold those three secrets and variable.

Four optional inputs make a skip explicit instead of silent; each name must be a unit of the request:

- `allow_host_exclusion`: crates that `ci/gate.config.json` keeps off one verify host.
- `allow_no_test`: npm packages without a `test` script.
- `allow_ignored`: units that may have ignored tests (Rust `ignored` in a libtest summary). The count is recorded.
- `allow_missing_assets`: units whose tests may skip for a missing asset, sibling checkout or platform (a
  `SKIP` marker, a vitest or node:test skip). A unit listed in `ci/asset-gated.json` (tests that need models,
  FFmpeg, a desktop session or sibling product checkouts the workflow does not provision) is refused unless it
  is named here; naming any other unit is refused, and a named unit runs with `RIGHTKIT_REQUIRE_ASSETS=0`.
  Every unit not named runs strict (`RIGHTKIT_REQUIRE_ASSETS=1`), so a missing asset fails instead of skipping.
  A publish test fails when a crate or package reads `RIGHTKIT_REQUIRE_ASSETS` and is not listed.

A unit that a host would skip, a package with nothing to run, or an asset-gated unit fails the plan unless it is named.

| Job | Runs on | Proves | Secrets / id-token |
|---|---|---|---|
| `guard` | ubuntu | The ref is `refs/heads/main` and the repository is public | none |
| `plan` | ubuntu | The publish contract (`scripts/ci/workflow-contract.mjs`, parsed structure plus mutation proofs) and the publish unit tests pass; every name matches a strict pattern and exists in the workspace (crates from `cargo metadata`, so quoted keys, inherited and renamed dependencies are resolved by Cargo); none is private, `publish = false` or in `ci/known-red.json`; versions already on the registry are skipped; each workspace dependency is on the registry at a version meeting its requirement (Cargo semantics: caret default, `~`, `1.2.*` = `>=1.2.0, <1.3.0`, comparison operators, comma lists) or earlier in the request; order is dependencies first | none |
| `pack` | ubuntu (npm), `xcode-27` (crates) | Builds the artifacts once: `pnpm pack` tarballs, or `cargo package --locked` (which compiles and verifies each crate without any token). Seals them in `manifest.json` (name, version, size, sha256; npm also sha1 and integrity; crates also the upload metadata read from the packaged `Cargo.toml`) and uploads the artifact `publish-packed`. A packed `package.json` must name the unit, hold no `workspace:` specifier and carry the npm provenance `repository` | none |
| `verify` | `xcode-27` **and** `windows-2025` | Downloads `publish-packed` and re-hashes it. `gate.mjs --registry <r> --units "<names>"` builds and tests exactly the planned units and READS the runners' own results (`test result:` per libtest binary, node:test and vitest summaries, `SKIP` markers with or without a prefix, line-buffered so a marker split across stream chunks counts): a unit passes only with a positive executed count and no ignored or skipped test the allow inputs did not name; a unit with no test step, a host exclusion or missing test script not named, and a red unit all fail. Then `publish.mjs verify` checks the sealed bytes (npm: name, version, entry points exist in the tarball, provenance `repository`; crates: every file equals the commit's blob and `Cargo.toml.orig` equals the crate's own manifest) and **consumes them**: npm installs the sealed tarballs of the request together (`npm install --ignore-scripts`, dependencies from the registry) into a scratch project and proves every `main`/`module`/`types`/`bin`/`exports` target exists and each exports subpath resolves and each JS entry imports without throwing (a package that cannot import in plain Node declares `rightkit.publishCheck.skipImport` in `package.json`; it is still resolved), then **runs every declared `bin` from the installed copy** with the safe read-only command the package declares under `rightkit.publishCheck.binSmoke` (an argument array expecting exit 0, or `{ args, exitCode, output }` for a CLI whose only safe call prints its usage; an undeclared bin, a stray declaration or a wrong exit code fails the unit); crates extract each sealed `.crate`, require the packaged `Cargo.toml` to reproduce the sealed upload metadata, and run `cargo check` on the packaged crate (`--locked` when it ships a `Cargo.lock` and needs no sibling patch; unpublished siblings of the same request are patched in from their sealed crates; a host-excluded crate is not built there). It writes this host's receipt, bound to the manifest hash, run id and attempt, with the executed, ignored and skipped counts and the consumer report. | none |
| `seal-check` | ubuntu | `upload.mjs run --dry-run`: re-hashes the sealed bytes, requires both hosts' receipts to name the same manifest, commit and run, re-checks provenance preconditions, and builds the exact publish commands and request bodies without sending them. Runs for dry and real dispatches | none |
| `upload` | ubuntu, `environment: release`, only when `dry_run=false` | Fresh runner; checks out only `scripts/ci/upload.mjs`; installs and builds nothing; repeats the hash and receipt checks (the manifest's run id must be this run; a receipt's attempt may not predate the manifest's or exceed this attempt, and it must name the manifest run and attempt it verified); uploads the sealed bytes; compares the registry's bytes with them | `NPM_TOKEN` or `CARGO_REGISTRY_TOKEN`, and `id-token: write`: this job only |

- **npm.** `npm publish <sealed .tgz> --ignore-scripts --access public --provenance --registry
  https://registry.npmjs.org/`. Afterwards the registry's `dist.shasum` and `dist.integrity` must equal
  the sealed tarball's. Provenance needs each package's `repository` to name
  `https://github.com/Orthic-Labs/rightkit` with its own `directory`; the pack, verify and upload steps
  check that in the packed manifest, in dry and real runs alike.
- **Crates.** `cargo publish` cannot take a prebuilt `.crate`, and it would compile with the token in the
  environment, so `upload.mjs` PUTs the sealed `.crate` to `https://crates.io/api/v1/crates/new` with the
  token on that one request (`redirect: "error"`). The body is the registry web API's: a 32-bit
  little-endian length, the JSON metadata, a 32-bit little-endian length, the `.crate` bytes. The JSON
  comes from `cargo metadata` of the packaged manifest, and the request carries `Accept: application/json`.
  The answer is parsed: an `errors` array (crates.io can send it with HTTP 200) or a body that is not the
  documented JSON fails the run before anything is recorded as published, and `warnings` are logged and kept in the
  receipt. The `.crate` sha256 must then equal the `cksum` the sparse index serves. crates.io's publish rate limits apply to a large first publication; a refusal
  stops the run with its status.
- **Child processes.** Every child gets an allow-listed environment (runner basics, `GITHUB_*`, `RUNNER_*`,
  `ACTIONS_ID_TOKEN_*`), never `process.env`; only the `npm publish` child receives `NODE_AUTH_TOKEN`.
  `pack`, `verify`, `seal-check` and the dry run refuse to start if a registry token or an OIDC request
  token is present.
- **Dry run.** `pack`, `verify` and `seal-check` run in full. No step references a secret, and no job in
  the dry run requests `id-token`. `seal-check` uploads `publication-receipt-dry-run`.

### What protects a publication

Stated as configured, not as hoped:

- **No approval gate.** The `release` environment has no required reviewers. A run proceeds to `upload`
  automatically once `guard`, `plan`, `pack`, both `verify` hosts and `seal-check` are green.
- **Who can dispatch.** Anyone with write access to the repository can run `workflow_dispatch`, and the
  workflow runs from `main`'s workflow file only (`guard`). That is the whole gate; a collaborator with
  write access can publish.
- **Secrets.** The workflow reads both tokens in the `upload` job only. `NPM_TOKEN` is an environment secret
  of `release`, and the `release` environment's deployment branches are limited to `main`, so a workflow on
  another branch cannot read it. `CARGO_REGISTRY_TOKEN` is a **repository secret**: it is readable by any
  workflow job on a same-repository branch, with or without the environment, so a collaborator who can push a
  branch can write a workflow that reads it. This is an accepted residual risk until the token moves into
  `release`; the move needs no workflow change. These settings are stated from the owner's report; they cannot be
  verified from this repository.
- **Contract.** `scripts/ci/workflow-contract.mjs` runs in `plan` and asserts on the parsed workflow: the
  `upload` job and step conditions equal exact strings (an appended `|| true` is a different condition), and any
  use of the `secrets` context in any form (`secrets.X`, `secrets['X']`, `secrets["X"]`, `toJSON(secrets)`, a
  bare `secrets`, `secrets: inherit`, workflow, job or step `env`/`with`) outside the two token variables of
  the two upload steps is refused. The YAML subset parser rejects block scalars it does not model (folded `>`
  scalars, indentation indicators, `+` chomping), anchors, aliases, tags, tabs, multi-line flow collections and
  duplicate keys.
- **Chrome for Testing.** `@rightkit/qa` and `@rightkit/qa-chrome` run real Chrome. `verify` installs the version
  pinned in `ci/chrome-for-testing.json` through `scripts/ci/provision-chrome.mjs` (URL derived from the version under
  Google's bucket, sha256 checked, cached by the pin file's hash, no secret) and sets
  `RIGHTKIT_CHROME_FOR_TESTING_ROOT`. The workspace installer is not used because it downloads the moving Stable
  build and does not check a digest. To fill a pin without a local download, set `version` and leave a platform's
  `sha256` empty, then run a **dry run** that names a qa package: the step downloads to a temp file, prints
  `CHROME_PIN <platform> <version> <sha256> <bytes>` to the log and job summary, deletes the file and fails with
  "pin recorded above". An unpinned download is never installed or run, and a real publish refuses an empty
  sha256 (the contract ties the step's `PUBLISH_DRY_RUN` to the plan's `dry_run`).
  `ci/chrome-for-testing.json` also pins `headlessShell.win64` (`chrome-headless-shell-win64.zip` of the same version);
  `provisionHeadlessShell` unpacks it beside `chrome-win64` on Windows for the `rightkit-browser` fixture, which cannot
  run `chrome.exe` inside an AppContainer (its crashpad handler cannot create a named pipe there).
- **Chrome for Testing in ordinary CI.** The ci gate (`scripts/ci/gate.mjs`, `chromeGate`) calls the same module
  (`provisionForTests` in `scripts/ci/provision-chrome.mjs`) before any unit runs whenever the selection includes
  `packages/qa`, `packages/qa-chrome`, `rightkit-browser`, `rightkit-qa` or `rightkit-pdf`, on both legs, so Mac and
  Windows test the same pinned build instead of whatever the image carries. It installs under
  `$RUNNER_TOOL_CACHE/rightkit-chrome-for-testing` (else `$RUNNER_TEMP`) and exports `RIGHTKIT_CHROME_FOR_TESTING_ROOT`
  and `RIGHTKIT_CHROME_FOR_TESTING` to every later step. A directory whose `install.json` already records the pinned
  sha256 is reused with no download, so the download happens once per job (about 200 MB on Windows, less on Mac) and
  not at all on a persistent runner. The rendered `ci.yml` has no cache step for it and right-git offers no option
  to add an arbitrary cache path (`ciIncremental` caches only DerivedData), so hosted runners pay the download on
  every run that touches those units. A provisioning failure is a red `chrome:provision` unit. Publish verify is
  unchanged: it provisions in its own step and the gate does not repeat it in `--units` mode.
- **What the checks do stop.** A red or missing verification on either host, bytes that differ from what
  was verified, a dependency installed or built in the privileged job, a token reaching a build step, a
  dry run touching a credential, and a workflow edit that weakens any of this on `main` (the contract
  fails in `plan` and in `packages/release`'s contract test). They do not stop an authorized collaborator
  dispatching a legitimate-looking real run, or a malicious change to `main` that also changes the
  contract.

### Retrying failed jobs

A re-run of failed jobs keeps the run id, so `upload` downloads the earlier attempt's `publication-receipt`
(artifact pattern `publication-receipt`, into `sealed/previous`) and `upload.mjs` starts from it: the receipt must
be this run's, commit and registry, real (not dry), and from an earlier attempt, else the run is refused.
Before sending anything it asks the registry about EVERY planned unit: a unit whose exact version and sealed hash
are already served is recorded as `published-by-earlier-attempt` (with the attempt and outcome that preceded it) and
skipped; the same version with different bytes, a registry answer that cannot be read, or a unit the earlier
receipt called verified that the registry no longer serves, refuses the run before any publish. Rows replaced by a
later attempt move to `history`, so no earlier evidence is erased; each unit row carries `outcome`
(`pending`, `published-unverified`, `published`, `published-by-earlier-attempt`, `failed`, `refused`) and the
attempt that wrote it. An ambiguous failure (a network error after the request) stays `pending` or `failed` and the
next attempt settles it from the registry. A crates.io index that lags a publish from the earlier attempt can show
the crate as absent; the PUT is then refused by crates.io as already uploaded and a later retry reconciles it.

### Ledger

A real run uploads the artifact `publication-receipt` (`publication-receipt.json`): per unit the version,
registry, commit SHA, tarball or crate sha256, timestamp, and whether it was published and
byte-verified against the registry. After a real run:

```bash
gh run download <run-id> --name publication-receipt --dir /tmp/rk-receipt
node scripts/ci/publish.mjs ledger /tmp/rk-receipt/publication-receipt.json
```

`ledger` refuses a dry-run receipt, skips any unit that was not published and byte-verified (and fails when
none is), and records the rest through `recordPublished` into `packages/release/rightkit-versions.json` and
its consumer copies. It is strict: a failed write or copy refresh throws (`recordPublished(..., { strict: true })`;
every other caller keeps log-and-continue), and it reads the ledger back, exiting non-zero unless every
recorded version (or a later one) is present and the session-hook copy is byte-identical. Commit those files. `node packages/release/rightkit-sync.mjs` reaches the same state
from the registries when no receipt is at hand.
