# rightkit-embed

Product-neutral local text embeddings and a durable SQLite vector store for Right Suite apps.

- `Embedder` runs a local ONNX embedding model. `Embedder::load` uses caller-supplied ONNX Runtime and model paths. It never downloads or silently switches models. It runs two models and picks one from the model directory, never from a setting: EmbeddingGemma 2 Q4 (`embeddinggemma2-q4`, the text-only export; selected when the directory's `config.json` has `model_type` `embedding_gemma2`) and the older EmbeddingGemma 300M Q4 (`embeddinggemma-300m-q4`, any other directory). The suite's current model is EmbeddingGemma 2; vectors from the two models are not comparable, and `pipeline_id()` changes with the model. Both use the Gemma document prompt (`title: none | text: `) and query prompt (`task: search result | query: `), L2 normalisation, and Matryoshka prefix truncation (native 768, default 256, at most 2,048 tokens).
- `VectorStore` keeps little-endian `f32` vectors in SQLite and ranks them by cosine similarity in Rust.

Catalog capability: `embeddings`. Published to crates.io (see [INDEX.md](../../INDEX.md)). Licensed under MIT OR Apache-2.0.

## When to use it

- Use it for on-device text embeddings with a model the app ships or downloads itself.
- Use `VectorStore` when vectors must survive restarts and be searched by cosine similarity.

Do not use it to fetch models, and do not use it to pick a model at run time. Model identity and downloads belong to the app.

## Cargo features

| Feature | Default | Pulls in | Effect |
|---|---|---|---|
| `inference` | on | `rightkit-ort` (owns the `ort` pin and loads it dynamically), `tokenizers` (`onig`), `rightkit-fs` (`digest`), `serde_json` (`Cargo.toml:16`) | `Embedder`, `EmbedderConfig`, `ExecutionProvider`, `truncate_and_normalize`. |
| `vector` | on | `rusqlite` and `rightkit-db` (both without default features; `Cargo.toml:17`) | `VectorStore` (persistent cosine store) and `cosine_similarity`. |

`--no-default-features --features vector` builds the store without `ort` or `tokenizers`. Accelerator support is still compiled by `rightkit-ort` for each target OS.

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
- **Execution provider.** `EmbedderConfig::with_provider(ExecutionProvider)` accepts `Cpu` (default), `Auto` (DirectML on Windows, CoreML on macOS, CPU elsewhere), or an explicit `DirectMl` or `CoreMl` that errors instead of falling back. `Auto` registers the accelerator through `rightkit-ort`, runs a probe inference, and falls back to CPU if registration, model load or the probe fails.
- `Embedder::provider_used()` returns `"cpu"`, `"directml"` or `"coreml"`. `Embedder::fallback_reason()` says why an accelerator was dropped.
- Accelerated runs bucket shapes (length to 64, 128, ... 2048; rows to a power of two) to avoid per-shape recompiles.
- `ExecutionProvider` is defined by this crate. `rightkit_ort::ExecutionProvider` values still convert.
- An accelerator needs an ONNX Runtime build that includes the provider. Pass that runtime path.
- **Pipeline id.** `Embedder::pipeline_id()` returns `rkpipe1-<32 hex>`, a stable fingerprint that tells the app when stored vectors need re-embedding. It covers the pipeline schema version, model id and native dimension, SHA-256 of `model_q4.onnx`, `model_q4.onnx_data`, `tokenizer.json` and `config.json`, the document and query prompt templates, pooling and normalisation descriptions, truncate dimension, max tokens, and Gemma 2 sentinel scrubbing. Provider, threads and batch size are excluded. The first call hashes the model files; the result is cached.

## Platform support

- CPU: all platforms.
- Accelerators (`Auto`, `DirectMl`, `CoreMl`): DirectML on Windows, CoreML on macOS, through `rightkit-ort`. Needs a matching ONNX Runtime build.

## Tests

`tests/vector_e2e.rs` (vector store). `tests/real_model_e2e.rs`, `tests/batch_real_model_e2e.rs`, `tests/gemma2_real_model_e2e.rs` (real model files).

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Release notes: [CHANGELOG.md](CHANGELOG.md).
