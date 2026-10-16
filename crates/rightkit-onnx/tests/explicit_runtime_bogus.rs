//! A regular file with a versioned name passes path validation but fails ort's load check with
//! an `Err` (no panic). Own binary: the environment outcome is process-global and cached.
#![cfg(all(target_os = "macos", feature = "session"))]

mod common;

use rightkit_onnx::{
    explicit_file_candidate, init_environment_with_options, resolve_runtime, EnvironmentInitOptions,
};

#[test]
fn bogus_versioned_file_fails_the_ort_load_check() {
    let dir = std::env::temp_dir().join(format!("rightkit-ort-bogus-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bogus = dir.join("libbogus.9.dylib");
    std::fs::write(&bogus, b"not a mach-o library").unwrap();
    let selection = resolve_runtime([explicit_file_candidate(&bogus)]).unwrap();
    let error = init_environment_with_options(&selection, &EnvironmentInitOptions::default())
        .expect_err("a non-library must fail ort's load check");
    assert!(error.starts_with("load "), "{error}");
    assert!(error.contains("libbogus.9.dylib"), "{error}");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = common::add_model;
}
