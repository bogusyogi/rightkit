//! Windows: `init_environment_default_loader` refuses the `relative` case with an `Err` before any
//! load. Own binary: the first outcome is cached for the process.
#![cfg(all(target_os = "windows", feature = "session"))]

mod common;

use rightkit_onnx::{init_environment_default_loader, EnvironmentInitOptions};

#[test]
fn default_loader_refuses_relative() {
    std::env::set_var("ORT_DYLIB_PATH", r"runtime\onnxruntime.dll");
    let error = init_environment_default_loader(&EnvironmentInitOptions::default())
        .expect_err("must be refused");
    assert!(error.contains("must be absolute"), "{error}");
    // The refusal is the cached outcome: nothing was loaded or committed.
    assert!(init_environment_default_loader(&EnvironmentInitOptions::default()).is_err());
    let _ = common::add_model;
}
