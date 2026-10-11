//! Windows: `init_environment_default_loader` refuses the `unset` case with an `Err` before any
//! load. Own binary: the first outcome is cached for the process.
#![cfg(all(target_os = "windows", feature = "session"))]

mod common;

use rightkit_ort::{init_environment_default_loader, EnvironmentInitOptions};

#[test]
fn default_loader_refuses_unset() {
    std::env::remove_var("ORT_DYLIB_PATH");
    let error = init_environment_default_loader(&EnvironmentInitOptions::default())
        .expect_err("must be refused");
    assert!(error.contains("hangs at session init"), "{error}");
    // The refusal is the cached outcome: nothing was loaded or committed.
    assert!(init_environment_default_loader(&EnvironmentInitOptions::default()).is_err());
    let _ = common::add_model;
}
