# rightkit-embed

Local text embeddings and a durable SQLite vector store for Right Suite apps.

`Embedder::load` uses caller-supplied ONNX Runtime & model paths. It never
downloads or silently switches models. EmbeddingGemma 300M Q4 is default,
with Gemma document/query prompts, L2 normalisation, & Matryoshka prefix
truncation. `VectorStore` stores little-endian `f32` vectors in SQLite & ranks
them by cosine similarity in Rust.

Licensed under MIT OR Apache-2.0.
