# @rightkit/git

Public-repository-only GitHub Actions workflow layer for Right Suite products. The CLI `right-git` renders and installs CI lanes into `.github/workflows/` from a `.rightgit.json` manifest.

| Item | Value |
|---|---|
| Where it runs | Build and release tooling (repository CI setup) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Commands

- `right-git install <repo> --lanes ci,...` writes `.rightgit.json` and renders workflows.
- Lanes: `ci`, `qualification`, `package-smoke`, `release-candidate`, `publish-npm`, `publish-package-managers`, `regenerate`. The `regenerate` lane is manual-only: it runs `regenerate.script`, checks the result, and opens a pull request from a `right-git/regenerate-<run>` branch; it never writes to the default branch.
- `right-git sync` re-renders installed workflows from the manifest.
- `right-git drift --all` checks installed workflows against templates and re-checks visibility. When the manifest declares the `regenerate` lane, drift also warns (never fails) if the repository's "Allow GitHub Actions to create and approve pull requests" setting is off. It reads `gh api repos/{owner}/{repo}/actions/permissions/workflow` and prints a note instead when gh is unavailable.
- `right-git adopt` imports existing workflows into the template library.
- `right-git uninstall` removes every managed file.

`install` and `sync` refuse private, internal and unconfirmed-visibility repositories. There is no override flag.

## Optional QA evidence upload (rust-hybrid)

Set `qaEvidencePath` in `.rightgit.json` to upload the gate's QA output as a workflow artifact. The path is relative to `$RUNNER_TEMP`; the rendered step uses `${{ runner.temp }}/<path>`. Absolute paths and expression syntax are rejected.

```json
{
  "profile": "rust-hybrid",
  "gate": { "path": "scripts/gate.sh" },
  "qaEvidencePath": "pulse-hub-qa",
  "qaEvidenceRetentionDays": 14
}
```

- The step runs after the gate with `if: always()` and `if-no-files-found: ignore`.
- Artifact name: `qa-evidence-${{ matrix.os || runner.os }}-${{ github.run_attempt }}`.
- `qaEvidenceRetentionDays` is an integer from 1 to 90 and defaults to 7. It requires `qaEvidencePath`.
- Omit the key and the rendered `ci.yml` is unchanged.

## Full validation on every OS (rust-hybrid)

Set `"ciAllOs": true` to run the rust-hybrid `ci` gate on every `matrix.os` runner instead of one. Runners run in declared order with `fail-fast: false`. Each leg uploads its own artifact, because the QA artifact name already includes the runner label.

```json
{
  "profile": "rust-hybrid",
  "matrix": { "os": ["xcode-27", "windows-2025"], "node": ["26.8.1"] },
  "ciAllOs": true
}
```

Omit the key, or set it to `false`, and the rendered `ci.yml` keeps one runner, preferring the first `windows-*` entry. `ciAllOs: true` is rejected for other profiles.

## Incremental CI and dev artifacts (rust-hybrid)

Both keys are opt-in; unset, the rendered `ci.yml` is byte-identical.

`"ciIncremental": true` switches Rust caching to `cache-workspace-crates: true`, sets `CARGO_INCREMENTAL=1` for the ci gate, and caches `~/Library/Developer/Xcode/DerivedData` on macOS legs (key: runner + `Cargo.lock` + `Package.resolved` + `project.pbxproj` hash).

`devArtifact` publishes fast development builds as workflow artifacts, separate from the release-candidate lane. It runs on push to the default branch only, never on pull requests. `mac` is `"sign"` (Developer ID via the `dev-mac-sign` job), `"notarize"` (signed, notarized and stapled; required for apps that ship app extensions, which macOS refuses from unnotarized Developer ID apps), `"unsigned"` (the bare `.app` zipped as `dev-mac-unsigned-<sha>`) or `"none"`; `windows` is `"unsigned"` or `"none"`. With `dispatch: true` the unsigned legs also run on a manual `workflow_dispatch` of any ref; the signing job never does.

