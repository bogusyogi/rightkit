use std::error::Error;
use std::fs;
use std::path::Path;

use rightkit_onnx::kleidiai::{assess, Host, OrtVersion, Verdict, MIN_SAFE_ORT_VERSION};
use rightkit_onnx::{
    resolve_runtime, runtime_filename, verify_runtime_manifest, CandidateSource, ExecutionProvider,
    RuntimeBindingOptions, RuntimeCandidate, RuntimeError, SessionOptions,
};
use serde_json::json;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn resolution(root: &Path) -> Result<()> {
    let first = root.join("first").join(runtime_filename());
    let second = root.join("second").join(runtime_filename());
    let missing = root.join("missing").join(runtime_filename());
    fs::create_dir_all(first.parent().unwrap())?;
    fs::create_dir_all(second.parent().unwrap())?;
    fs::write(&first, b"resolution-only fixture")?;
    fs::write(&second, b"resolution-only fixture")?;

    let selected = resolve_runtime([
        RuntimeCandidate::new(&missing, CandidateSource::Bundled),
        RuntimeCandidate::new(&first, CandidateSource::Explicit),
        RuntimeCandidate::new(&second, CandidateSource::AppData),
    ])?;
    assert_eq!(selected.path, first.canonicalize()?);
    assert_eq!(selected.source, CandidateSource::Explicit);
    assert_eq!(selected.diagnostics.len(), 2);
    assert!(!selected.diagnostics[0].exists);
    assert!(selected.diagnostics[1].is_file);

    let reversed = resolve_runtime([
        RuntimeCandidate::new(&second, CandidateSource::AppData),
        RuntimeCandidate::new(&first, CandidateSource::Explicit),
    ])?;
    assert_eq!(reversed.path, second.canonicalize()?);
    assert_eq!(reversed.source, CandidateSource::AppData);
    assert_eq!(reversed.diagnostics.len(), 1);
    let relative = Path::new("relative").join(runtime_filename());
    assert!(matches!(
        resolve_runtime([RuntimeCandidate::new(&relative, CandidateSource::Explicit)]),
        Err(RuntimeError::UnsafePath { path }) if path == relative
    ));
    Ok(())
}

fn integrity(root: &Path) -> Result<()> {
    let bundle = root.join("bundle");
    fs::create_dir(&bundle)?;
    let name = runtime_filename();
    let library = bundle.join(name);
    // SHA-256 test vector for exactly b"abc"; not a native library.
    let digest = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    fs::write(&library, b"abc")?;
    let manifest = json!({
        "schema": 1,
        "platform": "qualification-fixture",
        "onnxruntime": {
            "version": "1.27.0",
            "variant": "fixture",
            "catalogRow": "pk18-fixture"
        },
        "files": [{"path": name, "sha256": digest, "bytes": 3}]
    });
    fs::write(bundle.join("manifest.json"), serde_json::to_vec(&manifest)?)?;
    let verified = verify_runtime_manifest(&bundle)?;
    assert_eq!(verified.files.len(), 1);
    assert_eq!(verified.files[0].sha256, digest);
    assert_eq!(verified.files[0].bytes, 3);
    assert_eq!(
        verified.onnxruntime_library_path(),
        Some(bundle.canonicalize()?.join(name))
    );

    // Same size isolates digest refusal from byte-count refusal.
    fs::write(&library, b"abd")?;
    assert!(matches!(
        verify_runtime_manifest(&bundle),
        Err(RuntimeError::BundleFileHash { path, expected, actual })
            if path == bundle.canonicalize()?.join(name)
                && expected == digest && actual != expected
    ));
    Ok(())
}

fn guard_policy() {
    let affected = Host {
        apple_arm64: true,
        sme2: true,
    };
    let old = OrtVersion::parse("1.26.0").unwrap();
    assert_eq!(OrtVersion::parse("unreadable"), None);
    assert_eq!(MIN_SAFE_ORT_VERSION, OrtVersion::parse("1.27.0").unwrap());
    for version in [Some(old), None] {
        assert_eq!(assess(affected, version, false), Verdict::Refuse);
        assert_eq!(assess(affected, version, true), Verdict::ForceDisable);
    }
    for allow in [false, true] {
        assert_eq!(
            assess(affected, Some(MIN_SAFE_ORT_VERSION), allow),
            Verdict::Safe
        );
        assert_eq!(
            assess(affected, OrtVersion::parse("1.30.0"), allow),
            Verdict::Safe
        );
        for host in [
            Host {
                apple_arm64: true,
                sme2: false,
            },
            Host {
                apple_arm64: false,
                sme2: true,
            },
            Host {
                apple_arm64: false,
                sme2: false,
            },
        ] {
            for version in [Some(old), Some(MIN_SAFE_ORT_VERSION), None] {
                assert_eq!(assess(host, version, allow), Verdict::NotApplicable);
            }
        }
    }
    assert!(!RuntimeBindingOptions::default().allow_known_bad_runtime);
    assert!(!SessionOptions::default().disable_kleidiai);
    let options = SessionOptions::new(ExecutionProvider::Cpu);
    assert!(!options.disable_kleidiai);
    let disabled = options.with_kleidiai(false);
    assert!(disabled.disable_kleidiai);
    assert!(!disabled.with_kleidiai(true).disable_kleidiai);
}

fn main() -> Result<()> {
    let run_root = std::path::PathBuf::from(
        std::env::var_os("RK_CONSUMER_RUN_ROOT")
            .ok_or("RK_CONSUMER_RUN_ROOT must be supplied by freshRunRoot")?,
    );
    if !run_root.is_absolute() || !run_root.is_dir() {
        return Err("RK_CONSUMER_RUN_ROOT must be an existing absolute directory".into());
    }
    let fixture = tempfile::Builder::new()
        .prefix("rightkit-onnx-pk18-")
        .tempdir_in(run_root)?;
    resolution(fixture.path())?;
    integrity(fixture.path())?;
    guard_policy();
    fixture.close()?;
    println!(
        "PK18 standalone ONNX public API, resolution, integrity & guard policy assertions passed"
    );
    Ok(())
}
