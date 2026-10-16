//! EmbeddingGemma 2 accelerator bundles: the Core ML build (macOS, Hugging Face at a pinned
//! commit) and the E40 fp16 DirectML export (Windows, model-worker route pending). The raw
//! pin tables exist on every host so the pins are checked everywhere; the specs exist only on
//! their own platform.

use rightkit_models::suite::catalog::{
    add_ons, all_models, base_catalog, embedding_gemma2_platform_specs, SpecFile,
    EMBEDDINGGEMMA2_COREML_COMMIT, EMBEDDINGGEMMA2_COREML_FILES, EMBEDDINGGEMMA2_COREML_ID,
    EMBEDDINGGEMMA2_COREML_REMOTE_FILES, EMBEDDINGGEMMA2_COREML_SIZE,
    EMBEDDINGGEMMA2_E40_DML_FILES, EMBEDDINGGEMMA2_E40_DML_ID,
    EMBEDDINGGEMMA2_E40_DML_REMOTE_FILES, EMBEDDINGGEMMA2_E40_DML_REVISION,
    EMBEDDINGGEMMA2_E40_DML_SIZE, MODEL_DOWNLOAD_BASE,
};

/// (path, bytes, sha256) of the Core ML text bundle at commit 3f978ddf.
const COREML_EXPECTED: [(&str, u64, &str); 6] = [
    (
        "EmbeddingGemma2Text.mlpackage/Manifest.json",
        617,
        "83eace80b69b46f8415af620ba1b40d7c490d00f7c00ba9dedd2e29da6e9ac19",
    ),
    (
        "EmbeddingGemma2Text.mlpackage/Data/com.apple.CoreML/model.mlmodel",
        7_323_423,
        "0df9aca02807cf490db6d4f1c68607a8c7f486ad23b376d1968fd3be5eb14677",
    ),
    (
        "EmbeddingGemma2Text.mlpackage/Data/com.apple.CoreML/weights/weight.bin",
        276_815_424,
        "75b97ab036ca87eae60560c4034a63d604201498dc4b6f6971dd0b1e4558d61f",
    ),
    (
        "embeddings.bf16",
        268_435_456,
        "56ebbdbfd706c827f3fae1d97aa39135c7ddc0f1e6d9b12f8565f84bbfbda877",
    ),
    (
        "tokenizer.json",
        32_170_510,
        "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4",
    ),
    (
        "config.json",
        2_845,
        "30668aba89f54a200a3fa38913f21e2a2f18aba7215de67c1806542a75b9629b",
    ),
];

/// (path, bytes, sha256) of the E40 export: the eg2-directml README expected-output table
/// plus the upstream tokenizer.
const E40_EXPECTED: [(&str, u64, &str); 8] = [
    (
        "model_32.onnx",
        416_097,
        "8e1aaddd28ca5437e2da0da5f9dac5ea54766dc26b0bb760f104f5963c3bb418",
    ),
    (
        "model_64.onnx",
        416_179,
        "bcc8b63ea05c9963130cd2995be013f8dd516e40fc6bdac81b1313ea6ca5c2cc",
    ),
    (
        "model_128.onnx",
        417_636,
        "7afb92bf83831e3c496a993555f82f85afa9b107b77ef13297aa12d2f9d7e1f8",
    ),
    (
        "model_256.onnx",
        417_638,
        "1874b05842faf3c9b3badc6261765d95ad809a0ec2afc6f7dbb647aa161f8ce0",
    ),
    (
        "model_512.onnx",
        417_641,
        "f6fd91519cf1023da0adcedeab9fb7c77b01913f84ed647bb79d7f839994b741",
    ),
    (
        "model.onnx_data",
        290_203_648,
        "c398505134b2bb717becc109c00408ecd9faef0a8418981a549653aea6bb73ac",
    ),
    (
        "embed_tokens.f16.npy",
        268_435_584,
        "b574d03a53a4cfbcf76d07dc291d2ffe614ca7720c79cb655ec338f583ee53e8",
    ),
    (
        "tokenizer.json",
        32_170_510,
        "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4",
    ),
];

fn assert_pins(files: &[SpecFile], expected: &[(&str, u64, &str)]) {
    assert_eq!(files.len(), expected.len());
    for (file, (path, bytes, sha)) in files.iter().zip(expected) {
        assert_eq!(file.path, *path);
        assert_eq!(file.size, Some(*bytes), "{path} size");
        assert_eq!(file.sha256, Some(*sha), "{path} sha256");
    }
}

#[test]
fn coreml_pins_cover_the_text_package_table_tokenizer_and_config() {
    assert_pins(EMBEDDINGGEMMA2_COREML_FILES, &COREML_EXPECTED);
    let total: u64 = COREML_EXPECTED.iter().map(|(_, bytes, _)| bytes).sum();
    assert_eq!(total, 584_748_275);
    assert_eq!(EMBEDDINGGEMMA2_COREML_SIZE, total);
    // The audio and vision packages are not part of the text bundle.
    assert!(EMBEDDINGGEMMA2_COREML_FILES
        .iter()
        .all(|f| !f.path.contains("Audio") && !f.path.contains("Vision")));
    assert_eq!(EMBEDDINGGEMMA2_COREML_COMMIT.len(), 40);
}

