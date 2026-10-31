#![cfg(feature = "onnx")]

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
        "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1"
    );
    assert_eq!(
        manifest.recognizer_sha256,
        "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634"
    );
    assert_eq!(
        manifest.dictionary_sha256,
        "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d"
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
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
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

/// The files `cargo package` ships for this crate: the `include` globs of its Cargo.toml. The
/// crate directory also holds `target/` (the build output of this crate's own workspace), which is
/// not a packaged path, so it is never walked. Only `dir/**` and exact file entries are understood;
/// any other pattern fails the test instead of being skipped.
fn packaged_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let mut patterns = Vec::new();
    let mut in_include = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with("include = [") {
            in_include = true;
        }
        if in_include {
            patterns.extend(line.split('"').skip(1).step_by(2).map(str::to_owned));
            if line.ends_with(']') {
                break;
            }
        }
    }
    assert!(!patterns.is_empty(), "Cargo.toml declares no include list");

    let mut files = Vec::new();
    for pattern in patterns {
        if let Some(dir) = pattern.strip_suffix("/**") {
            let mut pending = vec![root.join(dir)];
            while let Some(dir) = pending.pop() {
                for entry in std::fs::read_dir(dir).unwrap() {
                    let entry = entry.unwrap();
                    if entry.file_type().unwrap().is_dir() {
                        pending.push(entry.path());
                    } else {
                        files.push(entry.path());
                    }
                }
            }
        } else {
            assert!(
                !pattern.contains(['*', '?', '[']),
                "unsupported include pattern {pattern:?}"
            );
            let path = root.join(&pattern);
            if path.is_file() {
                files.push(path);
            }
        }
    }
    files
}

#[test]
fn crate_bundles_no_model_or_runtime_binary() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = packaged_files(root);
    assert!(!files.is_empty(), "no packaged files found");
    for path in files {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        assert!(
            !["onnx", "dll", "dylib", "so"].contains(&extension),
            "{} is a model or runtime binary inside the packaged set",
            path.display()
        );
    }
}
