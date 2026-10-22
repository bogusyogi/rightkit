// rightkit-embed 0.6.1 consumed by exact registry pin, outside the workspace.
//
// Proves the model-free public API: reciprocal_rank_fusion and RRF_K, cosine_similarity,
// truncate_and_normalize, installed_model_dir, the EmbedderConfig builder defaults, bundle_files on an
// empty directory, and Embedder::load refusing a missing directory and an invalid dimension with typed
// errors before any model, tokenizer or ONNX Runtime is touched.
// Does not prove: model loading, embedding, accelerator parity, Core ML or E40 backends, VectorStore.
// State: one empty temp directory, removed at the end. No network, no model files, no real user data.
use std::path::{Path, PathBuf};

use rightkit_embed::{
    bundle_files, cosine_similarity, installed_model_dir, reciprocal_rank_fusion,
    truncate_and_normalize, EmbedError, Embedder, EmbedderConfig, ExecutionProvider,
    DEFAULT_INGEST_BATCH, DEFAULT_MAX_BATCH, RRF_K,
};

fn close(left: f64, right: f64) -> bool {
    (left - right).abs() < 1e-12
}

fn main() -> Result<(), String> {
    // Reciprocal-rank fusion: an item in several lists beats a single top hit, scores are exact.
    assert_eq!(RRF_K, 60.0);
    let lexical = ["a", "b", "c"];
    let semantic = ["b", "d", "a"];
    let fused = reciprocal_rank_fusion(&[&lexical[..], &semantic[..]], RRF_K);
    let order: Vec<&str> = fused.iter().map(|(key, _)| *key).collect();
    assert_eq!(order, vec!["b", "a", "d", "c"]);
    assert!(close(fused[0].1, 1.0 / 62.0 + 1.0 / 61.0));
    // A repeat inside one list counts once, at its first rank.
    let repeated = ["x", "x", "y"];
    let once = reciprocal_rank_fusion(&[&repeated[..]], RRF_K);
    assert_eq!(once.len(), 2);
    assert!(close(once[0].1, 1.0 / 61.0));
    // A negative or non-finite damping constant returns an empty result.
    assert!(reciprocal_rank_fusion(&[&lexical[..]], -1.0).is_empty());
    assert!(reciprocal_rank_fusion(&[&lexical[..]], f64::NAN).is_empty());

    // Cosine similarity: identical, orthogonal, mismatched length and zero vectors.
    assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert_eq!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]), 0.0);
    assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);

    // Truncate and normalise: the leading dimensions are kept and the result has unit length.
    let truncated = truncate_and_normalize(&[3.0, 4.0, 12.0], 2);
    assert_eq!(truncated.len(), 2);
    assert!((truncated[0] - 0.6).abs() < 1e-6 && (truncated[1] - 0.8).abs() < 1e-6);

    // Installed model directory: derived from the executable path only, refusing a non-plain subdir.
    #[cfg(target_os = "macos")]
    assert_eq!(
        installed_model_dir(Path::new("/Applications/Foo.app/Contents/MacOS/foo"), "models"),
        Some(PathBuf::from("/Applications/Foo.app/Contents/Resources/models"))
    );
    #[cfg(windows)]
    assert_eq!(
        installed_model_dir(Path::new("C:\\Apps\\Foo\\foo.exe"), "models"),
        Some(PathBuf::from("C:\\Apps\\Foo\\models"))
    );
    #[cfg(not(any(target_os = "macos", windows)))]
    assert_eq!(
        installed_model_dir(Path::new("/opt/foo/foo"), "models"),
        Some(PathBuf::from("/opt/foo/resources/models"))
    );
    let exe = if cfg!(windows) { "C:\\Apps\\Foo\\foo.exe" } else { "/opt/foo/foo" };
    for bad in ["", "..", ".", "a/b"] {
        assert_eq!(installed_model_dir(Path::new(exe), bad), None, "subdir {bad:?}");
    }
    assert_eq!(installed_model_dir(Path::new("relative/foo"), "models"), None);

    // Config builder: defaults and chained overrides.
    let config = EmbedderConfig::new("model-dir", "runtime-lib");
    assert_eq!(config.provider, ExecutionProvider::Cpu);
    assert_eq!(config.max_batch, DEFAULT_MAX_BATCH);
    assert_eq!(config.ingest_batch, DEFAULT_INGEST_BATCH);
    assert_eq!(config.intra_threads, 1);
    assert!(config.prewarm.is_none() && config.fallback_model_dir.is_none());
    let tuned = config.clone().with_truncate_dim(128).with_max_batch(4).with_intra_threads(2).with_prewarm(false);
    assert_eq!((tuned.truncate_dim, tuned.max_batch, tuned.intra_threads), (128, 4, 2));
    assert_eq!(tuned.prewarm, Some(false));

    // Embedder::load refuses before touching any model: missing directory, then invalid dimension.
    let missing = Embedder::load(EmbedderConfig::new("rightkit-embed-consumer-absent-dir", "runtime-lib"));
    assert!(matches!(missing, Err(EmbedError::MissingModelDirectory(_))));

    let dir: PathBuf = std::env::temp_dir().join(format!("rightkit-embed-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let outcome = (|| -> Result<(), String> {
        // An empty directory holds no recognised bundle.
        assert!(matches!(bundle_files(&dir), Ok(None)));
        let invalid = Embedder::load(EmbedderConfig::new(&dir, "runtime-lib").with_truncate_dim(0));
        assert!(matches!(invalid, Err(EmbedError::InvalidConfig(_))));
        let too_wide = Embedder::load(EmbedderConfig::new(&dir, "runtime-lib").with_truncate_dim(100_000));
        assert!(matches!(too_wide, Err(EmbedError::InvalidConfig(_))));
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&dir);
    outcome?;

    println!("OK rightkit-embed 0.6.1 (registry consumer)");
    Ok(())
}
