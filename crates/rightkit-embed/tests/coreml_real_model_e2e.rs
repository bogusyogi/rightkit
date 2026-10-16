//! Real EmbeddingGemma 2 Core ML bundle (macOS 15+): the Core ML backend serves on all compute
//! units and reproduces the same package on `cpuOnly` (the parity reference) to at least
//! `ACCELERATOR_PARITY_MIN_COSINE` for every function length.
//!
//! Ignored by default: the first load compiles the package and the Neural Engine program for all
//! functions, which takes minutes. Run with `cargo test -p rightkit-embed --test
//! coreml_real_model_e2e -- --ignored --nocapture`.
//!
//! Needs `RIGHTKIT_EMBED_GEMMA2_COREML_DIR`: a directory holding `EmbeddingGemma2Text.mlpackage`,
//! `embeddings.bf16`, `tokenizer.json` and `config.json` (the `embeddinggemma2-coreml` store
//! entry), from the environment or the gitignored `tests/inputs.local.json`. No ONNX Runtime is
//! needed. Without the directory the tests report a skip.

#![cfg(all(target_os = "macos", feature = "inference", feature = "vector"))]

use rightkit_embed::{
    cosine_similarity, Embedder, EmbedderConfig, ExecutionProvider, ACCELERATOR_PARITY_MIN_COSINE,
    EMBEDDINGGEMMA2_COREML_MODEL_ID,
};

fn input(key: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(key).map(Into::into).or_else(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/inputs.local.json");
        let local: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        local[key].as_str().map(Into::into)
    })
}

fn bundle_dir() -> Option<std::path::PathBuf> {
    let dir = input("RIGHTKIT_EMBED_GEMMA2_COREML_DIR");
    if dir.is_none() {
        eprintln!("RIGHTKIT_EMBED_GEMMA2_COREML_DIR not provided; Core ML real-model gate skipped");
    }
    dir
}

/// Native-dimension vectors, like the load-time parity check compares. The ONNX runtime path is
/// unused by a Core ML bundle.
fn config(dir: &std::path::Path, provider: ExecutionProvider) -> EmbedderConfig {
    EmbedderConfig::new(dir, std::path::PathBuf::new())
        .with_truncate_dim(768)
        .with_provider(provider)
}

/// Texts of roughly 10, 25, 50, 70, 140, 270 and 800 tokens (about 13 tokens per sentence plus
/// the prompt prefix), so every function (32 to 512, the last one with truncation) runs.
fn texts() -> Vec<String> {
    let sentence = "The nightly billing job reconciles every invoice against the ledger. ";
    let mut texts = vec![
        "budget".to_owned(),
        "Q3 forecast discussion and budget meeting with finance".to_owned(),
    ];
    for repeats in [3, 5, 10, 20, 60] {
        texts.push(sentence.repeat(repeats));
    }
    texts
}

#[test]
#[ignore = "compiles the package and the Neural Engine program: minutes on first run"]
fn coreml_is_admitted_and_matches_cpu_only_on_every_function_length() {
    let Some(dir) = bundle_dir() else { return };
    let auto = Embedder::load(config(&dir, ExecutionProvider::Auto)).expect("load Core ML bundle");
    eprintln!(
        "auto provider: {} (fallback: {:?}, parity: {:?})",
        auto.provider_used(),
        auto.fallback_reason(),
        auto.accelerator_parity()
    );
    assert_eq!(auto.model_id(), EMBEDDINGGEMMA2_COREML_MODEL_ID);
    assert_eq!(
        auto.provider_used(),
        "coreml",
        "not admitted: {:?}",
        auto.fallback_reason()
    );
    assert!(auto.fallback_reason().is_none());
    let parity = auto.accelerator_parity().expect("parity recorded");
    assert!(
        parity.lowest_cosine >= ACCELERATOR_PARITY_MIN_COSINE,
        "{parity:?}"
    );

    let cpu = Embedder::load(config(&dir, ExecutionProvider::Cpu)).expect("load cpuOnly");
    assert_eq!(cpu.provider_used(), "cpu");
    assert_eq!(cpu.model_id(), EMBEDDINGGEMMA2_COREML_MODEL_ID);
    assert_eq!(auto.pipeline_id(), cpu.pipeline_id());
    assert!(auto.pipeline_id().starts_with("rkpipe1-"));

    for text in texts() {
        let accelerated = auto.embed(&text).unwrap();
        let reference = cpu.embed(&text).unwrap();
        assert_eq!(accelerated.len(), 768);
        let norm = accelerated.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "norm {norm}");
        let cosine = cosine_similarity(&accelerated, &reference);
        eprintln!("{} chars: coreml vs cpuOnly cosine {cosine}", text.len());
        assert!(
            cosine >= ACCELERATOR_PARITY_MIN_COSINE,
            "{} chars: coreml vs cpuOnly cosine {cosine}",
            text.len()
        );
    }
}

#[test]
#[ignore = "loads the package: minutes on first run"]
fn coreml_ranks_the_relevant_document_first() {
    let Some(dir) = bundle_dir() else { return };
    let embedder = Embedder::load(config(&dir, ExecutionProvider::Auto)).expect("load");
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
        "relevant document must rank first: {scores:?}"
    );
}

#[test]
#[ignore = "loads the package: minutes on first run"]
fn explicit_core_ml_is_admitted_or_errors_never_falls_back() {
    let Some(dir) = bundle_dir() else { return };
    match Embedder::load(config(&dir, ExecutionProvider::CoreMl)) {
        Ok(embedder) => {
            assert_eq!(embedder.provider_used(), "coreml");
            assert!(embedder.fallback_reason().is_none());
            let parity = embedder.accelerator_parity().expect("parity recorded");
            assert!(parity.lowest_cosine >= ACCELERATOR_PARITY_MIN_COSINE);
        }
        Err(error) => eprintln!("explicit CoreMl rejected: {error}"),
    }
    assert!(Embedder::load(config(&dir, ExecutionProvider::DirectMl)).is_err());
}
