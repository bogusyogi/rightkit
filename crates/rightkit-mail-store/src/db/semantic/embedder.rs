//! Text embedding for local semantic search (C5).
//!
//! Backed by RightKit's shared `rightkit-embed` crate. Loading is bundled-
//! resource-only and FAIL-LOUD: it never downloads and never silently
//! degrades to a wrong-space fallback.
//!
//! The model is whichever EmbeddingGemma bundle ships in the resource
//! directory: EmbeddingGemma 2 when its `config.json` `model_type` is
//! `embedding_gemma2`, otherwise EmbeddingGemma-300M Q4. Both are 768-dim
//! native; `rightkit-embed` truncates them to `STORED_DIM` via Matryoshka
//! Representation Learning (prefix-truncate + re-normalize), and the crate's
//! pipeline id covers that truncation. The embedder's `model_id` and its
//! `index_key` are persisted, so swapping the bundle or changing its pipeline
//! triggers a full reindex.

use rightkit_embed::{
    Embedder as SharedEmbedder, EmbedderConfig, EMBEDDINGGEMMA2_COREML_MODEL_ID,
    EMBEDDINGGEMMA2_E40_MODEL_ID, EMBEDDINGGEMMA2_MODEL_ID, EMBEDDINGGEMMA_MODEL_ID,
};
use std::path::Path;

/// The dimension every stored vector is truncated to, regardless of the
/// embedding model's native output width. Fixed by the vec0 schema
/// (`message_vec` declares `float[256]`, see db_schema.rs SCHEMA_V13) — a
/// model swap changes HOW a 256-dim vector is produced, never its width, so
/// the storage schema never needs to migrate for a model change.
pub const STORED_DIM: usize = 256;

/// Anything that can turn text into a fixed-length vector for semantic
/// search. A real model natively embeds wider than `STORED_DIM` (Gemma 768);
/// `FastEmbedder` returns vectors already truncated to `STORED_DIM`, and
/// callers apply `truncate_and_normalize` to any other embedder's output.
pub trait Embedder: Send + Sync {
    /// Embed `text` as a DOCUMENT (subject + body), L2-normalized, at the
    /// embedder's output dimension (`STORED_DIM` for `FastEmbedder`).
    fn embed(&self, text: &str) -> Result<Vec<f32>, String>;
    /// Embed `text` as a search QUERY. Gemma uses a different instruction
    /// prefix for queries vs documents so both land in the same similarity
    /// space as intended by the model's training.
    fn embed_query(&self, text: &str) -> Result<Vec<f32>, String>;
    /// Stable model identifier, persisted in `settings.semantic_embed_model_id`
    /// — a mismatch against the stored value is the "needs reindex" signal.
    fn model_id(&self) -> &'static str;
    /// Stable id of everything that shapes the vectors this embedder emits
    /// (model, quantisation, pooling, prompts, truncation, model files).
    /// Defaults to the model id for embedders with no richer pipeline.
    fn pipeline_id(&self) -> String {
        self.model_id().to_string()
    }
    /// The key persisted per account and compared by `needs_reindex`: the
    /// pipeline id plus MailRight's own truncation. Never override.
    fn index_key(&self) -> String {
        index_key_for(&self.pipeline_id())
    }
}

/// Persisted pipeline key for `pipeline_id`: appends the `STORED_DIM`
/// truncation marker. The crate's pipeline id already covers the truncation
/// width; the suffix stays so the key format is unchanged. The only place
/// the key is built.
pub fn index_key_for(pipeline_id: &str) -> String {
    format!("{pipeline_id}|mr-trunc{STORED_DIM}-l2")
}

/// ONNX sentence embeddings via `rightkit-embed`, loaded exclusively from a
/// bundled resource directory. Any missing/corrupt file is a hard `Err`.
pub struct FastEmbedder {
    inner: SharedEmbedder,
    model_id: &'static str,
}

impl FastEmbedder {
    /// Load the embedder from a flat directory of bundled resource files.
    /// The bundle itself decides the model (EmbeddingGemma 2 in any layout
    /// rightkit-embed loads, or EmbeddingGemma-300M Q4). `dir` is the resolved Tauri resource path
    /// (`resource_dir().join("embed_model")` in production, or
    /// `MAILRIGHT_EMBED_MODEL_DIR` override for dev/test).
    ///
    /// `onnxruntime_dylib` is the path to the bundled onnxruntime shared
    /// library; the caller resolves it once per process (see
    /// `resolve::onnxruntime_dylib_path`).
    pub fn load(dir: &Path, onnxruntime_dylib: &Path) -> Result<Self, String> {
        let config = EmbedderConfig::new(dir, onnxruntime_dylib).with_truncate_dim(STORED_DIM);
        let inner = SharedEmbedder::load(config)
            .map_err(|e| format!("bundled embedder init failed ({dir:?}): {e}"))?;
        let model_id = [
            EMBEDDINGGEMMA2_MODEL_ID,
            EMBEDDINGGEMMA2_E40_MODEL_ID,
            EMBEDDINGGEMMA2_COREML_MODEL_ID,
        ]
        .into_iter()
        .find(|id| *id == inner.model_id())
        .unwrap_or(EMBEDDINGGEMMA_MODEL_ID);
        Ok(Self { inner, model_id })
    }
}

impl Embedder for FastEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        self.inner
            .embed(text)
            .map_err(|e| format!("embed failed: {e}"))
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, String> {
        self.inner
            .embed_query(text)
            .map_err(|e| format!("embed failed: {e}"))
    }

    fn model_id(&self) -> &'static str {
        self.model_id
    }

    fn pipeline_id(&self) -> String {
        self.inner.pipeline_id()
    }
}
