//! Dumps what the Rust side feeds the E40 graphs for the six `e40_resident_bench` texts, so the
//! Python harness (`.model-bench\eg2-matrix-20261016\token_parity.py`) can run the same graphs
//! on exactly the same token ids. Writes JSON to the path in `EG2_TOKEN_DUMP`; skipped if unset.
//! Per text: label, text, prompt, token ids, graph length, and the 768-d vector `embed_query`
//! returned (to check the Python graph output against it).
//!
//! `EG2_TOKEN_DUMP=<file.json> cargo test -p rightkit-embed --features bench-internals --test e40_token_dump -- --ignored --nocapture`
//! Inputs as in `e40_real_model_e2e.rs`.

#![cfg(all(
    windows,
    feature = "inference",
    feature = "vector",
    feature = "bench-internals"
))]

mod e40_common;

use e40_common::{cases, input};
use rightkit_embed::{Embedder, EmbedderConfig, ExecutionProvider};

#[test]
#[ignore = "needs the E40 bundle and ONNX Runtime; writes the file named by EG2_TOKEN_DUMP"]
fn e40_token_dump() {
    let Some(out_path) = input("EG2_TOKEN_DUMP") else {
        eprintln!("EG2_TOKEN_DUMP not set; skipped");
        return;
    };
    let (Some(dir), Some(runtime)) = (
        input("RIGHTKIT_EMBED_GEMMA2_E40_DIR"),
        input("RIGHTKIT_ORT_TEST_DYLIB"),
    ) else {
        eprintln!("E40 bundle or ONNX Runtime not provided; skipped");
        return;
    };
    // Same configuration as the benchmark, so the tokens and vectors are the ones it times.
    let e = Embedder::load(
        EmbedderConfig::new(&dir, &runtime)
            .with_truncate_dim(768)
            .with_provider(ExecutionProvider::Auto),
    )
    .expect("load E40");
    assert_eq!(e.provider_used(), "directml");

    let mut pad_id = 0;
    let mut rows = Vec::new();
    for (label, text) in cases() {
        let tokens = e.bench_query_tokens(&text).expect("tokenize");
        pad_id = tokens.pad_id;
        let vector = e.embed_query(&text).expect("embed");
        eprintln!(
            "{label:>8}: {} tokens, graph {:?}",
            tokens.ids.len(),
            tokens.graph_tokens
        );
        rows.push(serde_json::json!({
            "label": label,
            "text": text,
            "prompt": tokens.prompt,
            "ids": tokens.ids,
            "graph_tokens": tokens.graph_tokens,
            "vector": vector,
        }));
    }
    let dump = serde_json::json!({
        "model_dir": dir.to_string_lossy(),
        "pad_id": pad_id,
        "provider": e.provider_used(),
        "model_id": e.model_id(),
        "cases": rows,
    });
    std::fs::write(&out_path, serde_json::to_vec_pretty(&dump).unwrap()).expect("write dump");
    eprintln!("wrote {}", std::path::Path::new(&out_path).display());
}
