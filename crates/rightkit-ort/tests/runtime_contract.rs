use rightkit_ort::{resolve_runtime, CandidateSource, RuntimeCandidate, RuntimeError};

#[test]
fn relative_runtime_paths_fail_closed() {
    let error = resolve_runtime([RuntimeCandidate::new(
        "onnxruntime.dll",
        CandidateSource::Explicit,
    )])
    .unwrap_err();
    assert!(matches!(error, RuntimeError::UnsafePath { .. }));
}

#[test]
fn candidates_are_checked_in_caller_order_with_diagnostics() {
    let root = std::env::temp_dir().join(format!("rightkit-ort-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let runtime = root.join(rightkit_ort::runtime_filename());
    std::fs::write(&runtime, b"runtime-fixture").unwrap();

    let selection = resolve_runtime([
        RuntimeCandidate::new(
            root.join("missing").join(rightkit_ort::runtime_filename()),
            CandidateSource::Bundled,
        ),
        RuntimeCandidate::new(&runtime, CandidateSource::Explicit),
    ])
    .unwrap();

    assert_eq!(selection.path, runtime.canonicalize().unwrap());
    assert_eq!(selection.source, CandidateSource::Explicit);
    assert_eq!(selection.diagnostics.len(), 2);
    assert!(!selection.diagnostics[0].exists);
    assert!(selection.diagnostics[1].exists);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn wrong_platform_filename_is_rejected() {
    let root = std::env::temp_dir().join(format!("rightkit-ort-wrong-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let wrong = root.join("not-onnx-runtime.bin");
    std::fs::write(&wrong, b"runtime-fixture").unwrap();
    let error =
        resolve_runtime([RuntimeCandidate::new(&wrong, CandidateSource::Explicit)]).unwrap_err();
    assert!(matches!(error, RuntimeError::UnexpectedFilename { .. }));
    std::fs::remove_dir_all(root).unwrap();
}
