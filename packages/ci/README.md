# @rightkit/ci

Public-repository-only GitHub Actions workflow layer for Right Suite products. The CLI `right-git` renders and installs CI lanes into `.github/workflows/` from a `.rightgit.json` manifest.

| Item | Value |
|---|---|
| Where it runs | Build and release tooling (repository CI setup) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## Commands

- `right-git install <repo> --lanes ci,...` writes `.rightgit.json` and renders workflows.
- Lanes: `ci`, `qualification`, `package-smoke`, `release-candidate`, `publish-npm`, `publish-package-managers`, `regenerate`, `swift-check`. The `regenerate` lane is manual-only: it runs `regenerate.script`, checks the result, and opens a pull request from a `right-git/regenerate-<run>` branch; it never writes to the default branch.
- `right-git sync` re-renders installed workflows from the manifest.
- `right-git drift --all` checks installed workflows against templates and re-checks visibility. When the manifest declares the `regenerate` lane, drift also warns (never fails) if the repository's "Allow GitHub Actions to create and approve pull requests" setting is off. It reads `gh api repos/{owner}/{repo}/actions/permissions/workflow` and prints a note instead when gh is unavailable.
- `right-git adopt` maps an existing repository's workflows onto lanes for review. With `--out <dir>` it writes the candidate templates to that directory.
- `right-git uninstall` removes every managed file.

`install` and `sync` refuse private, internal and unconfirmed-visibility repositories. There is no override flag.

## Default branch

`"defaultBranch": "master"` renders the `push` trigger of the `ci`, `qualification` and `package-smoke` lanes, and the rust-hybrid `release-candidate` admission check (`refs/heads/<branch>`), for a repository whose default branch is not `main`. Unset, it is `main` and output is byte-identical.

## Checkout options (all lanes)

Opt-in `checkout` block applied to every `actions/checkout` step the manifest renders. Unset, `{}` or `longPaths: false` with no `sparse`, the rendered workflows are byte-identical.

```json
{ "checkout": { "longPaths": true, "sparse": ["packages/", "examples/SliceApp"] } }
```

- `longPaths: true` renders a step directly before each checkout: `if: runner.os == 'Windows'`, `shell: bash`, `git config --system core.longpaths true`. It is the documented workaround for `Filename too long` on Windows runners (actions/checkout issue 242 recommends setting `core.longpaths` before the checkout step). Linux and macOS legs skip it, so matrix jobs stay one definition.
- `sparse` renders `sparse-checkout: |` (one directory per line) and `sparse-checkout-cone-mode: true` into each checkout of the repository itself, merged with any existing `with:` inputs. Cone mode always keeps root-level files. Entries are unique safe relative directory paths (no `..`, whitespace, leading `/` or expression syntax). Use it when only part of the tree is needed, or to skip paths that cannot exist on Windows at all. Checkouts of another repository (such as the Homebrew tap) are never sparse.

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
- `devArtifact.macSignManifest` (with `macPayloadMetadata`) replaces `codesign --deep` on the `.app` with `right-release sign-dev-artifact`: each file in the repository-relative sign manifest (`{ "schema": 1, "entries": [{ "file": "Legion.app/Contents/MacOS/legion", "identifier": "com.example.legion" }] }`, paths relative to the extracted archive, nested code before its bundle) is signed with its fixed identifier, the archive's payload metadata (`macPayloadMetadata`, relative to the archive root) is verified before and refreshed after signing, and `sign-evidence.json` is uploaded beside the zip. The job then also checks out the repo and installs dependencies, so `@rightkit/release` 0.2.172 or later must be a dependency. Requires `mac: "sign"`; not combinable with `macEntitlements` (put per-file entitlements in the manifest) or `mac: "notarize"`. Unset keeps the `--deep` lane.
- `devArtifact.parallel: true` moves each platform's build into its own job, `dev-artifact-mac` or `dev-artifact-windows`. Each runs beside the QA gate job instead of after it, needs only the private-repo guard, and compiles the dev app with `devArtifact.buildScript` (a package script name; required with `parallel`, rejected without it) before staging it. Parallel jobs run on their own platform runners, so they do not need `ciAllOs`. `mac: "sign"` still waits for the gate: `dev-mac-sign` needs `ci` and `dev-artifact-mac`. The trade-off: unsigned uploads no longer wait for the gate, so a failing gate can still leave `dev-windows-<sha>` or `dev-mac-unsigned-<sha>` behind. Unset or `false` keeps the staging inside the gate job.
- The `~/Library/Developer/Xcode/DerivedData` cache helps only builds that use Xcode's default derived data location. A gate that passes its own `-derivedDataPath` writes outside that directory and gets nothing from the cache; use the default location, or cache the custom path in the gate.

