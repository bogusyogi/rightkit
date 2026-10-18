//! EG2 matrix rows W12 (resident query latency) and W14 (resident ingestion throughput) for the
//! E40 DirectML export through rightkit-embed. One warm Embedder, no reload between calls.
//! Ignored by default; inputs as in `e40_real_model_e2e.rs`.
//! `cargo test -p rightkit-embed --features inference --test e40_resident_bench -- --ignored --nocapture`
//!
//! Also prints the load phases (`Embedder::load_timings`). With `--features bench-internals` it
//! adds a `SPLIT` line per text: host-side phases against `Session::run`.
//! `EG2_PARITY_CACHE_DIR` (a scratch directory you name) turns the parity verdict cache on, so a
//! second run shows the cached load.
//! Run it with `--release`: the test profile builds this crate, `half` and `tokenizers` at
//! opt-level 0, which inflates the host-side phases.

#![cfg(all(windows, feature = "inference", feature = "vector"))]

mod e40_common;

use e40_common::{cases, input, print_load_timings};
use rightkit_embed::{Embedder, EmbedderConfig, ExecutionProvider};

fn pct(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

#[cfg(feature = "bench-internals")]
fn p50_of(samples: impl Iterator<Item = std::time::Duration>) -> f64 {
    let mut ms: Vec<f64> = samples.map(|d| d.as_secs_f64() * 1000.0).collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    pct(&ms, 0.5)
}

/// One `SPLIT` line per text: p50 of 200 warm calls (after 20 warm-ups) for each phase.
#[cfg(feature = "bench-internals")]
fn split_report(e: &Embedder, cases: &[(&str, String)]) {
    for (label, text) in cases {
        for _ in 0..20 {
            e.bench_embed_query_split(text).unwrap();
        }
        let splits: Vec<_> = (0..200)
            .map(|_| e.bench_embed_query_split(text).unwrap())
            .collect();
        eprintln!(
            "SPLIT {label:>8} ({} tok, graph {}): tokenize {:.2} | build_inputs {:.2} | tensors {:.2} | \
             lookup {:.2} | session.run {:.2} | extract {:.2} | post {:.2} | engine {:.2} | total {:.2} ms (p50 each)",
            splits[0].engine.tokens,
            splits[0].engine.bucket,
            p50_of(splits.iter().map(|s| s.tokenize)),
            p50_of(splits.iter().map(|s| s.engine.build_inputs)),
            p50_of(splits.iter().map(|s| s.engine.tensors)),
            p50_of(splits.iter().map(|s| s.engine.session_lookup)),
            p50_of(splits.iter().map(|s| s.engine.session_run)),
            p50_of(splits.iter().map(|s| s.engine.extract)),
            p50_of(splits.iter().map(|s| s.post)),
            p50_of(splits.iter().map(|s| s.engine.total)),
            p50_of(splits.iter().map(|s| s.total)),
        );
    }
}

#[test]
#[ignore = "benchmark: needs a DirectML GPU, the E40 bundle and an idle machine"]
fn e40_resident_w12_w14() {
    let (Some(dir), Some(runtime)) = (input("RK_EMBED_GEMMA2_E40_DIR"), input("RK_ORT_TEST_DYLIB"))
    else {
        eprintln!("E40 bundle or ONNX Runtime not provided; skipped");
        return;
    };
    let mut config = EmbedderConfig::new(&dir, &runtime)
        .with_truncate_dim(768)
        .with_provider(ExecutionProvider::Auto);
    if let Some(cache) = input("EG2_PARITY_CACHE_DIR") {
        config = config.with_parity_cache_dir(cache);
    }
    // Session options profile: `EG2_SESSION_PROFILE` = `rust` (default), `eg2run`, or overrides
    // such as `intra=0,mem=on` (see `E40SessionProfile::parse`). Read through `input`, so it
    // can sit in tests/inputs.local.json when the build broker strips environment variables.
    // `EG2_PREWARM=0` turns the load-time pre-warm off (for a before/after comparison).
    #[cfg(feature = "bench-internals")]
    {
        let spec = input("EG2_SESSION_PROFILE")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let profile = rightkit_embed::E40SessionProfile::parse(&spec)
            .unwrap_or_else(|e| panic!("EG2_SESSION_PROFILE {spec:?}: {e}"));
        eprintln!("session profile {spec:?} -> {profile:?}");
        config = config.with_e40_session_profile(profile);
    }
    if input("EG2_PREWARM").is_some_and(|v| v.to_string_lossy() == "0") {
        config = config.with_prewarm(false);
    }
    let load = std::time::Instant::now();
    let e = Embedder::load(config).expect("load E40");
    eprintln!(
        "load {:.0} ms, provider {}",
        load.elapsed().as_secs_f64() * 1000.0,
        e.provider_used()
    );
    print_load_timings(e.load_timings());
    eprintln!("parity {:?}", e.accelerator_parity());
    assert_eq!(e.provider_used(), "directml");

    let cases = cases();

    // W12: resident single-call latency per bucket.
    for (label, text) in &cases {
        for _ in 0..20 {
            e.embed_query(text).unwrap();
        }
        let mut ms: Vec<f64> = (0..200)
            .map(|_| {
                let t = std::time::Instant::now();
                e.embed_query(text).unwrap();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        eprintln!(
            "W12 {label:>8}: p50 {:.2} ms  p95 {:.2} ms  min {:.2} ms",
            pct(&ms, 0.5),
            pct(&ms, 0.95),
            ms[0]
        );
    }

    // Host-side phases against Session::run, per text (p50 of 200 warm calls each).
    #[cfg(feature = "bench-internals")]
    {
        split_report(&e, &cases);
    }

    // W14: resident ingestion throughput over a mixed-length corpus, three passes.
    let docs: Vec<&str> = (0..400).map(|i| cases[1 + i % 5].1.as_str()).collect();
    for pass in 1..=3 {
        let t = std::time::Instant::now();
        let out = e.embed_batch(docs.iter().copied()).unwrap();
        let s = t.elapsed().as_secs_f64();
        assert_eq!(out.len(), docs.len());
        eprintln!(
            "W14 pass {pass}: {} docs in {:.2} s = {:.1} docs/s",
            docs.len(),
            s,
            docs.len() as f64 / s
        );
    }
}
