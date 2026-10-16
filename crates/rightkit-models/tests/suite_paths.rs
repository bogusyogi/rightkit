//! The suite root contract: one root shared by every app, resolved through
//! `rightkit_daemon::Suite::resolve`. Own test binary because it mutates process environment.
//!
//! Run both modes: `cargo test -p rightkit-models --test suite_paths` (no overrides read) and
//! `... --features test-mode` (overrides honoured).

use rightkit_models::suite::paths;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The tests mutate process environment, so they run one at a time, each from a clean slate.
static ENV_LOCK: Mutex<()> = Mutex::new(());
static REAL_HOME: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
/// The variable the per-user default root follows: `%LOCALAPPDATA%` on Windows, `$HOME` elsewhere.
#[cfg(windows)]
const HOME_VAR: &str = "LOCALAPPDATA";
#[cfg(not(windows))]
const HOME_VAR: &str = "HOME";
fn env_guard() -> MutexGuard<'static, ()> {
    let guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Second guard: even a path that wrongly ignored the override lands in a temp HOME, not the
    // real one. Tests that need the real HOME restore it from REAL_HOME.
    REAL_HOME.get_or_init(|| std::env::var_os(HOME_VAR));
    static TEMP_HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    let temp = TEMP_HOME.get_or_init(|| tempfile::tempdir().unwrap());
    std::env::set_var(HOME_VAR, temp.path());
    for k in [
        paths::RIGHTKIT_SUITE_ROOT_ENV,
        paths::SUITE_ROOT_ENV,
        paths::ORTHIC_SHARED_DIR_ENV,
    ] {
        std::env::remove_var(k);
    }
    guard
}

#[cfg(feature = "test-mode")]
#[test]
fn test_mode_honours_overrides_in_order_and_trims_empty() {
    let _env = env_guard();
    let tmp = tempfile::tempdir().unwrap();
    let (primary, legacy, shared) = (
        tmp.path().join("primary"),
        tmp.path().join("suite"),
        tmp.path().join("orthic-shared"),
    );
    let clear = || {
        for k in [
            paths::RIGHTKIT_SUITE_ROOT_ENV,
            paths::SUITE_ROOT_ENV,
            paths::ORTHIC_SHARED_DIR_ENV,
        ] {
            std::env::remove_var(k);
        }
    };
    clear();
    std::env::set_var(paths::ORTHIC_SHARED_DIR_ENV, &shared);
    assert_eq!(paths::suite_root(), shared);
    assert_eq!(paths::models_root(), shared.join("models"));
    assert_eq!(paths::tools_root(), shared.join("tools"));
    assert_eq!(paths::legacy_suite_root(), shared);
    std::env::set_var(paths::SUITE_ROOT_ENV, &legacy);
    assert_eq!(paths::suite_root(), legacy);
    // The variable apps' test builds set wins, and is read live.
    std::env::set_var(paths::RIGHTKIT_SUITE_ROOT_ENV, &primary);
    assert_eq!(paths::suite_root(), primary);
    assert_eq!(paths::models_root(), primary.join("models"));
    assert_eq!(paths::legacy_suite_root(), primary);
    // Trimmed-empty counts as unset: falls through to the next variable.
    std::env::set_var(paths::RIGHTKIT_SUITE_ROOT_ENV, "  \t ");
    assert_eq!(paths::suite_root(), legacy);
    std::env::set_var(paths::SUITE_ROOT_ENV, "");
    assert_eq!(paths::suite_root(), shared);
    clear();
    assert_eq!(
        paths::model_refs_dir_at(&paths::models_root_at(&legacy)),
        legacy.join("models/refs")
    );
    assert_eq!(
        paths::tool_dir_at(&paths::tools_root_at(&legacy), "yt-dlp", "1.0"),
        legacy.join("tools/yt-dlp/1.0")
    );
}

/// No override and the real HOME: a test-mode build refuses (reads nothing, writes nothing).
/// Without the variable at all (a test runner that passes no environment) it refuses as well.
#[cfg(feature = "test-mode")]
#[test]
fn test_mode_without_override_and_real_home_fails_closed() {
    let _env = env_guard();
    match REAL_HOME.get() {
        Some(Some(home)) => std::env::set_var(HOME_VAR, home),
        _ => std::env::remove_var(HOME_VAR),
    }
    let err = std::panic::catch_unwind(paths::suite_root)
        .expect_err("a test-mode build must refuse the real per-user suite root");
    let msg = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(
        msg.contains("refusing the real per-user suite root")
            || msg.contains("cannot resolve the suite root"),
        "unexpected refusal: {msg}"
    );
}

/// With HOME redirected and no override, the root is under that HOME, never the real folder.
#[cfg(feature = "test-mode")]
#[test]
fn test_mode_without_override_follows_the_redirected_home() {
    let _env = env_guard();
    let home = std::path::PathBuf::from(std::env::var_os(HOME_VAR).unwrap());
    assert!(paths::suite_root().starts_with(&home));
    assert!(paths::app_data_base().unwrap().starts_with(&home));
}

#[cfg(feature = "test-mode")]
#[test]
#[should_panic(expected = "cannot resolve the suite root")]
fn test_mode_relative_override_fails_closed() {
    let _env = env_guard();
    std::env::set_var(paths::RIGHTKIT_SUITE_ROOT_ENV, "relative/dir");
    let _ = paths::suite_root();
}

#[cfg(not(feature = "test-mode"))]
#[test]
fn release_build_reads_no_override_variable() {
    let _env = env_guard();
    let tmp = tempfile::tempdir().unwrap();
    let default = rightkit_daemon::Suite::per_user_default().unwrap();
    for k in [
        paths::RIGHTKIT_SUITE_ROOT_ENV,
        paths::SUITE_ROOT_ENV,
        paths::ORTHIC_SHARED_DIR_ENV,
    ] {
        std::env::set_var(k, tmp.path().join(k));
    }
    // HOME is redirected too: the account database, not the environment, names the root.
    std::env::set_var("HOME", tmp.path().join("home"));
    assert_eq!(paths::suite_root(), default.root());
    assert_eq!(paths::models_root(), default.root().join("models"));
    assert_eq!(paths::tools_root(), default.root().join("tools"));
    assert!(!paths::legacy_suite_root().starts_with(tmp.path()));
    assert!(!paths::suite_root().starts_with(tmp.path()));
    assert!(!rightkit_models::suite::paths::suite_root().starts_with(tmp.path()));
}

#[cfg(feature = "test-mode")]
#[test]
fn test_mode_device_id_and_siblings_stay_under_the_override() {
    let _env = env_guard();
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var(paths::RIGHTKIT_SUITE_ROOT_ENV, tmp.path().join("root"));
    let id = rightkit_models::suite::device_id_get_or_create("CutRight").unwrap();
    let file = tmp.path().join("root/app-data/CutRight/model-device-id");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), id);
    assert_eq!(
        rightkit_models::suite::device_id_get_or_create("CutRight").unwrap(),
        id
    );
    for dir in rightkit_models::suite::handle::sibling_parakeet_dirs() {
        assert!(dir.starts_with(tmp.path()), "{dir:?}");
    }
    assert!(paths::app_data_base().unwrap().starts_with(tmp.path()));
}
