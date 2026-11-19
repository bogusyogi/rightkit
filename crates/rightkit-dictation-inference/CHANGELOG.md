# Changelog

## 0.2.3 (2026-11-19)

- Allow hosts to set bounded wake-spotter thread, beam, blank, timing and onset-history options. Default settings preserve the existing wake policy.
- Include chosen cut timing and source in onset diagnostics when a non-default cut source is selected.

## 0.2.2 (2026-11-16)

Host switches for the wake detector's cut policy and debounce, so a host that runs its own fork of `kws.rs` can move onto this crate. Additive and default-equal: a host that sets none of the new `KwsSwitches` fields gets the 0.2.1 behaviour, cut samples and labels.

- New `KwsSwitches::cut_source` (`KwsCutSource`: `WakeCandidate` default, `ConfirmAligned`, `ConfirmFirstToken`, `ConfirmEarlier`, `WakeWithinConfirm` and the three `*Emission` variants) and `KwsSwitches::cut_source_shift_ms` (default -200, read by `ConfirmAligned` only). With a non-default source a latched command's cut is re-decided from the confirm path's first-token sample (aligned estimate or emission timestamp) behind a temporal plausibility guard; the sources other than `ConfirmAligned` return no boundary (the command keeps its text, carries no timed token, no audio cut is attempted) instead of falling back to the wake candidate's cut. The default source returns the wake candidate untouched, so the cut and its source label are exactly as before.
- New `KwsSwitches::wake_refractory_samples` (default 0, off) and `KwsSwitches::wake_start_proximity_samples` (default 0, off): drop a wake detection that starts within the window after the previous accepted detection's start (refractory, survives candidate expiry, cleared by a reset), or within the window of the held candidate's start (proximity), before the overlap rule, which still runs.
- `KwsSwitches::validate` rejects a non-default `cut_source` combined with `fricative_strict` (the wake candidate then carries no cut) or `shared_front_end` (not qualified together).
- Not ported, still different from a host's fork: configurable threads, beam width, trailing-blank, decode step, confirm context, confirm timeout, fresh-confirmation tail, keyword-span and onset-history parameters (constants here); the onset report's cut-evidence fields; the text of three stream error messages.
- The package no longer ships `src/asr_stage_timing.rs`, a benchmark stage-timing type that `src/lib.rs` never declared and nothing used (the `rightkit_asr_stage_timing` tracing target is unrelated). No public item, feature or behaviour changes; the compiled library is unchanged.

## 0.2.1 (2026-11-16)

HeardRight's fork of this crate (`heardright-inference`) is compared with it again, so the fork can be replaced by this crate. Everything below is additive; a host that makes none of these calls gets exactly the behaviour, output and stderr text of 0.2.0.

- New host option: `configure_asr_stderr_prefix(&'static str) -> bool` sets the prefix of the two ASR diagnostic lines the CoreML decoder writes to stderr, once, before the first decode (`<prefix>WINDOW_STATS_JSON=...`, `<prefix>DECODER_CALLS ...`). Default `RIGHTKIT_ASR_`; the first call wins and a later one returns `false`. `asr_stderr_prefix()` reads the value in force. The `tracing` target of the stage-timing events stays `rightkit_asr_stage_timing`: a target must be a constant, and nothing filters on it except the default filter of the `windows_asr_bench` example.
- New: `kws_cascade` module that re-exports every item of `kws` and adds `KwsCascade`, an alias of `kws::Kws`, so a host that names the cascade `kws_cascade::KwsCascade` links unchanged.
- New: `CutResolverSwitches` (seam search on or off, the VAD plausibility pre-roll in samples, the residue strip on or off, the cleanup-first forced alignment on or off, the straddling-word tolerance in seconds). `CutResolverSwitches::default()` is the shipped behaviour: seam search on, 4000 samples, residue strip on, alignment on, 0.16 s.
- New: `resolve_trigger_audio_cut_with_switches`, `control_cleanup::cleanup_confirmed_control_with_switches` (explicit `allow_exact_retry`) and `strip_control_residue` (KWS-onset residue strip, then the fired control-tail strip). `resolve_trigger_audio_cut`, `cleanup_confirmed_control` and `cleanup_confirmed_control_with_exact_retry` keep their signatures and call the new code with default switches.
- New: `wake_handoff` (`wake_handoff_start`, `wake_handoff_audio`, `IDLE_AUDIO_WINDOW_SAMPLES`, `WAKE_HANDOFF_MARGIN_SAMPLES`): the idle-wake to recording handoff arithmetic.
- Compared and kept different: `kws.rs` against HeardRight's `kws_cascade.rs` (merged separately); `calibration::run` takes `collect_diagnostics` here and reads an environment switch there; the dead evaluator probes, ONNX beam experiments and hypothesis-batch configuration HeardRight still carries are not ported.

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-asr` 0.4 -> 0.5, `rightkit-dictation-core` 0.2 -> 0.3, `rightkit-dictation-text-pipeline` 0.1 -> 0.2, `rightkit-onnx` 0.2 -> 0.3, `rightkit-vad` 0.1 -> 0.2. 0.1.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.0 in place (#1572). No source change in this crate since 0.1.0.

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
