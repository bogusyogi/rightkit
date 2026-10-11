# Changelog

All notable changes to `rightkit-tts`. Versions follow Cargo semver; while the
version is `0.x`, a minor bump may carry breaking changes.

## Unreleased

- Fixed: model verification rewrote `.verified.json` on every successful check, even when nothing was hashed and the cache was unchanged, so a read-only check modified the model folder. It now writes only when the cache content changes (found by GenRight, whose unit tests touched a real model folder this way). Not built or run locally.

## 0.2.0 - 2026-10-11

Security (release builds): ONNX Runtime selection no longer reads `ORT_DYLIB_PATH`. Before this
release, `models::find_onnxruntime` went through `rightkit_ort::discover_runtime`, which honours that
variable in every build, and the client exported it to the worker. Now:

- Release builds never read `ORT_DYLIB_PATH`. The client removes it from the worker environment, and
  the worker clears any inherited value at start. Debug builds keep it as a developer override, the
  same gate `rightkit-ort` uses for its developer override.
- A runtime given explicitly with `QtsClient::with_ort_runtime` is used in every build; it must be a
  file, else synthesis fails with `runtime_missing`.
- Without an explicit runtime, release builds search `runtime/` beside the exe, the exe directory,
  and `<data>/runtime/qts`, skipping Windows-ML `System32` copies.

Other changes:

- Wire: `Request::Synth` gains `mode`, `instruct` and `speaker` after `voice_prompt`, all
  `#[serde(default)]`. A new `Mode` enum (`base`, `custom_voice`, `voice_design`; absent means
  `base`). `Reply` is unchanged, and old workers ignore the new keys.
- `rightkit-tts-worker` accepts `--mode <base|custom_voice|voice_design>`, `--vocoder`, `--data-root`
  and `--ort`, and loads the engine for that mode. Unknown arguments and an unknown `--mode` exit 64.
  The worker reports a missing model file as `model_missing: <mode>: <paths>`, which the client
  surfaces with code `models_missing`, the same class as its pre-spawn check. A worker holds one
  engine per mode (below) and never answers `mode_mismatch`.
- `SynthOptions` gains `mode`, `speaker`, `instruct` and `max_interior_silence_ms`. The new
  `SynthOptions::validate` checks the mode's inputs before any worker starts. `speaker` names are
  checked against `PRESET_SPEAKERS` in every build (case and surrounding space ignored, as the engine
  does); a test under `engine` keeps the list equal to `PresetSpeaker::ALL`.
- `max_interior_silence_ms: Option<usize>` (default `None`, output unchanged). Each chunk's interior
  silent runs (|sample| <= 327 at 24 kHz) longer than the cap shrink to exactly the cap, keeping the
  first `max / 2` and last `max - max / 2` samples. This runs after edge trimming and before the join
  pause. The cap converts milliseconds to samples at 24 kHz.
- Data root: `QtsClient::with_data_root(PathBuf)` resolves models, voices and runtime search under the
  caller's root (absolute). `try_data_root` remains the fallback. `RIGHTKIT_TTS_MODEL_DIR` still
  overrides the model folder. New `models::data_root_in`, `model_dir_in`, `try_voices_dir_in`,
  `find_onnxruntime_in`, and `QtsClient::clone_into_library` / `resolve_voice_record`.
- `models::MODE_MANIFESTS` lists the 1.7B CustomVoice and VoiceDesign exports (names only).
  Their sizes and SHA-256 are not pinned yet, so `models::verify_mode` skips them; the worker's
  path check still reports a missing file.
- Requires `rightkit-qts` 0.2.6 (`from_model_dir_for_mode_with_vocoder`, `PresetSpeaker::parse`).
- Requires `rightkit-transcript` 0.3.0 (new `EndSource::SourceBoundary` and `TranscriptError::MissingField`).
  rightkit-tts only constructs `EndSource::Interpolated` and matches no `EndSource` exhaustively, so
  it needs no new arm.
- Breaking for Rust callers: `SynthOptions` and `worker::run` changed. Construct `SynthOptions`
  with `..Default::default()`; the worker is started with `worker::WorkerConfig`.
- `worker_args` is a new module that parses the worker command line.

Locations, worker identity and idle policy (additive; defaults and environment behaviour for a
client that sets none of these are unchanged):

