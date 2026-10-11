//! Real-model accelerator vector parity: an accelerator serves inference only
//! if it reproduces the CPU provider's vectors at load.
//!
//! Needs `RIGHTKIT_EMBED_GEMMA2_DIR` (or `RIGHTKIT_EMBED_MODEL_DIR` for the
//! original EmbeddingGemma bundle) and `RIGHTKIT_ORT_TEST_DYLIB`, from the
//! environment or the gitignored `tests/inputs.local.json`. Without them the
//! test reports a skip. On a platform without an accelerator (not Windows, not
//! Apple) `Auto` serves CPU and only the CPU assertions run.

#![cfg(all(feature = "inference", feature = "vector"))]

use rightkit_embed::{
    cosine_similarity, EmbedError, Embedder, EmbedderConfig, ExecutionProvider,
    ACCELERATOR_PARITY_MIN_COSINE,
};

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

const TEXTS: [&str; 3] = [
    "budget meeting forecast",
    "Q3 forecast discussion and budget meeting with finance and the whole planning group about next year",
    "pub struct VectorStore { connection: rusqlite::Connection, dimension: usize } impl VectorStore { pub fn put(&self, key: &str, model: &str, vector: &[f32]) -> Result<(), VectorError> { todo!() } }",
];

/// Native-dimension vectors so the comparison matches what the load-time check
/// compares (before Matryoshka truncation).
fn native_config(dir: &std::path::Path, runtime: &std::path::Path) -> EmbedderConfig {
    EmbedderConfig::new(dir, runtime).with_truncate_dim(768)
}

#[test]
fn cpu_does_no_parity_work() {
    let Some((dir, runtime)) = inputs() else {
        eprintln!("embed model bundle / ORT runtime unavailable; parity gate skipped");
        return;
    };
    let cpu = Embedder::load(native_config(&dir, &runtime)).unwrap();
    assert_eq!(cpu.provider_used(), "cpu");
    assert!(cpu.fallback_reason().is_none());
    assert!(cpu.accelerator_parity().is_none());
}

#[test]
fn auto_serves_cpu_with_a_parity_reason_or_an_accelerator_matching_cpu() {
    let Some((dir, runtime)) = inputs() else {
        eprintln!("embed model bundle / ORT runtime unavailable; parity gate skipped");
        return;
    };
    let cpu = Embedder::load(native_config(&dir, &runtime)).unwrap();
    let auto = Embedder::load(native_config(&dir, &runtime).with_provider(ExecutionProvider::Auto))
        .unwrap();
    eprintln!(
        "auto provider: {} (fallback: {:?}, parity: {:?})",
        auto.provider_used(),
        auto.fallback_reason(),
        auto.accelerator_parity()
    );
    assert_eq!(auto.pipeline_id(), cpu.pipeline_id());
    if auto.provider_used() == "cpu" {
        assert!(auto.accelerator_parity().is_none());
        match auto.fallback_reason() {
            Some(reason) if reason.contains("parity") => {
                assert!(reason.contains("vector parity failed"), "{reason}");
                assert!(reason.contains("floor 0.995"), "{reason}");
            }
            // Registration or the probe failed before parity ran, or there is
            // no accelerator on this platform.
            Some(_) => {}
            None => {
                if cfg!(any(windows, target_vendor = "apple")) {
                    panic!("an accelerator platform must record why Auto serves CPU");
                }
            }
        }
        return;
    }
    let parity = auto
        .accelerator_parity()
        .expect("an admitted accelerator records its parity result");
    assert!(
        parity.lowest_cosine >= ACCELERATOR_PARITY_MIN_COSINE,
        "{parity:?}"
    );
    assert!(auto.fallback_reason().is_none());
    for text in TEXTS {
        let cos = cosine_similarity(&auto.embed(text).unwrap(), &cpu.embed(text).unwrap());
        assert!(
            cos >= ACCELERATOR_PARITY_MIN_COSINE,
            "{}: auto vs cpu cosine {cos} for {text:?}",
            auto.provider_used()
        );
    }
}

#[test]
fn explicit_accelerator_is_admitted_or_errors_never_falls_back() {
    let Some((dir, runtime)) = inputs() else {
        eprintln!("embed model bundle / ORT runtime unavailable; parity gate skipped");
        return;
    };
    let provider = if cfg!(windows) {
        ExecutionProvider::DirectMl
    } else if cfg!(target_vendor = "apple") {
        ExecutionProvider::CoreMl
    } else {
        eprintln!("no accelerator on this platform; explicit-provider parity gate skipped");
        return;
    };
    match Embedder::load(native_config(&dir, &runtime).with_provider(provider)) {
        Ok(embedder) => {
            assert_ne!(embedder.provider_used(), "cpu");
            assert!(embedder.fallback_reason().is_none());
            let parity = embedder.accelerator_parity().expect("parity recorded");
            assert!(parity.lowest_cosine >= ACCELERATOR_PARITY_MIN_COSINE);
        }
        Err(EmbedError::AcceleratorParity {
            lowest_cosine,
            floor,
            detail,
            ..
        }) => {
            assert_eq!(floor, ACCELERATOR_PARITY_MIN_COSINE);
            if let Some(cosine) = lowest_cosine {
                assert!(cosine < floor, "{detail}");
            }
            eprintln!("explicit {provider:?} rejected by the parity check: {detail}");
        }
        // The runtime lacks the provider: loud, and still not a silent fallback.
        Err(other) => eprintln!("explicit {provider:?} failed to load: {other}"),
    }
}
