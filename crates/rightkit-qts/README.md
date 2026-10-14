# rightkit-qts

Native Qwen3-TTS inference for Right Suite apps: GGUF and GGML talker and code-predictor models, a tokenizer GGUF, and a shared 12 Hz ONNX vocoder. It is vendored from QTS v0.2.0 (see [PINNED.md](PINNED.md)). The Rust library name is `qts`. It owns synthesis (base, custom voice, and voice design modes), streaming, model-path resolution, and the GGUF export contract. It does not own voice selection UX, model downloads, or worker lifetime. `rightkit-tts` wraps this crate in a worker with idle unload.

**Use it** through `rightkit-tts` in Right Suite apps. Use this crate directly only when an app needs the engine without the worker.

**Do not use it** to download models. Model files must already be on disk; the crate does not fetch them. It is not a speech recognizer.

## Features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `metal` | off | `rightkit-tts-qwen-sys/metal` | ggml Metal backend (Apple targets). |
| `vulkan` | off | `rightkit-tts-qwen-sys/vulkan` | ggml Vulkan backend. |
| `blas` | off | `rightkit-tts-qwen-sys/blas` | ggml BLAS backend. |
| `native` | off | `rightkit-tts-qwen-sys/native` | Builds ggml for the host CPU. |
| `acl`, `armnn`, `azure`, `cann`, `coreml`, `cuda`, `directml`, `migraphx`, `nnapi`, `nvrtx`, `onednn`, `openvino`, `qnn`, `rknpu`, `tensorrt`, `tvm`, `vitis`, `webgpu`, `xnnpack` | off | `rightkit-ort/<name>` | Execution-provider features for the vocoder's ONNX Runtime session. Each forwards to the same-named `rightkit-ort` feature (`Cargo.toml:21`). |

The default feature set is empty. This crate has no direct `ort` dependency; the ONNX Runtime pin (`=2.0.0-rc.13`, `load-dynamic`, API 24) is owned by `rightkit-ort` (`crates/rightkit-ort/Cargo.toml:59`).

## Platform support

- `metal` applies to Apple targets. `rightkit-tts-qwen-sys` ignores it with a build warning on other targets.
- `vulkan` is for hosts with a Vulkan SDK.
- The vocoder runs through ONNX Runtime. The execution provider is chosen by feature and by the `rightkit-ort` binding that the app configures.
- `cfg(target_vendor = "apple")` branches exist in `src/pipeline/backend.rs` and `src/pipeline/vocoder.rs`.

Published version: see `INDEX.md` at the repository root. Release notes: see [CHANGELOG.md](CHANGELOG.md).

## Compatibility

Existing `SynthesizeRequest`, `ModelPaths`, Base x-vector/ICL synthesis,
streaming, profiling & 0.6B filename resolution retain their public signatures.
Existing `synthesize()` also retains its zero-embedding fallback. Mode APIs are
additive, so existing struct literals need no new fields.

Set `RIGHTKIT_QTS_DUMP_CODES` to a file path (for example `<dir>/qts-codes.json`) to capture first 32 generated
codec frames as JSON `[[codebook_0, ..., codebook_15], ...]`. Capture excludes
reference frames, persists after each frame, & resets to `[]` on each rollout.
Recompute, cached, & streaming rollouts support it; path errors return IO errors.
Unset variable leaves synthesis unchanged.

## Example

The Mode API section below shows the call sequence (illustrative). The unit tests named in Synthetic tests are the compiled checks.

## Mode API

Illustrative; not compiled as a doctest. The compiled checks are the unit tests named in Synthetic tests.

```rust,ignore
use qts::{ModeSynthesizeRequest, PresetSpeaker, QtsMode, Qwen3TtsEngine,
          SynthesizeRequest};

let engine = Qwen3TtsEngine::from_model_dir_for_mode(
    "/models/customvoice", QtsMode::CustomVoice,
)?;
let request = ModeSynthesizeRequest {
    synthesis: SynthesizeRequest {
        text: "Welcome!".into(),
        ..Default::default()
    },
    mode: QtsMode::CustomVoice,
    speaker: Some(PresetSpeaker::Vivian),
    instruct: Some("Speak warmly & slowly.".into()),
    ..Default::default()
};
let audio = engine.synthesize_mode(&request)?;
# Ok::<(), qts::Qwen3TtsError>(())
```

```rust,ignore
pub enum QtsMode { Base, CustomVoice, VoiceDesign }
pub enum PresetSpeaker {
    Vivian, Serena, Uncle_Fu, Dylan, Eric, Ryan, Aiden, Ono_Anna, Sohee,
}
pub struct ModeSynthesizeRequest<'a> {
    pub synthesis: SynthesizeRequest,
    pub mode: QtsMode,
    pub instruct: Option<String>,
    pub speaker: Option<PresetSpeaker>,
    pub voice_clone_prompt: Option<&'a VoiceClonePromptV2>,
    pub speaker_embedding: Option<&'a [f32]>,
}
```

