//! A session built before any initialisation commits the default options (telemetry off) first,
//! and a later explicit selection is refused instead of being silently ignored.
#![cfg(all(target_os = "macos", feature = "session"))]

mod common;

use rightkit_ort::{
    explicit_file_candidate, init_environment_default_loader, init_environment_with_options,
    resolve_runtime, EnvironmentInitOptions, EnvironmentStatus, ExecutionProvider, SessionOptions,
};

#[test]
fn first_session_commits_telemetry_off_and_blocks_a_later_selection() {
    if !common::runtime_or_skip() {
        return;
    }
    let (dir, runtime) = common::staged_runtime("session-first", "libonnxruntime.1.24.4.dylib");
    std::env::set_var("ORT_DYLIB_PATH", &runtime);

    // No init call at all: the session build must commit telemetry-off options itself.
    let session = SessionOptions::new(ExecutionProvider::Cpu)
        .build_from_memory(&common::add_model())
        .expect("session without an explicit init");
    drop(session);

    // The environment now exists, so an explicit selection cannot take effect: refused.
    let selection = resolve_runtime([explicit_file_candidate(&runtime)]).unwrap();
    let error =
        init_environment_with_options(&selection, &EnvironmentInitOptions::default()).unwrap_err();
    assert!(
        error.contains("before building sessions") || error.contains("Initialise before"),
        "{error}"
    );

    // The default loader reports the session-built environment (telemetry off was applied).
    let report = init_environment_default_loader(&EnvironmentInitOptions::default()).unwrap();
    assert_eq!(report.status, EnvironmentStatus::Deferred);
    let _ = std::fs::remove_dir_all(dir);
}
