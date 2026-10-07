//! Real EmbeddingGemma 2 bundle: model identity, ranking at the default 256-dim
//! Matryoshka truncation and at native 768, and modality-sentinel scrubbing.
//!
//! Needs `RIGHTKIT_EMBED_GEMMA2_DIR` (the text-only Q4 bundle, e.g. the hosted
//! `build-inputs/coderight/embed-model/embeddinggemma2-q4-b64acc4/`) and
//! `RIGHTKIT_ORT_TEST_DYLIB` (ONNX Runtime >= 1.20 for GatherBlockQuantized).
//! Without both it reports a skip.

use rightkit_embed::{cosine_similarity, Embedder, EmbedderConfig, EMBEDDINGGEMMA2_MODEL_ID};

/// Environment first; managed runners filter ambient variables, so the same
/// keys may come from the gitignored `tests/inputs.local.json` (paths only).
fn input(key: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(key).map(Into::into).or_else(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/inputs.local.json");
        let local: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        local[key].as_str().map(Into::into)
    })
}

fn inputs() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    Some((
        input("RIGHTKIT_EMBED_GEMMA2_DIR")?,
        input("RIGHTKIT_ORT_TEST_DYLIB")?,
    ))
}

#[test]
fn embeddinggemma2_bundle_is_identified_ranks_and_scrubs_sentinels() {
    let Some((dir, runtime)) = inputs() else {
        eprintln!("EmbeddingGemma 2 bundle or ONNX Runtime not provided; real-model gate skipped");
        return;
    };
    for dim in [256, 768] {
        let embedder = Embedder::load(EmbedderConfig::new(&dir, &runtime).with_truncate_dim(dim))
            .expect("load EmbeddingGemma 2 bundle");
        assert_eq!(embedder.model_id(), EMBEDDINGGEMMA2_MODEL_ID);
        assert_eq!(embedder.dimension(), dim);
        let query = embedder
            .embed_query("how are invoices matched to the ledger")
            .unwrap();
        let docs = [
            "Invoices are reconciled against the ledger every night by the billing job.",
            "The footer shows the launch directory and current usage.",
            "Recipe for lemon cake with vanilla icing.",
        ];
        let scores: Vec<f32> = docs
            .iter()
            .map(|doc| cosine_similarity(&query, &embedder.embed(doc).unwrap()))
            .collect();
        assert!(
            scores[0] > scores[1] && scores[0] > scores[2],
            "dim {dim}: relevant document must rank first: {scores:?}"
        );
        let with_sentinel = embedder.embed("caption <|image|> here").unwrap();
        let scrubbed = embedder
            .embed("caption <\u{2502}image\u{2502}> here")
            .unwrap();
        assert!(cosine_similarity(&with_sentinel, &scrubbed) > 0.9999);
    }
}
