# Local patch ledger

Baseline: `parakeet-rs 0.3.5` at
`7d680db3d62e6aeaf68bc93aaae0ab891a09fae7`. This ledger covers every source
file that differs from the packaged upstream baseline.

| ID | Files | Local change and reason | Upstream disposition |
|---|---|---|---|
| RKASR-001 | `src/decoder.rs` | Comment wording only; executable decoder code is unchanged. | Restore upstream wording when the surrounding decoder patch is refreshed. |
| RKASR-002 | `src/execution.rs` | Add typed graph-optimization level, device index, and auxiliary-provider configuration. Removes application-prefixed environment reads while retaining CPU fallback and all upstream providers. | Propose generic provider/device/optimization controls upstream. |
| RKASR-003 | `src/model_nemotron.rs` | Route the small decoder/joint graph through the typed auxiliary provider instead of an application-prefixed environment variable. | Propose generic hybrid placement upstream. |
| RKASR-004 | `src/vocab.rs` | Add deterministic normalization and greedy existing-piece phrase encoding with context-bias tests. It never creates token IDs absent from the fixed vocabulary. | Propose phrase encoding/context bias upstream. |
| RKASR-005 | `src/model_transducer.rs`, `src/transducer.rs`, `src/lib.rs` | Add the generic offline RNN-T/TDT runtime, NeMo mel-stage support, static-window decoding, DirectML graph-fusion workaround selected from the model graph, context bias, timestamp grouping, app-neutral `Transducer`, and typed `TransducerOptions`. The previous mechanical source splits were rejoined. | Submit focused generic transducer/timestamp and DirectML correctness patches upstream; retain locally only until acceptance or recorded rejection. |
| RKASR-006 | `Cargo.toml`, `tests/runtime_contract.rs` | Rename the internal derivative, set `publish = false`, retain upstream features/dependencies plus required `half` support, bundle notices/provenance, and lock the app-neutral shared API contract. | RightKit-only packaging; not an upstream source change. |

No model name, model digest, weight, private URL, credential, application tier,
or product orchestration policy is part of this derivative. The two current
applications remain unchanged until the full Phase 6 gate passes.

## 2026-10-06 — Whisper decoding and VAD split

Added `whisper` (CoreML on macOS, whisper.cpp FFI everywhere), `routing`, and `chunked` modules. Donors: HeardRight `whisper_coreml`, `whisper_win_ffi`. Parakeet code paths are untouched. VAD lives in sibling crate `rightkit-vad`.

## 2026-10-06 — Parakeet CoreML path and HeardRight decode parity (RKASR-007 to RKASR-010)

| ID | Files | Change | Donor |
|---|---|---|---|
| RKASR-007 | `src/parakeet_core/**` | Runtime-agnostic Parakeet TDT core: beam search (`tdt_step`/`rnnt_step`), emission reward, per-hypothesis context bias, N0 detokenizer, score arithmetic, decode policy. Product env reads removed (explicit `emit_reward`), command-locator helper and unit tests left in the app / moved to e2e. `TimedToken`/`TranscriptionResult` are the crate's existing types. | HeardRight `heardright-parakeet-core` |
| RKASR-008 | `src/parakeet_coreml/**`, `src/whisper/coreml/bridge.rs` | In-process CoreML Parakeet (`CoreMlParakeet`): 4-stage static bundle, width-2 beam, predictor cache, bias, `.mlpackage` compile-once, pipeline.json validation, sampled model fingerprint. Bridge gains output introspection, `predict`, row-pitch fast reads. Fused graph / batching / look-ahead / health telemetry not ported. | HeardRight `coreml_asr/*`, ScrapeRight `coreml_asr.rs` |
| RKASR-009 | `src/longform.rs` | One padded-window long-form route with resumable cursor, cancel/progress, `SeamPolicy`, terminal-silence option, words/cues/SRT/VTT/JSON. | HeardRight `asr/{windowing,runtime}.rs`, ScrapeRight `asr_window.rs`, CutRight `window.rs` |
| RKASR-010 | `src/onnx_beam.rs`, `src/transducer.rs`, `src/model_transducer.rs`, `src/decoder.rs`, `src/error.rs` | ONNX beam decode over the shared core (`Transducer::transcribe_window_beam`/`transcribe_long`, `BeamOptions`), `ContextBias` unified with the core type, `Error::Cancelled`, `PartialEq` on result types. Model-backed gate pending (no ONNX Parakeet TDT export on the dev machine). | HeardRight `onnx_beam_decode.rs` |
