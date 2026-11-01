//! rightkit-embed: embed real texts with the local EmbeddingGemma ONNX model, rank them by cosine, persist the
//! vectors in the SQLite `VectorStore` (file-backed, reopened), and check batch/single agreement and the pipeline id.
//! The vector store's own ranking, persistence and validation are checked first with fixed vectors.
//!
//! Needs, and fails naming them when absent:
//!   * the ONNX Runtime library (see `probes::onnx`: `<resources>/runtime/<libonnxruntime.dylib|onnxruntime.dll>`);
//!   * an EmbeddingGemma bundle in `<resources>/models/embed/` (`rightkit_embed::installed_model_dir`-style layout:
//!     `<resources>` is `Contents/Resources` beside a macOS `Contents/MacOS` executable, the executable's directory on
//!     Windows): `model_q4.onnx`, `model_q4.onnx_data`, `tokenizer.json`, `config.json` (EmbeddingGemma 2 Q4 text export;
//!     the directory's `config.json` `model_type` picks the model). Core ML (`EmbeddingGemma2Text.mlpackage`,
//!     `embeddings.bf16`, `tokenizer.json`, `config.json`) and E40 bundles are also loaded by `Embedder::load` if present.
use rightkit_embed::{bundle_files, cosine_similarity, Embedder, EmbedderConfig, VectorStore};
use serde_json::{json, Value};

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    // 1. The vector store, with fixed vectors: ranking, persistence across reopen, model scoping, validation.
    let dir = ctx.dir("embed")?;
    let db = dir.join("vectors.sqlite");
    {
        let mut store = VectorStore::open(&db, 3).map_err(|e| format!("VectorStore::open: {e}"))?;
        store
            .put_batch([
                ("near", "m1", &[1.0, 0.0, 0.0][..]),
                ("middle", "m1", &[0.7, 0.7, 0.0][..]),
                ("far", "m1", &[0.0, 0.0, 1.0][..]),
                ("other-model", "m2", &[1.0, 0.0, 0.0][..]),
            ])
            .map_err(|e| format!("put_batch: {e}"))?;
        let rank: Vec<String> = store
            .nearest("m1", &[1.0, 0.0, 0.0], 5)
            .map_err(|e| format!("nearest: {e}"))?
            .into_iter()
            .map(|n| n.key)
            .collect();
        if rank != ["near", "middle", "far"] {
            return Err(format!("fixed-vector ranking was {rank:?}"));
        }
        if store.put("bad", "m1", &[1.0]).is_ok()
            || store.put("bad", "m1", &[1.0, f32::NAN, 0.0]).is_ok()
        {
            return Err("the store accepted a wrong-dimension or non-finite vector".into());
        }
    }
    let reopened = VectorStore::open(&db, 3).map_err(|e| format!("reopen: {e}"))?;
    if reopened.len().map_err(|e| format!("len: {e}"))? != 4
        || reopened
            .clear_model("m2")
            .map_err(|e| format!("clear_model: {e}"))?
            != 1
    {
        return Err("vectors did not persist across reopen".into());
    }
    drop(reopened);

    // 2. The model.
    let model_dir = super::models::asset_dir("embed")?;
    let have = bundle_files(&model_dir)
        .map_err(|e| format!("model bundle in {}: {e}", model_dir.display()))?;
    if have.is_none() {
        return Err(format!(
            "host_missing:models: needs an EmbeddingGemma bundle in {} (model_q4.onnx, model_q4.onnx_data, tokenizer.json, config.json); \
             the vector store itself passed",
            model_dir.display()
        ));
    }
    let runtime = super::onnx::ensure_runtime()?;
    let embedder = Embedder::load(EmbedderConfig::new(&model_dir, &runtime.path))
        .map_err(|e| format!("Embedder::load: {e}"))?;
    let dim = embedder.dimension();
    if dim == 0 || embedder.native_dim() < dim {
        return Err(format!(
            "dimension {dim} of native {}",
            embedder.native_dim()
        ));
    }

    let query = embedder
        .embed_query("budget meeting forecast")
        .map_err(|e| format!("embed_query: {e}"))?;
    let docs = [
        ("related", "Q3 forecast discussion and budget meeting"),
        (
            "related2",
            "quarterly financial planning session about next year's budget",
        ),
        ("unrelated", "recipe for lemon cake with vanilla icing"),
    ];
    let vectors = embedder
        .embed_batch(docs.iter().map(|d| d.1))
        .map_err(|e| format!("embed_batch: {e}"))?;
    if vectors.len() != docs.len()
        || vectors
            .iter()
            .any(|v| v.len() != dim || v.iter().any(|x| !x.is_finite()))
    {
        return Err("embed_batch returned the wrong number or size of vectors".into());
    }
    if (norm(&query) - 1.0).abs() > 1e-3 || vectors.iter().any(|v| (norm(v) - 1.0).abs() > 1e-3) {
        return Err("vectors are not L2-normalised".into());
    }
    let score = |i: usize| cosine_similarity(&query, &vectors[i]);
    if !(score(0) > score(2) && score(1) > score(2)) {
        return Err(format!(
            "cosine ranking is wrong: related {:.3}, related2 {:.3}, unrelated {:.3}",
            score(0),
            score(1),
            score(2)
        ));
    }
    // Batch and single-text results agree.
    let single = embedder
        .embed(docs[0].1)
        .map_err(|e| format!("embed: {e}"))?;
    let agree = cosine_similarity(&single, &vectors[0]);
    if agree < 0.999 {
        return Err(format!(
            "batched and single embeddings differ: cosine {agree:.5}"
        ));
    }
    let id = embedder.pipeline_id();
    if !id.starts_with("rkpipe1-") || id.starts_with("rkpipe1-unavailable") {
        return Err(format!("pipeline id {id}"));
    }

    // 3. Persist and search the real vectors.
    let db2 = dir.join("embedded.sqlite");
    let mut store =
        VectorStore::open(&db2, dim).map_err(|e| format!("open embedded store: {e}"))?;
    store
        .put_batch(
            docs.iter()
                .zip(&vectors)
                .map(|((key, _), v)| (*key, embedder.model_id(), v.as_slice())),
        )
        .map_err(|e| format!("put real vectors: {e}"))?;
    let top = store
        .nearest(embedder.model_id(), &query, 1)
        .map_err(|e| format!("nearest: {e}"))?;
    if !top.first().is_some_and(|n| n.key.starts_with("related")) {
        return Err(format!(
            "the nearest stored vector was {:?}",
            top.first().map(|n| &n.key)
        ));
    }

    Ok(json!({
        "model_id": embedder.model_id(),
        "dimension": dim,
        "provider": embedder.provider_used(),
        "pipeline_id": id,
        "cos_related": score(0),
        "cos_unrelated": score(2),
        "batch_vs_single_cosine": agree,
    }))
}
