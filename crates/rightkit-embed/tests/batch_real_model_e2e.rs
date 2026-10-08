//! Real-model batching, provider and fingerprint checks.
//!
//! Needs `RIGHTKIT_EMBED_GEMMA2_DIR` (or `RIGHTKIT_EMBED_MODEL_DIR` for the
//! original EmbeddingGemma bundle) and `RIGHTKIT_ORT_TEST_DYLIB`, from the
//! environment or the gitignored `tests/inputs.local.json`. Without them the
//! test reports a skip.
//!
//! Documented tolerance, batched vs single-text run of the same text:
//! cosine >= 0.9999 and max absolute element difference <= 2e-3 (Q4 kernels
//! pick different accumulation paths for different shapes; the math is equal).

use std::time::Instant;

use rightkit_embed::{cosine_similarity, Embedder, EmbedderConfig, ExecutionProvider};

fn input(key: &str) -> Option<std::path::PathBuf> {
    std::env::var_os(key).map(Into::into).or_else(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/inputs.local.json");
        let local: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        local[key].as_str().map(Into::into)
    })
}

fn inputs() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let dir = input("RIGHTKIT_EMBED_GEMMA2_DIR").or_else(|| input("RIGHTKIT_EMBED_MODEL_DIR"))?;
    Some((dir, input("RIGHTKIT_ORT_TEST_DYLIB")?))
}

const TEXTS: [&str; 7] = [
    "fn main() { println!(\"hello\"); }",
    "budget meeting forecast",
    "Q3 forecast discussion and budget meeting with finance and the whole planning group about next year",
    "recipe for lemon cake with vanilla icing",
    "x",
    "pub struct VectorStore { connection: rusqlite::Connection, dimension: usize } impl VectorStore { pub fn put(&self, key: &str, model: &str, vector: &[f32]) -> Result<(), VectorError> { todo!() } }",
    "short",
];

fn assert_close(a: &[f32], b: &[f32], what: &str) {
    let cos = cosine_similarity(a, b);
    let max = a
        .iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max);
    assert!(cos >= 0.9999, "{what}: cosine {cos}");
    assert!(max <= 2e-3, "{what}: max abs diff {max}");
}

#[test]
fn batched_matches_single_and_reports_provider_and_pipeline() {
    let Some((dir, runtime)) = inputs() else {
        eprintln!("embed model bundle / ORT runtime unavailable; real-model batch gate skipped");
        return;
    };
    let embedder = Embedder::load(
        EmbedderConfig::new(dir.clone(), runtime.clone())
            .with_max_batch(4)
            .with_intra_threads(2),
    )
    .unwrap();
    assert_eq!(embedder.provider_used(), "cpu");
    assert!(embedder.fallback_reason().is_none());

    let single_start = Instant::now();
    let singles: Vec<_> = TEXTS
        .iter()
        .map(|t| {
            let start = Instant::now();
            let v = embedder.embed(t).unwrap();
            eprintln!(
                "single embed {:.1} ms ({} chars)",
                start.elapsed().as_secs_f64() * 1000.0,
                t.len()
            );
            v
        })
        .collect();
    let single_ms = single_start.elapsed().as_secs_f64() * 1000.0;
    let batch_start = Instant::now();
    let batched = embedder.embed_batch(TEXTS).unwrap();
    let batch_ms = batch_start.elapsed().as_secs_f64() * 1000.0;
    eprintln!("single {single_ms:.1} ms, batched {batch_ms:.1} ms");
    assert_eq!(batched.len(), TEXTS.len());
    for (i, (b, s)) in batched.iter().zip(&singles).enumerate() {
        assert_close(b, s, &format!("doc {i}"));
    }
    eprintln!("queries");
    let q_single: Vec<_> = TEXTS
        .iter()
        .map(|t| embedder.embed_query(t).unwrap())
        .collect();
    let q_batched = embedder.embed_query_batch(TEXTS).unwrap();
    for (i, (b, s)) in q_batched.iter().zip(&q_single).enumerate() {
        assert_close(b, s, &format!("query {i}"));
    }
    assert!(embedder.embed_batch(Vec::<&str>::new()).unwrap().is_empty());

    let id = embedder.pipeline_id();
    assert!(
        id.starts_with("rkpipe1-") && !id.contains("unavailable"),
        "{id}"
    );
    let other = Embedder::load(EmbedderConfig::new(dir.clone(), runtime.clone())).unwrap();
    assert_eq!(id, other.pipeline_id());
    let truncated =
        Embedder::load(EmbedderConfig::new(dir.clone(), runtime.clone()).with_truncate_dim(128))
            .unwrap();
    assert_ne!(id, truncated.pipeline_id());

    // Auto never fails to load: it serves an accelerator or CPU with a reason.
    let auto =
        Embedder::load(EmbedderConfig::new(dir, runtime).with_provider(ExecutionProvider::Auto))
            .unwrap();
    eprintln!(
        "auto provider: {} (fallback: {:?})",
        auto.provider_used(),
        auto.fallback_reason()
    );
    if auto.provider_used() == "cpu" {
        assert!(
            cfg!(not(any(windows, target_vendor = "apple"))) || auto.fallback_reason().is_some()
        );
    }
    // Different execution providers (DirectML/CoreML vs CPU) differ in float rounding:
    // compare semantically, not bit-for-bit.
    let cos = cosine_similarity(&auto.embed(TEXTS[2]).unwrap(), &singles[2]);
    assert!(cos >= 0.999, "auto vs cpu: cosine {cos}");
}
