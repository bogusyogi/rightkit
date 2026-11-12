# Changelog

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7, `rightkit-daemon` 0.5 -> 0.8, `rightkit-dictation-command-host` 0.1 -> 0.2, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-inference` 0.1 -> 0.2, `rightkit-dictation-runtime` 0.1 -> 0.2, `rightkit-dictation-text-pipeline` 0.1 -> 0.2, `rightkit-onnx` 0.2 -> 0.3. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

## Unreleased

- The `guardrail_replay` usage line is a code span, so rustdoc no longer reads `<path>` as an unclosed HTML tag under `-D warnings` (Mac release compile 2026-11-07).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-engine` (tauri-app-next).
- Sherpa removed: the `rust-kws` cargo feature is removed and the `sherpa_kws` re-export is `kws` (`kws::Kws`). The `sherpa_kws_probe_replay` example is deleted; the KWS probe examples lose their `--backend` option and the `backend` column. Test-mode directory override is `RIGHTKIT_KWS_DIR`.
- Staged in the unreleased 0.1.0 section: this crate has not been published, so the removal is not a version bump.
