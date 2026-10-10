# Changelog

## 0.2.5 - 2026-10-10

- Hand-write the three `qwen3tts.prompt` protobuf messages (`TensorI32`, `TensorF32`, `VoiceClonePromptV2`) with `#[derive(prost::Message)]`. Field tags and wire types are unchanged; `proto/voice_clone_prompt.proto` stays as the schema reference. This removes `build.rs`, the `prost-build` and `protoc-bin-vendored` build dependencies, and the prost 0.13 build-time copy. New tests round-trip both checked-in fixtures (decode, encode, decode compares equal; the fixtures were written by another encoder that emits empty proto3 scalars, so their bytes are not compared) and pin the wire layout byte for byte.
- Replace the `rand` dependency with `getrandom`: the single sampling draw is now a uniform 24-bit `f32` in `[0, 1)` from the OS random source (same distribution as before). A random-source failure returns `Qwen3TtsError::InvalidInput` instead of panicking.
- Set `repository` to `https://github.com/bogusyogi/rightkit` and add `rust-version = "1.98.0"`, matching the other RightKit crates. `license` stays `Apache-2.0` because this crate is vendored from Apache-2.0 upstream.
- The 21 forwarded `ort`/ggml features are unchanged.

## 0.2.3 — 2026-10-09

- Depend on `rightkit-qts-ggml-sys` directly instead of the `rightkit-qts-ggml` facade. The `metal`, `vulkan`, `blas` and `native` features forward to `rightkit-qts-ggml-sys`. Public API is unchanged. The `rightkit-qts-ggml` crate is deprecated and no longer used by this crate.

## 0.2.2 — 2026-10-09

- Drop `ort/ndarray` from the ONNX Runtime dependency. The vocoder uses tuple/`Tensor`
  inputs and never touches `ndarray`, so no public API changes. Dependency tree: 97 to 93 crates
  (`rightkit cargo tree -p rightkit-qts -e normal,build`).

## 0.2.1 — 2026-10-08

- Fix 1.7B CustomVoice/VoiceDesign noise and runaway generation: mode synthesis
  now prefills the full text (non-streaming, as the official runtime does); the
  talker hidden state is marked as a graph output so GGML no longer reuses its
  buffer before download; projected models suppress EOS for the first two frames.
  0.6B Base output is unchanged (fingerprint 7701ed0f55fd8d71).
- Add `RIGHTKIT_QTS_DUMP_CODES` (first 32 codec frames) and `RIGHTKIT_QTS_TRACE_CB6`
  diagnostics; `mode_synth` example honours `QTS_GREEDY`.

## 0.2.0 — 2026-10-08

- Add independent talker/predictor configuration & optional
  `small_to_mtp_projection` weight/bias for Qwen3-TTS 1.7B (2048→1024).
  Project predictor prefill & later codebook inputs; retain talker-wide codec embeddings.
- Add `QtsMode`, nine named presets, `ModeSynthesizeRequest`, mode loading,
  synthesis/streaming/profiling methods, instruction prefill & validation.
  Resolve preset IDs/dialects from GGUF; omit VoiceDesign speaker embedding.
- Add mode-specific 1.7B split/bundled paths with shared 12 Hz ONNX vocoder.
- Add synthetic metadata/tensor/prompt/validation tests & API/export documentation.
- Preserve existing 0.6B Base APIs, struct literals, clone/ICL behavior & defaults.