#[test]
fn coreml_urls_are_per_file_at_the_full_commit_and_match_the_pins() {
    assert_eq!(
        EMBEDDINGGEMMA2_COREML_REMOTE_FILES.len(),
        COREML_EXPECTED.len()
    );
    for (file, (path, bytes, sha)) in EMBEDDINGGEMMA2_COREML_REMOTE_FILES
        .iter()
        .zip(COREML_EXPECTED.iter())
    {
        assert_eq!(file.path, *path);
        assert_eq!(file.size, Some(*bytes));
        assert_eq!(file.sha256, Some(*sha));
        let expected = format!(
            "https://huggingface.co/FluidInference/embeddinggemma-2-coreml/resolve/{}/{}",
            EMBEDDINGGEMMA2_COREML_COMMIT, path
        );
        assert_eq!(file.url, Some(expected.as_str()), "{path} url");
        assert!(!path.contains(':') && !path.contains("..") && !path.starts_with('/'));
    }
}

#[test]
fn e40_pins_are_the_readme_table_plus_tokenizer() {
    assert_pins(EMBEDDINGGEMMA2_E40_DML_FILES, &E40_EXPECTED);
    let total: u64 = E40_EXPECTED.iter().map(|(_, bytes, _)| bytes).sum();
    assert_eq!(total, 592_894_933);
    assert_eq!(EMBEDDINGGEMMA2_E40_DML_SIZE, total);
}

#[test]
fn e40_urls_follow_the_parakeet_worker_layout_and_match_the_pins() {
    assert_eq!(
        EMBEDDINGGEMMA2_E40_DML_REMOTE_FILES.len(),
        E40_EXPECTED.len()
    );
    for (file, (path, bytes, sha)) in EMBEDDINGGEMMA2_E40_DML_REMOTE_FILES
        .iter()
        .zip(E40_EXPECTED.iter())
    {
        assert_eq!(file.path, *path);
        assert_eq!(file.size, Some(*bytes));
        assert_eq!(file.sha256, Some(*sha));
        let expected = format!(
            "{MODEL_DOWNLOAD_BASE}/{EMBEDDINGGEMMA2_E40_DML_ID}/{EMBEDDINGGEMMA2_E40_DML_REVISION}/{path}"
        );
        assert_eq!(file.url, Some(expected.as_str()), "{path} url");
    }
}

#[test]
fn neither_bundle_is_in_the_base_or_add_on_lists() {
    for id in [EMBEDDINGGEMMA2_COREML_ID, EMBEDDINGGEMMA2_E40_DML_ID] {
        assert!(!base_catalog().iter().any(|s| s.id == id));
        assert!(!add_ons().iter().any(|s| s.id == id));
        assert!(!all_models().iter().any(|s| s.id == id));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_holds_only_the_coreml_spec_and_can_download_it() {
    use rightkit_models::suite::catalog::{embedding_gemma2_coreml_spec, SpecSource};
    let spec = embedding_gemma2_coreml_spec();
    assert_eq!(spec.id, EMBEDDINGGEMMA2_COREML_ID);
    assert_eq!(spec.size_hint_bytes, EMBEDDINGGEMMA2_COREML_SIZE);
    assert!(spec.can_download());
    assert!(!spec.requires_auth() && !spec.requires_grant() && !spec.requires_entitlement());
    assert_pins(spec.files, &COREML_EXPECTED);
    let SpecSource::Files(remote) = &spec.source else {
        panic!("Core ML bundle must use the per-file source");
    };
    assert!(remote.iter().all(|f| f.url.is_some()));
    assert_eq!(spec.expected_manifest().files.len(), COREML_EXPECTED.len());
    let ids: Vec<_> = embedding_gemma2_platform_specs()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, [EMBEDDINGGEMMA2_COREML_ID]);
}

#[cfg(windows)]
#[test]
fn windows_holds_only_the_e40_spec_and_can_download_it() {
    use rightkit_models::suite::catalog::{embedding_gemma2_e40_dml_spec, SpecSource};
    let spec = embedding_gemma2_e40_dml_spec();
    assert_eq!(spec.id, EMBEDDINGGEMMA2_E40_DML_ID);
    assert_eq!(spec.version, EMBEDDINGGEMMA2_E40_DML_REVISION);
    assert_eq!(spec.size_hint_bytes, EMBEDDINGGEMMA2_E40_DML_SIZE);
    assert!(spec.can_download());
    assert!(!spec.requires_auth() && !spec.requires_grant() && !spec.requires_entitlement());
    assert_pins(spec.files, &E40_EXPECTED);
    let SpecSource::Files(remote) = &spec.source else {
        panic!("E40 bundle must use the per-file source");
    };
    assert!(remote.iter().all(|f| f.url.is_some()));
    assert_eq!(spec.expected_manifest().files.len(), E40_EXPECTED.len());
    let ids: Vec<_> = embedding_gemma2_platform_specs()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids, [EMBEDDINGGEMMA2_E40_DML_ID]);
}

#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn other_platforms_hold_neither_spec() {
    assert!(embedding_gemma2_platform_specs().is_empty());
}
