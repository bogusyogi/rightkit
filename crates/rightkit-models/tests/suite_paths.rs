//! The suite root contract: one root shared by every app, env-overridable and
//! read live. Own test binary because it mutates process environment.

use rightkit_models::suite::paths;

#[test]
fn suite_root_resolution_order_and_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let (suite, shared) = (tmp.path().join("suite"), tmp.path().join("orthic-shared"));
    std::env::remove_var(paths::SUITE_ROOT_ENV);
    std::env::set_var(paths::ORTHIC_SHARED_DIR_ENV, &shared);
    assert_eq!(paths::suite_root(), shared);
    assert_eq!(paths::models_root(), shared.join("models"));
    assert_eq!(paths::tools_root(), shared.join("tools"));
    // RIGHT_SUITE_DIR (dev/test) wins and is read live, never cached.
    std::env::set_var(paths::SUITE_ROOT_ENV, &suite);
    assert_eq!(paths::suite_root(), suite);
    assert_eq!(paths::legacy_suite_root(), suite);
    std::env::remove_var(paths::SUITE_ROOT_ENV);
    assert_eq!(paths::suite_root(), shared);
    // Without overrides the default is `<data>/Orthic Labs/shared`.
    std::env::remove_var(paths::ORTHIC_SHARED_DIR_ENV);
    assert!(paths::suite_root().ends_with("Orthic Labs/shared") || paths::suite_root().ends_with("Orthic Labs\\shared"));
    assert_eq!(paths::model_refs_dir_at(&paths::models_root_at(&suite)), suite.join("models/refs"));
    assert_eq!(paths::tool_dir_at(&paths::tools_root_at(&suite), "yt-dlp", "1.0"), suite.join("tools/yt-dlp/1.0"));
}
