# Changelog

## Unreleased

- **An accelerator is used only if it reproduces the CPU provider's vectors at load.** DirectML
  (ONNX Runtime 1.24.4) returns wrong EmbeddingGemma 2 Q4 vectors (cosine to CPU 0.58-0.72) that are
  finite, non-zero and faster than CPU, which the probe and any speed check accept. Whenever the
  session was built on anything other than CPU, `Embedder::load` now builds a CPU session for the same
  model, embeds a short query, a medium document and a long document (each alone, then as one batch)
  through both, and requires cosine at least `ACCELERATOR_PARITY_MIN_COSINE` (0.995) on every pair at
  native dimension, with finite non-zero output. Under `Auto` a failed check falls back to CPU with
  `fallback_reason` like `directml -> cpu: vector parity failed: cosine 0.579 at ~20 tokens, floor
  0.995`. Under explicit `DirectMl` / `CoreMl` it fails the load with the new
  `EmbedError::AcceleratorParity { provider, lowest_cosine, floor, detail }`. The CPU session is
  dropped when the check passes; load costs one extra CPU session build and four ONNX calls per
  session. `Cpu` is unchanged. The check cannot be turned off and the probe is unchanged.
- New public items: `ACCELERATOR_PARITY_MIN_COSINE`, `AcceleratorParity`,
  `Embedder::accelerator_parity()`, `EmbedError::AcceleratorParity`. Adding an `EmbedError` variant
  breaks exhaustive matches on it. `pipeline_id()` is unchanged by this (providers are not inputs).
- Consequence: `Auto` on Windows currently resolves to CPU for EmbeddingGemma 2 Q4 on the hardware
  measured, and an explicit `DirectMl` errors. CoreML is unmeasured.
- Written, unverified: nothing was compiled or run for this change.

## 0.3.2

- Internal: model-digest and pipeline-fingerprint SHA-256 now go through `rightkit_fs::digest`
  (`digest` feature only); the direct `sha2` 0.10 dependency is gone. Fingerprints are unchanged.
- `VectorStore::open` / `in_memory` open the connection through `rightkit-db`'s bootstrap
  (`DbOptions::open_writer`, `open_in_memory`): WAL with verified fallback, 5 s busy timeout,
  owner-only database file. Bootstrap failures that are not SQLite errors surface as
  `VectorError::Sqlite` with `SQLITE_CANTOPEN` and the original message. `from_connection` is
  unchanged for callers that bring their own connection.
