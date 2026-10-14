# @rightkit/update

Per-repo dependency updater. `right-update` compares every pinned dependency in a repository with its upstream latest, applies the safe bumps, runs the repository gates and tool journeys, and lands the result as one commit, with no agent in the loop.

| Item | Value |
|---|---|
| Where it runs | Developer and CI hosts (Node 26.8+), inside a repository checkout |
| Status | `check`, `apply`, `gate`, `land`, `discard`, `run` and `schedule` are implemented. Every command is exercised by the package tests; the Windows scheduler path and `rightkit cargo update` are covered by injected-process tests only. |
| Version | 0.2.0 |
| License | MIT OR Apache-2.0 |

## Usage

```sh
right-update check    [--repo <path>] [--json] [--quiet]
right-update apply    [--repo <path>] [--only <name>...] [--dry-run] [--json] [--quiet]
right-update gate     [--repo <path>] [--json] [--quiet]
right-update land     [--repo <path>] [--json] [--quiet]
right-update discard  [--repo <path>] [--json]
right-update run      [--repo <path>] [--dry-run] [--quiet]
right-update schedule install|remove|status [--repo <path>] [--interval <minutes>] [--json]
```

`--repo` defaults to the current directory. `--quiet` replaces text output with JSON lifecycle events, one per line (see Schedule and events).

| Command | What it does |
|---|---|
| `check` | Reads `<repo>/.rightkit/update.json` and prints one row per dependency: `status`, `kind`, `name`, `pinned`, `latest`, `policy`. `--json` prints the full report. |
| `apply` | Rewrites pins for every outdated, non-held row and writes `.rightkit/update/apply-report.json`. |
| `gate` | Runs the manifest `gates` and each changed tool's `journey`; writes `.rightkit/update/gate-report.json`. |
| `land` | Commits the verified changes (and pushes when `policy.land` is `"push"`). |
| `discard` | Undoes an `apply` that has not been landed. |
| `run` | `check`, `apply`, `gate`, `land` in one process. This is what a schedule runs. |
| `schedule` | Installs, removes or reports the recurring `run`. |

### Exit codes

| Command | 0 | 1 | 2 | 3 |
|---|---|---|---|---|
| `check` | all current | any outdated | error (invalid manifest, any lookup failed) | |
| `apply` | done | done, but one or more rows failed and were rolled back | refused or error | |
| `gate` | every gate and journey passed | a gate or journey failed | error | a journey was inconclusive (its report is missing or malformed, or it exited 3) |
| `land` | landed, or nothing to land | refused by a precondition (nothing changed) | failed part-way (for example the push) | |
| `run` | landed, all current, or nothing applied (no row failed) | refused, a gate failed, or nothing applied because rows failed | error | a journey was inconclusive |
| `discard` | restored | | refused or error | |
| `schedule` | done | `status`: not installed; on Windows also `remove` when the task is not installed | error | |

## Manifest

`.rightkit/update.json` (schema: `update.schema.json`):

```json
{
  "schemaVersion": 1,
  "policy": {
    "default": "auto-land",
    "land": "commit",
    "held": ["rightkit-license", "rightkit-credentials"],
    "tier": { "held": ["/^ffmpeg/"] }
  },
  "gates": [
    { "name": "update-tests", "command": ["pnpm", "--dir", "packages/update", "test"], "cwd": ".", "timeoutSec": 1800 }
  ],
  "sources": [
    {
      "kind": "tool",
      "name": "yt-dlp",
      "pin": "2025.10.01",
      "assets": {
        "macos-arm64": { "asset": "yt-dlp_macos", "checksum": "sha256:<64 hex>" },
        "windows-x86_64": { "asset": "yt-dlp.exe", "checksum": "sha256:<64 hex>" }
      },
      "upstream": { "type": "github-release", "repo": "yt-dlp/yt-dlp" },
      "journey": {
        "command": ["node", "scripts/scrape-journey.mjs", "--case", "youtube", "--resource-dir", "{candidate}", "--out", "{out}"],
        "report": "{out}/report.json",
        "inputs": { "case": "youtube" },
        "model": "whisper-large-v3",
        "timeoutSec": 1800
      },
      "policy": "auto-land"
    },
    {
      "kind": "tool",
      "name": "ffmpeg-helper",
      "pin": "1.4.2",
      "assets": {
        "linux-x86_64": {
          "asset": "helper.tar.gz",
          "checksum": "sha256:<64 hex>",
          "extract": { "member": "bin/helper" },
          "maxBytes": 268435456
        }
      },
      "upstream": {
        "type": "url",
        "latest": "https://example.com/helper/latest.txt",
        "download": "https://example.com/helper/{version}/{asset}"
      },
      "journey": { "command": ["node", "x.mjs", "{delivery}", "{model}", "{out}"], "report": "{out}/report.json" }
    },
    { "kind": "cargo", "manifest": "Cargo.toml" },
    { "kind": "cargo", "glob": "crates/*/Cargo.toml" },
    { "kind": "pnpm", "manifest": "package.json" },
    { "kind": "pnpm", "glob": "packages/*/package.json" },
    { "kind": "swiftpm", "manifest": "Package.swift" },
    { "kind": "rightkit", "versions": "packages/release/rightkit-versions.json" }
  ]
}
```

