# Changelog

## Unreleased

- Renamed from @rightkit/update; it was never published under the old name.

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