| Mode | Required | Optional | Rejected |
|---|---|---|---|
| Base | Voice-clone prompt | Talker-wide speaker embedding override | Preset speaker, instruct |
| CustomVoice | Preset speaker | Instruct (ignored by 0.6B, matching upstream) | Clone prompt, embedding override |
| VoiceDesign | Nonempty instruct | Synthesis settings | Speaker, embedding, clone prompt |

Request mode must match loaded checkpoint mode. Presets use codec embeddings at
GGUF-provided IDs. Chinese/Auto preset dialect overrides also come from metadata.
VoiceDesign omits speaker embedding entirely. CustomVoice/VoiceDesign synthesis
prefills all text plus TTS EOS before codec BOS, matching the official wrappers'
`non_streaming_mode=true` default, including when audio delivery is streamed.
Base synthesis retains incremental text/codec prefill.
Instruction tokens are projected
& prepended separately, preserving assistant/codec alignment:

```text
<|im_start|>user\n{instruct}<|im_end|>\n
<|im_start|>assistant\n{text}<|im_end|>\n<|im_start|>assistant\n
```

Public additions (`Result<T>` below means `Result<T, Qwen3TtsError>`):

```rust,ignore
QtsMode::as_str(self) -> &'static str
PresetSpeaker::ALL: [PresetSpeaker; 9]
PresetSpeaker::as_str(self) -> &'static str
ModeSynthesizeRequest::validate(&self) -> Result<()>

Qwen3TtsEngine::load_mode(paths: ModeModelPaths, mode: QtsMode) -> Result<Self>
Qwen3TtsEngine::from_model_dir_for_mode(dir: impl AsRef<Path>, mode: QtsMode) -> Result<Self>
Qwen3TtsEngine::mode(&self) -> QtsMode
Qwen3TtsEngine::synthesize_mode(&self, req: &ModeSynthesizeRequest<'_>) -> Result<SynthesizeResult>
Qwen3TtsEngine::synthesize_mode_with_profile(&self, req: &ModeSynthesizeRequest<'_>) -> Result<(SynthesizeResult, SynthesisStageTimings)>
Qwen3TtsEngine::synthesize_mode_streaming<S: StreamingSynthesis + Send>(&self, req: &ModeSynthesizeRequest<'_>, sink: &mut S) -> Result<StreamingSynthesizeResult>

TtsTransformer::load_from_ggufs(talker: &GgufFile, predictor: &GgufFile) -> Result<Self>
TtsTransformer::code_predictor_config(&self) -> &CodePredictorConfig
TtsTransformer::preset_speaker_id(&self, speaker: PresetSpeaker) -> Result<i32>
TtsTransformer::preset_speaker_embedding(&self, speaker: PresetSpeaker, thread_count: usize) -> Result<Vec<f32>>
TtsTransformer::prepend_instruction(&self, inputs: &mut PreparedPrefillInputs, tokens: &[i32], thread_count: usize) -> Result<()>
TextTokenizer::encode_instruction_for_tts(&self, instruct: &str) -> Result<Vec<i32>>
GgufFile::get_str(&self, key: &str) -> Option<String>
```

`CodePredictorConfig` exposes `hidden_size`, `n_layers`, `n_attention_heads`,
`n_key_value_heads`, `intermediate_size`, `head_dim`, `rms_norm_eps`, `rope_theta`
& `vocab_size`. Legacy `TtsTransformer::config()` remains talker configuration.
Predictor embeddings remain talker-wide; projection runs before predictor
layers for both prefill & every codebook step. Thus 1.7B uses 2048→1024 while
0.6B uses identity with no projection tensors.

## Paths & GGUF export contract

```rust,ignore
pub struct ModeModelPaths {
    pub talker_gguf: PathBuf,
    pub code_predictor_gguf: PathBuf,
    pub tokenizer_gguf: PathBuf,
    pub vocoder_onnx: PathBuf,
}
ModeModelPaths::from_model_dir(dir: impl AsRef<Path>, mode: QtsMode) -> Self
```

Default split filenames use `mode = base | customvoice | voicedesign`:

- `qwen3-tts-1.7b-{mode}-talker-f16.gguf`
- `qwen3-tts-1.7b-{mode}-code-predictor-f16.gguf`
- `qwen3-tts-1.7b-{mode}-tokenizer.gguf`
- `qwen3-tts-vocoder.onnx` (shared across sizes & modes)

Talker/predictor resolution also accepts `q8_0`, `q6_k`, `q5_k` & `q4_k`, in
that preference order after F16. If a bundled QTS file named
`qwen3-tts-1.7b-{mode}-{quant}.gguf` exists, all three GGUF paths use it.
Explicit paths can name any compatible export. Filenames describe local export
conventions, not verified downloadable distributions; community GGUFs need
QTS tensor names & metadata.