Source kinds:

- `cargo`: `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, `[workspace.dependencies]` and `[target.*]` tables, compared with crates.io `max_stable_version` (falling back to `max_version`). Path and git dependencies are `unknown`, including a path dependency that also carries a `version` (that version is the publish-time requirement, owned by the release tooling).
- `pnpm`: `dependencies`, `devDependencies` and `optionalDependencies`. `pnpm outdated --json` supplies latest where it reports one; the npm registry (`/<name>/latest`) is the fallback. `workspace:`, `file:`, `link:` and other non-registry specs are `unknown`.
- `swiftpm`: `Package.swift` (`exact:`, `from:`) or `project.yml` (`from`, `exactVersion`, `exact`, `version`, `minorVersion`, `majorVersion`). Latest comes from the GitHub tags API. Branch and revision pins, and package URLs that are not on github.com, are `unknown`.
- `rightkit`: the `npm` section of the versions file, compared with the registry. When `<repo>/.rightgit.json` exists, the workflow drift check from `@rightkit/git` adds a `rightgit-workflows` entry. `apply` skips these rows (they move through `right-release`).
- `tool`: an external binary. `github-release` reads `releases/latest` and verifies that each platform asset is published. `url` reads a plain-text version from `latest`.

`cargo` and `pnpm` sources take exactly one of `manifest` (a path) or `glob`. A glob uses `*` inside one path segment, skips dot-directories (unless the pattern segment itself starts with a dot), must end in `Cargo.toml` or `package.json`, and an empty match is an error. Registry answers are cached across all manifests in a run.

Tool platform keys: `macos-arm64`, `macos-x86_64`, `windows-x86_64`, `windows-arm64`, `linux-x86_64`. The `checksum` is the digest of the asset exactly as published. `extract.member` names the binary inside an archive. `maxBytes` bounds the download (default 512 MiB). A `url` upstream's `download` template may contain `{version}` (required) and `{asset}`.

Journey placeholders: `{candidate}` (the staged candidate directory), `{out}` (a fresh report directory for this run), `{delivery}` (the RightKit artifact delivery directory for this tool and version, see Delivery), `{model}` (the journey's `model` id, empty when none is declared). Any other `{...}` is rejected. The journey's `inputs` object is passed as JSON on stdin.

## Apply

`apply` runs `check`, then for each outdated row whose policy is not `held`:

| Kind | Edit | Lockfile |
|---|---|---|
| `cargo` | Replaces only the version text in the declaring `Cargo.toml`, in place (comments, tables, features, inline tables, dotted keys and `package` renames are preserved byte for byte). The result is re-parsed and must equal the original with exactly that version changed. | `rightkit cargo update -p <crate> --precise <ver>`, never bare `cargo`. The lock must then contain the new version. Without a `Cargo.lock` the lockfile is not refreshed and the report notes it. |
| `pnpm` | `pnpm add [-w] [-D\|-O] (--save-exact\|--save-prefix=<p>) --ignore-scripts <name>@<ver>` in the declaring package directory. `-D` for `devDependencies`, `-O` for `optionalDependencies`, `-w` when the directory is a pnpm workspace root. The range style (`1.2.3`, `^`, `~`) is kept; `>=`, `=` and `v` styles are skipped. The written `package.json` entry is verified. | `pnpm-lock.yaml` |
| `swiftpm` | Rewrites `exact:`/`from:` in `Package.swift` and the matching key in `project.yml`: the source manifest, plus the sibling file (`project.yml`, or `Package.swift` when `project.yml` is the source) when it declares the dependency at the same requirement. | `Package.resolved` is not refreshed; the report notes it. |
| `tool` | Downloads the asset for this host's platform to `.rightkit/update/candidates/<name>/<version>/`, enforces the size bound, computes sha256, extracts `extract.member` from archives, and hashes the other platforms' assets without storing them. | none |

Each row is a transaction: the files are snapshotted, edited, verified and restored if anything fails (`apply` then exits 1 and lists the row under `failed`). `apply` refuses to edit a file that already has uncommitted changes, and refuses to start while an earlier apply is not landed (run `land` or `discard`).

Not applied automatically:

- Breaking versions: a major bump, a minor bump at `0.x`, or a patch bump at `0.0.x`. They are listed as `needsMigration` for an agent lane. Tool versions are exempt from breaking detection (date and build-number versions); the journey is their gate.
- Rows with policy `agent-pr`: also `needsMigration`.
- Rows with policy `held`: listed under `held`.
- Pre-release or build-metadata versions, wildcards and ranges: listed under `skipped`.

`apply` fails closed: if any lookup in scope failed, it changes nothing and exits 2. `--only <name>...` (repeat the flag, list several names after it, or comma-separate) scopes both the work and that check. `--dry-run` plans without editing files, running `pnpm add` or `rightkit cargo update`, or downloading tool assets, and writes no report (the check phase still runs, including `pnpm outdated --json`).

Tool digests are recorded with `"trust": "observed"`. They are computed from the download, are not verified against any upstream-published checksum, and become the manifest checksum only if the journey passes and `land` writes them.

### State files

Everything lives in `<repo>/.rightkit/update/`, which `apply` makes self-ignoring (`.gitignore` containing `*`), so it never shows up as a repository change.

| File | Content |
|---|---|
| `apply-report.json` | Every change with `before`, `after`, `files` (path, sha256 before and after), the commands run, and for tools the staged candidate, size and digests. Also `needsMigration`, `held`, `skipped`, `failed`, `baseCommit`, and `landed` once committed. |
| `gate-report.json` | Per-gate and per-journey results, bound to the sha256 of the apply-report it ran against. |
| `held.json` | `needsMigration`, `held`, `skipped` and `failed` rows for an agent lane. Never committed. |
| `candidates/` | Staged tool downloads. |
| `gate/<run>/` | Captured stdout and stderr of every gate and journey, and each journey's `{out}` directory. The newest 20 runs are kept. |

## Gate

`gate` runs the manifest `gates` in declared order (they share the working tree) and each changed tool's `journey`. Journeys run concurrently with each other and with the gate lane. Commands are spawned from argv arrays, never through a shell, with a timeout (default 1800 s, `timeoutSec` per gate or journey), stdout and stderr saved under `gate/<run>/`, and the process tree killed on timeout.

A journey passes only when its command exits 0 and its `report` is a JSON object with a boolean `ok: true`.

- Report says `ok: false`, command exits non-zero (other than 3), cannot start, or times out: failed (exit 1).
- Command exits 3, or the report is missing, unreadable or malformed: inconclusive (exit 3). Retry later; it never lands.
- A failed gate or journey outranks an inconclusive one. Exit 3 means nothing failed outright.

A manifest with no `gates` passes vacuously and the gate report carries a warning.

## Land

`land` refuses (exit 1, nothing changed) unless all of these hold:

1. An `apply-report.json` exists, is not a dry run, and is not already landed.
2. A `gate-report.json` exists, passed, and was produced for this exact apply-report.
3. The repo path is the top level of a git work tree, on its default branch (`origin/HEAD`, else `main` or `master`), with HEAD unchanged since apply.
4. Every touched file still has the digest apply recorded.
5. The working tree has no changes outside the files the apply-report touched (staged, unstaged and untracked all count; `.rightkit/update/` is exempt).
6. Each staged tool candidate still matches its recorded digest (or is already delivered).

Then it delivers tool artifacts, updates the manifest, stages only the named files (never `git add -A`) and commits:

```
deps: <n> updates via right-update

