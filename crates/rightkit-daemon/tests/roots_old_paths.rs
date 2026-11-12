//! The service-name rule is the daemon's; the moved root items stay reachable (deprecated) under
//! their old paths and are the same types as `rightkit-roots`'s.

#![allow(deprecated)]

#[test]
fn service_name_rule_is_owned_here() {
    assert!(rightkit_daemon::valid_service_name("speech-1"));
    assert!(!rightkit_daemon::valid_service_name("Speech"));
    assert!(!rightkit_daemon::valid_service_name(""));
    assert!(rightkit_daemon::valid_service_name(
        &"a".repeat(rightkit_daemon::MAX_SERVICE_NAME_LEN)
    ));
    assert!(!rightkit_daemon::valid_service_name(
        &"a".repeat(rightkit_daemon::MAX_SERVICE_NAME_LEN + 1)
    ));
}

#[test]
fn old_root_paths_are_the_same_items() {
    let _: Option<rightkit_roots::AppRoot> = None::<rightkit_daemon::AppRoot>;
    let _: Option<rightkit_roots::Suite> = None::<rightkit_daemon::Suite>;
    assert_eq!(
        rightkit_daemon::SUITE_DIR_NAME,
        rightkit_roots::SUITE_DIR_NAME
    );
    assert_eq!(
        rightkit_daemon::MIGRATION_MARKER,
        rightkit_roots::MIGRATION_MARKER
    );
    assert!(rightkit_daemon::valid_app_name("ViewRight"));
    let _ = rightkit_daemon::mode::AppId::new("x");
}
