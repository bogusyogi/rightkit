# Changelog

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
