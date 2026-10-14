//! End-to-end on Windows with the catalog-pinned `onnxruntime.dll` staged on its own, with no
//! `DirectML.dll` beside it: the file matches the catalog digest, the environment initialises
//! from the installed layout, and a tiny ONNX model runs on the CPU provider.
//!
//! This proves the pinned bytes load and compute. It does not prove the DirectML path: that is
//! `e2e_runtime_windows.rs`, which needs the pinned `DirectML.dll` as well.
#![cfg(all(target_os = "windows", feature = "session"))]

mod common;

use rightkit_onnx::ort::value::Tensor;
use rightkit_onnx::{
    configure_runtime, init_environment, installed_runtime_candidate, probe_providers,
    runtime_filename, CandidateSource, EnvironmentOptions, ExecutionProvider, GlobalPool,
    ProviderKind, SessionOptions,
};

#[test]
fn pinned_runtime_alone_matches_the_catalog_and_runs_on_cpu() {
    let Some(source) = common::runtime_file_or_skip() else {
        return;
    };
    let record = common::windows_runtime_record();
    assert_eq!(record.version, "1.24.4");

    // `<app>\example.exe` with `<app>\runtime\onnxruntime.dll` and nothing else.
    let app = std::env::temp_dir().join(format!("rightkit-ort-e2e-cpu-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&app);
    let runtime_dir = app.join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let staged = runtime_dir.join(runtime_filename());
    std::fs::copy(&source, &staged).unwrap();
    record
        .verify_file(&staged)
        .expect("staged runtime must match the catalog digest");

    let candidate = installed_runtime_candidate(&app.join("example.exe"), "runtime").unwrap();
    let selection = configure_runtime([candidate]).unwrap();
    assert_eq!(selection.source, CandidateSource::Bundled);
    let report = init_environment(
        &selection,
        &EnvironmentOptions {
            global_pool: Some(GlobalPool::from_budget()),
        },
    )
    .expect("environment");
    assert_eq!(report.runtime_path, staged.canonicalize().unwrap());
    assert!(
        report.runtime_info.starts_with("ORT Build Info:"),
        "{}",
        report.runtime_info
    );

    let probes = probe_providers();
    let probe = |kind| probes.iter().find(|p| p.provider == kind).unwrap();
    assert!(probe(ProviderKind::Cpu).available);
    // Recorded, not asserted: with no DirectML.dll beside the runtime, Windows may still supply
    // its own older copy.
    eprintln!(
        "EVIDENCE directml probe with no DirectML.dll beside the runtime: {:?}",
        probe(ProviderKind::DirectMl)
    );

    let mut session = SessionOptions::new(ExecutionProvider::Cpu)
        .build_from_memory(&common::add_model())
        .expect("cpu session");
    let x = Tensor::from_array(([4usize], vec![1.0f32, 2.0, 3.0, 4.0])).unwrap();
    let y = Tensor::from_array(([4usize], vec![10.0f32, 20.0, 30.0, 40.0])).unwrap();
    let outputs = session
        .run(rightkit_onnx::ort::inputs!["x" => x, "y" => y])
        .unwrap();
    let (shape, data) = outputs["z"].try_extract_tensor::<f32>().unwrap();
    assert_eq!(&**shape, &[4i64]);
    assert_eq!(data.to_vec(), vec![11.0, 22.0, 33.0, 44.0]);
}
