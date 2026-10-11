//! A versioned absolute runtime file is bound by `explicit_file_candidate`, loaded by
//! `init_environment_with_options`, verified by ort's load check, and serves a session.
#![cfg(all(target_os = "macos", feature = "session"))]

mod common;

use rightkit_ort::ort::value::Tensor;
use rightkit_ort::{
    explicit_file_candidate, init_environment_with_options, resolve_runtime, CandidateSource,
    EnvironmentInitOptions, EnvironmentStatus, ExecutionProvider, RuntimeCandidate, RuntimeError,
    SessionOptions,
};

#[test]
fn versioned_runtime_binds_loads_and_runs() {
    let (dir, versioned) = common::staged_runtime("versioned", "libonnxruntime.1.24.4.dylib");
    assert!(matches!(
        resolve_runtime([RuntimeCandidate::new(&versioned, CandidateSource::Explicit)]),
        Err(RuntimeError::UnexpectedFilename { .. })
    ));
    assert!(matches!(
        resolve_runtime([explicit_file_candidate("libonnxruntime.1.24.4.dylib")]),
        Err(RuntimeError::UnsafePath { .. })
    ));
    let selection = resolve_runtime([explicit_file_candidate(&versioned)]).unwrap();
    assert_eq!(selection.path, versioned.canonicalize().unwrap());
    assert_eq!(selection.source, CandidateSource::ExplicitFile);

    let report =
        init_environment_with_options(&selection, &EnvironmentInitOptions::default()).unwrap();
    assert_eq!(report.status, EnvironmentStatus::Initialized);
    assert_eq!(report.runtime_path, selection.path);
    assert!(
        report.runtime_info.starts_with("ORT Build Info:"),
        "{}",
        report.runtime_info
    );

    let mut session = SessionOptions::new(ExecutionProvider::Cpu)
        .build_from_memory(&common::add_model())
        .expect("session");
    let x = Tensor::from_array(([4usize], vec![1.0f32, 2.0, 3.0, 4.0])).unwrap();
    let y = Tensor::from_array(([4usize], vec![10.0f32, 20.0, 30.0, 40.0])).unwrap();
    let out = session
        .run(rightkit_ort::ort::inputs!["x" => x, "y" => y])
        .unwrap();
    assert_eq!(
        out["z"].try_extract_tensor::<f32>().unwrap().1,
        &[11.0f32, 22.0, 33.0, 44.0]
    );
    let _ = std::fs::remove_dir_all(dir);
}
