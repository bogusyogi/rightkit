# Shared Whisper route (rightkit-asr 0.2.0 + rightkit-models) — requirements from HeardRight and ScrapeRight

Decision (Adrian, 2026-10-11, via ScrapeRight): no app-side Whisper; Whisper is a shared model and runtime through rightkit-asr for HeardRight and ScrapeRight. Ledger #24.

## HeardRight today (report from the HeardRight Windows chat, 2026-10-11; Haiku read-only sweep, cites not re-verified; paths under heardright/tauri-app-next unless noted; inf = heardright-inference/src)

- Engines: Windows whisper.cpp 1.9.3 via libloading FFI (inf/whisper_win_ffi.rs:45 ABI pin, :237-244 version refusal; wrapper + whisper-cli fallback inf/whisper_win.rs:67-186,217-338). macOS native CoreML decode loop via ort (inf/whisper_coreml/engine.rs, mel.rs Rust log-mel, tokenizers 0.23.2). iOS already on RightKitSwift WhisperEngine (ios-native/HeardRight/LocalASRRuntimeStore.swift:2,421-443).
- Models (all large-v3-turbo, Pro): macOS `whisper-multi` CoreML tar (heardright-models.adrdsouza.workers.dev/whisper-multi.tar.gz, sha256 046ce2ae…; heardright-model-store/src/whisper_model.rs:72-76 → <app-data>/models/whisper-multi). Windows `whisper_turbo_q5_k.bin` ggml q5_k, 574,041,195 bytes, archive whisper-win.tar.gz sha256 e4fa834a… (:78-89 → <app-data>/models/whisper-win; the .bin itself has no hash pin). iOS `whisper-multi` tar sha a37fc293… (ModelArtifactInstaller.swift:63-71), URL from HR_MODEL_WHISPER_URL. No Whisper manifest ships.
- CoreML assets (macOS): AudioEncoder.mlmodelc + TextDecoder.mlmodelc (engine.rs:67-68); MelSpectrogram not loaded; tokenizer.json beside the bundle (mel.rs:85-91); lookup order downloaded copy → HR_WHISPER_COREML_MODEL → <models>/coreml/whisper-multi (inf/asr/runtime.rs:92-108); one-time ANE compile `prepare-whisper`, marker .ane-prepared (whisper_model.rs:289-320).
- Runtime delivery (Windows): right-release.config.mjs:92-97 ships whisper-cli.exe, whisper.dll, ggml.dll, ggml-base.dll, ggml-cpu.dll, ggml-vulkan.dll (DLLs built outside the repo).
- Routing: Whisper when dictation_language != auto or asr_backend starts with `whisper`; `multi` = Whisper auto (inf/asr/runtime.rs:737-744 macOS, :765-779 Windows). 100 language codes incl. yue. Detection: macOS argmax at SOT (engine.rs:251-279) locked per recording; Windows whisper.cpp auto; iOS fixed auto with a lease. No Pro check at route/load time, only at download.
- Decode: no VAD; macOS 30 s padded windows with quiet cuts; Windows whole buffer, deadline 6× audio with 10 s floor; text-only (timestamps suppressed), no_context, greedy; initial_prompt declared but never set; n0 filler strip; no streaming partials.

## What the shared route must cover

1. rightkit-asr: whisper.cpp 1.9.3 dynamic-library route on Windows (ggml + Vulkan variant) and macOS; CoreML encoder/decoder + tokenizer route on macOS with the ANE prepare step; per-recording language lock; the 100-language list; detect-language-only call (ScrapeRight) and auto-detect decode; text-only greedy decode with deadline; word-level timing option (ScrapeRight). CLI fallback is dropped once the in-process route is proven.
2. rightkit-models: catalogued, hash-pinned entries for the CoreML `whisper-multi` bundle (encoder, decoder, tokenizer.json), the ggml `whisper_turbo_q5_k.bin` (pin the .bin hash, not only the archive), and the iOS bundle; one shared install under the Suite root; a Whisper manifest.
3. rightkit-media catalog: whisper.cpp 1.9.3 per-platform native-library rows (dylib / dll + ggml dlls) replacing `whisper-cpp-8ef5277-any`; delivered through the verified-tool-artifact path (#20).
4. Apps keep: Pro gating (licensing), routing settings and UI, filler stripping policy, iOS RightKitSwift consumption.

## HeardRight-side defects (to Adrian, not RightKit)

whisper_model.rs:148-161 `installed()` accepts only .gguf while the shipped file is .bin; macOS installed() ignores tokenizer.json though the loader needs it; src-tauri/src/whisper_mel_128.bin unreferenced; stale iOS smoke references.
