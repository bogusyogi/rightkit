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

#[test]
fn legacy_default_candidate_is_opt_in_and_workspace_relative() {
    use rightkit_ort::{legacy_runtime_candidate, runtime_filename};

    let root = std::env::temp_dir().join(format!("rightkit-ort-legacy-{}", std::process::id()));
    std::fs::create_dir_all(root.join("tools/bin")).unwrap();
    let runtime = root.join("tools/bin").join(runtime_filename());
    std::fs::write(&runtime, b"runtime-fixture").unwrap();
    let candidate = legacy_runtime_candidate(&root).unwrap();
    assert_eq!(candidate.source, CandidateSource::LegacyDefault);
    // Compare resolved identities, including macOS /var vs /private/var aliases.
    assert_eq!(
        candidate.path.canonicalize().unwrap(),
        runtime.canonicalize().unwrap()
    );
    let selection = resolve_runtime([candidate]).unwrap();
    assert_eq!(selection.path, runtime.canonicalize().unwrap());
    assert_eq!(selection.source, CandidateSource::LegacyDefault);
    assert!(matches!(
        resolve_runtime([]),
        Err(RuntimeError::NoCandidates)
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_default_candidate_rejects_relative_workspace_roots() {
    assert!(matches!(
        rightkit_ort::legacy_runtime_candidate(std::path::Path::new("workspace")),
        Err(RuntimeError::UnsafePath { .. })
    ));
}
