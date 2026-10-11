//! EmbeddingGemma 2 Q4 (b64acc4) catalog spec: the six files, their sizes and SHA-256
//! digests, the 187,632,172-byte total, and the adopt-only shape (no download route).

use rightkit_models::suite::catalog::{embedding_gemma_q4_spec, SpecSource, EMBEDDINGGEMMA2_Q4_ID};

/// (path, bytes, sha256) for revision b64acc4, as published.
const EXPECTED: [(&str, u64, &str); 6] = [
    (
        "model_q4.onnx",
        475_574,
        "8c7c392bc8a6989a0dbfb524d819a635de088812691acc61e898e13597e36336",
    ),
    (
        "model_q4.onnx_data",
        154_979_328,
        "e976952e890a9c515e62ecf67a4129aecbe92c7994f7cc7b9f6d33a5fb33c04e",
    ),
    (
        "tokenizer.json",
        32_170_510,
        "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4",
    ),
    (
        "tokenizer_config.json",
        1_599,
        "17bd5d6e9364ca49a534e1502076593317c298d4a663623091ed45388f004874",
    ),
    (
        "config.json",
        5_031,
        "8d011bfe08b5e345bbe0b81e5c6fd02c381920b345b986047bc2a33ce7b90d1d",
    ),
    (
        "special_tokens_map.json",
        130,
        "61f5ea3e7ed0547b2d45ef93c1321de2b9e3429463041dc97d26057db11382d0",
    ),
];

#[test]
fn embedding_gemma_spec_pins_six_files_sizes_digests_and_total() {
    let spec = embedding_gemma_q4_spec();
    assert_eq!(spec.id, "embeddinggemma2-q4");
    assert_eq!(spec.id, EMBEDDINGGEMMA2_Q4_ID);
    assert_eq!(spec.version, "b64acc4");

    let total: u64 = EXPECTED.iter().map(|(_, bytes, _)| bytes).sum();
    assert_eq!(total, 187_632_172);
    assert_eq!(spec.size_hint_bytes, total);

    assert_eq!(spec.files.len(), EXPECTED.len());
    for (file, (path, bytes, sha)) in spec.files.iter().zip(EXPECTED.iter()) {
        assert_eq!(file.path, *path);
        assert_eq!(file.size, Some(*bytes), "{path} size");
        assert_eq!(file.sha256, Some(*sha), "{path} sha256");
    }

    let SpecSource::Files(remote) = &spec.source else {
        panic!("EmbeddingGemma 2 must use the per-file source, not an archive");
    };
    assert_eq!(remote.len(), EXPECTED.len());
    for (file, (path, bytes, sha)) in remote.iter().zip(EXPECTED.iter()) {
        assert_eq!(file.path, *path);
        assert_eq!(file.size, Some(*bytes), "{path} remote size");
        assert_eq!(file.sha256, Some(*sha), "{path} remote sha256");
        assert!(file.url.is_none(), "{path} must have no download url");
    }

    let manifest = spec.expected_manifest();
    assert_eq!(manifest.files.len(), EXPECTED.len());
}

#[test]
fn embedding_gemma_has_no_download_route_and_no_grant() {
    let spec = embedding_gemma_q4_spec();
    // Route not served: adopt a local copy only.
    assert!(!spec.can_download());
    assert!(!spec.requires_auth());
    assert!(!spec.requires_grant());
    assert!(!spec.requires_entitlement());
    assert!(spec.is_add_on());
}

#[test]
fn embedding_gemma_is_not_in_base_or_add_on_lists() {
    use rightkit_models::suite::catalog::{add_ons, base_catalog};
    assert!(!base_catalog().iter().any(|s| s.id == EMBEDDINGGEMMA2_Q4_ID));
    assert!(!add_ons().iter().any(|s| s.id == EMBEDDINGGEMMA2_Q4_ID));
}
