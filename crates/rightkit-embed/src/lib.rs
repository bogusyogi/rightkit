//! Local embeddings and a persistent cosine vector store.
//!
//! Feature boundaries: `inference` (ONNX text embedder) and `vector` (SQLite-backed
//! cosine store). Both are default-on; a `vector`-only build does not pull ort or tokenizers.

#[cfg(all(feature = "inference", target_os = "macos"))]
mod coreml;
#[cfg(feature = "inference")]
mod coreml_units;
#[cfg(feature = "inference")]
mod e40;
#[cfg(feature = "inference")]
mod embedder;
#[cfg(feature = "inference")]
mod parity_cache;
#[cfg(feature = "vector")]
mod vector;

#[cfg(feature = "inference")]
pub use coreml_units::{
    CoreMlComputeUnit, CoreMlUnits, CoreMlUnitsParseError, NEURAL_ENGINE_MAX_TOKENS,
};
#[cfg(feature = "inference")]
pub use embedder::{
    truncate_and_normalize, AcceleratorParity, EmbedError, Embedder, EmbedderConfig,
    ExecutionProvider, LoadTimings, ParityCacheOutcome, SessionCreate,
    ACCELERATOR_PARITY_MIN_COSINE, DEFAULT_MAX_BATCH, EMBEDDINGGEMMA2_COREML_MODEL_ID,
    EMBEDDINGGEMMA2_E40_MODEL_ID, EMBEDDINGGEMMA2_MODEL_ID, EMBEDDINGGEMMA_MODEL_ID,
};
/// Benchmark support, behind the `bench-internals` feature; not part of the stable API.
#[cfg(feature = "bench-internals")]
pub use e40::{E40SessionProfile, RunSplit};
#[cfg(feature = "bench-internals")]
pub use embedder::{BenchEmbedSplit, BenchTokens};
#[cfg(feature = "vector")]
pub use vector::{cosine_similarity, Nearest, VectorError, VectorStore, DEFAULT_VECTOR_DIM};
