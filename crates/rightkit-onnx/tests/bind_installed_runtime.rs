//! `bind_installed_runtime` against the real process environment. One test function, so the
//! environment is only touched in a fixed order: every refusal first, then the success.

use std::path::PathBuf;

use rightkit_onnx::{
    bind_installed_runtime, configured_runtime, installed_runtime_candidate, runtime_filename,
    BindError, BindSpec, BindVerification, RuntimeError, VerificationFailure,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write_manifest(dir: &std::path::Path, digest: String, bytes: usize) {
    let manifest = json!({
        "files": [{ "path": runtime_filename(), "sha256": digest, "bytes": bytes }]
    });
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn bind_installed_runtime_binds_only_a_verified_bundled_runtime() {
    std::env::remove_var("ORT_DYLIB_PATH");
    let root = std::env::temp_dir().join(format!("rightkit-onnx-bind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let executable = root.join("App").join("app");
    let bundled = installed_runtime_candidate(&executable, "runtime").unwrap();
    let dir = bundled.path.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();
    let bytes: &[u8] = b"onnxruntime-library-bytes";
    std::fs::write(&bundled.path, bytes).unwrap();
    let spec = BindSpec {
        executable: Some(executable),
        prepend_path: true,
        ..BindSpec::new(BindVerification::Manifest)
    };

    // No runtime anywhere: the typed error, nothing bound.
    let empty = BindSpec {
        executable: Some(root.join("Nowhere").join("app")),
        ..BindSpec::new(BindVerification::Manifest)
    };
    assert!(matches!(
        bind_installed_runtime(&empty),
        Err(BindError::NoRuntime { .. })
    ));
    assert!(std::env::var_os("ORT_DYLIB_PATH").is_none());

    // The catalog has no row for this fixture on any host: refused, environment untouched.
    #[cfg(feature = "catalog")]
    {
        let catalog = BindSpec {
            verification: BindVerification::Catalog,
            ..spec.clone()
        };
        assert!(matches!(
            bind_installed_runtime(&catalog),
            Err(BindError::VerificationFailed {
                reason: VerificationFailure::Catalog(_),
                ..
            })
        ));
        assert!(std::env::var_os("ORT_DYLIB_PATH").is_none());
    }

    // No manifest, then a wrong digest: refused, environment untouched.
    assert!(matches!(
        bind_installed_runtime(&spec),
        Err(BindError::VerificationFailed {
            reason: VerificationFailure::Manifest(RuntimeError::BundleManifest { .. }),
            ..
        })
    ));
    write_manifest(&dir, sha256_hex(b"something else"), bytes.len());
    assert!(matches!(
        bind_installed_runtime(&spec),
        Err(BindError::VerificationFailed {
            reason: VerificationFailure::Manifest(RuntimeError::BundleFileHash { .. }),
            ..
        })
    ));
    assert!(std::env::var_os("ORT_DYLIB_PATH").is_none());
    assert_eq!(configured_runtime(), None);

    // Matching digest: bound, recorded and its directory is first on PATH.
    write_manifest(&dir, sha256_hex(bytes), bytes.len());
    let result = bind_installed_runtime(&spec);
    if rightkit_onnx::kleidiai::Host::detect().affected() {
        // Hash verification does not establish an ORT version: arbitrary fixture bytes
        // must still fail closed on SME2, without publishing a runtime binding.
        assert!(matches!(
            result,
            Err(BindError::Rejected(RuntimeError::KnownBadRuntime { .. }))
        ));
        assert!(std::env::var_os("ORT_DYLIB_PATH").is_none());
    } else {
        let bound = result.expect("verified runtime binds on an unaffected host");
        let path = bundled.path.canonicalize().unwrap();
        assert_eq!(bound.report.path, path);
        assert_eq!(
            PathBuf::from(std::env::var_os("ORT_DYLIB_PATH").unwrap()),
            path
        );
        assert_eq!(configured_runtime(), Some(path.clone()));
        let first_on_path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .next()
            .unwrap();
        assert_eq!(first_on_path, path.parent().unwrap());
    }
    let _ = std::fs::remove_dir_all(&root);
}
