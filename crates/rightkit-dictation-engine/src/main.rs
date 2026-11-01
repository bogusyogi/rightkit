#![cfg_attr(windows, windows_subsystem = "windows")]

/// Fail closed before any filesystem access: a suite-root override that this build would silently
/// ignore (rightkit-daemon compiled without `test-mode`) means the real user data folder would be used.
/// A test-mode build with no override is refused too: it must never resolve a real folder.
/// Debug builds only; release builds read nothing. It guards non-test-mode debug builds too,
/// so it must stay on debug_assertions, not test-mode.
// rightkit-allow: debug-assertions-env (real-folder safety guard)
#[cfg(debug_assertions)]
fn refuse_ignored_suite_override() {
    let set = [
        rightkit_daemon::ENV_SUITE_ROOT,
        "RIGHT_SUITE_DIR",
        "ORTHIC_SHARED_DIR",
    ]
    .into_iter()
    .find(|name| std::env::var_os(name).is_some_and(|v| !v.to_string_lossy().trim().is_empty()));
    match (rightkit_daemon::mode::TEST_CAPABILITY, set) {
        (false, Some(name)) => {
            eprintln!(
                "refusing to start: {name} is set but this build has no test-mode (rightkit-daemon would ignore it and use the real user data folder); build with --features test-mode"
            );
            std::process::exit(78);
        }
        (true, None) => {
            eprintln!(
                "refusing to start: this is a test-mode build but no suite override (RIGHTKIT_SUITE_ROOT, RIGHT_SUITE_DIR or ORTHIC_SHARED_DIR) is set, so it would resolve the real user data folder"
            );
            std::process::exit(78);
        }
        _ => {}
    }
}

fn main() -> anyhow::Result<()> {
    #[cfg(debug_assertions)]
    refuse_ignored_suite_override();
    // QA build only: name the lever set once so every later log line is attributable.
    #[cfg(feature = "test-mode")]
    rightkit_dictation_engine::levers::log_startup();
    rightkit_dictation_runtime::entry::run()
}
