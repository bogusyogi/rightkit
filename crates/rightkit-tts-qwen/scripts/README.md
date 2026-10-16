# Qwen3-TTS 1.7B → QTS GGUF

`export_1p7b.py` exports official local **12Hz 1.7B CustomVoice or VoiceDesign**
checkpoints for rightkit-tts-qwen 0.2.0. Requires Python ≥3.10, NumPy & `gguf>=0.18.0`
in caller's Python environment. No Torch, Transformers, safetensors Python
package, model downloads or ONNX export are needed. BF16 safetensors decode
directly through NumPy; `gguf.quants.quantize` supplies upstream Q8_0 conversion.

## Usage

Header-only inventory requires Python standard library only; reads no weights:

```sh
python3 crates/rightkit-tts-qwen/scripts/inspect_safetensors_header.py /path/to/model.safetensors
```

From RightKit checkout:

```sh
python3 crates/rightkit-tts-qwen/scripts/export_1p7b.py \
  --model-dir /path/to/Qwen3-TTS-12Hz-1.7B-CustomVoice \
  --weights /Volumes/D/.cache/qts-1p7b/Qwen3-TTS-12Hz-1.7B-CustomVoice/model.safetensors \
  --mode customvoice \
  --out /path/to/qts-customvoice \
  --quant q8_0
```

For VoiceDesign, use `--mode voicedesign` & matching VoiceDesign config/weights.
`--model-dir` must contain `config.json`, `tokenizer_config.json`, `vocab.json`
& `merges.txt`. `generation_config.json` & `preprocessor_config.json` are not
consumed by GGUF loaders. Sampling remains caller-controlled.

Each invocation writes:

| Output | Tensor count | Expected storage types |
|---|---:|---|
| `qwen3-tts-1.7b-{mode}-talker-q8_0.gguf` | 316 | 198 Q8_0, 3 F16, 115 F32 |
| `qwen3-tts-1.7b-{mode}-code-predictor-q8_0.gguf` | 88 | 36 Q8_0, 30 F16, 22 F32 |
| `qwen3-tts-1.7b-{mode}-tokenizer.gguf` | 0 | Metadata only; 151936 token slots |

