//! Text embedding for semantic memory retrieval.
//!
//! An [`Embedder`] turns text into a fixed-dimension vector so memories can be
//! ranked by *meaning* (cosine similarity), not just shared keywords.
//!
//! Backend:
//! - [`HashEmbedder`] — dependency-free, deterministic feature-hashing of
//!   character trigrams. Not truly semantic, but always available (tests, and a
//!   graceful fallback when the model is missing or not loaded). Captures
//!   lexical overlap, so it still beats nothing.

pub use crate::fingerprint::EMBEDDING_MAX_SEQUENCE_TOKENS;

/// Which ONNX execution provider was requested and which one is really serving inference.
///
/// Provider choice never changes the vector fingerprint identity: DirectML/CoreML produce
/// numerically near-identical vectors to CPU (pilot showed identical rankings), so the
/// fingerprint stays model + graph + tokenizer based and existing vectors remain valid.
#[derive(Clone, Debug, PartialEq)]
pub struct EmbedProviderReport {
    /// `cpu`, `dml`, `coreml`, or `auto` (as requested via `RIGHTKIT_CONTEXT_EMBED_PROVIDER`).
    pub requested: String,
    /// `cpu`, `dml`, `coreml`, `uninitialized`, or `unavailable`. Never claims an accelerator
    /// that failed to initialize.
    pub used: String,
    /// Explicit adapter index (`RIGHTKIT_CONTEXT_EMBED_DEVICE`), if any.
    pub device: Option<i32>,
    /// Why the accelerator was not used, when `used` differs from the resolved request.
    pub fallback_reason: Option<String>,
    /// Mean ms per padded probe embed on DirectML (startup self-check), when measured.
    pub dml_probe_ms: Option<f64>,
    /// Mean ms per probe embed on CPU (startup self-check), when measured.
    pub cpu_probe_ms: Option<f64>,
}

impl EmbedProviderReport {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "provider_requested": self.requested,
            "provider_used": self.used,
            "device": self.device,
            "fallback_reason": self.fallback_reason,
            "dml_probe_ms": self.dml_probe_ms,
            "cpu_probe_ms": self.cpu_probe_ms,
        })
    }
}

/// Anything that can turn text into a fixed-length vector.
pub trait Embedder: Send + Sync {
    /// Embed `text` (a document/passage) into a `dim()`-length, L2-normalized vector.
    fn embed(&self, text: &str) -> Vec<f32>;
    /// Fallible embedding path for persistence/reindex operations. Runtime backends override
    /// this so inference failures are returned instead of being disguised as vectors.
    fn try_embed(&self, text: &str) -> Result<Vec<f32>, String> {
        Ok(self.embed(text))
    }
    /// Embed a document batch in input order. Native backends should override this to execute one
    /// model batch; the default preserves compatibility for simple/test embedders.
    fn try_embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, String> {
        texts.iter().map(|text| self.try_embed(text)).collect()
    }
    /// Embed a search QUERY. BGE-class models are asymmetric — the query path can differ from the
    /// document path. Default is the document embedding; real backends (FastEmbedder) override it to
    /// use the model's native query embedding so query and document vectors share the same space.
    fn embed_query(&self, text: &str) -> Vec<f32> {
        self.embed(text)
    }
    fn try_embed_query(&self, text: &str) -> Result<Vec<f32>, String> {
        self.try_embed(text)
    }
    /// The dimensionality of vectors this embedder produces.
    fn dim(&self) -> usize;
    /// Stable identifier of the embedding model — memoization key alongside the
    /// content hash: a row whose (content_hash, embed_model) both match needs no
    /// re-embedding on `reindex`.
    fn model_id(&self) -> &'static str {
        "hash-256"
    }
    /// Hardware execution provider actually serving this embedder, when it has one. `None` for
    /// backends with no ONNX session (hash embedder). Health surfaces report this; it is
    /// deliberately NOT part of [`Embedder::pipeline_fingerprint`] (see [`EmbedProviderReport`]).
    fn execution_provider(&self) -> Option<EmbedProviderReport> {
        None
    }
    /// Deterministic identity of the active model, prompts, pooling, and normalization pipeline.
    fn pipeline_fingerprint(&self) -> crate::PipelineFingerprint {
        crate::PipelineFingerprint::for_model_id(self.model_id()).unwrap_or_else(|| {
            crate::PipelineFingerprint::compute(0, "", "", self.model_id(), "unspecified", true, 1)
        })
    }
}

/// Deterministic, dependency-free embedder using signed feature-hashing of
/// character trigrams. Always available; used in tests and as the stand-in when
/// the model is missing or this process is not the resident.
#[derive(Clone)]
pub struct HashEmbedder {
    dim: usize,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl HashEmbedder {
    pub fn new() -> Self {
        Self { dim: 256 }
    }

    pub fn with_dim(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }
}

impl Embedder for HashEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        let chars: Vec<char> = text.to_lowercase().chars().collect();
        let push = |v: &mut [f32], h: u64| {
            let idx = (h % self.dim as u64) as usize;
            // Signed hashing reduces collision bias.
            let sign = if (h >> 63) & 1 == 1 { -1.0 } else { 1.0 };
            v[idx] += sign;
        };
        let fnv = |bytes: &[u8]| -> u64 {
            let mut h: u64 = 1469598103934665603;
            for b in bytes {
                h ^= *b as u64;
                h = h.wrapping_mul(1099511628211);
            }
            h
        };
        if chars.len() < 3 {
            // Too short for trigrams: hash whole-string + each char.
            push(&mut v, fnv(text.to_lowercase().as_bytes()));
            for c in &chars {
                let mut buf = [0u8; 4];
                push(&mut v, fnv(c.encode_utf8(&mut buf).as_bytes()));
            }
        } else {
            for w in chars.windows(3) {
                let s: String = w.iter().collect();
                push(&mut v, fnv(s.as_bytes()));
            }
        }
        rightkit_embed::truncate_and_normalize(&v, self.dim)
    }

    fn dim(&self) -> usize {
        self.dim
    }
}
