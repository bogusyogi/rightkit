# Changelog

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