Reuse existing shared **`qwen3-tts-vocoder.onnx`** in output directory (or supply
its explicit path through `ModeModelPaths`). Exporter never reads, copies or
modifies vocoder. Filename resolution accepts Q8_0 split files via
[`ModeModelPaths::from_model_dir`, `src/model/paths.rs:57`](../src/model/paths.rs#L57).
Existing F16 or bundled GGUFs take precedence; choose an export directory
containing only intended mode artifacts when using automatic resolution.

Config identity, tensor names/shapes, safetensors byte ranges, tokenizer IDs
& speaker/dialect metadata are validated before writing. Missing/unmapped
tensors fail explicitly. Existing output GGUFs are never reused or overwritten.
All three files finish as temporary siblings before promotion to final names;
handled failures remove temporary files. Conversion streams row chunks rather
than loading 3.8 GB checkpoint or converted tensors into RAM.

Summary prints per-file tensor/storage counts, exact bytes/MiB, projection
& embedding shapes in **HF / GGML** order, tokenizer counts & preset table.

## Loader contract

References below name functions & source lines in rightkit-tts-qwen 0.2.0.
Metadata integers use **U32**, scalars use **F32**, dialects/identity use
**STRING** & tokenizer token/merge lists use **ARRAY of STRING**.

| Output / data | Consumer (`file:line`) | Exported keys or tensors |
|---|---|---|
| Talker identity | [`Qwen3Tts::load_mode`, `src/lib.rs:174`](../src/lib.rs#L174) | `qwen3-tts.tts_model_type=custom_voice\|voice_design`; `qwen3-tts.tts_model_size=1.7b` |
| Talker text dimensions | [`TtsTransformer::load_from_ggufs`, `src/pipeline/tts_transformer.rs:216`](../src/pipeline/tts_transformer.rs#L216) | `qwen3-tts.text.{vocab_size,embedding_dim}` |
| Talker decoder geometry | [`TtsTransformer::load_from_ggufs`, `src/pipeline/tts_transformer.rs:228`](../src/pipeline/tts_transformer.rs#L228) | `qwen3-tts.talker.{embedding_length,block_count,feed_forward_length,codec_vocab_size,num_codebooks,attention.head_count,attention.head_count_kv,attention.key_length,attention.layer_norm_rms_epsilon,rope.freq_base}` |
| Talker conditioning tokens | [`TtsTransformer::load_from_ggufs`, `src/pipeline/tts_transformer.rs:322`](../src/pipeline/tts_transformer.rs#L322) | `qwen3-tts.codec.{pad_id,bos_id,eos_id,think_id,nothink_id,think_bos_id,think_eos_id}`; `qwen3-tts.tts_{bos,eos,pad}_token_id`; `qwen3-tts.language.english_id`; `qwen3-tts.talker.codec_language_id.chinese` |
| Talker weights | [`TalkerWeights::load`, `src/pipeline/tts_transformer.rs:4111`](../src/pipeline/tts_transformer.rs#L4111) | `talker.{text_embd,codec_embd,codec_head,output_norm}.weight`; `talker.text_proj.{fc1,fc2}.{weight,bias}`; `talker.blk.{0..27}.*` |
| Predictor geometry (in predictor file) | [`predictor_config`, `src/pipeline/tts_transformer.rs:3887`](../src/pipeline/tts_transformer.rs#L3887) | `qwen3-tts.code_pred.{embedding_length,attention.key_length,attention.head_count,attention.head_count_kv,feed_forward_length,layer_count,vocab_size,attention.layer_norm_rms_epsilon,rope.freq_base}` |
| Predictor projection | [`projection_source`, `src/pipeline/tts_transformer.rs:4003`](../src/pipeline/tts_transformer.rs#L4003); [`validate_projection_shapes`, `:3990`](../src/pipeline/tts_transformer.rs#L3990) | `code_pred.small_to_mtp_projection.{weight,bias}` in predictor file; GGML shapes `[2048,1024]` & `[1024]` |
| Predictor weights | [`CodePredWeights::load`, `src/pipeline/tts_transformer.rs:4586`](../src/pipeline/tts_transformer.rs#L4586) | `code_pred.output_norm.weight`; `code_pred.blk.{0..4}.*`; `code_pred.codec_embd.{0..14}.weight`; `code_pred.lm_head.{0..14}.weight` |
| CustomVoice preset IDs | [`load_speaker_table`, `src/pipeline/tts_transformer.rs:4034`](../src/pipeline/tts_transformer.rs#L4034) | `qwen3-tts.talker.spk_id.{lowercase_speaker}`, sourced from `talker_config.spk_id`; present only in CustomVoice talker |
| CustomVoice dialects | [`load_speaker_languages`, `src/pipeline/tts_transformer.rs:4054`](../src/pipeline/tts_transformer.rs#L4054) | STRING `qwen3-tts.talker.spk_is_dialect.{speaker}` & U32 `qwen3-tts.talker.codec_language_id.{dialect}`; false entries omitted |
| Text tokenizer | [`TextTokenizer::load_from_gguf`, `src/pipeline/tokenizer.rs:48`](../src/pipeline/tokenizer.rs#L48) | `tokenizer.ggml.{tokens,merges,bos_token_id,eos_token_id,padding_token_id}`; actual `assistant` & newline tokens |

Both weight files contain complete explicit talker & predictor geometry.
Predictor uses **1024 hidden, 5 layers, 16 query heads, 8 KV heads, 128 head
dimension, 3072 FFN width**. Query width is 2048 despite 1024 hidden width;
exporter retains config head counts & validates actual Q/K/O tensor shapes.
Talker uses **2048 hidden, 28 layers, 16 query heads, 8 KV heads, 128 head
dimension, 6144 FFN width**. No tensor data transpose or RoPE permutation occurs;
only dimension descriptors reverse from NumPy to GGML order.

Both decoder block families export these weight suffixes:
`attn_norm`, `attn_q`, `attn_k`, `attn_v`, `attn_output`, `attn_q_norm`,
`attn_k_norm`, `ffn_norm`, `ffn_gate`, `ffn_up`, `ffn_down`.

Expected verification shapes:

| Tensor | HF shape | GGML shape |
|---|---|---|
| `talker.text_embd.weight` | `[151936,2048]` | `[2048,151936]` |
| `talker.codec_embd.weight` | `[3072,2048]` | `[2048,3072]` |
| `talker.text_proj.fc{1,2}.weight` | `[2048,2048]` | `[2048,2048]` |
| `talker.text_proj.fc{1,2}.bias` | `[2048]` | `[2048]` |
| `code_pred.small_to_mtp_projection.weight` | `[1024,2048]` | `[2048,1024]` |
| `code_pred.small_to_mtp_projection.bias` | `[1024]` | `[1024]` |
| `code_pred.codec_embd.{0..14}.weight` | `[2048,2048]` | `[2048,2048]` |
| `code_pred.lm_head.{0..14}.weight` | `[2048,1024]` | `[1024,2048]` |

Local CustomVoice config supplies nine presets:
`serena=3066`, `vivian=3065`, `uncle_fu=3010`, `ryan=3061`, `aiden=2861`,
`ono_anna=2873`, `sohee=2864`, `eric=2875`, `dylan=2878`.
Eric uses `sichuan_dialect=2062`; Dylan uses `beijing_dialect=2074`.
These values are read from each invocation's config, never hardcoded.
VoiceDesign exports no preset/dialect speaker table.

## Upstream differences & verification scope

Reference: local QTS v0.2.0 `scripts/export_model_artifacts.py`.
Tensor mapping & quantization exclusions match its `_map_tensor_name` &
`_should_quantize`: all vectors are F32; embeddings & output heads are F16;
eligible matrices, including width/text projections, are Q8_0. Q8_0 uses
upstream `gguf.quants.quantize`, including its rounding. Quantization errors
fail export instead of upstream's logged F16 fallback.

Upstream supports 0.6B Base only & omits 1.7B projection, independent predictor
geometry & preset/dialect keys. These additions follow loader code & official
configs; local safetensors headers confirm all 404 names/shapes in each mode.

Upstream `_load_tokenizer` only reads `vocab.json`, then pads unused entries.
Local 1.7B `vocab.json` ends at ID 151642; 33 added tokens live in
`tokenizer_config.json.added_tokens_decoder`. Exporter inserts these at exact
IDs before padding, preserving chat & TTS tokens. Explicit tokenizer IDs are
`bos=151644` (model `im_start_token_id`), `eos=151645`, `pad=151643`.
Upstream's missing-special-token behavior appears unsuitable for these inputs;
its intent is uncertain. Loader defaults corroborate exported boundary IDs.

GGUF v3 streaming layout follows vendored
`crates/rightkit-tts-qwen-sys/vendor/ggml/include/gguf.h:1` &
`src/gguf.cpp:1279`; metadata/tensor descriptors use little endian & 32-byte
alignment. `gguf` supplies enums & quantization; streaming serializer keeps
conversion memory bounded without Torch or an additional tensor spool.

Authoring verification: loader/config/header inspection & static Python syntax
parse only. Exporter, inference, downloads, builds & tests were not run.