## Sign an existing artifact (rust-hybrid, `sign-existing` lane)

For a public repository whose signing must run on protected CI, but whose artifact was already built and qualified by an earlier run. The lane never rebuilds, never uses `--release` and never notarizes.

```json
{
  "lanes": ["ci", "sign-existing"],
  "signExisting": {
    "macSignManifest": "build/dev-sign.json",
    "macPayloadMetadata": "payload.json",
    "environment": "release"
  }
}
```

- `macSignManifest` (repository-relative) and `macPayloadMetadata` (relative to the extracted artifact, and outside every signed bundle: `right-release sign-dev-artifact` refuses metadata inside a signed `.app`, because refreshing it after signing would break that signature) are the same as for `devArtifact.macSignManifest`; `environment` is optional and defaults to the signing environment (`release`). `matrix.os` needs a macOS runner; the first one is used.
- `signExisting.macPayloadMetadataSource` (optional, repository-relative, for example `dist/dev-macos/payload.json`) makes the unsigned dev packaging step put the payload metadata inside the inner zip. The step stages the `.app` (wrapper layout kept, copied with `ditto`) and that file together and zips them, so the file sits at the inner-zip root under the file name `macPayloadMetadata`, which must then be a bare name such as `payload.json`; the step fails if the file is missing. This applies to every macOS unsigned packaging step (`devArtifact.mac` `sign`, `notarize` or `unsigned`, in the gate job or the parallel `dev-artifact-mac` job). Unset, the step is the single `ditto -c -k --keepParent` line and the render is byte-identical. The app generates the file itself; Legion writes it with `fileEvidence` from `@rightkit/release/macos-file-evidence.mjs`, so its hashes use the same file and bundle-tree digests that `sign-dev-artifact` verifies.
- Invoke it from the default branch: `gh workflow run sign-existing.yml -f run_id=39001031529 -f artifact=12089612527 -f source_sha=59f451ae`. `artifact` is a numeric artifact id or an artifact name (all digits means id). `source_sha` is 7 to 40 lowercase hex characters and must be a prefix of the run's full head sha.
- The job refuses, before it downloads the artifact or reads a secret, unless the GitHub API shows that the run belongs to this repository, was built from it (not a fork), completed successfully and has a head sha starting with `source_sha`; that exactly one unexpired artifact of that run matches and records the same run, head sha and repository; and that the source commit is an ancestor of the signing revision (the default-branch commit that runs the job, which also supplies the signing code and sign manifest).
- The artifact must hold one zip (extracted with `ditto`) or a plain file tree. The job signs it with `right-release sign-dev-artifact`, then uploads `signed-<artifact name>-<sha>` containing the signed zip (same layout), `sign-evidence.json` and `source-verification.json`. It uses the same three Developer ID secrets and temporary keychain as `dev-mac-sign`, with `contents: read` and `actions: read` only. Needs `@rightkit/release` 0.2.172 or later as a dependency. Unset, nothing changes.

## Cache keys and concurrency (rust-hybrid)

Both keys are opt-in; unset, the rendered workflows are byte-identical.

- `rustCache.keySuffix` (1 to 64 characters: letters, digits, `.`, `_`, `-`; starts with a letter or digit) renders `key: <lane>-<suffix>` into every Swatinem/rust-cache step: `ci`, `regenerate`, `release-candidate`. Each lane therefore keeps its own cache entry. The dev-artifact jobs use `<lane>-dev-artifact-<platform>-<suffix>`. Swatinem/rust-cache does not re-save a cache on an exact key hit (third-party behavior, not verified in this repository). To rotate the cache, change the suffix, which gives it a new key.
- `concurrency` (requires lane `ci`; both profiles). The rule is one CI run in progress per repository. By default (`mainPushes: "cancel"`) the ci workflow renders a `supersede` job, the only job with `actions: write`, which runs `supersede.mjs`: a newer push to the default branch cancels an older run of the workflow that never started, has a failed job, or is younger than `graceFraction` (default 0.7) of the median duration of the last `medianWindow` (default 10) successful runs. An older run past that point is left to finish, and the new run's `ci` job waits for it through a job-level group with `cancel-in-progress: false`. PR runs still cancel superseded runs.
- `concurrency.mainPushes: "queue"` is the opt-out: default-branch pushes each run to completion, no `supersede` job is rendered, and a non-empty single-line `concurrency.reason` is required (the manifest is invalid without one). The reason is rendered as a YAML comment above the `concurrency` block. `graceFraction` and `medianWindow` are not allowed with `queue`. The publish workflow never cancels.

