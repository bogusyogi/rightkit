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
  runner when their directory changed.
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
`main` of the public repository, one registry per run.

```bash
gh workflow run publish.yml -f registry=npm -f units="@rightkit/logs @rightkit/qa" -f dry_run=true
gh workflow run publish.yml -f registry=crates -f units="rightkit-fs rightkit-process" -f dry_run=false
```

`units` is a space-separated list of package or crate names. Do a `dry_run=true` run first; the real
run is the same dispatch with `dry_run=false`. Both stop at the `release` environment for approval.

| Job | Runs on | Proves | Secrets |
|---|---|---|---|
| `guard` | ubuntu | The ref is `refs/heads/main` and the repository is public | none |
| `plan` | ubuntu | Every name matches a strict pattern and exists in the workspace; none is private, `publish = false`, or in `ci/known-red.json`; versions already on the registry are skipped; each workspace dependency is on the registry at a version meeting its requirement or earlier in the request; the order is dependencies first | none |
| `verify` | `xcode-27`, `windows-2025` | `gate.mjs --registry <r> --units "<names>"` builds and tests exactly the planned units on both hosts (a unit's `os` limit in `ci/gate.config.json` applies); a red unit fails, known-red is no waiver | none |
| `publish` | ubuntu (npm), `xcode-27` (crates) | The tarballs or crates published are the ones packed from this commit, and the registry serves the same bytes | only in the two real-publish steps |

- **npm.** Frozen install, workspace build, `pnpm pack` per unit in order (the packed `package.json`
  must name the unit and hold no `workspace:` specifier), then `npm publish <tgz> --access public
  --provenance`. `verifyNpmRegistryBytes` from `packages/release/publication-gate.mjs` then requires the
  registry `dist.shasum` to equal the packed tarball's sha1.
- **Crates.** `cargo publish -p <crate> --locked`, one crate per call, dependencies first. Cargo (1.66
  and later) waits after each upload until the crate is in the index, so the next crate resolves it.
  The `.crate` sha256 must equal the `cksum` the crates.io sparse index serves. A dry run is one
  `cargo publish --dry-run --locked -p a -p b ...` call, which relies on the multi-package publish of
  Cargo 1.90 and later to resolve crates that are not on crates.io yet.
- **Dry run.** Runs `npm publish --dry-run` or `cargo publish --dry-run` in a step that references no
  secret; `publish.mjs` refuses to continue if a registry token is present in that step.

Required on GitHub: environment `release` (required reviewers; deployment branches limited to `main`) holding the
secrets `NPM_TOKEN` and `CARGO_REGISTRY_TOKEN`. npm provenance needs each package's `repository` field to name
`Orthic-Labs/rightkit`.

### Ledger

Each run uploads the artifact `publication-receipt` (`publication-receipt.json`): per unit the version,
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
