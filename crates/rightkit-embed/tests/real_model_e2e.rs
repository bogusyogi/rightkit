#![cfg(all(target_os = "macos", feature = "inference", feature = "vector"))]

use rightkit_embed::{Embedder, EmbedderConfig, VectorStore, EMBEDDINGGEMMA_MODEL_ID};

const MODEL_DIR: &str = "/Applications/CodeRight.app/Contents/Resources/embed-model";
const RUNTIME: &str = "/Applications/CodeRight.app/Contents/Resources/runtime/libonnxruntime.dylib";

fn model_dir() -> std::path::PathBuf {
    std::env::var_os("RIGHTKIT_EMBED_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| MODEL_DIR.into())
}

fn runtime_path() -> std::path::PathBuf {
    std::env::var_os("RK_ORT_TEST_DYLIB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| RUNTIME.into())
}

#[test]
fn embeddinggemma_real_model_ranks_related_text_and_persists_vector() {
    let model_dir = model_dir();
    let runtime = runtime_path();
    if !model_dir.join("model_q4.onnx").is_file() || !runtime.is_file() {
        eprintln!("CodeRight model resources unavailable; real-model gate skipped");
        return;
    }
    // QA opt-out (bogusyogi/rightkit#889): CodeRight bundles ONNX Runtime 1.26.0, which the
    // rightkit-onnx 0.2.7 bind-time guard refuses on an SME2 host (M4 and newer). This gate checks
    // ranking and persistence on the real model, not the Conv leak, so bind it explicitly.
    let binding = rightkit_onnx::RuntimeBindingOptions {
        allow_known_bad_runtime: true,
        ..Default::default()
    };
    let embedder =
        Embedder::load(EmbedderConfig::new(model_dir, runtime).with_runtime_binding(binding))
            .unwrap();
    assert_eq!(embedder.model_id(), EMBEDDINGGEMMA_MODEL_ID);
    assert_eq!(embedder.dimension(), 256);
    let query = embedder.embed_query("budget meeting forecast").unwrap();
    let related = embedder
        .embed("Q3 forecast discussion and budget meeting")
        .unwrap();
    let unrelated = embedder
        .embed("recipe for lemon cake with vanilla icing")
        .unwrap();
    assert_eq!(query.len(), 256);
    assert!(
        rightkit_embed::cosine_similarity(&query, &related)
            > rightkit_embed::cosine_similarity(&query, &unrelated)
    );

    let store = VectorStore::in_memory(256).unwrap();
    store.put("related", embedder.model_id(), &related).unwrap();
    store
        .put("unrelated", embedder.model_id(), &unrelated)
        .unwrap();
    let hit = store.nearest(embedder.model_id(), &query, 1).unwrap();
    assert_eq!(hit[0].key, "related");
}
