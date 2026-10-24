# rightkit-tts 0.2.3 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-tts =0.2.3` exactly with its default
features, which are `system` (the OS voice backend and the word-timed transcript) and `client` (the
QTS worker client). GenRight uses the `client` subset; the default set is a superset of it. It also
depends on `serde_json = "1"` to read and write the worker protocol. It runs on Windows and macOS in
hosted qualification, where the `say` and SAPI code paths compile in; nothing here calls either.

What it proves, in plain sentences, through the public API only:

- The sample-rate constants and the `wav` helpers: `encode_pcm16` (its 44-byte header),
  `decode_pcm16` (first channel of a stereo file, and refusal of truncated, foreign and non-WAVE
  input), `f32_to_i16` (clamped, scaled, truncated) and `silence`.
- Long-text chunking: `split_text` for one chunk, paragraphs with a 280 ms join, sentence packing
  with 200 ms joins, an unpunctuated sentence cut at whitespace without splitting a word, the
  20-character floor on the limit, blank input and non-ASCII sentence ends, `DEFAULT_MAX_CHARS` and
  `frame_budget`.
- The worker wire protocol: `Mode` (`as_str`, `parse`, serde names, default), `PRESET_SPEAKERS`
  and `is_preset_speaker`, `REPLY_PREFIX`, the exact JSON of `Request` (`Synth`, `Clone`, `Quit`)
  and `Reply` (`Ready`, `Done`, `Error`), a request from an older client with no mode fields, and
  unknown keys being ignored.
- Model locations: `ModeLocation`, `resolve_location` (defaults, the last entry wins, the folder's
  own vocoder before the default) and `EngineCache` (one load per mode, load order, a failed load
  retried).
- The worker command line: `WorkerArgs`, `worker_args::parse` and `to_command_line` as an exact round
  trip, each parse error text, and the idle window from the arguments, `RIGHTKIT_TTS_IDLE_SECS`, the
  legacy `GENRIGHT_QTS_IDLE_SECS` and the 600 s default.
- The model manifest and folders: `MODEL_ID`, `HF_REPO`, `MANIFEST` (names, byte counts, digest
  shape), `DOWNLOAD_BYTES`, `MODE_MANIFESTS`; `data_root_in`, `try_data_root`, `data_root`,
  `model_dir_in`, `try_model_dir`, `model_dir`, `try_voices_dir_in` and `voices_dir` with their
  environment overrides, legacy names and the relative-path refusal; `files_present`,
  `base_files_present`, `missing_for_mode`, `mode_model_files`, `verify`, `verify_base` and
  `verify_mode` on synthetic files (missing files, wrong sizes, the exact messages, unpinned 1.7B
  entries passing); `sha256_file` on known bytes; `find_onnxruntime_in`, `runtime_lib_name`,
  `default_backend` and `bind_ort_runtime` (an inherited `ORT_DYLIB_PATH` dropped, an explicit
  runtime exported with its folder first on `PATH`).
- The system backend's types: `chunk_sentences` (abbreviations, initials, decimals, domain names,
  character offsets in non-ASCII text, `?!`), `SystemTtsOptions` defaults, every `SystemTtsError`
  text, `SystemTtsSegment` and `SystemTtsManifest`, and `SystemTts::synthesize` refusing blank text
  before any voice is asked for.
- `SynthOptions` defaults and `validate` for all three modes with each refusal text, `language_id`,
  `SynthResult::rtf` and the `QtsError` text.
- `QtsClient` configuration (`new`, `with_idle_secs`, `with_worker_exe`, `with_model_dir`,
  `with_vocoder`, `with_mode_location`, `with_mode_model_dir`, `with_data_root`, `with_ort_runtime`)
  and every refusal that happens before a process could start, through the client and the free
  functions: `invalid_request`, `voice_missing`, `models_missing` (with the file list),
  `worker_missing` (an explicit path is never replaced by the environment or by discovery),
  `runtime_missing`, plus `worker_exe`, `engine_info`, `clone_from_wav`, `clone_into_library`,
  `resolve_voice_record`, `resolve_voice_record_in` and `cancel_active`, `shutdown` and
  `worker_pid`.

What it deliberately does not prove: speech rendered by `say` or SAPI (the recipe never asks for a
non-blank utterance on Windows or macOS), the Qwen3-TTS worker and the `engine`, `metal`, `vulkan`
and `dev-overrides` features, real model files (the 1.3 GB checkpoint size is never faked, so the
positive size check is not exercised), ONNX Runtime, and any audio output. No worker or runtime file
is ever executed or loaded: the worker and runtime placeholders are empty or tiny files that only
have to exist.

State: a per-process directory under the OS temp directory, removed at the end, and process
environment variables of the crate (`RIGHTKIT_*`, `GENRIGHT_*`, `ORT_DYLIB_PATH`, and `PATH`, which
is restored) that the program clears first and again after each step. No network, no process spawn,
no audio device, no user data. Runs in about a second. Run only by the CI consumer lane.
