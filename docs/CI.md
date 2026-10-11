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
gh workflow run publish.yml -f registry=crates -f units="rightkit-fs rightkit-process" -f dry_run=false
```

`units` is a space-separated list of package or crate names. Do a `dry_run=true` run first; the real
run is the same dispatch with `dry_run=false`. **Nothing waits for approval**: the `release`
environment has no required reviewers, so a real run uploads as soon as its checks pass (see "What
protects a publication" below).

Two optional inputs make a skip explicit instead of silent: `allow_host_exclusion` names crates that
`ci/gate.config.json` keeps off one verify host, and `allow_no_test` names npm packages without a `test`
script. A unit that a host would skip, or a package with nothing to run, fails the plan unless it is named.

| Job | Runs on | Proves | Secrets / id-token |
|---|---|---|---|
| `guard` | ubuntu | The ref is `refs/heads/main` and the repository is public | none |
| `plan` | ubuntu | The publish contract (`scripts/ci/workflow-contract.mjs`, parsed structure plus mutation proofs) and the publish unit tests pass; every name matches a strict pattern and exists in the workspace (crates from `cargo metadata`, so quoted keys, inherited and renamed dependencies are resolved by Cargo); none is private, `publish = false` or in `ci/known-red.json`; versions already on the registry are skipped; each workspace dependency is on the registry at a version meeting its requirement (Cargo semantics: caret default, `~`, `1.2.*` = `>=1.2.0, <1.3.0`, comparison operators, comma lists) or earlier in the request; order is dependencies first | none |
| `pack` | ubuntu (npm), `xcode-27` (crates) | Builds the artifacts once: `pnpm pack` tarballs, or `cargo package --locked` (which compiles and verifies each crate without any token). Seals them in `manifest.json` (name, version, size, sha256; npm also sha1 and integrity; crates also the upload metadata read from the packaged `Cargo.toml`) and uploads the artifact `publish-packed`. A packed `package.json` must name the unit, hold no `workspace:` specifier and carry the npm provenance `repository` | none |
| `verify` | `xcode-27` **and** `windows-2025` | Downloads `publish-packed` and re-hashes it. `gate.mjs --registry <r> --units "<names>"` builds and tests exactly the planned units; every unit must end `pass`: a skipped test, a unit with no test step, a host exclusion or a missing test script not named in the allow inputs, and a red unit all fail. Then `publish.mjs verify` checks the sealed bytes (npm: name, version, entry points exist in the tarball, provenance `repository`; crates: every file equals the commit's blob and `Cargo.toml.orig` equals the crate's own manifest) and writes this host's receipt, bound to the manifest hash. | none |
| `seal-check` | ubuntu | `upload.mjs run --dry-run`: re-hashes the sealed bytes, requires both hosts' receipts to name the same manifest, commit and run, re-checks provenance preconditions, and builds the exact publish commands and request bodies without sending them. Runs for dry and real dispatches | none |
| `upload` | ubuntu, `environment: release`, only when `dry_run=false` | Fresh runner; checks out only `scripts/ci/upload.mjs`; installs and builds nothing; repeats the hash and receipt checks; uploads the sealed bytes; compares the registry's bytes with them | `NPM_TOKEN` or `CARGO_REGISTRY_TOKEN`, and `id-token: write`: this job only |

- **npm.** `npm publish <sealed .tgz> --ignore-scripts --access public --provenance --registry
  https://registry.npmjs.org/`. Afterwards the registry's `dist.shasum` and `dist.integrity` must equal
  the sealed tarball's. Provenance needs each package's `repository` to name
  `https://github.com/Orthic-Labs/rightkit` with its own `directory`; the pack, verify and upload steps
  check that in the packed manifest, in dry and real runs alike.
- **Crates.** `cargo publish` cannot take a prebuilt `.crate`, and it would compile with the token in the
  environment, so `upload.mjs` PUTs the sealed `.crate` to `https://crates.io/api/v1/crates/new` with the
  token on that one request (`redirect: "error"`). The body is the registry web API's: a 32-bit
  little-endian length, the JSON metadata, a 32-bit little-endian length, the `.crate` bytes. The JSON
  comes from `cargo metadata` of the packaged manifest. The `.crate` sha256 must then equal the `cksum`
  the sparse index serves. crates.io's publish rate limits apply to a large first publication; a refusal
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
- **Secrets.** The workflow reads `NPM_TOKEN` and `CARGO_REGISTRY_TOKEN` as secrets of `release`, in the
  `upload` job only. The owner is moving `NPM_TOKEN` into `release` (confirm it is gone from repository secrets). **`CARGO_REGISTRY_TOKEN` is still a
  repository secret**, readable by any workflow job on a same-repository branch, with or without the
  environment, until it is moved into `release`. Until then a collaborator who can push a branch can write
  a workflow that reads it. Moving it needs no workflow change. Limit `release` deployment branches to
  `main` in the environment settings (not verifiable from this repository).
- **What the checks do stop.** A red or missing verification on either host, bytes that differ from what
  was verified, a dependency installed or built in the privileged job, a token reaching a build step, a
  dry run touching a credential, and a workflow edit that weakens any of this on `main` (the contract
  fails in `plan` and in `packages/release`'s contract test). They do not stop an authorized collaborator
  dispatching a legitimate-looking real run, or a malicious change to `main` that also changes the
  contract.

### Ledger

A real run uploads the artifact `publication-receipt` (`publication-receipt.json`): per unit the version,
registry, commit SHA, tarball or crate sha256, timestamp, and whether it was published and
byte-verified against the registry. After a real run:

```bash
gh run download <run-id> --name publication-receipt --dir /tmp/rk-receipt
node scripts/ci/publish.mjs ledger /tmp/rk-receipt/publication-receipt.json
```

`ledger` refuses a dry-run receipt, skips any unit that was not published and byte-verified, and
records the rest through `recordPublished` into `packages/release/rightkit-versions.json` and its
consumer copies. Commit those files. `node packages/release/rightkit-sync.mjs` reaches the same state
from the registries when no receipt is at hand.
