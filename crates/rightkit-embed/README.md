# rightkit-embed

Product-neutral local text embeddings and a durable SQLite vector store for Right Suite apps.

- `Embedder` runs a local ONNX embedding model. `Embedder::load` uses caller-supplied ONNX Runtime and model paths. It never downloads or silently switches models. It runs two models and picks one from the model directory, never from a setting: EmbeddingGemma 2 Q4 (`embeddinggemma2-q4`, the text-only export; selected when the directory's `config.json` has `model_type` `embedding_gemma2`) and the older EmbeddingGemma 300M Q4 (`embeddinggemma-300m-q4`, a directory with no `config.json` or another top-level `model_type`; a `config.json` that does not parse fails the load, `src/embedder.rs:208`). The suite's current model is EmbeddingGemma 2; vectors from the two models are not comparable, and `pipeline_id()` changes with the model. Both use the Gemma document prompt (`title: none | text: `) and query prompt (`task: search result | query: `), L2 normalisation, and Matryoshka prefix truncation (native 768, default 256, at most 2,048 tokens).
- `VectorStore` keeps little-endian `f32` vectors in SQLite and ranks them by cosine similarity in Rust.

Catalog capability: `embeddings`. Published to crates.io (see [INDEX.md](../../INDEX.md)). Licensed under MIT OR Apache-2.0.

## When to use it

- Use it for on-device text embeddings with a model the app ships or downloads itself.
- Use `VectorStore` when vectors must survive restarts and be searched by cosine similarity.

Do not use it to fetch models, and do not use it to pick a model at run time. Model identity and downloads belong to the app.

## Cargo features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `inference` | on | `rightkit-onnx` (owns the `ort` pin and loads it dynamically), `tokenizers` (`onig`), `rightkit-fs` (`digest`), `serde_json` (`Cargo.toml:16`) | `Embedder`, `EmbedderConfig`, `ExecutionProvider`, `truncate_and_normalize`. |
| `vector` | on | `rusqlite` and `rightkit-db` (both without default features; `Cargo.toml:17`) | `VectorStore` (persistent cosine store) and `cosine_similarity`. |

`--no-default-features --features vector` builds the store without `ort` or `tokenizers`. Accelerator support comes from `rightkit-onnx`, which only `inference` enables; with `inference` on, it compiles CoreML on Apple targets and DirectML on Windows (`rightkit-onnx/Cargo.toml:65-72`).

## Usage

Vector store, taken from `tests/vector_e2e.rs`:

```rust
use rightkit_embed::{cosine_similarity, VectorStore};

let mut store = VectorStore::open(&path, 3)?;
store.put_batch([
    ("near", "gemma-v1", &[1.0, 0.0, 0.0][..]),
    ("middle", "gemma-v1", &[0.7, 0.7, 0.0][..]),
])?;
let nearest = store.nearest("gemma-v1", &[1.0, 0.0, 0.0], 5)?;  // ranked by cosine score
```

Embedding, illustrative. `EmbedderConfig::new(model_dir, runtime_path)` and `Embedder::load` are the entry points in `src/embedder.rs`:

```rust
use rightkit_embed::{Embedder, EmbedderConfig};

let embedder = Embedder::load(EmbedderConfig::new("/abs/model-dir", "/abs/onnxruntime"))?;
let vectors = embedder.embed_batch(["first text", "second text"])?;
```

## Batching and providers

- **Batching.** `embed_batch` and `embed_query_batch` tokenize all texts, order them by length, and run `max_batch` rows per ONNX call (`EmbedderConfig::with_max_batch(n)`, default 16, also capped at 16384 padded tokens per call). Rows are right-padded to the chunk maximum with a zero attention mask. `token_type_ids` is sent only if the model declares it. Each row is pooled with its mask and normalised like a single run. Output order equals input order. Batched and single-text results agree to cosine at least 0.9999 and max element difference at most 2e-3, because Q4 kernels accumulate differently per shape. The pooling and padding math is unit-tested to 1e-6.
- **Threads.** `EmbedderConfig::with_intra_threads(n)` sets session intra-op threads. The default is 1.
- **Execution provider.** `EmbedderConfig::with_provider(ExecutionProvider)` accepts `Cpu` (default), `Auto` (DirectML on Windows, CoreML on macOS, CPU elsewhere), or an explicit `DirectMl` or `CoreMl` that errors instead of falling back. `Auto` registers the accelerator through `rightkit-onnx`, runs a probe inference, and falls back to CPU if registration, model load or the probe fails.
- **Runtime binding (QA only).** `EmbedderConfig::with_runtime_binding(rightkit_onnx::RuntimeBindingOptions)` sets the ONNX Runtime bind policy. The default refuses an ONNX Runtime older than 1.27.0 on an SME2 host (bogusyogi/rightkit#889). `allow_known_bad_runtime: true` is a QA reproduction opt-out for that refusal; production code must not set it.
- **Accelerators are admitted only after a vector parity check against CPU, at load.** Whenever the session was built on an accelerator (`Auto`, `DirectMl`, `CoreMl`), `Embedder::load` builds a CPU session for the same model, embeds three fixed prompts (a short query, a medium document and a long document of several hundred tokens, with the crate's prefixes), each alone and then all together as one batch, through both sessions with the real inference path, and requires every pair to have cosine similarity of at least `ACCELERATOR_PARITY_MIN_COSINE` (0.995) on the full 768-dimension normalised vector (before Matryoshka truncation), with finite non-zero output. Under `Auto` a failed check falls back to the CPU session with `fallback_reason` such as `directml -> cpu: vector parity failed: cosine 0.579 at ~20 tokens, floor 0.995`. Under an explicit `DirectMl` or `CoreMl` it is `EmbedError::AcceleratorParity { provider, lowest_cosine, floor, detail }`, never a silent fallback and never a success. The probe still runs first under `Auto`; the parity check is in addition to it and cannot be turned off. `Cpu` does none of this (no second session, no extra inference).
- **Load cost with an accelerator.** One extra CPU session build and four ONNX calls on each of the two sessions (three prompts alone, then one batched call of the three; more calls only if `max_batch` is below 3). The CPU session is dropped before `load` returns when the check passes, so steady-state memory is unchanged; while loading, both sessions are resident.
- `Embedder::provider_used()` returns `"cpu"`, `"directml"` or `"coreml"`. `Embedder::fallback_reason()` says why an accelerator was dropped. `Embedder::accelerator_parity()` returns the parity result of the accelerator in use (`AcceleratorParity { lowest_cosine, at_tokens, pairs }`) so an app can show it, or `None` on CPU.
- Accelerated runs bucket shapes (length to 64, 128, ... 2048; rows to a power of two) to avoid per-shape recompiles.
- `ExecutionProvider` is defined by this crate. `rightkit_onnx::ExecutionProvider` values still convert.
- An accelerator needs an ONNX Runtime build that includes the provider. Pass that runtime path.
- **Pipeline id.** `Embedder::pipeline_id()` returns `rkpipe1-<32 hex>`, a stable fingerprint that tells the app when stored vectors need re-embedding. It covers the pipeline schema version, model id and native dimension, SHA-256 of `model_q4.onnx`, `model_q4.onnx_data`, `tokenizer.json` and `config.json`, the document and query prompt templates, pooling and normalisation descriptions, truncate dimension, max tokens, and Gemma 2 sentinel scrubbing. Provider, threads and batch size are excluded, and so is the parity check: an admitted accelerator reproduces the CPU vectors to the parity floor, so the id does not change with the provider. The first call hashes the model files; the result is cached. If a model file cannot be read, the id is `rkpipe1-unavailable-…` instead, which never equals a good id (`src/embedder.rs:344-345`).

## EmbeddingGemma 2 E40 (Windows DirectML) and Core ML

- **E40 bundle.** A directory with `embed_tokens.f16.npy`, `model_32/64/128/256/512.onnx`, `model.onnx_data` and `tokenizer.json` is loaded as `embeddinggemma2-e40-dml` (`EMBEDDINGGEMMA2_E40_MODEL_ID`); the directory chooses, never a setting. Export and contract: `scripts/embed-export/eg2-directml/README.md`; catalog entry and its pending download route: `rightkit-models`. Each text is tokenized (max 512 tokens), embedded row by row from the fp16 table on the host, and run through the smallest fixed graph that fits (batch 1, no row batching). Output is the in-graph normalised 768-d vector.
- **Batched ingest and int8 table (0.6).** A bundle with a schema-2 `embed-manifest.json` (preferred) or `manifest.json` (fallback; older bundles and adopt-only flat directories) names its query graphs, ingest graphs (batch 4 and 8), the one shared `model.onnx_data` and the token table; it is parsed strictly. On DirectML `embed_batch` takes chunks of the longest-first sorted remaining documents: batch 8 for five or more, batch 4 for two to four, the batch-1 graph for one; padding rows have attention mask 0 and are dropped, order is kept (`EmbedderConfig::with_ingest_batch`, cap 4 or 8, default 8; 16 is refused with `EmbedError::UnsupportedIngestBatch`). `token_table` `int8-rowwise` loads `embed_tokens.i8.npy` with `embed_scales.f16.npy` (per-row fp16 scale); no table entry or `fp16` is the fp16 table. No CUDA or TensorRT path.
- **Providers.** `Cpu` runs the graphs on CPU. `Auto` is DirectML on Windows (probe, then parity against the same graphs on CPU, five prompts covering all five graphs) and CPU elsewhere. `DirectMl` has no fallback. `CoreMl` is refused. **Windows default:** E40 on DirectML. After admission every graph is built and run once at load (pre-warm, `EmbedderConfig::with_prewarm(true)`, off by default for E40 because it added 6-18 s to a 2 s cached load; time in `LoadTimings::prewarm`) and stays resident, so no query pays a graph build (about 2 s each on DirectML in the export measurements); resident, all five cost about 2.1 GB of process memory and 4.6 GB on the card. By default the graphs are released after the check and each is built on first use. The parity check costs a CPU reference build, so the opt-in `with_parity_cache_dir(dir)` stores the verdict (keyed on bundle file sizes and mtimes, the ONNX Runtime library and the provider; trusted for 7 days): a load with a stored pass takes about 2 s warm against 42-51 s cold, with no CPU session built. Nothing is written without the directory.
- **Fallback.** `EmbedderConfig::with_fallback_model_dir(q4_dir)` makes `Auto` serve that Q4 bundle on CPU when DirectML is not admitted; without it `Auto` serves E40 on CPU. Either way `fallback_reason()` says why, and `model_id()` / `pipeline_id()` name what is serving, so consumers reindex when the model changes. The E40 CPU path is unmeasured for speed.
- **Core ML bundle (macOS 15+).** A directory with `EmbeddingGemma2Text.mlpackage`, `embeddings.bf16`, `tokenizer.json` and `config.json` (`FluidInference/embeddinggemma-2-coreml`, store id `embeddinggemma2-coreml`, `EMBEDDINGGEMMA2_COREML_MODEL_ID`) is loaded by the Core ML backend (`src/coreml.rs`), chosen from the directory. ONNX Runtime's CoreML provider takes an ONNX graph and cannot load an `.mlpackage`, so the backend uses the Core ML framework directly through `objc2-core-ml` (macOS-only dependency; no ONNX Runtime is initialised, `runtime_path` need not exist). The package is a multi-function ML Program; `embed_32`, `embed_64`, `embed_128`, `embed_256` and `embed_512` are used (batch 1, smallest function that holds the text, first 512 tokens kept). Inputs: `inputs_embeds` float16 `[1, L, 512]` = `embeddings.bf16` row (bf16 widened by a 16-bit shift) times `embed_scale` (sqrt(512), from `config.json`) rounded to float16, pad rows included; `attention_mask` float16 `[1, L]`. Output `embedding` float32 `[1, 768]`, pooled, projected and normalised in the graph.
- **Core ML compile and cache.** The package is compiled once with `MLModel.compileModelAtURL` into `<model_dir>/.coreml-compiled/` (or `with_coreml_cache_dir`), keyed on the package file hashes and the macOS version, reused when present. Each function loads via `MLModelConfiguration.functionName` on its own compute units; the first load compiles the Neural Engine program and builds the GPU shaders (minutes for the Neural Engine, once, cached by Core ML; up to 51 s cold per length on the GPU), so the first `Auto` load is slow. The compiled `.mlmodelc` does not depend on the units, so changing them never recompiles. macOS older than 15 is refused.
- **Core ML compute units and pre-warm (macOS default).** Core ML fp16 with per-length units, `CoreMlUnits::per_length_default()`: `cpu_and_ne` (CPU and Neural Engine) for the 32, 64 and 128 token functions (p50 about 2.3-3.0 ms) and `cpu_and_gpu` for 256 (about 5.1 ms) and 512 (about 9.8-18.7 ms), from the Mac matrix of RightKit issue #81. One `MLModel` per function is loaded once and kept resident. `with_coreml_units(...)` overrides it: pass `CoreMlComputeUnit::All`, `CpuAndNe`, `CpuAndGpu` or `CpuOnly` for every length, or a `CoreMlUnits` map (`"256=cpu_only".parse()`); read the map in use with `Embedder::coreml_units()`. After admission `load` runs one dummy predict per function (pre-warm, `with_prewarm`, default on), so the first query per length pays no compilation or shader build; the total and per-length times are in `LoadTimings::prewarm` and `prewarm_per_length`. `with_prewarm(false)` leaves the first query of each length to pay it.
- **Core ML providers.** `Auto` runs the per-length compute units above (`provider_used()` `"coreml"`) after a probe and the parity check against the same package on `cpuOnly`; `CoreMl` is the same without fallback; `Cpu` is the package on `cpuOnly` (`"cpu"`); `DirectMl` is refused. `with_fallback_model_dir(q4_dir)` applies as for E40, and also covers macOS older than 15 or a compile failure under `Auto`. `pipeline_id()` hashes the package files, token table, tokenizer and `config.json`.

## Platform support

- CPU: all platforms.
- Core ML bundle (`EmbeddingGemma2Text.mlpackage`): macOS 15+ only, through the Core ML framework directly (no ONNX Runtime).
- Accelerators (`Auto`, `DirectMl`, `CoreMl`): DirectML on Windows, CoreML on macOS, through `rightkit-onnx`. Needs a matching ONNX Runtime build, and the accelerator must pass the load-time vector parity check against CPU.

## Vector helpers and model directories

- `VectorStore::resident_index(model_id)` returns a `ResidentIndex`: an in-memory int8 snapshot of one model's vectors (per-row `i8` codes and an `f32` scale). Its `nearest(query, limit)` ranks by cosine on the dequantised rows. It does not see writes made after it was built.
- `reciprocal_rank_fusion(&[&[K]], k)` fuses ranked lists by reciprocal rank (`RRF_K` = 60 is the conventional `k`). It is pure Rust with no feature gate.
- `bundle_files(dir)` names the files the Core ML, E40 or Q4 layout requires, checks they exist, and returns `None` when `dir` is none of them.
- `installed_model_dir(exe, subdir)` returns `<resources>/<subdir>` for an installed app: `Contents/Resources` in a macOS bundle, the executable's directory on Windows, `<exe dir>/resources` elsewhere. It returns the candidate without checking it exists.

## Known limits

- **DirectML currently fails the parity check for EmbeddingGemma 2 Q4 on the hardware measured** (Windows laptop, RTX 4070 and Intel Iris Xe, ONNX Runtime DirectML 1.24.4): cosine to the CPU vector was 0.58 at about 20 tokens, 0.59 at 40, 0.66 at 100 and 0.71-0.72 at 400, against about 0.999 between two correct runs. The output was finite, non-zero and faster than CPU, so only the parity check catches it. As a result `Auto` on Windows resolves to CPU with a recorded reason, and an explicit `DirectMl` returns `EmbedError::AcceleratorParity`. Other hardware or runtime versions are unmeasured and are admitted only if they pass.
- **CoreML through ONNX Runtime is unmeasured.** It must pass the same check; nothing is known yet about the result for EmbeddingGemma 2 Q4. The Core ML backend for the `.mlpackage` has not been built or run yet; the card claims cosine >= 0.9984 to PyTorch fp32 and the parity floor against `cpuOnly` is 0.995.
- The parity prompts are subject to `max_tokens`: a config that truncates below the long prompt's length checks the shorter lengths only.

## Tests

`tests/vector_e2e.rs` (vector store). `tests/real_model_e2e.rs`, `tests/batch_real_model_e2e.rs`, `tests/gemma2_real_model_e2e.rs`, `tests/accelerator_parity_e2e.rs` (real model files, accelerator parity; each skips without its inputs).

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Release notes: [CHANGELOG.md](CHANGELOG.md).