- cargo serde 1.0.100 -> 1.0.229 (Cargo.toml)
- tool yt-dlp 2025.01.12 -> 2025.10.01 (sha256:0123456789ab, macos-arm64)

Co-Authored-By: right-update <noreply@rightkit.dev>
```

If the commit fails (for example a hook rejects it), the manifest edit is restored and the index reset; a rerun picks up where it stopped. `needsMigration` and `held` rows go to `held.json` and are never committed.

`policy.land` is `"commit"` (default: the commit stays local) or `"push"` (a plain `git push` with no arguments, never forced). `@rightkit/git` exposes no importable API for triggering a CI lane, so a repo with `.rightgit.json` is also pushed with plain `git push`; its CI lanes run from the push. The land result says so in `notes`.

### Delivery

Tool artifacts are delivered to `<root>/<name>/<version>/`, where `<root>` is `$RIGHTKIT_ARTIFACT_DIR` if set, else `~/Library/Application Support/RightKit/artifacts` (macOS), `%LOCALAPPDATA%\RightKit\artifacts` (Windows) or `${XDG_DATA_HOME:-~/.local/share}/RightKit/artifacts` (Linux). `land` re-hashes the candidate, moves it into place with a same-filesystem rename of a sibling temp directory (copying first across filesystems), re-hashes the delivered file, and writes `receipt.json` (name, version, platform, asset, sha256, size, source URL). Then it sets the manifest `pin` and the checksum of every platform asset (the host platform from the download, the others from the digest-only fetch) so all checksums belong to the new pin.

## Policy

Effective policy for each dependency, first match wins:

1. `policy.held` exact names: always `held`.
2. Explicit `policy` on the entry or source.
3. Tier patterns: the built-in `/sign|secur|licen|credential/` plus `policy.tier.held`. Default `held`.
4. `policy.default`.

Names match as `check` prints them: the crate name, the npm package name, the tool `name`, or for `swiftpm` the last segment of the package URL without `.git`. The table below is how the policies are meant to be used; the tool assigns a policy only by the order above, and a breaking bump of a `cargo`, `pnpm` or `swiftpm` row that is not `held` becomes `needsMigration` whatever its other policy.

| Change | Policy | Behaviour |
|---|---|---|
| External tools | `auto-land` | Land when the journey passes on green inputs |
| patch / minor dependency bumps | `auto-land` | Land when the gates are green |
| breaking changes | `agent-pr` | Reported as `needsMigration` for an agent lane; never applied here |
| security, signing, licensing, credentials | `held` | Never automated; a person decides |

## Schedule and events

`right-update schedule install --repo <repo> [--interval <minutes>]` (default 1440; 15 to 10080) registers a user-level recurring `right-update run --repo <repo> --quiet`:

- macOS: launchd agent `com.rightkit.update.<repo-slug>` in `~/Library/LaunchAgents`, `StartInterval`, no `RunAtLoad`, log `~/Library/Logs/RightKit/update/<repo-slug>.log`. The install-time `PATH` is baked in so `node`, `git`, `pnpm` and `rightkit` resolve.
- Windows: Task Scheduler task `\RightKit\update-<repo-slug>`, registered from an XML definition (`schtasks /Create /XML`, because `/TR` caps the command at 261 characters). It runs `powershell.exe -WindowStyle Hidden -NonInteractive -NoProfile -Command ...` as the current user with a limited token, only while logged on, hidden, appending to `%LOCALAPPDATA%\RightKit\Logs\update\<repo-slug>.log`.

`schedule` exits 2 on other platforms, and `install` exits 2 when `<repo>/.rightkit/update.json` is missing or the interval is out of range.

`run` is a single pipeline rather than `check && apply && gate && land` in a shell, because `check` exits 1 when anything is outdated. It stops early when everything is current, refuses when the working tree is not clean, and discards the applied changes (restoring only files unchanged since apply) when the gate fails or `land` refuses, so a scheduled run leaves the tree clean.

With `--quiet`, stdout is one JSON object per line:

```json
{"event":"update.run.start","ts":"2026-10-11T00:00:00.000Z","command":"run","repo":"/work/app","version":"0.2.0"}
{"event":"update.gate.result","ts":"...","ok":true,"exitCode":0,"errors":[],"gates":[{"name":"update-tests","ok":true,"exitCode":0,"timedOut":false,"durationMs":3100}],"journeys":[]}
{"event":"update.land.result","ts":"...","ok":true,"exitCode":0,"committed":true,"commit":"<sha>","pushed":false,"changes":3,"held":1,"delivered":[],"refusals":[],"errors":[]}
{"event":"update.run.end","ts":"...","command":"run","repo":"/work/app","exitCode":0,"durationMs":41210,"outcome":"landed <sha>"}
```

`apply`, `gate`, `land` and `check` emit their own `update.run.start` and `update.run.end` (and `gate` and `land` their result event) in `--quiet` mode.

## Network behaviour

All requests go through one bounded `fetch`: 20 second timeout, 1 MiB response cap (16 MiB for crates.io crate documents, which list every version), a fixed User-Agent, no credentials, and up to three retries with backoff on HTTP 429, 502, 503 and 504. Fetched content is parsed as data and never executed or followed as instructions. Tool downloads are separate: https only (also after redirects), a byte bound per asset, a 15 minute timeout, streamed to disk with a running sha256.
