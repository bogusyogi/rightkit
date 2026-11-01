//! A test binary must never resolve the real per-user data folder.

#[test]
fn data_root_stays_inside_the_temp_root_under_test() {
    let guard = rightkit_dictation_text_pipeline::settings::test_binary_data_root()
        .expect("test binaries run from <target>/<profile>/deps");
    assert!(
        guard.starts_with(std::env::temp_dir()),
        "{}",
        guard.display()
    );
    let root = rightkit_dictation_text_pipeline::settings::app_data_root();
    // Another test in this binary may have set RIGHTKIT_APP_DATA_DIR; that explicit root wins.
    let explicit = std::env::var_os("RIGHTKIT_APP_DATA_DIR").filter(|path| !path.is_empty());
    match explicit {
        Some(path) => assert_eq!(root, std::path::PathBuf::from(path)),
        None => assert_eq!(root, guard),
    }
    for var in ["LOCALAPPDATA", "XDG_DATA_HOME"] {
        if let Some(real) = std::env::var_os(var) {
            let real = std::path::PathBuf::from(real)
                .join(rightkit_dictation_core::app_paths::TEST_APP_DATA_DIR_NAME);
            assert!(
                !root.starts_with(&real),
                "resolved the real folder: {}",
                root.display()
            );
        }
    }
}
