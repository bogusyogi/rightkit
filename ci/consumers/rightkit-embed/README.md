# rightkit-embed 0.6.1 (registry consumer recipe)

Standalone consumer, outside the workspace, pinning `rightkit-embed =0.6.1` exactly with the default
features. It uses only public API that needs no model and no ONNX Runtime: `reciprocal_rank_fusion`
with `RRF_K` (exact scores, order, tie and invalid-`k` behaviour), `cosine_similarity`,
`truncate_and_normalize`, `installed_model_dir` (macOS, Windows and other layouts, refusal of a
non-plain subdir), the `EmbedderConfig` builder defaults, `bundle_files` on an empty directory, and
`Embedder::load` refusing a missing directory and an invalid dimension with typed `EmbedError`s.

State: one empty directory under the OS temp dir, removed at the end. No network, no model files, no
ONNX Runtime library, no real user data.

Not covered: loading a model, `embed` and `embed_batch`, accelerator parity, the Core ML and E40
backends, and the persistent `VectorStore`.
