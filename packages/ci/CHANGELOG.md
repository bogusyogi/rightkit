# Changelog

## 0.2.44

- rust-hybrid release candidate: the `admission` job runs on `macos-15`, not `ubuntu-24.04`. Its ownership step (`right-release ownership`) fails on Linux with "ownership approval unsupported platform: linux" (Ember run 38715042025), because ownership approval supports only macOS and Windows. A render test asserts admission never renders on a Linux runner. The `ci` and `regenerate` jobs of this profile still render `ubuntu-24.04` with the same ownership step; they were not part of this fix.

## 0.2.43

- Release candidate (non-chain mode): the Windows and macOS sign steps now set `RIGHT_GIT_SOURCE_REVISION` (and `LEGION_SOURCE_REVISION`) from the admission job's output. Before, `LEGION_SOURCE_REVISION` read an undefined workflow variable and right-release's admitted-dispatch exemption could not fire, so signing refused the detached HEAD.

## Unreleased

- Renamed from @rightkit/git; @rightkit/git@0.2.42 remains published under the old name.

- New optional manifest key `defaultBranch` (default `main`) for the push triggers of `ci`, `qualification`, `package-smoke` and the rust-hybrid `release-candidate` admission check. Unset, output is byte-identical.
- New optional manifest block `checkout { longPaths, sparse }`. `longPaths: true` renders a Windows-only `git config --system core.longpaths true` step before every `actions/checkout`; `sparse` renders `sparse-checkout` (cone mode) into each checkout of the repository itself. Applied after template rendering (`checkout.mjs`, called from `renderLanes`), so every lane and generated job is covered. Unset, output is byte-identical.

## 0.2.42

- Node `release-candidate`: the ownership step compared `git rev-parse HEAD` with `inputs.source_revision`, but the workflow declared no inputs, so the comparison was always against an empty string. `workflow_dispatch` now declares a required `source_revision` input (as the rust-hybrid lane does), the checkout uses `ref: ${{ inputs.source_revision || github.sha }}`, and the step compares against the same expression. A dispatch builds and verifies the commit it names; a `v*` tag push has no input and compares against the pushed commit. rust-hybrid and every other lane render unchanged.
- Tests: the actionlint ignore for the undeclared input is gone, and both triggers (dispatch with a matching and a mismatched revision, tag push at and away from HEAD) run the rendered comparison in a scratch git repository.

## 0.2.41

- `swift.privatePackages` now renders its configure and cleanup steps into every lane that builds Swift, not only `ci`. Same steps as `ci` (one implementation): the token reaches the step through `env`, `~/.netrc` is mode 600 and never overwritten, the `insteadOf` rewrite is job-scoped, and the cleanup step runs with `if: always()`. Jobs that now carry them, when the manifest sets `swift.privatePackages` and declares the lane:
  - `release-candidate` (both profiles): the `candidate` job. rust-hybrid: configure before "Build unsigned release candidate", cleanup right after "Check unsigned release candidate", so the stage-summary and upload steps run without the credential. node: configure after the ownership step, cleanup after `check-release.mjs`.
  - `qualification` (node): the `qualification` job (`test:all`). The `schema`, `security`, `hardening` and `conformance` jobs stay secret-free.
  - `regenerate` (both profiles): the `regenerate` job. Configure before the regenerate script; cleanup after the test or gate step and before the pull-request step, so the `git push` never runs under the rewrite.
  - rust-hybrid `devArtifact.parallel`: the `dev-artifact-mac` job (around "Build dev app"). `dev-artifact-windows` and `dev-mac-sign` stay secret-free.
- Never rendered: `package-smoke` (clean-room tarball check), `publish-npm`, `publish-package-managers`, and the signing, installed-qualification and publish jobs of the release chain. Their output is byte-identical with and without `swift.privatePackages`.
- Rendering the configure step into a workflow whose `on:` block lists `pull_request_target` or `workflow_run` now throws. No shipped template has either trigger.
- Validation: `swift.privatePackages` needs one of the lanes `ci`, `qualification`, `release-candidate` or `regenerate` (it previously needed `ci`).
- Unset, every lane renders byte-identical to 0.2.40 (pinned by hash in `render.test.mjs`).
- `@rightkit/release` `right-git-ci-only` still allows the secret only in the `ci` job of `ci.yml` and refuses it when the workflow has a `workflow_dispatch` trigger, so a repository using the new lanes fails its release check until that policy is extended (see the README).