- Model locations: `QtsClient::with_model_dir(dir)` (default folder; beats `RIGHTKIT_TTS_MODEL_DIR`),
  `with_vocoder(path)` (default vocoder), `with_mode_location(ModeLocation { mode, model_dir,
  vocoder })` and the shorthand `with_mode_model_dir(mode, dir)`. `vocoder: None` means the mode
  folder's own `qwen3-tts-vocoder.onnx` when present, else the default vocoder. The client resolves
  all three modes and sends every location to the worker as `--mode-model-dir <mode> <dir>` and
  `--mode-vocoder <mode> <path>` before it reports ready. `WorkerConfig` gains `mode_locations` (and
  `with_mode_location`); `--model-dir` / `--vocoder` stay the starting mode's location.
- One worker, one engine per mode: modes load on first use and are kept; switching mode no longer
  respawns the worker. A respawn happens only when the client's configuration differs (executable,
  locations, runtime, data root, idle window). Cloning always uses the Base engine and the Base
  location, even when the worker started for `VoiceDesign`.
- Pre-spawn presence check per mode (`models::missing_for_mode`, `mode_model_files`): a missing
  file is `models_missing` before any worker starts. The 1.7B file names follow `rightkit-qts`
  (any of f16, q8_0, q6_k, q5_k, q4_k; a bundled export; per-component files). `models::verify_base`
  accepts a vocoder outside the model folder.
- `QtsClient::engine_info(&self, backend)` runs on the client's own configuration; the free
  `engine_info` is unchanged.
- `QtsClient::with_worker_exe(path)`: an exact path that beats `RIGHTKIT_TTS_WORKER`,
  `GENRIGHT_QTS_WORKER` and discovery; when the file is absent the result is `worker_missing`,
  never a fall back. `QtsClient::worker_exe()` returns what a launch would use; both go through one
  resolver.
- Idle policy: `WorkerConfig::idle_timeout: Duration`, worker argument `--idle-secs <n>` and
  `QtsClient::with_idle_secs(n)`. An explicit value wins over `RIGHTKIT_TTS_IDLE_SECS` and
  `GENRIGHT_QTS_IDLE_SECS`; `0` keeps its meaning (a zero window: the worker exits as soon as no
  request is waiting). With nothing explicit the environment decides, then 600 s
  (`worker_args::env_idle_timeout`, `WorkerArgs::idle_timeout`).
- Explicit ONNX Runtime: `models::bind_ort_runtime` sets `ORT_DYLIB_PATH` and puts the runtime's
  directory first on `PATH` before the engine loads, so Windows finds the DLLs beside it. Needs
  `rightkit-ort` 0.2.3 (`prepend_runtime_dir_to_path`; `export_discovered_runtime` now also adds the
  directory when `ORT_DYLIB_PATH` was already set).
- `protocol::PRESET_SPEAKERS` and `is_preset_speaker`; `ModeLocation` and `PRESET_SPEAKERS` are
  re-exported at the crate root.
- Breaking for Rust callers (unpublished 0.2.0): `WorkerConfig` gains `mode_locations` and
  `idle_timeout`.

## 0.1.1

(0.1.0 was already on crates.io; the entries below ship as 0.1.1.)

After the first-release feature list below, before publication:

- Requires `rightkit-transcript` 0.2.0 (the 0.1.x line is no longer in the workspace).
- `thiserror` is now compiled only with `system` (its only user, `SystemTtsError`); `client`-only
  builds drop it.
- Model manifest hashing (`models::sha256_file`) uses `rightkit_fs::digest`; the direct `sha2`
  dependency is gone.
- New fail-closed `models::try_data_root`, `try_model_dir`, `try_voices_dir` (error type
  `DataRootError`): no temp-directory fallback and a relative base is rejected. The client and
  worker use them, so an unresolvable data root now reports `models_missing` / `voice_missing`
  instead of reading from the OS temp directory. `data_root`, `model_dir`, `voices_dir` keep their
  signatures and legacy fallback.

First crates.io release. Features:

- `system`: OS voice backend (`say` and `afconvert` on macOS, SAPI on Windows)
  rendering to an isolated WAV with a word-timed manifest.
- `client`: QTS worker client (`QtsClient`), sentence chunking, and model
  manifest verification.
- `engine`, `metal`, `vulkan`: the Qwen3-TTS worker (`rightkit-tts-worker`
  binary) and its accelerator features.

Earlier history is in the git log of this repository; this file starts at the
publication point.
