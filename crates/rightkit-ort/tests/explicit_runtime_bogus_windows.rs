//! Windows: a regular file with a versioned DLL name passes path validation but fails ort's load
//! check with an `Err`, not a panic. Own binary: the environment outcome is process-global.
#![cfg(all(target_os = "windows", feature = "session"))]

mod common;

use rightkit_ort::{
    explicit_file_candidate, init_environment_with_options, resolve_runtime, EnvironmentInitOptions,
};

#[test]
fn bogus_versioned_dll_fails_the_ort_load_check() {
    let dir = std::env::temp_dir().join(format!("rightkit-ort-win-bogus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bogus = dir.join("onnxruntime-9.dll");
    std::fs::write(&bogus, b"not a PE image").unwrap();
    let selection = resolve_runtime([explicit_file_candidate(&bogus)]).unwrap();
    let error = init_environment_with_options(&selection, &EnvironmentInitOptions::default())
        .expect_err("a non-DLL must fail ort's load check");
    assert!(error.starts_with("load "), "{error}");
    assert!(error.contains("onnxruntime-9.dll"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = common::add_model;
}
