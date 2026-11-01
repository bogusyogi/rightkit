use super::*;

/// The resolved production path must equal the literal the three former copies produced.
#[test]
fn production_root_equals_the_former_literal() {
    let expected = {
        // rightkit-fs 0.4.7 takes the AppData folder from the account token, never
        // %LOCALAPPDATA% (which a redirected profile or launcher can change), so the expectation
        // comes from the account profile too.
        #[cfg(target_os = "windows")]
        {
            rightkit_fs::app_dirs::account_home()
                .expect("account home")
                .join("AppData")
                .join("Local")
                .join(TEST_APP_DATA_DIR_NAME)
        }
        // rightkit-fs 0.4.7 resolves from the account database (passwd home), never $HOME or
        // $XDG_DATA_HOME, so the expectation does too.
        #[cfg(target_os = "macos")]
        {
            rightkit_fs::app_dirs::account_home()
                .expect("account home")
                .join("Library")
                .join("Application Support")
                .join(TEST_APP_DATA_DIR_NAME)
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            rightkit_fs::app_dirs::account_home()
                .expect("account home")
                .join(".local")
                .join("share")
                .join(TEST_APP_DATA_DIR_NAME)
        }
    };
    assert_eq!(production_data_root(), expected);
}

#[test]
fn windows_resolves_from_local_app_data_alone() {
    let local = "C:/Users/a/AppData/Local";
    let env = |key: &str| (key == "LOCALAPPDATA").then(|| OsString::from(local));
    assert_eq!(
        local_root_from(Os::Windows, &env).unwrap(),
        PathBuf::from(local).join(TEST_APP_DATA_DIR_NAME)
    );
}

#[test]
fn no_base_folder_is_an_error_not_the_current_directory() {
    assert!(local_root_from(Os::Windows, &|_| None).is_err());
    assert!(local_root_from(Os::MacOs, &|_| None).is_err());
}

#[test]
fn production_root_is_resolved_once_per_process() {
    let first = production_data_root();
    let started = std::time::Instant::now();
    for _ in 0..1000 {
        assert_eq!(production_data_root(), first);
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "1000 lookups took {elapsed:?}; the OS resolver is running per call"
    );
}

#[test]
fn production_root_ignores_dev_namespace_and_qa_variables() {
    let root = production_data_root();
    assert!(root.ends_with(TEST_APP_DATA_DIR_NAME), "{}", root.display());
}
