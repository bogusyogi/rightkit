//! Local embeddings and a persistent cosine vector store.
//!
//! Feature boundaries: `inference` (ONNX text embedder) and `vector` (SQLite-backed
//! cosine store). Both are default-on; a `vector`-only build does not pull ort or tokenizers.

#[cfg(feature = "inference")]
mod embedder;
#[cfg(feature = "vector")]
mod vector;

#[cfg(feature = "inference")]
pub use embedder::{
    truncate_and_normalize, AcceleratorParity, EmbedError, Embedder, EmbedderConfig,
    ExecutionProvider, ACCELERATOR_PARITY_MIN_COSINE, DEFAULT_MAX_BATCH, EMBEDDINGGEMMA2_MODEL_ID,
    EMBEDDINGGEMMA_MODEL_ID,
};
#[cfg(feature = "vector")]
pub use vector::{cosine_similarity, Nearest, VectorError, VectorStore, DEFAULT_VECTOR_DIM};
