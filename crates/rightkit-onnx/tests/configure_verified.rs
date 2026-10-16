//! `configure_runtime_verified` fails closed. One test function, so the process
//! environment is only touched in a fixed order: every refusal first, then success.

use std::path::{Path, PathBuf};

use rightkit_onnx::{
    configure_runtime_verified, runtime_filename, CandidateSource, RuntimeCandidate, RuntimeError,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn candidate(path: &Path) -> RuntimeCandidate {
    RuntimeCandidate::new(path.to_path_buf(), CandidateSource::Explicit)
}

#[test]
fn configure_verified_binds_only_a_digest_checked_library() {
    std::env::remove_var("ORT_DYLIB_PATH");
    let dir = std::env::temp_dir().join(format!("rightkit-onnx-verified-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let lib = dir.join(runtime_filename());
    let bytes: &[u8] = b"onnxruntime-library-bytes";
    std::fs::write(&lib, bytes).unwrap();
    std::fs::write(dir.join("unrelated.bin"), b"unlisted").unwrap();

    // No manifest: refused, environment untouched.
    assert!(matches!(
        configure_runtime_verified([candidate(&lib)]).unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));
    assert!(std::env::var_os("ORT_DYLIB_PATH").is_none());

    // Digest mismatch: refused, environment untouched.
    let manifest = |digest: String| {
        json!({
            "appField": true,
            "files": [
                { "path": runtime_filename(), "sha256": digest, "bytes": bytes.len() },
                { "path": "models/absent.onnx", "sha256": "nope", "bytes": 1 }
            ]
        })
    };
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&manifest(sha256_hex(b"something else"))).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        configure_runtime_verified([candidate(&lib)]).unwrap_err(),
        RuntimeError::BundleFileHash { .. }
    ));
    assert!(std::env::var_os("ORT_DYLIB_PATH").is_none());

    // Matching digest, tolerant manifest and unlisted files: bound.
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&manifest(sha256_hex(bytes))).unwrap(),
    )
    .unwrap();
    let selection = configure_runtime_verified([candidate(&lib)]).expect("verified library binds");
    assert_eq!(selection.path, lib.canonicalize().unwrap());
    assert_eq!(
        PathBuf::from(std::env::var_os("ORT_DYLIB_PATH").unwrap()),
        selection.path
    );

    let _ = std::fs::remove_dir_all(&dir);
}
