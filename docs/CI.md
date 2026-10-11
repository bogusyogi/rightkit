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
