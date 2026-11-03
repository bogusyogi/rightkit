# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-runtime` (tauri-app-next).
- Sherpa removed: keyword spotting is the Rust `rightkit_asr::kws` through `rightkit-dictation-inference`'s `kws::Kws`. The `rust-kws` cargo feature is removed; the `sherpa_kws` re-export is `kws`; `engine_paths::EnginePaths::sherpa_kws_dir` is `kws_dir`; the test-mode directory override is `RIGHTKIT_KWS_DIR` (was `RIGHTKIT_SHERPA_KWS_DIR`); the hey-wake event model label is `kws-hey-zephyr` (was `sherpa-onnx-hey-zephyr`) and the control classifier result is `control_complete_kws` (was `control_complete_sherpa`).
- Staged in the unreleased 0.1.0 section: this crate has not been published, so the removal is not a version bump.
