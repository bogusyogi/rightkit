# Changelog

## 0.2.28

- macOS candidate targets accept self-hosted Xcode runner labels (`xcode-*`, e.g. `xcode-27`) as well as hosted `macos-*` labels.

## 0.2.27

- Add optional `qaEvidencePath` and `qaEvidenceRetentionDays` manifest keys for the rust-hybrid `ci` lane. When `qaEvidencePath` is set, the rendered workflow uploads `${{ runner.temp }}/<path>` after the gate with `if: always()`. Without the key, rendered output is unchanged.
