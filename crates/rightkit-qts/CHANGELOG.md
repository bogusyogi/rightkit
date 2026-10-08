# Changelog

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
