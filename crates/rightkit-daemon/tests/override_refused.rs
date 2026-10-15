//! A debug build without `test-mode` must refuse an override it cannot honour, never fall back to
//! the real per-user folder. Own test binary: it mutates the process environment.
#![cfg(all(not(feature = "test-mode"), debug_assertions))]

use rightkit_daemon::{Suite, SuiteError, ENV_SUITE_ROOT};

#[test]
fn override_without_test_mode_is_refused() {
    let dir = std::env::temp_dir().join("rightkit-daemon-override-refused");
    // SAFETY: this test binary has exactly one test, so nothing reads the environment concurrently.
    unsafe { std::env::set_var(ENV_SUITE_ROOT, &dir) };
    assert!(matches!(Suite::resolve(), Err(SuiteError::Insecure(_))));
    assert!(matches!(Suite::resolve_ignoring_env_overrides(false), Err(SuiteError::Insecure(_))));
    assert!(Suite::resolve_ignoring_env_overrides(true).is_ok());
    unsafe { std::env::remove_var(ENV_SUITE_ROOT) };
}
