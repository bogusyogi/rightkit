use rightkit_ocr::{OcrArtifactManifest, OcrError, OcrProvenance};
use sha2::{Digest, Sha256};

#[test]
fn unresolved_ppocr_label_conflict_fails_closed() {
    let manifest = OcrArtifactManifest::characterized_windows_contract();
    assert!(matches!(
        manifest.provenance,
        OcrProvenance::Conflicting { .. }
    ));
    let error = manifest.validate_metadata().unwrap_err();
    assert!(matches!(error, OcrError::UnresolvedProvenance { .. }));
}

#[test]
fn provenance_resolution_requires_label_source_and_license() {
    let error = OcrArtifactManifest::characterized_windows_contract()
        .with_resolved_provenance("", "signed-catalog", "model-license-id")
        .unwrap_err();
    assert!(matches!(error, OcrError::InvalidProvenance));

    let error = OcrArtifactManifest::characterized_windows_contract()
        .with_resolved_provenance("authoritative-label", "signed-catalog", "")
        .unwrap_err();
    assert!(matches!(error, OcrError::InvalidProvenance));
}

#[test]
fn committed_fixture_and_model_digests_are_locked() {
    let manifest = OcrArtifactManifest::characterized_windows_contract();
    assert_eq!(
        manifest.detector_sha256,
        "61824840edf6e74581898930b8091b1b2318f4b2705a2e8a40ad3de7ac480133"
    );
    assert_eq!(
        manifest.recognizer_sha256,
        "4e16deb22c4da6468bdca539b2cd3c8687825538b67109177c47d359ab994cd7"
    );
    assert_eq!(
        manifest.dictionary_sha256,
        "e025a66d31f327ba0c232e03f407ae8d105e1e709e7ccb3f408aa778c24e70d6"
    );
    assert_eq!(
        manifest.expected_fixture_output,
        "RIGHTKIT OCR 2026\nShared parity fixture"
    );
}

#[test]
fn model_directory_must_be_absolute() {
    let manifest = OcrArtifactManifest::characterized_windows_contract()
        .with_resolved_provenance(
            "PP-OCR-authoritative",
            "signed-test-catalog",
            "model-license-id",
        )
        .unwrap();
    let error = manifest
        .validate_model_dir(std::path::Path::new("models/ocr"))
        .unwrap_err();
    assert!(matches!(error, OcrError::UnsafeArtifactPath { .. }));
}

#[test]
fn committed_fixture_bytes_match_characterization_digest() {
    use base64::Engine;
    let encoded = include_str!("fixtures/rightkit-ocr-parity.png.base64").trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(bytes)),
        OcrArtifactManifest::characterized_windows_contract().fixture_sha256
    );
}

#[test]
fn artifact_digest_mismatch_fails_before_ort_loading() {
    let root = std::env::temp_dir().join(format!("rightkit-ocr-artifacts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for name in ["det.onnx", "rec.onnx", "en_dict.txt"] {
        std::fs::write(root.join(name), b"wrong-artifact").unwrap();
    }
    let manifest = OcrArtifactManifest::characterized_windows_contract()
        .with_resolved_provenance(
            "PP-OCR-authoritative",
            "signed-test-catalog",
            "model-license-id",
        )
        .unwrap();
    let error = manifest.validate_model_dir(&root).unwrap_err();
    assert!(matches!(error, OcrError::DigestMismatch { .. }));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn crate_bundles_no_model_or_runtime_binary() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
                continue;
            }
            let path = entry.path();
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("");
            assert!(!["onnx", "dll", "dylib", "so"].contains(&extension));
        }
    }
}
