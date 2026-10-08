//! Local embeddings and a persistent cosine vector store.

mod embedder;
mod vector;

pub use embedder::{
    truncate_and_normalize, EmbedError, Embedder, EmbedderConfig, ExecutionProvider,
    DEFAULT_MAX_BATCH, EMBEDDINGGEMMA2_MODEL_ID, EMBEDDINGGEMMA_MODEL_ID,
};
pub use vector::{cosine_similarity, Nearest, VectorError, VectorStore, DEFAULT_VECTOR_DIM};