```json
{
  "profile": "rust-hybrid",
  "matrix": { "os": ["xcode-27", "windows-2025"], "node": ["26.8.1"] },
  "ciAllOs": true,
  "ciIncremental": true,
  "devArtifact": {
    "mac": "sign",
    "windows": "unsigned",
    "retentionDays": 7,
    "macAppPath": "target/release/bundle/macos/Pulse.app",
    "windowsGlob": "target/release/*.exe"
  }
}
```

- `mac: "sign"` needs `macAppPath` (the built `.app`, repository-relative) and a macOS runner in the ci gate (`ciAllOs`). The unsigned app is zipped with `ditto` and handed to a separate `dev-mac-sign` job that installs the Developer ID identity from the `release` environment secrets, runs `codesign --force --deep --strict --options runtime`, verifies, zips and uploads `dev-mac-<sha>`. No notarization, admission or ownership gate. Optional `macEntitlements` passes `--entitlements`.
- `windows: "unsigned"` needs `windowsGlob` and a `windows-*` runner in the ci gate; matched files upload as `dev-windows-<sha>`.
- The `dev-mac-sign` job uses the `release` environment, so that environment's protection rules (required reviewers, branch policy) apply. The job only exists for pushes to the default branch.
- Install a mac dev build with `right-release install-app --dev --zip <file>` (Developer ID-signed, not notarized).
- `devArtifact.parallel: true` moves each platform's build into its own job, `dev-artifact-mac` or `dev-artifact-windows`. Each runs beside the QA gate job instead of after it, needs only the private-repo guard, and compiles the dev app with `devArtifact.buildScript` (a package script name; required with `parallel`, rejected without it) before staging it. Parallel jobs run on their own platform runners, so they do not need `ciAllOs`. `mac: "sign"` still waits for the gate: `dev-mac-sign` needs `ci` and `dev-artifact-mac`. The trade-off: unsigned uploads no longer wait for the gate, so a failing gate can still leave `dev-windows-<sha>` or `dev-mac-unsigned-<sha>` behind. Unset or `false` keeps the staging inside the gate job.
- The `~/Library/Developer/Xcode/DerivedData` cache helps only builds that use Xcode's default derived data location. A gate that passes its own `-derivedDataPath` writes outside that directory and gets nothing from the cache; use the default location, or cache the custom path in the gate.

## Cache keys and concurrency (rust-hybrid)

Both keys are opt-in; unset, the rendered workflows are byte-identical.

- `rustCache.keySuffix` (1 to 64 characters: letters, digits, `.`, `_`, `-`; starts with a letter or digit) renders `key: <lane>-<suffix>` into every Swatinem/rust-cache step: `ci`, `regenerate`, `release-candidate`. Each lane therefore keeps its own cache entry. The dev-artifact jobs use `<lane>-dev-artifact-<platform>-<suffix>`. rust-cache never re-saves a cache on an exact key hit, so change the suffix to rotate the cache: the next run on the default branch saves a fresh entry under the new key.
- `concurrency.mainPushes: "queue"` (requires lane `ci`; both profiles) renders the ci workflow's `cancel-in-progress` as an expression that is false only for pushes to the default branch. Default-branch pushes queue and each one runs to completion; PR runs still cancel superseded runs. `"cancel"` is the default and renders the current `cancel-in-progress: true`.

```json
{
  "profile": "rust-hybrid",
  "rustCache": { "workspaces": ["engine"], "keySuffix": "2026-10" },
  "concurrency": { "mainPushes": "queue" },
  "devArtifact": {
    "mac": "unsigned",
    "windows": "unsigned",
    "parallel": true,
    "buildScript": "build:dev",
    "macAppPath": "target/release/bundle/macos/Pulse.app",
    "windowsGlob": "target/release/*.exe"
  }
}
```

## Entry points

No importable API. The package has no `main` and no `exports` map. The supported surface is the `right-git` bin.

## Peer dependencies

None. No runtime dependencies (Node builtins only).

## Example

```sh
pnpm add -D @rightkit/git
pnpm exec right-git install . --lanes ci
pnpm exec right-git drift --all
```

## Known issues

- The optional `publish-package-managers` lane needs two long-lived tokens in GitHub Actions secrets. Their names and scope are in the package templates, and they are not zero-exposure. Read `docs/rules/github-access.md` in the workspace before enabling it.
- Manifest fields are charset-checked before they reach workflow YAML.
