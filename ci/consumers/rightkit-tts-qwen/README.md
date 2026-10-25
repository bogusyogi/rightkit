# rightkit-tts-qwen 0.2.7 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-tts-qwen =0.2.7` exactly with its
default features (empty: the plain CPU build). GenRight reaches the crate through `rightkit-tts` and
selects `metal` or `vulkan` itself. The library crate is named `qts`. The dependency closure includes
`rightkit-tts-qwen-sys`, so the sealed archives build their vendored ggml with CMake on the host and
link it; the checked-in bindings for `x86_64-pc-windows-msvc` and `aarch64-apple-darwin` are used. It
runs on Windows and macOS in hosted qualification.

The program writes its own GGUF version 3 files byte by byte (metadata of four kinds and F32
tensors), so no model file is needed.

What it proves, in plain sentences, through the public API only:

- `SAMPLE_RATE_HZ`, the `SynthesizeRequest` defaults, `SynthesizeResult`,
  `StreamingSynthesizeResult`, `TalkerKvMode` (names, aliases, the refusal text), `QtsMode`
  (names and default) and `PresetSpeaker` (the nine names in order, `parse` ignoring case and
  surrounding space and refusing `uncle-fu`).
- `ModeSynthesizeRequest::validate` for each mode: the valid combinations, what each mode
  requires, what each mode rejects, and an invalid clone prompt refused through the request.
- `VoiceClonePromptV2`, `TensorF32` and `TensorI32`: an x-vector prompt and an in-context prompt
  survive a protobuf round trip, the accessors, and each validation refusal by exact text (schema
  version, missing or 2D embedding, shape and length mismatch, zero or empty shape, both or neither
  mode flag, reference text and code rules), and garbage bytes refused as a protobuf error.
- `ModelPaths` and `ModeModelPaths`: the 0.6B quantisation order, the 1.7B per-component names, a
  bundled export taking precedence, and `with_vocoder`.
- `GgufFile` on the generated files: `open`, `n_tensors`, `data_offset`, `key_index`, `get_u32`,
  `get_f32`, `get_str` (a number is not a string), `get_arr_str`, `tensor_info`, `read_tensor_f32`
  and `read_tensor_bytes` with exact contents, a missing tensor, and foreign and missing files
  refused as `InvalidGguf`; `load_and_validate` checking the checkpoint, then the vocoder, then the
  container. ggml prints one line to stderr for each GGUF refusal; those lines are expected.
- `TextTokenizer` built from a generated tokenizer file: `config`, byte-level BPE with a merge,
  `encode`, `encode_for_tts`, `encode_ref_for_tts`, `encode_for_voice_clone`,
  `encode_instruction_for_tts` (and its refusal without a `user` token), `decode` and
  `decode_token`, and the four load refusals (no tokens, empty vocabulary, no `assistant`, no
  newline token).
- `SpeakerEncoder` and `SpeakerEncoderConfig` over generated WAV audio: the defaults, the two
  configuration refusals, an embedding of the requested length with zero mean and RMS 0.25, equal
  audio giving an equal embedding, different audio a different one, 8 kHz and stereo input, and the
  refusals (not a WAV, no frames, silence, too short).
- `SynthesisStageTimings` (`total`, `average`, `format_table`), `CodecRolloutSubTimings`,
  `BackendKind::Cpu`, `VocoderExecutionProvider::Cpu`, `VocoderConfig`, `TtsTransformerConfig`,
  `CodePredictorConfig` and the `StreamingSynthesis` blanket implementation for closures.
- The engine and vocoder load refusals that happen before any transformer, backend or ONNX Runtime
  session exists: `Qwen3TtsEngine::load` (missing checkpoint, missing vocoder, a file that is not
  GGUF, a GGUF with no vocabulary), `load_mode` (a model type that does not match the mode, a missing
  predictor or talker, a tokenizer file with no vocabulary), `from_model_dir`,
  `from_model_dir_for_mode`, `from_model_dir_for_mode_with_vocoder`, and `Vocoder::load_from_onnx` on a
  missing file.
- The `Display` text of every `Qwen3TtsError` variant that can be built here.

What it deliberately does not prove: a loaded transformer or any synthesized waveform (that needs
real GGUF weights and a ggml backend), the vocoder session and ONNX Runtime, the `synthesize*`
methods, the `metal`, `vulkan`, `blas`, `native` and execution-provider features, GPU use, and the
model files themselves. Speaker-encoder values are checked as properties (length, mean, RMS, equal
and different), not as exact floats, so the result does not depend on a platform math library.

State: a per-process directory under the OS temp directory (generated GGUF, WAV and placeholder
files), removed at the end. No network, no process spawn, no GPU, no user data. The first build
compiles ggml with CMake and needs a C and C++ compiler and CMake on the host, as the crate's own
build does; the run itself takes about a second. Run only by the CI consumer lane.
