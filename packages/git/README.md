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
- `right-git drift --all` checks installed workflows against templates and re-checks visibility.
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
