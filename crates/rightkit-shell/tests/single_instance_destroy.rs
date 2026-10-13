//! `SingleInstance::destroy` for the relaunch-for-update path: it releases every claim this process
//! holds plus every registered legacy release, each once, and a second call is a no-op.
//!
//! Claims and hooks are process-global, so these tests share one lock and run one at a time (a
//! `destroy` in one test would otherwise release another test's claim).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rightkit_daemon::Suite;
use rightkit_shell::{ClaimOutcome, InstanceMessage, SingleInstance};

const ID: &str = "test-shell-destroy";

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn claim(suite: &Suite) -> ClaimOutcome {
    SingleInstance::claim_in(suite, ID, InstanceMessage::default(), |_| {})
}

/// A primary whose accept loop is provably running: a second claim forwards to it. A claim released
/// before its accept thread has taken the first pipe instance keeps the pipe name until that thread
/// runs (a race in `rightkit-daemon`'s Windows listener, reported separately), which would make the
/// immediate re-claim below wait out the 60 s takeover timeout. A real relaunch holds its claim for
/// the life of the process, so the settled primary is the case that matters.
fn settled_primary(suite: &Suite) -> ClaimOutcome {
    let first = claim(suite);
    assert!(first.is_primary(), "the first claim must be the primary");
    assert!(
        matches!(claim(suite), ClaimOutcome::Forwarded),
        "the primary must be serving before it is released"
    );
    first
}

#[test]
fn destroy_releases_the_claim_so_a_relaunch_can_become_primary() {
    let _serial = serial();
    let suite = Suite::per_run_temp("shell-single-instance-destroy").unwrap();
    let first = claim(&suite);
    assert!(first.is_primary());

    // While the claim is held, a second claim does not become primary.
    let second = claim(&suite);
    assert!(!second.is_primary());

    // Relaunch-for-update: release everything this process holds, then the next claim wins.
    SingleInstance::destroy();
    assert!(claim(&suite).is_primary());
    drop(first);
    SingleInstance::destroy();
}

#[test]
fn destroy_is_idempotent_and_a_guard_dropped_afterwards_is_harmless() {
    let _serial = serial();
    let suite = Suite::per_run_temp("shell-single-instance-idem").unwrap();
    // Nothing held: a no-op, however often it is called.
    SingleInstance::destroy();
    SingleInstance::destroy();

    let guard = match settled_primary(&suite) {
        ClaimOutcome::Primary(guard) => guard,
        _ => unreachable!(),
    };
    SingleInstance::destroy();
    SingleInstance::destroy();

    // The relaunched claim takes the name; the stale guard must not release it when it drops.
    let relaunched = match claim(&suite) {
        ClaimOutcome::Primary(guard) => guard,
        _ => panic!("the claim after destroy must be the primary"),
    };
    drop(guard);
    assert!(
        !claim(&suite).is_primary(),
        "dropping a guard that destroy already released must not release the new claim"
    );
    drop(relaunched);
    SingleInstance::destroy();
}

#[test]
fn destroy_releases_every_claim_in_every_suite() {
    let _serial = serial();
    let a = Suite::per_run_temp("shell-single-instance-a").unwrap();
    let b = Suite::per_run_temp("shell-single-instance-b").unwrap();
    let ga = claim(&a);
    let gb = claim(&b);
    assert!(ga.is_primary() && gb.is_primary());
    SingleInstance::destroy();
    assert!(claim(&a).is_primary());
    assert!(claim(&b).is_primary());
    drop((ga, gb));
    SingleInstance::destroy();
}

#[test]
fn a_registered_legacy_release_runs_once_per_destroy_and_before_the_claims_go() {
    let _serial = serial();
    let suite = Suite::per_run_temp("shell-single-instance-hook").unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let guard = settled_primary(&suite);

    let counter = ran.clone();
    SingleInstance::on_destroy(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    assert_eq!(
        ran.load(Ordering::SeqCst),
        0,
        "registering must not run the release"
    );

    SingleInstance::destroy();
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    SingleInstance::destroy();
    assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "a release runs once, not on every destroy"
    );
    assert!(claim(&suite).is_primary());
    drop(guard);
    SingleInstance::destroy();
}

#[test]
fn a_panicking_legacy_release_does_not_stop_the_other_releases() {
    let _serial = serial();
    let suite = Suite::per_run_temp("shell-single-instance-panic").unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let guard = settled_primary(&suite);

    SingleInstance::on_destroy(|| panic!("legacy release failed"));
    let counter = ran.clone();
    SingleInstance::on_destroy(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    SingleInstance::destroy();
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(
        claim(&suite).is_primary(),
        "the claim is released even though a hook panicked"
    );
    drop(guard);
    SingleInstance::destroy();
}

/// Windows: the legacy plugin's claim is a named mutex `<identifier>-sim`. While it is held, a claim
/// with that legacy identifier fails closed. Closing the plugin's handle (what
/// `tauri_plugin_single_instance::destroy` does) from a registered release lets the next claim win.
#[cfg(windows)]
#[test]
fn after_destroy_with_the_plugin_release_a_claim_with_the_legacy_identifier_succeeds() {
    use rightkit_shell::{ClaimFailure, ClaimOptions, LegacySingleInstanceNames};

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *const u8, initial_owner: i32, name: *const u16) -> isize;
        fn CloseHandle(handle: isize) -> i32;
    }

    let _serial = serial();
    let identifier = format!("rk.legacy-destroy-{}", std::process::id());
    let mutex_name = LegacySingleInstanceNames::for_identifier(&identifier).windows_mutex;
    let wide: Vec<u16> = mutex_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // The plugin's mutex, created the way the plugin creates it.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, wide.as_ptr()) };
    assert_ne!(handle, 0, "could not create the stand-in legacy mutex");

    let suite = Suite::per_run_temp("shell-legacy-destroy").unwrap();
    let options = || ClaimOptions::with_legacy(&identifier);
    let held =
        SingleInstance::claim_in_with(&suite, ID, options(), InstanceMessage::default(), |_| {});
    assert!(
        matches!(held, ClaimOutcome::Failed(ClaimFailure::Legacy(_))),
        "a live legacy mutex must refuse the claim, not let it become a second primary"
    );

    // The plugin's destroy, as a registered release: close its handle.
    let raw = handle;
    SingleInstance::on_destroy(move || {
        unsafe { CloseHandle(raw) };
    });
    SingleInstance::destroy();

    let after =
        SingleInstance::claim_in_with(&suite, ID, options(), InstanceMessage::default(), |_| {});
    assert!(
        after.is_primary(),
        "after destroy the legacy identifier must no longer block the claim"
    );
    drop(after);
    SingleInstance::destroy();
}
