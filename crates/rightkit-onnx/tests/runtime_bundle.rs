//! `verify_runtime_manifest` against temporary bundles. No ONNX Runtime is loaded.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rightkit_onnx::{runtime_filename, verify_runtime_manifest, RuntimeError};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary `root/` holding `root/runtime/`, the bundle directory under test.
struct Bundle {
    root: PathBuf,
    dir: PathBuf,
}

impl Bundle {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "rightkit-ort-bundle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let dir = root.join("runtime");
        std::fs::create_dir_all(&dir).unwrap();
        Self { root, dir }
    }

    fn write(&self, relative: &str, bytes: &[u8]) {
        let path = self.dir.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn write_manifest(&self, value: &Value) {
        std::fs::write(
            self.dir.join("manifest.json"),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }

    fn verify(&self) -> Result<rightkit_onnx::VerifiedRuntime, RuntimeError> {
        verify_runtime_manifest(&self.dir)
    }
}

impl Drop for Bundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn entry(path: &str, content: &[u8]) -> Value {
    json!({ "path": path, "sha256": sha256_hex(content), "bytes": content.len() })
}

fn manifest(files: Vec<Value>) -> Value {
    json!({
        "schema": 1,
        "platform": "windows-x64",
        "onnxruntime": {
            "version": "1.24.4",
            "variant": "directml",
            "catalogRow": "onnxruntime-1.24.4-directml-windows-x64"
        },
        "files": files
    })
}

const LIBRARY_BYTES: &[u8] = b"onnxruntime-library-bytes";
const DIRECTML_BYTES: &[u8] = b"directml-library-bytes";
const MODEL_BYTES: &[u8] = b"yunet-model-bytes";

/// The spec layout: library, provider DLL, and a nested model.
fn spec_layout() -> Bundle {
    let bundle = Bundle::new();
    bundle.write(runtime_filename(), LIBRARY_BYTES);
    bundle.write("DirectML.dll", DIRECTML_BYTES);
    bundle.write("models/face_detection_yunet_2023mar.onnx", MODEL_BYTES);
    bundle.write_manifest(&manifest(vec![
        entry(runtime_filename(), LIBRARY_BYTES),
        // Uppercase digests in the manifest are accepted.
        json!({
            "path": "DirectML.dll",
            "sha256": sha256_hex(DIRECTML_BYTES).to_uppercase(),
            "bytes": DIRECTML_BYTES.len()
        }),
        entry("models/face_detection_yunet_2023mar.onnx", MODEL_BYTES),
    ]));
    bundle
}

#[test]
fn ok_bundle_verifies_every_file_and_reports_identity() {
    let bundle = spec_layout();
    let verified = bundle.verify().expect("valid bundle verifies");

    assert_eq!(verified.dir, bundle.dir.canonicalize().unwrap());
    assert_eq!(verified.platform, "windows-x64");
    assert_eq!(verified.onnxruntime.version, "1.24.4");
    assert_eq!(verified.onnxruntime.variant, "directml");
    assert_eq!(
        verified.onnxruntime.catalog_row,
        "onnxruntime-1.24.4-directml-windows-x64"
    );
    let paths: Vec<&str> = verified.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            runtime_filename(),
            "DirectML.dll",
            "models/face_detection_yunet_2023mar.onnx"
        ]
    );
    assert_eq!(verified.files[1].sha256, sha256_hex(DIRECTML_BYTES));
    assert_eq!(verified.files[1].bytes, DIRECTML_BYTES.len() as u64);
    assert_eq!(
        verified.onnxruntime_library_path(),
        Some(verified.dir.join(runtime_filename()))
    );
}

#[test]
fn library_path_is_none_when_the_bundle_does_not_list_the_library() {
    let bundle = Bundle::new();
    bundle.write("DirectML.dll", DIRECTML_BYTES);
    bundle.write_manifest(&manifest(vec![entry("DirectML.dll", DIRECTML_BYTES)]));
    let verified = bundle.verify().expect("verifies");
    assert_eq!(verified.onnxruntime_library_path(), None);
}

#[test]
fn bad_hash_is_rejected_even_when_the_size_matches() {
    let bundle = Bundle::new();
    bundle.write(runtime_filename(), LIBRARY_BYTES);
    bundle.write_manifest(&manifest(vec![entry(runtime_filename(), LIBRARY_BYTES)]));
    // Same length, different content.
    bundle.write(runtime_filename(), &vec![b'x'; LIBRARY_BYTES.len()]);
    match bundle.verify().unwrap_err() {
        RuntimeError::BundleFileHash {
            expected, actual, ..
        } => {
            assert_eq!(expected, sha256_hex(LIBRARY_BYTES));
            assert_eq!(actual, sha256_hex(&vec![b'x'; LIBRARY_BYTES.len()]));
        }
        other => panic!("expected hash mismatch, got {other:?}"),
    }
}

