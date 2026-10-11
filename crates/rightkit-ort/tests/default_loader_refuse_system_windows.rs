//! Windows: `init_environment_default_loader` refuses the `system` case with an `Err` before any
//! load. Own binary: the first outcome is cached for the process.
#![cfg(all(target_os = "windows", feature = "session"))]

mod common;

use rightkit_ort::{init_environment_default_loader, EnvironmentInitOptions};

#[test]
fn default_loader_refuses_system() {
    std::env::set_var("ORT_DYLIB_PATH", r"C:\Windows\SysWOW64\onnxruntime.dll");
    let error = init_environment_default_loader(&EnvironmentInitOptions::default())
        .expect_err("must be refused");
    assert!(error.contains("OS-provided"), "{error}");
    // The refusal is the cached outcome: nothing was loaded or committed.
    assert!(init_environment_default_loader(&EnvironmentInitOptions::default()).is_err());
    let _ = common::add_model;
}
