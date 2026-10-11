//! Windows: a session built before any initialisation commits telemetry-off defaults, and a later
//! selection-based init returns the documented error instead of being ignored.
#![cfg(all(target_os = "windows", feature = "session"))]

mod common;

use rightkit_ort::{
    explicit_file_candidate, init_environment_default_loader, init_environment_with_options,
    resolve_runtime, EnvironmentInitOptions, EnvironmentStatus, ExecutionProvider, SessionOptions,
};

#[test]
fn first_session_commits_telemetry_off_and_blocks_a_later_selection() {
    if !common::runtime_dir_or_skip() {
        return;
    }
    let (dir, runtime) =
        common::staged_runtime("win-session-first", common::WINDOWS_VERSIONED_NAME);
    std::env::set_var("ORT_DYLIB_PATH", &runtime);

    let session = SessionOptions::new(ExecutionProvider::Cpu)
        .build_from_memory(&common::add_model())
        .expect("session without an explicit init");
    drop(session);

    let selection = resolve_runtime([explicit_file_candidate(&runtime)]).unwrap();
    let error =
        init_environment_with_options(&selection, &EnvironmentInitOptions::default()).unwrap_err();
    assert!(error.contains("before building sessions"), "{error}");

    let report = init_environment_default_loader(&EnvironmentInitOptions::default()).unwrap();
    assert_eq!(report.status, EnvironmentStatus::Deferred);
    let _ = std::fs::remove_dir_all(dir);
}
