use crate::*;

#[test]
fn app_lock_enabled_only_accepts_explicit_one() {
    assert!(enabled_from_secret(Some("1".into())));
    assert!(!enabled_from_secret(Some("true".into())));
    assert!(!enabled_from_secret(Some("0".into())));
    assert!(!enabled_from_secret(None));
}

#[test]
fn startup_runtime_distinguishes_canceled_and_unavailable() {
    assert_eq!(
        startup_runtime(false, Err("ignored".into())).status().state,
        AppLockState::Disabled
    );
    assert_eq!(
        startup_runtime(true, Ok(false)).status().state,
        AppLockState::Locked
    );
    let unavailable = startup_runtime(true, Err("missing hardware".into())).status();
    assert_eq!(unavailable.state, AppLockState::Unavailable);
    assert!(!unavailable.available);
}

#[test]
fn an_unreadable_lock_preference_is_not_disabled() {
    assert_eq!(
        lock_preference_from_read(Ok(None)),
        LockPreference::Disabled
    );
    assert_eq!(
        lock_preference_from_read(Ok(Some("1".into()))),
        LockPreference::Enabled
    );
    assert_eq!(
        lock_preference_from_read(Err("credential read failed: denied".into())),
        LockPreference::Unreadable("credential read failed: denied".into())
    );
}

#[test]
fn startup_stays_locked_when_the_lock_item_read_is_denied_but_the_db_key_reads() {
    // The DB key read succeeding is irrelevant: the lock preference read failed, so no
    // verification runs, the result is an error, and the runtime is Unavailable (locked), never
    // Disabled or Unlocked.
    let preference =
        crate::lock_preference_from_read(Err("credential read failed: timed out".into()));
    let mut verify_called = false;
    let result = startup_verification(&preference, || {
        verify_called = true;
        Ok(true)
    });
    assert!(
        !verify_called,
        "an unreadable preference must not reach the OS verification"
    );
    assert!(result.is_err());
    let lock_enabled = !matches!(preference, LockPreference::Disabled);
    assert!(lock_enabled);
    let status = startup_runtime(lock_enabled, result).status();
    assert_eq!(status.state, AppLockState::Unavailable);
    assert_ne!(status.state, AppLockState::Unlocked);
    assert_ne!(status.state, AppLockState::Disabled);
}

#[test]
fn startup_verification_follows_the_preference() {
    assert_eq!(
        startup_verification(&LockPreference::Disabled, || Ok(false)),
        Ok(true)
    );
    assert_eq!(
        startup_verification(&LockPreference::Enabled, || Ok(false)),
        Ok(false)
    );
    assert_eq!(
        startup_verification(&LockPreference::Enabled, || Ok(true)),
        Ok(true)
    );
}

#[test]
fn preference_is_enabled_fails_closed() {
    assert!(LockPreference::Enabled.is_enabled());
    assert!(LockPreference::Unreadable("x".into()).is_enabled());
    assert!(!LockPreference::Disabled.is_enabled());
}

#[test]
fn status_round_trips_as_snake_case_json() {
    let status = AppLockRuntime::locked(None).status();
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["state"], "locked");
    assert!(AppLockRuntime::locked(None).blocks_db());
    assert!(!AppLockRuntime::unlocked().blocks_db());
}
