//! Real EmbeddingGemma 2 E40 DirectML export (Windows): `Auto` admits DirectML behind the
//! load-time parity check, and every fixed-length graph reproduces the same graphs on the CPU
//! provider to at least `ACCELERATOR_PARITY_MIN_COSINE`.
//!
//! Ignored by default: it needs a DirectML-capable GPU and loads five graphs. Run with
//! `cargo test -p rightkit-embed --test e40_real_model_e2e -- --ignored --nocapture`.
//!
//! Needs `RIGHTKIT_EMBED_GEMMA2_E40_DIR` (the `embeddinggemma2-e40-dml` store entry: the five
//! `model_<len>.onnx` graphs, `model.onnx_data`, `embed_tokens.f16.npy` and `tokenizer.json`) and
//! `RIGHTKIT_ORT_TEST_DYLIB` (an ONNX Runtime DirectML build), from the environment or the
//! gitignored `tests/inputs.local.json`. Without them the tests report a skip.

#![cfg(all(windows, feature = "inference", feature = "vector"))]

use rightkit_embed::{
    cosine_similarity, Embedder, EmbedderConfig, ExecutionProvider, ACCELERATOR_PARITY_MIN_COSINE,
    EMBEDDINGGEMMA2_E40_MODEL_ID,
};

fn input(key: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(key).map(Into::into).or_else(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/inputs.local.json");
        let local: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        local[key].as_str().map(Into::into)
    })
}

fn inputs() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let found = input("RIGHTKIT_EMBED_GEMMA2_E40_DIR").zip(input("RIGHTKIT_ORT_TEST_DYLIB"));
    if found.is_none() {
        eprintln!("E40 bundle or ONNX Runtime not provided; E40 real-model gate skipped");
    }
    found
}

fn config(
    dir: &std::path::Path,
    runtime: &std::path::Path,
    provider: ExecutionProvider,
) -> EmbedderConfig {
    EmbedderConfig::new(dir, runtime)
        .with_truncate_dim(768)
        .with_provider(provider)
}

/// Texts of roughly 10, 25, 50, 70, 140, 270 and 800 tokens, so every graph (32 to 512, the
/// last one with truncation) runs.
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
#[ignore = "needs a DirectML GPU and the E40 bundle"]
fn e40_is_admitted_on_directml_and_matches_cpu_on_every_graph() {
    let Some((dir, runtime)) = inputs() else { return };
    let auto = Embedder::load(config(&dir, &runtime, ExecutionProvider::Auto)).expect("load E40");
    eprintln!(
        "auto provider: {} (fallback: {:?}, parity: {:?})",
        auto.provider_used(),
        auto.fallback_reason(),
        auto.accelerator_parity()
    );
    assert_eq!(auto.model_id(), EMBEDDINGGEMMA2_E40_MODEL_ID);
    assert_eq!(
        auto.provider_used(),
        "directml",
        "not admitted: {:?}",
        auto.fallback_reason()
    );
    let parity = auto.accelerator_parity().expect("parity recorded");
    assert!(
        parity.lowest_cosine >= ACCELERATOR_PARITY_MIN_COSINE,
        "{parity:?}"
    );

    let cpu = Embedder::load(config(&dir, &runtime, ExecutionProvider::Cpu)).expect("load CPU");
    assert_eq!(cpu.provider_used(), "cpu");
    assert_eq!(auto.pipeline_id(), cpu.pipeline_id());

    for text in texts() {
        let started = std::time::Instant::now();
        let accelerated = auto.embed(&text).unwrap();
        let gpu_ms = started.elapsed().as_secs_f64() * 1000.0;
        let reference = cpu.embed(&text).unwrap();
        assert_eq!(accelerated.len(), 768);
        let cosine = cosine_similarity(&accelerated, &reference);
        eprintln!(
            "{} chars: directml {gpu_ms:.1} ms, cosine vs cpu {cosine}",
            text.len()
        );
        assert!(
            cosine >= ACCELERATOR_PARITY_MIN_COSINE,
            "{} chars: cosine {cosine}",
            text.len()
        );
    }
}
