# rightkit-embed

Local text embeddings and a durable SQLite vector store for Right Suite apps.

`Embedder::load` uses caller-supplied ONNX Runtime & model paths. It never
downloads or silently switches models. EmbeddingGemma 300M Q4 is default,
with Gemma document/query prompts, L2 normalisation, & Matryoshka prefix
truncation. `VectorStore` stores little-endian `f32` vectors in SQLite & ranks
them by cosine similarity in Rust.

Licensed under MIT OR Apache-2.0.

## Also (0.2.2)

- **True padded batching.** `embed_batch` / `embed_query_batch` tokenize all
  texts, order them by length, and run `max_batch` rows per ONNX call
  (`EmbedderConfig::with_max_batch(n)`, default 16, also capped at 16384 padded
  tokens per call). Rows are right-padded to the chunk maximum with a zero
  attention mask (`token_type_ids` is sent only if the model declares it),
  pooled per row honoring the mask, then normalized exactly like single runs.
  Output order equals input order. Batched and single-text results agree to
  cosine >= 0.9999 and max element difference <= 2e-3 (Q4 kernels accumulate
  differently per shape); the pooling/padding math is unit-tested to 1e-6.
- **`EmbedderConfig::with_intra_threads(n)`** sets session intra-op threads
  (default 1, unchanged for existing callers).
- **`EmbedderConfig::with_provider(ExecutionProvider)`**: `Cpu` (default),
  `Auto` (DirectML on Windows, CoreML on macOS, CPU elsewhere), or an explicit
  `DirectMl` / `CoreMl` that errors instead of falling back. `Auto` registers
  the accelerator through `rightkit-ort`, runs a probe inference, and falls
  back to CPU when registration, model load or the probe fails.
  `Embedder::provider_used()` returns `"cpu"`, `"directml"` or `"coreml"`;
  `Embedder::fallback_reason()` says why an accelerator was dropped. The
  accelerator support is already compiled by `rightkit-ort` per target OS, so
  there is no extra crate feature; it needs an ONNX Runtime build that
  includes the provider (the runtime path you pass). On accelerators, shapes
  are bucketed (length to 64/128/.../2048, rows to a power of two) to avoid
  per-shape recompiles. `ExecutionProvider` is now defined by this crate
  (adds `Auto`); `rightkit_ort::ExecutionProvider` values still convert.
- **`Embedder::pipeline_id()`** returns `rkpipe1-<32 hex>`, a stable
  fingerprint to decide when stored vectors need re-embedding. It covers the
  pipeline schema version, model id and native dimension, SHA-256 of
  `model_q4.onnx`, `model_q4.onnx_data`, `tokenizer.json` and `config.json`,
  the document and query prompt templates, pooling and normalization
  descriptions, truncate dimension, max tokens, and Gemma 2 sentinel
  scrubbing. Provider, threads and batch size are excluded. The first call
  hashes the model files; the result is cached.