#[test]
fn bad_size_is_rejected() {
    let bundle = Bundle::new();
    bundle.write("model.onnx", b"abcd");
    bundle.write_manifest(&manifest(vec![json!({
        "path": "model.onnx",
        "sha256": sha256_hex(b"abcd"),
        "bytes": 5
    })]));
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleFileSize {
            expected: 5,
            actual: 4,
            ..
        }
    ));
}

#[test]
fn missing_listed_file_is_rejected() {
    let bundle = Bundle::new();
    bundle.write_manifest(&manifest(vec![entry("models/absent.onnx", MODEL_BYTES)]));
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleFileMissing { path } if path.ends_with("absent.onnx")
    ));
}

#[test]
fn extra_file_not_in_the_manifest_is_rejected_at_top_level_and_nested() {
    let bundle = spec_layout();
    bundle.write("extra.bin", b"not listed");
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleFileUnlisted { path } if path.ends_with("extra.bin")
    ));

    std::fs::remove_file(bundle.dir.join("extra.bin")).unwrap();
    bundle.write("models/extra.onnx", b"not listed either");
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleFileUnlisted { path } if path.ends_with("extra.onnx")
    ));
}

#[test]
fn listed_path_escaping_the_bundle_is_rejected() {
    let bundle = Bundle::new();
    // A real file beside the runtime directory, not inside it.
    std::fs::write(bundle.root.join("outside.bin"), b"outside").unwrap();
    for escaping in ["../outside.bin", "models/../../outside.bin"] {
        bundle.write_manifest(&manifest(vec![entry(escaping, b"outside")]));
        assert!(
            matches!(
                bundle.verify().unwrap_err(),
                RuntimeError::BundlePath { ref path, reason }
                    if path.as_str() == escaping && reason.contains("..")
            ),
            "{escaping} must be rejected"
        );
    }

    let absolute = bundle.root.join("outside.bin");
    bundle.write_manifest(&manifest(vec![entry(
        &absolute.to_string_lossy(),
        b"outside",
    )]));
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundlePath { .. }
    ));
}

#[test]
fn malformed_listed_paths_are_rejected() {
    let bundle = Bundle::new();
    bundle.write("models/face.onnx", MODEL_BYTES);
    for raw in [
        "",
        "./models/face.onnx",
        "models//face.onnx",
        "models/",
        "models\\face.onnx",
        "manifest.json",
    ] {
        bundle.write_manifest(&manifest(vec![entry(raw, MODEL_BYTES)]));
        assert!(
            matches!(
                bundle.verify().unwrap_err(),
                RuntimeError::BundlePath { .. }
            ),
            "{raw:?} must be rejected"
        );
    }
}

#[test]
fn duplicate_listed_paths_are_rejected() {
    let bundle = Bundle::new();
    bundle.write("model.onnx", MODEL_BYTES);
    bundle.write_manifest(&manifest(vec![
        entry("model.onnx", MODEL_BYTES),
        entry("model.onnx", MODEL_BYTES),
    ]));
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));
}

#[test]
fn bad_schema_is_rejected() {
    let bundle = spec_layout();
    let mut value = manifest(vec![entry(runtime_filename(), LIBRARY_BYTES)]);
    value["schema"] = json!(2);
    bundle.write_manifest(&value);
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { ref message, .. } if message.contains("schema 2")
    ));

    // A string is not the integer schema 1.
    value["schema"] = json!("1");
    bundle.write_manifest(&value);
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));

    // Unknown top-level fields fail the schema too.
    let mut value = manifest(vec![]);
    value["notes"] = json!("extra");
    bundle.write_manifest(&value);
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));
}

#[test]
fn malformed_manifest_fields_are_rejected() {
    let bundle = Bundle::new();
    bundle.write("model.onnx", MODEL_BYTES);

    let mut bad_platform = manifest(vec![entry("model.onnx", MODEL_BYTES)]);
    bad_platform["platform"] = json!("Windows_X64");
    bundle.write_manifest(&bad_platform);
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));

    let mut bad_digest = manifest(vec![entry("model.onnx", MODEL_BYTES)]);
    bad_digest["files"][0]["sha256"] = json!("not-a-digest");
    bundle.write_manifest(&bad_digest);
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));

    let mut empty_version = manifest(vec![entry("model.onnx", MODEL_BYTES)]);
    empty_version["onnxruntime"]["version"] = json!(" ");
    bundle.write_manifest(&empty_version);
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));

    let mut missing_platform = manifest(vec![]);
    missing_platform.as_object_mut().unwrap().remove("platform");
    bundle.write_manifest(&missing_platform);
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));
}

#[test]
fn missing_or_unreadable_manifest_is_rejected() {
    let bundle = Bundle::new();
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));
    std::fs::write(bundle.dir.join("manifest.json"), b"{not json").unwrap();
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));
}

