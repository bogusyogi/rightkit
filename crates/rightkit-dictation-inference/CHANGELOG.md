# Changelog

## Unreleased

- **Breaking:** `calibration::run(models_dir, samples, collect_diagnostics)` takes the diagnostics option explicitly; the `RIGHTKIT_CALIBRATION_COLLECT_DIAGNOSTICS` environment switch is removed.
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.
- Every environment switch and test-mode path removed: no `rightkit_dictation_core::dev_var!`/`dev_var_os!` read remains (engine path overrides, KWS, VAD/ORT, CoreML and Whisper probes and A/B levers, ONNX beam experiments and time-synchronous pruning, tail-noise padding, beam width, context-bias knobs). Each site now has the behaviour the variable's absence always had. `KwsSwitches::from_env` is gone (`default()` is used), `FrontendLookaheadConfig::from_env` and `AsrRuntime::prepare_frontend_lookahead` are gone, `take_fused_probe_counts`/`take_decoder_probe_counts` are gone, the aligned-array probe (`coreml::aligned_probe`, `coreml_aligned_probe.m`) is gone, and the Windows whisper tuning struct (legacy init, GPU device, flash attention, threads, audio context) is gone. The `windows_engine_commit_probe` example is removed. Adrian, 2026-11-03: "My decision was no unit tests. Only e2e." and "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore."
- File transcription runs only a catalog-verified ffmpeg: the `DevFfmpeg::try_unverified` fallback and the `allow_path` PATH lookup are removed (rightkit-ffmpeg no longer has `dev-overrides`).
- Version stays 0.1.0: the crate is unpublished, so these removals fold into the unreleased first release.

## 0.1.0 (unreleased)

- First RightKit release, moved from `heardright-inference` (tauri-app-next).
- Keyword spotting runs only on the Rust spotter, `rightkit_asr::kws` (rightkit-asr 0.4.0). The Sherpa-ONNX C API backend is gone: no `libloading` of a Sherpa library, no `KwsBackend`, no `RIGHTKIT_KWS_BACKEND` or `RIGHTKIT_SHERPA_LIBRARY_PATH` switch, and the `rust-kws` cargo feature is removed (the spotter is always compiled).
- `sherpa_kws` is now `kws` and `SherpaKws` is `kws::Kws`. `Kws::load(dir)`, `Kws::load_with_switches(dir, switches)` and `Kws::load_for_replay_with_paths(dir, paths)` replace the `*_with_backend` / `*_and_backend` loaders; `Kws::backend` is removed. `engine_paths::sherpa_kws_dir` is `kws_dir` (field `EnginePaths::kws_dir`), and its test-mode override is `RIGHTKIT_KWS_DIR` (was `RIGHTKIT_SHERPA_KWS_DIR`). `AsrRuntime::SherpaKws` is `AsrRuntime::Kws` and its runtime name is `kws`.
- Tests: the Sherpa-vs-Rust per-snapshot comparison tests are removed; the Rust cascade tests stay. Probe examples lose their `--backend` / `backend` arms.
- Staged in the unreleased 0.1.0 section: this crate has not been published, so the removal is not a version bump.
