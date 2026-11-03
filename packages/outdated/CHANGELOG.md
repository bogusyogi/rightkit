# Changelog

## 0.3.1 (unpublished)

rightkit#1267 (open items: undeclared manifests, native sources and toolchains).

- New, coverage scan (`coverageEntries`, run by `checkSources`): every `Cargo.toml`, `package.json`, `Package.swift`, `project.yml`, `rust-toolchain.toml` and Gradle build or settings file that no source declares becomes an `unknown` row of kind `coverage`. `check` exits 1 until the file is declared or held with a reason. Build output, `node_modules`, hidden directories and `vendor` are not entered.
- New source kind `native`: `{ name, pin, repo }` judges a native library or tool built from source at a pinned version against the highest release tag of `repo`.
- New source kind `toolchain`: `{ file }` judges `rust-toolchain.toml` (`channel`, against `rust-lang/rust` tags) or `package.json` (`packageManager`, against the npm registry). A non-pinned channel is `unknown`.
- `apply` never rewrites `coverage`, `native` or `toolchain` rows; they are listed as skipped.
- **Fixed: a crate pinned by several workspace members is rewritten in all of them at once (rightkit#1429).** `apply` grouped nothing: it rewrote one `Cargo.toml` per row and ran a lock update per row, so a sibling member's exact pin (`=0.3.5`) made the resolver refuse the new version and every row rolled back. Cargo rows are now grouped by crate and target version; every member's pin is rewritten (edits verified in memory first), then each affected `Cargo.lock` is updated once through `rustright cargo update -p <crate> --precise <version>`, all in one transaction. A failure restores every member and lockfile and fails the whole group. Each member row still reports its own manifest and the lockfile(s) it shares.
- **Fixed: `discard` clears a report whose files moved on (rightkit#1429).** It refused ("changed after apply") and the stale report then blocked every later `apply`. Files that still hold the digest `apply` wrote are restored from HEAD; files that no longer match (edited, or the apply was committed outside right-update) are left untouched and listed under `moved`; the apply-report, gate-report and staged candidates are cleared either way. `DiscardOutcome` gains `moved` and `notes`.
- **Fixed: a gate step must go through `rightkit-gate` (rightkit#1429).** `gate` (and so `run`) spawned any manifest gate command, including `cargo`, `pnpm` or `swift` build commands outside the gate. A gate whose command is not `rightkit-gate ...` or `node <path>/rightkit-gate.mjs ...` now fails without being spawned, naming the command (`gateRouteProblem`, exported). Manifests whose gates run tools directly must move them behind `rightkit-gate`.
- **Fixed: `apply` regenerates every lockfile a pin change touches, in the same transaction (rightkit#1340).** Cargo (`Cargo.lock`) and pnpm (`pnpm-lock.yaml`) already were; SwiftPM now runs `swift package resolve` beside the `Package.swift` and the regenerated `Package.resolved` is part of the row's files (before, the row only noted that it was not refreshed). A `project.yml`-only project with no `Package.swift` beside it still notes that no resolved file was refreshed. The manifest has no Gradle or Python source kind, so there is no pin change that could touch a `gradle.lockfile` or a Python lock yet. Written from code, not compiled or run.
- Not run here: no compile or type check was run in this pass (gate pending).

## 0.3.0

rightkit#1267 (Adrian, 2026-10-29: "document it as an issue, a decision and fix and publish asap").

- **Behaviour change:** `check` never reports a row it could not judge as current. An `unknown` row (a path or git dependency, a
  `workspace:`/`file:` spec, a branch or revision pin, a non-comparable version, no upstream version found) makes `check` exit 1,
  unless its name has a justified hold. Before, every unknown row fell through to exit 0, "all current".
- `policy.held` entries may be `{ name, reason, evidence, exitCondition }` (each a non-empty string). Only such a justified hold
  excuses an unknown row. A bare name (the 0.2 form) still holds the dependency but excuses nothing.
- `apply` on Windows: a pnpm range prefix (`^`, `~`) is passed as `pnpm_config_save_prefix` in the environment, never as
  `--save-prefix=^` on a `.cmd` shim's command line, which cmd.exe refused for every caret range (HeardRight report).
- `apply` runs cargo updates through `rustright cargo`; the 0.2.0 build still spawned the retired `rightkit`.
- Renamed from @rightkit/update; it was never published under the old name.
- Still open under rightkit#1267: the undeclared-manifest coverage scan, and native-source and toolchain kinds.

## 0.2.0

- `right-update apply [--repo] [--only <name>...] [--dry-run] [--json]`: rewrites pins for outdated, non-held rows and writes `.rightkit/update/apply-report.json`. Cargo versions are edited in place (comments, tables, features and formatting preserved; the result is re-parsed and compared) and the lockfile is refreshed with `rightkit cargo update -p <crate> --precise <ver>`. pnpm uses `pnpm add` with `-D`/`-O`/`-w`, a kept range style and `--ignore-scripts`. SwiftPM rewrites `Package.swift` and `project.yml`. Tools are downloaded for the host platform to `.rightkit/update/candidates/<name>/<version>/` with a size bound and an observed sha256. Each row is a transaction and is rolled back on failure. Breaking bumps (major, 0.x minor, 0.0.x patch) and `agent-pr` rows are reported as `needsMigration`, held rows as `held`.
- `right-update gate`: runs the manifest `gates` and each changed tool's journey with `{candidate}`, `{out}`, `{delivery}` and `{model}` filled in. Argv only, a timeout on every command, stdout and stderr saved. Exit 0 passed, 1 failed, 3 a journey report is missing or malformed. Writes `gate-report.json`, bound to the apply-report.
- `right-update land`: refuses unless the gate passed for this apply, the tree has no changes outside the touched files, the branch is the default branch and HEAD is unchanged. Delivers tool artifacts into the RightKit artifact directory, updates `pin` and checksums, commits `deps: <n> updates via right-update` with a `Co-Authored-By: right-update` trailer, and pushes only when `policy.land` is `"push"`. Writes `held.json`.
- `right-update discard`, `right-update run` (the check, apply, gate, land pipeline, with cleanup) and `right-update schedule install|remove|status` (launchd on macOS, Task Scheduler on Windows). `--quiet` emits JSON lifecycle events: `update.run.start`, `update.run.end`, `update.gate.result`, `update.land.result`.
- Manifest: top-level `gates`, `policy.land`, `glob` on `cargo` and `pnpm` sources, `journey.model`, `journey.timeoutSec`, tool asset `maxBytes`, and an `{asset}` placeholder in `url` download templates.
- `check`: registry answers are shared across manifests, manifests in a glob are checked four at a time, crates.io documents may be up to 16 MiB (the `windows` crate exceeds 1 MiB), HTTP 429 and transient 5xx responses are retried, and a path dependency that also carries a `version` is `unknown` instead of a registry lookup.
- `.rightkit/update.json` for the RightKit repository itself (cargo and pnpm globs, `land: "commit"`, held security-sensitive names and crates, one gate).

## 0.1.0

- New package `@rightkit/update` with the `right-update` CLI.
- `right-update check [--repo <path>] [--json]` compares every pinned dependency with its upstream latest for cargo (crates.io), pnpm (`pnpm outdated --json`, npm registry fallback), swiftpm (GitHub tags), rightkit (registry section of the versions file, plus workflow drift through `@rightkit/git`), and external tools (GitHub release or version URL, with per-platform asset checks).
- `.rightkit/update.json` manifest with a JSON Schema (`update.schema.json`) and a hand validator. Tool entries carry per-platform assets, checksums, optional archive `extract` member, and an object-shaped `journey`.
- Policy resolution: explicit entry policy, `policy.held` names always held, tier patterns (default `/sign|secur|licen|credential/`) held, then `policy.default`.
- Exit codes: 0 all current, 1 any outdated, 2 error.
- `apply`, `gate` and `land` are stubs that exit 2 with "not implemented".
