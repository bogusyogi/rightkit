# Changelog

## 0.2.30

- The `ci` lane runs `right-release ownership --advisory-when-pending` (needs @rightkit/release 0.2.137): until ownership approval keys are enrolled the scan asserts nothing, so CI reports findings as diagnostics instead of failing. `release-candidate` stays strict.

## 0.2.29

- Add optional `ciAllOs` manifest key (boolean, default false, rust-hybrid only). When `true`, the rust-hybrid `ci` job's matrix uses every `matrix.os` entry in declared order, with `fail-fast: false`, so each runner runs the gate and uploads its own `qa-evidence-<os>` artifact. When unset or `false`, rendered output is byte-identical to 0.2.28.

## 0.2.28

- macOS candidate targets accept self-hosted Xcode runner labels (`xcode-*`, e.g. `xcode-27`) as well as hosted `macos-*` labels.

## 0.2.27

- Add optional `qaEvidencePath` and `qaEvidenceRetentionDays` manifest keys for the rust-hybrid `ci` lane. When `qaEvidencePath` is set, the rendered workflow uploads `${{ runner.temp }}/<path>` after the gate with `if: always()`. Without the key, rendered output is unchanged.