## 0.2.40

- `profile: rust-hybrid` ci: the "Detect Rust-affecting changes" step no longer diffs a push against the previous push (`github.event.before`). A failed run followed by a commit with no Rust changes produced a green run that never ran the Rust checks for the earlier commits. Each matrix job now records the commit it last passed on (a small `actions/cache` entry written as the job's final steps) and a push or manual run is diffed against that commit. No marker, an evicted marker, or a marker that is not an ancestor of the pushed commit means `changed=true`. Pull requests still diff against the PR base. The first run after `right-git sync` has no marker and runs the full Rust checks once.

## 0.2.39

- New opt-in `swift.privatePackages` (ci lane, rust-hybrid and node): a list of `{ host, secret }` entries (at most four, unique hosts, `secret` a GitHub Actions secret name) that lets a public repository's ci job fetch a private Swift package mirror. A configure step placed right before the first build or test step writes `~/.netrc` (mode 600; refuses to overwrite an existing file) and a job-scoped `git config --global url.<base>.insteadOf` rewrite, with the token passed through `env`. An always-run cleanup step at the end of the ci job removes both. Refused with `devArtifact.dispatch: true`. Unset renders byte-identical output.
- `profile: rust-hybrid` and node ci (`pnpm` only): when the frozen install fails, a new step runs `pnpm install --lockfile-only` and prints `git diff -- pnpm-lock.yaml` as a patch block (a `::group::` with a `::error file=pnpm-lock.yaml::` annotation). It runs only after a failed install, never changes the job result, and prints a notice instead when the lockfile already matches. npm and yarn renders are unchanged; the new pnpm step is the only difference in a pnpm render.

## 0.2.38

- `devArtifact.parallel: true` (rust-hybrid) renders one job per platform, `dev-artifact-mac` and `dev-artifact-windows`, that compiles the dev app with the new `devArtifact.buildScript` package script and stages it. The jobs run beside the QA gate job (they need only the private-repo guard) instead of inside it. `dev-mac-sign` now needs `ci` and `dev-artifact-mac`. Parallel jobs do not need `ciAllOs`. Unset or `false` renders the current in-gate output byte-for-byte.
- `rustCache.keySuffix` (rust-hybrid) renders `key: <lane>-<suffix>` into every rust-cache step, so each lane keeps its own cache entry and a repository can rotate its cache. Unset renders no key. Documented: rust-cache never re-saves on an exact key hit.
- `concurrency.mainPushes: "cancel" | "queue"` (requires lane `ci`). `"queue"` renders `cancel-in-progress` as an expression that is false only for default-branch pushes, so those queue; PR runs keep cancelling. `"cancel"` (default) renders the current `cancel-in-progress: true`.
- `right-git drift` (the existing check command; no `doctor` command exists) warns when the manifest declares the `regenerate` lane and the repository's "Allow GitHub Actions to create and approve pull requests" setting is off, read via `gh api repos/{owner}/{repo}/actions/permissions/workflow`. The warning never changes drift or the exit code; a note is printed when gh is unavailable or the setting cannot be read.
- Docs: a gate that passes its own `-derivedDataPath` gets nothing from the `~/Library/Developer/Xcode/DerivedData` cache.
- `manifest.schema.json`: documents the new keys and corrects the `devArtifact.mac` enum to match the validator (`notarize` and `unsigned` were missing).

## 0.2.37

- `devArtifact.mac` accepts `"notarize"`: the `dev-mac-sign` job also submits the Developer ID-signed dev app to the notary service and staples the ticket before uploading `dev-mac-<sha>`. Current macOS refuses to register app extensions (Finder Sync, Share, ...) from an unnotarized Developer ID app, so a `"sign"` dev install silently loses them; use `"notarize"` for apps that ship extensions. Same job gate (push to the default branch, release environment); `"sign"` output is byte-identical to 0.2.36.

## 0.2.36

- New manual-only `regenerate` lane (`lanes: [..., "regenerate"]` + `regenerate: { script }`). The managed `regenerate.yml` runs on `workflow_dispatch` only: checks out the default branch, runs the package script, runs the ownership scan and (rust-hybrid) the ci gate or (node) the test script on the regenerated tree, then pushes it to `right-git/regenerate-<run_id>-<attempt>` and opens a pull request with the workflow token. It never writes to the default branch; `contents: write` and `pull-requests: write` are job-scoped. Pull requests opened with the workflow token do not trigger `pull_request` workflows, which is why the checks run in the lane and the PR body links the run. rust-hybrid now accepts `regenerate` beside `ci` and `release-candidate`.

## 0.2.35

- Every identity-bound release-candidate artifact name (candidate, signed, summary, installed-qualification, and their downloads) now ends with the admission job's `artifact_suffix` output, so a `dry_run` rehearsal produces `…-<sha>-dry-run` artifacts that cannot be mistaken for a real candidate. Admission already emitted the output; the names never consumed it. The publish job's `if:` is unchanged (dry runs are refused by admission, and @rightkit/release pins that gate text). Re-render with `right-git sync`.

## 0.2.34

- `devArtifact.mac` accepts `"unsigned"`: the ci job zips `macAppPath` and uploads it as `dev-mac-unsigned-<sha>` with `retentionDays`, no signing job, no environment, no secrets. `macEntitlements` still requires `"sign"`.
- Opt-in `devArtifact.dispatch: true` adds `workflow_dispatch:` to ci.yml and lets the unsigned legs (`windows: "unsigned"`, `mac: "unsigned"`) also run on a manual dispatch of any ref. The `dev-mac-sign` job and its package step stay push-to-default-branch only, so no secret is reachable from a dispatched run. Unset renders byte-identical output.

## 0.2.33

- `ciIncremental: true` no longer sets `RUSTC_WRAPPER=sccache` (sccache refuses `CARGO_INCREMENTAL=1`); incremental builds rely on the rust-cache target directory. Unset output is unchanged.

## 0.2.32

- Add opt-in `ciIncremental` (boolean, rust-hybrid only): `Swatinem/rust-cache` with `cache-workspace-crates: true`, `CARGO_INCREMENTAL=1` on the ci gate, and an `actions/cache` step for Xcode DerivedData on macOS legs. Unset renders byte-identical output.
- Add opt-in `devArtifact` (rust-hybrid only): `mac: "sign" | "none"`, `windows: "unsigned" | "none"`, `retentionDays`, plus required `macAppPath` / `windowsGlob` (optional `macEntitlements`). On push to the default branch only, the ci job uploads unsigned Windows files as `dev-windows-<sha>`; macOS legs hand the unsigned app to a separate `dev-mac-sign` job (release environment, protection rules apply) that Developer ID-signs with hardened runtime, no notarization, no admission or ownership gate, and uploads `dev-mac-<sha>`. The release-candidate lane is unchanged.

## 0.2.31

- `release-candidate` also runs ownership with `--advisory-when-pending`, so signed builds proceed while approval keys are unenrolled. The flag is inert once keys are enrolled.

## 0.2.30

- The `ci` lane runs `right-release ownership --advisory-when-pending` (needs @rightkit/release 0.2.137): until ownership approval keys are enrolled the scan asserts nothing, so CI reports findings as diagnostics instead of failing. `release-candidate` stays strict.

## 0.2.29

- Add optional `ciAllOs` manifest key (boolean, default false, rust-hybrid only). When `true`, the rust-hybrid `ci` job's matrix uses every `matrix.os` entry in declared order, with `fail-fast: false`, so each runner runs the gate and uploads its own `qa-evidence-<os>` artifact. When unset or `false`, rendered output is byte-identical to 0.2.28.

## 0.2.28

- macOS candidate targets accept self-hosted Xcode runner labels (`xcode-*`, e.g. `xcode-27`) as well as hosted `macos-*` labels.

## 0.2.27

- Add optional `qaEvidencePath` and `qaEvidenceRetentionDays` manifest keys for the rust-hybrid `ci` lane. When `qaEvidencePath` is set, the rendered workflow uploads `${{ runner.temp }}/<path>` after the gate with `if: always()`. Without the key, rendered output is unchanged.
