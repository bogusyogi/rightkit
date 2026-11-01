//! Pipeline fingerprint for deterministic reindex skip decisions.
//!
//! A `PipelineFingerprint` captures the full embedding pipeline configuration
//! as a SHA-256 hash. Rows whose `(content_hash, full_pipeline_digest)` both
//! match need no re-embedding on `reindex`.

/// Pipeline fingerprint — SHA-256 of the embedding pipeline configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineFingerprint {
    /// The 64-hex-character SHA-256 digest.
    pub digest: String,
    /// Human-readable label for metrics: `pf:v1:<64-hex>`.
    pub label: String,
    /// Maximum sequence length used by the tokenizer.
    pub max_sequence_length: usize,
    /// Model identifier (e.g., "embeddinggemma2-q4").
    pub model_id: String,
}

impl PipelineFingerprint {
    /// Compute a fingerprint from the pipeline configuration.
    pub fn compute(
        max_sequence_length: usize,
        prompt_template_doc: &str,
        prompt_template_query: &str,
        model_id: &str,
        pooling: &str,
        normalize: bool,
        pipeline_version: u32,
    ) -> Self {
        let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
        hasher.update(&max_sequence_length.to_le_bytes());
        hasher.update(prompt_template_doc.as_bytes());
        hasher.update(prompt_template_query.as_bytes());
        hasher.update(model_id.as_bytes());
        hasher.update(pooling.as_bytes());
        hasher.update(&[normalize as u8]);
        hasher.update(&pipeline_version.to_le_bytes());

        let digest = hasher.finalize();
        let label = format!("pf:v1:{digest}");

        Self {
            digest,
            label,
            max_sequence_length,
            model_id: model_id.to_string(),
        }
    }

    /// Create a fingerprint for the HashEmbedder (no real model).
    pub fn hash_embedder() -> Self {
        Self::compute(0, "", "", "hash-256", "mean", true, 1)
    }

    /// Create a fingerprint for the selectable BGE-small embedding pipeline.
    pub fn bge_small_en_v1_5() -> Self {
        Self::compute(
            super::embed::EMBEDDING_MAX_SEQUENCE_TOKENS,
            "{text}",
            "Represent this sentence for searching relevant passages: {text}",
            "bge-small-en-v1.5",
            "mean",
            true,
            1,
        )
    }

    /// Create a fingerprint for the EmbeddingGemma 2 Q4 (b64, accuracy level 4)
    /// text-only bundle. Rows written by any other model id are served lexically
    /// until `reindex` re-embeds them.
    pub fn gemma2_q4() -> Self {
        Self::compute(
            super::embed::EMBEDDING_MAX_SEQUENCE_TOKENS,
            "title: none | text: {text}",
            "task: search result | query: {text}",
            "embeddinggemma2-q4",
            "mean",
            true,
            1,
        )
    }

    /// Resolve the declared pipeline for a built-in embedder model identifier.
    pub fn for_model_id(model_id: &str) -> Option<Self> {
        match model_id {
            "hash-256" => Some(Self::hash_embedder()),
            "bge-small-en-v1.5" => Some(Self::bge_small_en_v1_5()),
            "embeddinggemma2-q4" => Some(Self::gemma2_q4()),
            _ => None,
        }
    }
}
