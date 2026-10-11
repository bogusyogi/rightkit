//! Windows: `init_environment_default_loader` accepts an absolute `ORT_DYLIB_PATH` to the real
//! DLL (any file name), is idempotent, and then serves sessions.
#![cfg(all(target_os = "windows", feature = "session"))]

mod common;

use rightkit_ort::ort::value::Tensor;
use rightkit_ort::{
    explicit_file_candidate, init_environment_default_loader, init_environment_with_options,
    resolve_runtime, EnvironmentInitOptions, EnvironmentStatus, ExecutionProvider, SessionOptions,
};

#[test]
fn default_loader_accepts_an_absolute_runtime_path() {
    let (dir, runtime) =
        common::staged_runtime("win-default-loader", common::WINDOWS_VERSIONED_NAME);
    std::env::set_var("ORT_DYLIB_PATH", &runtime);

    let first = init_environment_default_loader(&EnvironmentInitOptions::default()).unwrap();
    assert_eq!(first.status, EnvironmentStatus::Initialized);
    assert_eq!(first.runtime_path, runtime);
    assert!(
        first.runtime_info.starts_with("ORT Build Info:"),
        "{}",
        first.runtime_info
    );
    let again = init_environment_default_loader(&EnvironmentInitOptions {
        tolerate_already_initialized: true,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(again.status, EnvironmentStatus::AlreadyInitialized);
    assert!(init_environment_default_loader(&EnvironmentInitOptions::default()).is_ok());

    let same = resolve_runtime([explicit_file_candidate(&runtime)]).unwrap();
    assert_eq!(
        init_environment_with_options(&same, &EnvironmentInitOptions::default())
            .unwrap()
            .runtime_info,
        first.runtime_info
    );
    let other = dir.join("onnxruntime-other.dll");
    std::fs::copy(&runtime, &other).unwrap();
    let other = resolve_runtime([explicit_file_candidate(&other)]).unwrap();
    let error =
        init_environment_with_options(&other, &EnvironmentInitOptions::default()).unwrap_err();
    assert!(error.contains("cannot be replaced"), "{error}");

    let mut session = SessionOptions::new(ExecutionProvider::Cpu)
        .build_from_memory(&common::add_model())
        .expect("session after default-loader init");
    let x = Tensor::from_array(([4usize], vec![1.0f32, 2.0, 3.0, 4.0])).unwrap();
    let y = Tensor::from_array(([4usize], vec![1.0f32, 1.0, 1.0, 1.0])).unwrap();
    let out = session
        .run(rightkit_ort::ort::inputs!["x" => x, "y" => y])
        .unwrap();
    assert_eq!(
        out["z"].try_extract_tensor::<f32>().unwrap().1,
        &[2.0f32, 3.0, 4.0, 5.0]
    );
    let _ = std::fs::remove_dir_all(dir);
}
