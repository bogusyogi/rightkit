# @rightkit/update

Per-repo dependency updater. `right-update` compares every pinned dependency in a repository with its upstream latest, with no agent in the loop.

| Item | Value |
|---|---|
| Where it runs | Developer and CI hosts (Node 26.8+), inside a repository checkout |
| Status | `check` is implemented. `apply`, `gate` and `land` are not implemented yet and exit 2. |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Usage

```sh
right-update check [--repo <path>] [--json]
```

`check` reads `<repo>/.rightkit/update.json` and prints one row per dependency: `status`, `kind`, `name`, `pinned`, `latest`, `policy`. `--json` prints the full report.

Exit codes: `0` all current, `1` any outdated, `2` error (invalid manifest, or any lookup failed).

## Manifest

`.rightkit/update.json` (schema: `update.schema.json`):

```json
{
  "schemaVersion": 1,
  "policy": {
    "default": "auto-land",
    "held": ["rightkit-license", "rightkit-secrets", "RightKitSwift:RightKitLicense"],
    "tier": { "held": ["/^ffmpeg/"] }
  },
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
        "inputs": { "case": "youtube" }
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
          "extract": { "member": "bin/helper" }
        }
      },
      "upstream": {
        "type": "url",
        "latest": "https://example.com/helper/latest.txt",
        "download": "https://example.com/helper/{version}/helper.tar.gz"
      },
      "journey": { "command": ["node", "x.mjs", "{delivery}", "{model}", "{out}"], "report": "{out}/report.json" }
    },
    { "kind": "cargo", "manifest": "Cargo.toml" },
    { "kind": "pnpm", "manifest": "package.json" },
    { "kind": "swiftpm", "manifest": "Package.swift" },
    { "kind": "rightkit", "versions": "packages/release/rightkit-versions.json" }
  ]
}
```

Source kinds:

- `cargo`: `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, `[workspace.dependencies]` and `[target.*]` tables, compared with crates.io `max_stable_version`. Path and git dependencies are `unknown`.
- `pnpm`: `dependencies`, `devDependencies` and `optionalDependencies`. `pnpm outdated --json` supplies latest where it reports one; the npm registry (`/<name>/latest`) is the fallback. `workspace:`, `file:`, `link:` and other non-registry specs are `unknown`.
- `swiftpm`: `Package.swift` (`exact:`, `from:`) or `project.yml` (`from`, `exactVersion`, `version`). Latest comes from the GitHub tags API. Branch and revision pins are `unknown`.
- `rightkit`: the `npm` section of the versions file, compared with the registry. When `<repo>/.rightgit.json` exists, the workflow drift check from `@rightkit/git` adds a `rightgit-workflows` entry.
- `tool`: an external binary. `github-release` reads `releases/latest` and verifies that each platform asset is published. `url` reads a plain-text version from `latest`.

Tool platform keys: `macos-arm64`, `macos-x86_64`, `windows-x86_64`, `windows-arm64`, `linux-x86_64`. The `checksum` is the digest of the asset exactly as published. `extract.member` names the binary inside an archive; `land` records the installed binary's identity later.

Journey placeholders: `{candidate}` (the candidate build), `{out}` (output directory), `{delivery}` (installed delivery receipt, read from the app's delivery record), `{model}` (hash-pinned model path from the same record). Any other `{...}` is rejected. `check` only validates the shape; `gate` runs it, where exit code 0 is pass, 1 is fail and 3 is inconclusive (retry later, never lands).

## Policy

Effective policy for each dependency, first match wins:

1. `policy.held` exact names: always `held`.
2. Explicit `policy` on the entry or source.
3. Tier patterns: the built-in `/sign|secur|licen|credential/` plus `policy.tier.held`. Default `held`.
4. `policy.default`.

| Change | Policy | Behaviour |
|---|---|---|
| External tools | `auto-land` | Land when the journey passes on green inputs |
| patch / minor dependency bumps | `auto-land` | Land when the gates are green |
| breaking changes | `agent-pr` | Agent migration, then a pull request |
| security, signing, licensing, credentials | `held` | Never automated; a person decides |

## Four-step model

1. `check`: compare pins with upstream latest. Implemented.
2. `apply`: rewrite pins to the latest allowed version. Not implemented.
3. `gate`: run each tool journey and the repository gates. Not implemented.
4. `land`: commit, push or open a pull request according to policy. Not implemented.

`apply`, `gate` and `land` currently exit 2 with "not implemented". Their policy effects above describe the intended behaviour, not current behaviour.

## Network behaviour

All requests go through one bounded `fetch`: 20 second timeout, 1 MiB response cap, a fixed User-Agent, and no credentials. Fetched content is parsed as data and never executed or followed as instructions.
