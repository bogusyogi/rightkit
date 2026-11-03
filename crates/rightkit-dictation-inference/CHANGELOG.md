# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-inference` (tauri-app-next).
- Keyword spotting runs only on the Rust spotter, `rightkit_asr::kws` (rightkit-asr 0.4.0). The Sherpa-ONNX C API backend is gone: no `libloading` of a Sherpa library, no `KwsBackend`, no `RIGHTKIT_KWS_BACKEND` or `RIGHTKIT_SHERPA_LIBRARY_PATH` switch, and the `rust-kws` cargo feature is removed (the spotter is always compiled).
- `sherpa_kws` is now `kws` and `SherpaKws` is `kws::Kws`. `Kws::load(dir)`, `Kws::load_with_switches(dir, switches)` and `Kws::load_for_replay_with_paths(dir, paths)` replace the `*_with_backend` / `*_and_backend` loaders; `Kws::backend` is removed. `engine_paths::sherpa_kws_dir` is `kws_dir` (field `EnginePaths::kws_dir`), and its test-mode override is `RIGHTKIT_KWS_DIR` (was `RIGHTKIT_SHERPA_KWS_DIR`). `AsrRuntime::SherpaKws` is `AsrRuntime::Kws` and its runtime name is `kws`.
- Tests: the Sherpa-vs-Rust per-snapshot comparison tests are removed; the Rust cascade tests stay. Probe examples lose their `--backend` / `backend` arms.
- Staged in the unreleased 0.1.0 section: this crate has not been published, so the removal is not a version bump.
