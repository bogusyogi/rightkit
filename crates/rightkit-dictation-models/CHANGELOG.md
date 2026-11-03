# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-model-store` (tauri-app-next).
- `model_paths`: the bundled keyword-spotting model resolution drops the Sherpa naming: `SHERPA_KWS_SUBDIR` is `KWS_SUBDIR`, `SherpaKwsCandidate(Kind)` is `KwsCandidate(Kind)`, and `sherpa_kws_candidates`, `select_sherpa_kws_dir`, `sherpa_kws_dir`, `sherpa_kws_ready`, `sherpa_kws_missing` are `kws_candidates`, `select_kws_dir`, `kws_dir`, `kws_ready`, `kws_missing`. Resolution order and the required model files are unchanged; the telemetry message `sherpa_kws_dir_resolved` is now `kws_dir_resolved`. Keyword spotting is the Rust `rightkit_asr::kws`. Staged in the unreleased 0.1.0 section (this crate has not been published).
