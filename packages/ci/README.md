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
- `right-git artifacts fetch --run <id> --out <dir> [--name <glob>] [--repo owner/repo] [--expect-sha256 <hex> [--file <path>]] [--receipt <file>] [--json]` downloads the artifacts of one workflow run through `gh`, merges them into one tree and writes a JSON receipt (see Fetch CI artifacts).

`install` and `sync` refuse private, internal and unconfirmed-visibility repositories. There is no override flag.

## RightKit check job (ci, qualification, package-smoke, release-candidate, swift-check, publish-npm)

Each lane that compiles or tests renders a `check` job right after `private-repo-guard`, and every other job of that workflow needs it, directly or through a job that does. Nothing builds or qualifies a tree the check refused (pipeline invariant P1, rightkit#833).

- **What it runs.** `actions/checkout` at depth 1 (`persist-credentials: false`), `setup-node` (first `matrix.node`), the package manager, a frozen install without lifecycle scripts (`pnpm install --frozen-lockfile --ignore-scripts`; `npm ci --ignore-scripts`; `yarn install --frozen-lockfile --ignore-scripts`), then `pnpm exec rightkit-gate check --commit HEAD`. Committed files only: no network after the install, no compile, no broker, no secret. The job runs where the workflow's guard runs, times out after 15 minutes and pins its actions like the other jobs of the same file (tags in the node lane files, commits in `swift-check` and the rust-hybrid files).
- **Dependencies.** It needs only `private-repo-guard`. `test` (node `ci`), `ci` and `quick` (rust-hybrid), `qualification`, `clean-tarball`, `candidate`, `swift-check`, `publish-npm`, the release chain's `admission` and the parallel `dev-artifact-*` jobs list it in `needs`. `ci-summary` still reports `ci` and `quick`; a failed check leaves both skipped, which it reports as failed.
- **The repository must depend on `@rightkit/release` 0.6.5 or later**, the first release with `rightkit-gate check`. Sync the new render only after the app has bumped it, or every run fails at the last step. `right-git sync` and `right-git install` read the pin from `package.json` (`devDependencies`, else `dependencies`) whenever a lane that renders the check job is declared: an exact pin below 0.6.5 is refused at stage `dependency` and nothing is written; a range, a non-registry spec or no `@rightkit/release` dependency prints a `warn:` line to stderr and the sync goes on, because the install may still resolve a new enough version. (`candidate.releaseChain` keeps its own, stricter rule: a direct devDependency pinned to an exact published version.) The install is not filtered to `@rightkit/release`: `pnpm install --filter` selects workspace projects, and in a consumer `@rightkit/release` is a registry dependency, so the filter matches no project, installs nothing and exits 0. npm and yarn call `./node_modules/.bin/rightkit-gate` by path, because `npm exec` fetches an unknown name from the registry in CI.
- **A release-candidate checks the revision it builds** (`inputs.source_revision`, or the pushed commit in the node lane's tag push). Its checkout is the one shallow checkout among the jobs of an admitted revision: the check only reads the committed files of HEAD and never runs `right-release`, so it has no reachability to prove.
- **publish-npm checks the release tag it publishes.** The lane checks out `github.event.release.tag_name`, installs, rebuilds the tarball from source (`build-candidate.mjs`), runs `check-release.mjs` and publishes with `id-token: write`, so it builds and tests like the other lanes. Its `check` job checks out the same tag, has no permission beyond the workflow's `contents: read`, no environment and no secret, and the publish job needs it: a tag whose committed files the check refuses never reaches the OIDC token or the registry.
- **Stale lockfile.** The `ci` lane's check job prints the same lockfile patch as the ci job after a failed frozen install; with the check first, the ci job no longer runs when the lockfile is stale.
- **Lanes without it.** `regenerate` repairs derived files and lockfiles, which the check flags, so a red check must not block it. `sign-existing`, the `supersede` job and `regenerate` are admitted by `@rightkit/release` (`right-git-ci-only`) only in their exact rendered shape, `needs: [private-repo-guard]`. `publish-package-managers` builds and tests nothing: it installs, syntax-checks the committed Homebrew formula and opens a tap pull request, or submits a WinGet manifest, for a version that is already released. Workflows under the manifest's `workflows` block are unchanged.
- **Breaking render change.** Every `ci`, `qualification`, `package-smoke`, `release-candidate`, `swift-check` and `publish-npm` render changes (a new job and a `check` entry in `needs`); there is no manifest flag. `right-git drift` on a synced repository shows exactly that job, and the `needs` lines. The hash-pinned renders in `render.test.mjs` were taken again, and each is also kept against its previous hash with the job removed.

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

## Parallel gate shards (rust-hybrid)

A gate that runs everything in one step on each runner is as slow as the sum of its parts. `gate.shards` splits it. The repository compiles locally for a warm cache; this is about running GitHub qualification in parallel.

```json
{
  "profile": "rust-hybrid",
  "ciAllOs": true,
  "gate": {
    "path": "scripts/gate.sh",
    "quick": true,
    "shards": [{ "id": "workspace" }, { "id": "linters" }, { "id": "rust-tools" }]
  }
}
```

- **Shards.** Each shard is one more `matrix.shard` value, so the gate runs once per shard per runner as `bash <path> --group <id>`. `fail-fast: false` still applies, and all legs share one Rust cache key, so one leg's build warms the others.
- **Per-leg state.** Each leg keeps its own last-green marker and its own QA evidence artifact (the shard id is in both names). On a default-branch push each leg is its own concurrency slot.
- **`quick: true`.** Adds a `quick` job (`bash <path> --quick`, on `ubuntu-24.04`, after the check job) for the fast deterministic checks. Every leg needs it, so a formatting or record error stops the run before any shard starts.
- **`ci-summary`.** Passes only when every leg passed (and `quick`, when set). Use it as the one required status check: matrix job names change with the shard list.
- **Validation.**
  - 2 to 16 shards.
  - Ids are lowercase letters, digits and `-`, at most 32 characters, and unique.
  - `quick` needs `shards`.
  - A `devArtifact` must use `parallel: true`, because a dev artifact staged inside the ci job would be built once per shard.
- **Without `shards`.** The rendered `ci.yml` is byte-identical.

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
- `devArtifact.parallel: true` moves each platform's build into its own job, `dev-artifact-mac` or `dev-artifact-windows`. Each runs beside the QA gate job instead of after it, needs only the private-repo guard and the check job, and compiles the dev app with `devArtifact.buildScript` (a package script name; required with `parallel`, rejected without it) before staging it. Parallel jobs run on their own platform runners, so they do not need `ciAllOs`. `mac: "sign"` still waits for the gate: `dev-mac-sign` needs `ci` and `dev-artifact-mac`. The trade-off: unsigned uploads no longer wait for the gate, so a failing gate can still leave `dev-windows-<sha>` or `dev-mac-unsigned-<sha>` behind. Unset or `false` keeps the staging inside the gate job.
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

## Named workflows: custom, swift-host and dispatch-matrix

The `workflows` block renders named workflow files beside the lane files (`right-git sync`, `drift` and `uninstall` treat them like lane files; the file starts with the managed marker). Each entry has a `kind`. Every kind renders: top-level `permissions: contents: read`, a `private-repo-guard` job that every job needs, actions pinned to commit SHAs, `persist-credentials: false` on every checkout, the shared ownership scan (skipped on Linux runners, which ownership approval does not support) after the pinned package-manager install, and no `secrets` reference. Steps may not mention `secrets` or interpolate untrusted event data (`github.head_ref`, `github.event.pull_request.*` and similar); `workflow_dispatch` inputs reach scripts as `RIGHT_GIT_INPUT_<NAME>` environment variables. The node and package-manager setup, install and ownership steps come from the manifest's `matrix.node[0]` and `packageManager`, so a sparse checkout must keep `package.json`, the lockfile and whatever the install needs (cone mode keeps root files).

Common fields: `file` (default `<name>.yml`; never a built-in lane filename), `on` (`push` and `pull_request`: `true` or `{ branches, paths }`; `workflow_dispatch: true`), `concurrency` (`{ group, cancelInProgress }`, rendered as `<group>-${{ github.ref }}`), `checkout` (`{ sparse, longPaths }`, replaces the global `checkout` for this workflow), `timeoutMinutes` (default 60).

**`custom`** (Orthic-Labs/rightkit#465): `runsOn` (one label or several, which become a matrix), `toolchain` (`rust`: `stable`, `beta`, `nightly` or an exact version, rendered as the pinned `dtolnay/rust-toolchain` `toolchain:` input; `swift`: see below), `steps` (`{ name?, shell: bash|pwsh, workingDirectory?, run }`) and `artifact` (`{ name, path, retentionDays 1-90 (default 14), ifNoFilesFound error|warn|ignore (default error), when success|always }`). Artifact names accept the tokens `{sha}`, `{run_id}`, `{run_attempt}`, `{os}` and `{matrix.<key>}`; with several runners the name must carry `{os}`.

```json
{ "workflows": { "render-windows": {
  "kind": "custom", "file": "render-windows.yml", "runsOn": "windows-2025",
  "on": { "push": { "branches": ["main"], "paths": ["render/**"] }, "workflow_dispatch": true },
  "concurrency": { "group": "render-windows", "cancelInProgress": false },
  "checkout": { "sparse": ["render"], "longPaths": true },
  "toolchain": { "rust": "1.91.0" },
  "steps": [{ "name": "Unit tests", "shell": "pwsh", "workingDirectory": "render", "run": "cargo test --release" }],
  "artifact": { "name": "swiftright-render-{sha}", "path": "render/target/release/render.dll", "retentionDays": 30 }
} } }
```

**`swift-host`** (Orthic-Labs/rightkit#437): `os` (for example `["windows-2025", "macos-15"]`, rendered as a matrix), `packages` (one `swift test` step per package directory), optional `swiftArgs` (plain options such as `--parallel`), `toolchain.swift` (required when `os` has a Windows runner) and an optional `artifact`. `on` defaults to push on the default branch plus `pull_request`.

**`dispatch-matrix`** (Orthic-Labs/rightkit#437): `on.workflow_dispatch: true`, `inputs` (`{ description, type string|boolean|choice, default, required, options }`), `plan` (`{ shell, run, runsOn?, workingDirectory? }`; the script writes `matrix=<JSON>` to `$GITHUB_OUTPUT`), `jobs` (up to 6 per-item jobs `{ id, runsOn, toolchain?, steps, artifact? }`, each run once per matrix entry, artifact names carrying `{matrix.<key>}`) and an optional `collect` job (`{ runsOn?, download: { pattern, path }, steps }`, which runs after the item jobs and only when the guard passed).

**`swift-toolchain` step** (Orthic-Labs/rightkit#436): `toolchain.swift = { version, sha256 }` renders, on Windows runners only, `<pm> exec right-release toolchain swift --version <v> --sha256 <pin> --github-env`. The installer's sha256 is verified before it runs, and `--github-env` exports what the installer changed to `GITHUB_ENV` and `GITHUB_PATH` for the later steps. The repository must depend on `@rightkit/release` (the same dependency the ownership scan needs). The step builder is exported as `swiftToolchainStep` from `workflows.mjs`.

`@rightkit/release`'s `right-git-ci-only` policy accepts only `<lane>.yml` files for the declared lanes, so a repository that declares `workflows` fails its release check until that policy also accepts the files named by `workflows` (needs an update in the release package).

## Fetch CI artifacts

`right-git artifacts fetch --run <id> --out <dir>` lists the run's artifacts (`gh api`), keeps those matching `--name` (a glob with `*` and `?`; default all), downloads each by exact name into a staging directory next to `<dir>`, and merges them into `<dir>`. Two artifacts that disagree about a file's bytes fail the fetch. `--repo owner/repo` selects the repository (default: the current checkout's). `--expect-sha256 <hex>` is verified before anything reaches `<dir>`: with `--file <relative path>` it pins that one file (the renderer DLL, for example); without it, the tree digest (sha256 over the sorted lines `<sha256>  <path>`). A mismatch, an expired or unsafe-named artifact, a symbolic link or a failed download exits 1 and leaves `<dir>` untouched. `--receipt <file>` writes the JSON receipt (`schema right-git.artifacts-receipt/1`: run, repo, name pattern, per-artifact files with sizes and sha256, `fileCount`, `treeSha256`, `pin`, `verified`); `--json` prints it.

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