#[test]
fn relative_bundle_directory_is_refused() {
    assert!(matches!(
        verify_runtime_manifest(Path::new("runtime")).unwrap_err(),
        RuntimeError::UnsafePath { .. }
    ));
}

#[cfg(unix)]
#[test]
fn symbolic_links_inside_the_bundle_are_rejected() {
    let bundle = spec_layout();
    let outside = bundle.root.join("outside.bin");
    std::fs::write(&outside, b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, bundle.dir.join("link.bin")).unwrap();
    assert!(matches!(
        bundle.verify().unwrap_err(),
        RuntimeError::BundleEntryUnsupported { path } if path.ends_with("link.bin")
    ));
}

// ---- verify_library_in_manifest: one library against a tolerant app manifest ----

use rightkit_onnx::verify_library_in_manifest;

fn app_manifest_with_noise() -> Bundle {
    let bundle = Bundle::new();
    bundle.write(runtime_filename(), LIBRARY_BYTES);
    // Unlisted file, and listed entries that are absent, malformed or not objects.
    bundle.write("extra/unrelated.bin", b"unrelated");
    bundle.write_manifest(&json!({
        "schema": 7,
        "appField": { "anything": true },
        "files": [
            entry(runtime_filename(), LIBRARY_BYTES),
            { "path": "missing/model.onnx", "sha256": "zz", "bytes": 1 },
            { "path": "../bad", "extra": 1 },
            42
        ]
    }));
    bundle
}

fn lib_path(bundle: &Bundle) -> PathBuf {
    bundle.dir.join(runtime_filename())
}

#[test]
fn library_verifies_despite_unrelated_manifest_and_directory_noise() {
    let bundle = app_manifest_with_noise();
    let verified = verify_library_in_manifest(&lib_path(&bundle)).expect("verifies");
    assert_eq!(verified.path, runtime_filename());
    assert_eq!(verified.sha256, sha256_hex(LIBRARY_BYTES));
    assert_eq!(verified.bytes, LIBRARY_BYTES.len() as u64);
    // The strict whole-bundle check still refuses the same directory.
    assert!(bundle.verify().is_err());
}

#[test]
fn library_digest_or_size_mismatch_is_never_tolerated() {
    let bundle = app_manifest_with_noise();
    bundle.write(runtime_filename(), b"tampered-library-bytes");
    assert!(matches!(
        verify_library_in_manifest(&lib_path(&bundle)).unwrap_err(),
        RuntimeError::BundleFileSize { .. }
    ));
    let same_len = vec![b'x'; LIBRARY_BYTES.len()];
    bundle.write(runtime_filename(), &same_len);
    assert!(matches!(
        verify_library_in_manifest(&lib_path(&bundle)).unwrap_err(),
        RuntimeError::BundleFileHash { .. }
    ));
}

#[test]
fn library_not_listed_duplicated_or_malformed_is_rejected() {
    let bundle = Bundle::new();
    bundle.write(runtime_filename(), LIBRARY_BYTES);
    let lib = lib_path(&bundle);

    bundle.write_manifest(&json!({ "files": [entry("other.dll", b"o")] }));
    assert!(matches!(
        verify_library_in_manifest(&lib).unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));

    bundle.write_manifest(&json!({ "files": [
        entry(runtime_filename(), LIBRARY_BYTES),
        entry(runtime_filename(), LIBRARY_BYTES)
    ]}));
    assert!(matches!(
        verify_library_in_manifest(&lib).unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));

    bundle.write_manifest(&json!({ "files": [
        { "path": runtime_filename(), "sha256": "abc", "bytes": LIBRARY_BYTES.len() }
    ]}));
    assert!(matches!(
        verify_library_in_manifest(&lib).unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));

    // No `files` array: fail closed.
    bundle.write_manifest(&json!({ "schema": 1 }));
    assert!(matches!(
        verify_library_in_manifest(&lib).unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));

    // No manifest at all: fail closed.
    std::fs::remove_file(bundle.dir.join("manifest.json")).unwrap();
    assert!(matches!(
        verify_library_in_manifest(&lib).unwrap_err(),
        RuntimeError::BundleManifest { .. }
    ));
}

#[test]
fn library_missing_on_disk_and_relative_path_are_rejected() {
    let bundle = Bundle::new();
    bundle.write_manifest(&json!({ "files": [entry(runtime_filename(), LIBRARY_BYTES)] }));
    assert!(matches!(
        verify_library_in_manifest(&lib_path(&bundle)).unwrap_err(),
        RuntimeError::BundleFileMissing { .. }
    ));
    assert!(matches!(
        verify_library_in_manifest(Path::new(runtime_filename())).unwrap_err(),
        RuntimeError::UnsafePath { .. }
    ));
}
