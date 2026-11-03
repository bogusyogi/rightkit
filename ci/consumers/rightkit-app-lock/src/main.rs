// rightkit-app-lock 0.1.0 consumed by exact registry pin (`=0.1.0`), outside the workspace.
//
// Proves, through the public API only, the cross-platform state and preference rules:
// - enabled_from_secret is true only for the on marker "1";
// - lock_preference_from_read fails closed: an unreadable item is Unreadable and counts as enabled;
// - AppLockRuntime forbids DB access whenever the lock is enabled and not unlocked, and status()
//   reports the state it was built with.
// The OS verification (verify_user, native_available) is not called: it shows a Hello or PIN
// prompt on Windows and a LocalAuthentication sheet on macOS. No I/O, no network, no prompt.
use rightkit_app_lock::{
    enabled_from_secret, lock_preference_from_read, AppLockRuntime, AppLockState, LockPreference,
};

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

fn main() {
    check(
        enabled_from_secret(Some("1".to_string())),
        "marker 1 means enabled",
    );
    check(
        !enabled_from_secret(Some("0".to_string())),
        "marker 0 means disabled",
    );
    check(!enabled_from_secret(None), "an absent item means disabled");

    check(
        matches!(
            lock_preference_from_read(Ok(Some("1".to_string()))),
            LockPreference::Enabled
        ),
        "a read of 1 is Enabled",
    );
    let unreadable = lock_preference_from_read(Err("denied".to_string()));
    check(
        matches!(&unreadable, LockPreference::Unreadable(why) if why == "denied")
            && unreadable.is_enabled(),
        "an unreadable preference fails closed as enabled",
    );

    check(
        AppLockRuntime::locked(None).blocks_db(),
        "a locked app blocks the database",
    );
    check(
        !AppLockRuntime::unlocked().blocks_db(),
        "an unlocked app does not block the database",
    );
    check(
        !AppLockRuntime::disabled().blocks_db(),
        "a disabled lock blocks nothing",
    );
    check(
        AppLockRuntime::unavailable(true, "no Hello".to_string()).blocks_db(),
        "an enabled lock with Hello missing blocks the database",
    );
    check(
        AppLockRuntime::locked(Some("x".to_string())).status().state == AppLockState::Locked,
        "status reports the locked state",
    );
    println!("OK rightkit-app-lock 0.1.0 (registry consumer)");
}