```json
{
  "profile": "rust-hybrid",
  "rustCache": { "workspaces": ["engine"], "keySuffix": "2026-10" },
  "concurrency": { "mainPushes": "queue", "reason": "deploys must see every main commit built in turn" },
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

## Dev qualification (rust-hybrid release-candidate)

`candidate.devQualification: { "buildScript": "...", "journeyScript": "..." }` adds a `dev-qualification` job beside the QA-free candidate (bogusyogi/rightkit#107). `@rightkit/release` refuses `qa-native`, `qa-control`, `dev-native`, `dev-inspect` and `test-mode` in any release tier, so in-app journeys cannot run against the sealed candidate. The job needs `admission`, uses the candidate's target matrix, checks out the same `source_revision`, runs `buildScript` (the dev build, QA surface on) and then `journeyScript`, and uploads its evidence as `<prefix>-<run>-<attempt>-devqual-<platform>-<arch>-<version>-<revision><suffix>` plus a `dev-qualification` stage summary. When set, `windows-sign` and `macos-sign` need it, so a failure blocks signing. Installed qualification of the signed candidate is unchanged. Unset, the workflow is byte-identical. Validation refuses a candidate build or check script whose name mentions a QA surface and a `devQualification.buildScript` equal to `candidate.buildScript`; it cannot see cargo features inside a script.

## Private Swift packages (ci, qualification, release-candidate, regenerate lanes)

Set `swift.privatePackages` to let a public repository's workflows fetch a private Swift package, for example `example-org/private-swift-package`:

```json
{
  "profile": "rust-hybrid",
  "swift": { "privatePackages": [{ "host": "github.com", "secret": "RIGHTKIT_SWIFT_TOKEN" }] }
}
```

- `host` is a bare host name, unique within the list (at most four entries). `secret` is a GitHub Actions secret name (letters, digits, `_`; not starting with a digit or `GITHUB_`). The manifest needs at least one of the lanes `ci`, `qualification`, `release-candidate`, `regenerate`.
- The secret must be a fine-grained token with `contents: read` on the private package repository only, and nothing else. The repository owner provisions it; the workflow never prints it.
- One implementation renders everywhere. The configure step runs right before the first build or test step of the job. It writes `~/.netrc` (`machine <host> login x-access-token password <token>`, mode 600) and a job-scoped `git config --global url."https://x-access-token:<token>@<host>/".insteadOf "https://<host>/"`. The token reaches the step through `env` (`${{ secrets.<NAME> }}` in an `env:` block), never the run script.
- It refuses to run when `~/.netrc` already exists, so a persistent self-hosted runner's own file is never overwritten. The cleanup step runs with `if: always()` after the last build or test step of the job, removes the `~/.netrc` and the git rewrite it created, and leaves a pre-existing file alone.
- Any code that runs between the configure and cleanup steps in that job can read the token. Keep the token read-only.
- Unset, the rendered workflows are byte-identical to earlier versions.
- A run from a fork gets no secrets, so the configure step fails with a message that names the secret.
- Refused with `devArtifact.dispatch: true`: a manual dispatch of any ref would reach the secret. Rendering the steps into a workflow triggered by `pull_request_target` or `workflow_run` throws; no shipped template has either trigger.

Which jobs get the steps, and why each is safe:

| Lane (workflow) | Job | Position | Triggers and why it is safe |
| --- | --- | --- | --- |
| `ci` (both profiles) | `ci` (rust-hybrid), `test` (node) | rust-hybrid: before the gate, cleanup after it. node: after install, cleanup is the last step | `pull_request` and `push` to `main`. Fork pull requests get no secrets, so the job fails closed |
| `ci`, rust-hybrid with `devArtifact.parallel` | `dev-artifact-mac` | before "Build dev app", cleanup before the staging and upload steps | Push to the default branch only (dispatch is refused). `dev-artifact-windows` and `dev-mac-sign` never get the steps |
| `qualification` (node) | `qualification` (runs `test:all`) | after the ownership step, cleanup right after `test:all` | `pull_request` and `push` to `main`, as ci. `schema`, `security`, `hardening` and `conformance` stay secret-free; `security` is a secret-egress suite |
| `release-candidate` (both profiles) | `candidate` | rust-hybrid: before "Build unsigned release candidate", cleanup right after "Check unsigned release candidate". node: after the ownership step, cleanup after `check-release.mjs` | rust-hybrid: `workflow_dispatch` only. node: `workflow_dispatch` and `v*` tag pushes. Dispatch needs write access, and this job holds no signing credential. The admission, signing, installed-qualification and publish jobs never get the steps |
| `regenerate` (both profiles) | `regenerate` | before the regenerate script, cleanup after the test or gate step and before the pull-request step | `workflow_dispatch` only. The cleanup runs before `git push`, so the push never runs under the `insteadOf` rewrite |

Never rendered: `package-smoke` (a clean-room tarball check), `publish-npm` and `publish-package-managers` (publishing credentials), and the release chain's `windows-sign`, `macos-sign`, `installed-qualification` and `publish` jobs (signing and publish credentials). Their output is the same with and without `swift.privatePackages`.

Release policy. `@rightkit/release` enforces the `right-git-ci-only` policy over the rendered workflows (`validateHostedWorkflows` in `release.mjs`). It refuses a managed workflow that references any secret outside an allowlist. The allowlist admits the `swift.privatePackages` secret only on its `RIGHT_GIT_PRIVATE_TOKEN_<n>: ${{ secrets.<NAME> }}` env line, inside the "Configure private Swift package access" or "Remove private Swift package access" step, of these jobs: `ci.yml` jobs `ci` (either profile), `dev-artifact-mac` (rust-hybrid with `devArtifact.parallel` and a `devArtifact.mac` leg) and `test` (node profile); `qualification.yml` job `qualification`; `release-candidate.yml` job `candidate`; and `regenerate.yml` job `regenerate` (`SWIFT_PRIVATE_JOBS`, `release.mjs:86-91`). Each reference also needs its lane listed in `.rightgit.json`, `devArtifact.dispatch` not true, and no `pull_request_target` or `workflow_run` trigger. `workflow_dispatch` is allowed only in `release-candidate.yml` and `regenerate.yml` (`swiftPrivateSecretRefusal`, `release.mjs`).

## Entry points

No importable API. The package has no `main` and no `exports` map. The supported surface is the `right-git` bin.

## Peer dependencies

None. No runtime dependencies (Node builtins only).

## Example

```sh
pnpm add -D @rightkit/ci
pnpm exec right-git install . --lanes ci
pnpm exec right-git drift --all
```

## Known issues

- The optional `publish-package-managers` lane needs two long-lived tokens in GitHub Actions secrets. Their names and scope are in the package templates, and they are not zero-exposure. Read `docs/rules/github-access.md` in the workspace before enabling it.
- Manifest fields are charset-checked before they reach workflow YAML.
- `swift.privatePackages` renders a secret reference into the workflows listed in its section. The `right-git-ci-only` policy in `@rightkit/release` refuses any reference outside the jobs listed under Release policy above.

## Native iOS XCTest (`swift-check`)

Use `lanes: ["swift-check"]` with default `node` profile for standalone Swift validation. Existing `ci` stays optional for this profile. Configure native tools & paths in `.rightgit.json`:

```json
{
  "swift": {
    "check": {
      "runner": "macos-15",
      "xcodeVersion": "16.4",
      "projectDirectory": "ios",
      "spec": "project.yml",
      "project": "Widgets.xcodeproj",
      "scheme": "Widgets",
      "xcodegenVersion": "2.44.1"
    },
    "privatePackages": [{ "host": "github.com", "secret": "SWIFT_READ_TOKEN" }]
  }
}
```

Versions above illustrate configuration; choose exact versions provisioned on selected hosted runner. Xcode & XcodeGen must already match; missing tools or version drift fail before generation. This lane installs no native tools. `projectDirectory` is repository-relative; spec & project paths are relative to that directory. Scheme names permit letters, digits, `.`, `_` & `-`.

Lane checks out without persisted GitHub credentials, installs pinned package-manager dependencies, runs shared ownership scan, verifies native tools, configures shared private-package access, generates project, discovers available iPhone simulator, then runs Debug XCTest with `-scmProvider system` & `CODE_SIGNING_ALLOWED=NO`. Repository must directly depend on published `@rightkit/release` for ownership scan. Credentials remain step-scoped & cleanup always runs before `.xcresult` upload. Fork pull requests fail closed when declared private-package secret is unavailable. Owner provisions a read-only credential limited to required private repositories.

Only push to configured default branch & `pull_request` trigger this lane. Workflow requests `contents: read`, pins actions to full commit SHAs & includes public-repository refusal guard. `swift.privatePackages` is optional for public-only dependencies. Omit `swift.check` & lane to preserve existing workflow bytes.
