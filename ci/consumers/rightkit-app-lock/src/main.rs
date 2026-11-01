//! Registry consumer for rightkit-app-lock 0.1.0: public API only, no OS prompt.
use rightkit_app_lock::{
    lock_preference_from_read, startup_runtime, startup_verification, AppLockState, LockPreference,
};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    check(
        lock_preference_from_read(Ok(None)) == LockPreference::Disabled,
        "an absent preference is disabled",
    );
    let unreadable = lock_preference_from_read(Err("denied".into()));
    check(unreadable.is_enabled(), "an unreadable preference fails closed");
    let result = startup_verification(&unreadable, || Ok(true));
    check(result.is_err(), "an unreadable preference never reaches the prompt");
    let status = startup_runtime(true, result).status();
    check(
        status.state == AppLockState::Unavailable,
        "start-up with an unreadable preference stays locked",
    );
}