Exports retain existing `talker.*` & `code_pred.*` tensor names & tokenizer
metadata. New contract:

- Talker width: `qwen3-tts.talker.embedding_length` (also `talker.hidden_size`
  under same namespace, legacy `qwen3-tts.embedding_length`, or output-norm
  tensor width).
- Predictor metadata: `qwen3-tts.code_pred.{embedding_length,attention.key_length,
  attention.head_count,attention.head_count_kv,feed_forward_length,layer_count,
  vocab_size,attention.layer_norm_rms_epsilon,rope.freq_base}`. Namespace
  `qwen3-tts.code_predictor.*` also works; `hidden_size` & `block_count` aliases
  are accepted. Missing width/head counts/FFN width derive from predictor
  norm/Q/K/FFN tensors; remaining values inherit legacy talker defaults.
- Optional projection: `code_pred.small_to_mtp_projection.weight` & `.bias`.
  Prefixes `talker.code_predictor.small_to_mtp_projection` &
  `small_to_mtp_projection` also work, in predictor or talker file. GGML shapes
  must be `[talker_width, predictor_width]` & `[predictor_width]`; differing
  widths require both tensors. F32/F16/quantized weights use normal GGML loading.
- Presets: U32 `qwen3-tts.talker.spk_id.{lowercase_speaker}`. IDs must fit codec
  vocabulary; absent requested presets fail explicitly. No numeric preset IDs
  are built into library.
- Optional dialects: string `qwen3-tts.talker.spk_is_dialect.{speaker}` plus U32
  `qwen3-tts.talker.codec_language_id.{dialect}`. Boolean false means no dialect.
- Optional model identity strings: `qwen3-tts.tts_model_type`
  (`base`, `custom_voice`, `voice_design`) & `qwen3-tts.tts_model_size`
  (`0.6b`, `1.7b`). Explicit loader mode applies when identity metadata is absent.

## Synthetic tests

These are the crate's unit tests in `src/`. Run them with `rightkit cargo test -p rightkit-qts`.

Unit fixtures use tiny local GGUFs & tensors. Coverage includes independent
2048/1024 metadata, 4→2 projection math/bias, cached/recompute equivalence for
split/equal widths, separate-file loading, invalid projection shapes,
metadata-derived presets/dialects, exact prompt token strings for every mode,
instruction-prefill placement, omitted design speaker & request validation.
No model downloads are required.

| New unit test | Assertion |
|---|---|
| `split_width_projection_preserves_talker_embeddings` | Separate 4/2 widths; exact two-row weight+bias output; codec embeddings remain width 4. |
| `predictor_cached_and_recompute_match_with_split_or_equal_widths` | Prefill & later codebook logits agree within F16 tolerance with/without projection. |
| `projection_shape_errors_fail_before_graph_construction` | Identity allowed at equal widths; absent/partial/transposed/bad-bias projections rejected at split widths. |
| `predictor_tensor_fallback_reads_independent_geometry` | Norm/Q/K/FFN tensors supply predictor width, head counts & FFN width without metadata. |
| `one_seven_b_predictor_metadata_is_not_talker_geometry` | 2048/1024 widths & independent FFN metadata; projection shape follows split. |
| `separate_ggufs_read_predictor_config_and_projection_from_either_file` | Predictor metadata comes from predictor file; projection may reside in either file. |
| `presets_use_metadata_ids_and_dialect_language` | Arbitrary GGUF IDs select correct codec row; missing presets error; dialect applies only to Chinese/Auto. |
| `instruction_prefill_precedes_assistant_and_design_omits_speaker` | Design omits one speaker row; projected instruction precedes original prefill; trailing text/pad unchanged. |
| `exact_prompt_token_strings_for_modes` | Exact Base assistant sequence & CustomVoice/VoiceDesign user+assistant sequences; uninstructed CustomVoice stays unchanged. |
| `instruction_requires_complete_user_role` | Missing/unencodable `user` role returns tokenizer error. |
| `optional_and_0_6b_instructions_are_omitted` | Missing/empty instructions emit no tokens; 0.6B ignores instruct; enabled 1.7B emits exact user turn. |
| `valid_mode_combinations` | Base clone+optional embedding, instructed preset & described design requests validate. |
| `missing_required_mode_inputs` | Missing clone/preset/instruct & blank design instruct produce errors. |
| `incompatible_conditioning_is_rejected` | Reject all tested cross-mode clone/preset/instruct/embedding conflicts. |
| `mode_specific_1_7b_paths_share_vocoder` | All three modes resolve distinct default talker/predictor/tokenizer filenames & shared vocoder. |
| `mode_paths_resolve_split_quantization_or_bundled_export` | Existing split quantized files resolve independently; bundled export supplies all GGUF components. |

## Changes

See [CHANGELOG.md](CHANGELOG.md).
