#![cfg(feature = "client-testing")]
//! Real threads against the native client: a deactivate racing a slow activate never
//! resurrects the record, and a write that outlives its caller keeps the licence operation lock
//! (worker-lifetime fencing) until it has really finished.

mod client_support;

use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

use client_support::*;
use rightkit_license::{LicenseError, MessageCode, RevisionState, SecretString, ViewStatus};

fn key() -> SecretString {
    SecretString::new(KEY)
}

/// Poll `condition` until it holds or `within` passes.
fn wait_for(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

#[test]
fn a_deactivate_racing_a_slow_activate_never_resurrects_the_record() {
    let f = fixture();
    f.state.activate_delay_ms.store(700, SeqCst);
    let first = f.client();
    let second = f.client();

    let activating = std::thread::spawn(move || first.activate(key()));
    // The activate is now on the wire, holding the operation lock.
    assert!(wait_for(Duration::from_secs(5), || {
        f.state.activate_calls.load(SeqCst) == 1
    }));
    // The deactivate waits behind it (bounded) and then clears what the activate wrote.
    let deactivated = second.deactivate();
    let activated = activating.join().unwrap();

    assert!(activated.is_ok(), "{activated:?}");
    assert!(deactivated.is_ok(), "{deactivated:?}");
    // The final state is the tombstone: nothing the late activate did survives it.
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
    assert!(f.backend.text(MARKER).is_none());
    let status = second.status();
    assert_eq!(status.status, ViewStatus::None);
    assert_eq!(status.revision_state, RevisionState::Tombstone);
    // And it stays so.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
    // The activation reached the server before the deactivation did.
    assert_eq!(f.state.activate_calls.load(SeqCst), 1);
    assert_eq!(f.state.deactivate_calls.load(SeqCst), 1);
}

#[test]
fn a_deactivate_that_cannot_get_the_lock_in_time_is_busy_not_a_resurrection() {
    let f = fixture();
    f.state.activate_delay_ms.store(900, SeqCst);
    let first = f.client();
    let impatient =
        f.client_with(|c| c.with_waits(Duration::from_millis(150), Duration::from_millis(100)));

    let activating = std::thread::spawn(move || first.activate(key()));
    assert!(wait_for(Duration::from_secs(5), || {
        f.state.activate_calls.load(SeqCst) == 1
    }));
    let started = Instant::now();
    assert_eq!(impatient.deactivate().unwrap_err(), LicenseError::Busy);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the wait is bounded"
    );
    // Status during the mutation: the last committed view (none yet), marked busy.
    let during = impatient.status();
    assert!(during.busy);
    assert_eq!(during.message, MessageCode::Busy);

    assert!(activating.join().unwrap().is_ok());
    // Nothing was cleared, so the licence stands; a deactivate that has the lock then works.
    // (A client with another process anchor must see wall time strictly past the ledger mark,
    // which the activation just wrote: that is the clock rule, not the lock.)
    std::thread::sleep(Duration::from_millis(1100));
    assert_eq!(impatient.status().status, ViewStatus::Active);
    assert!(impatient.deactivate().is_ok());
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
}

#[test]
fn a_write_that_outlives_its_caller_keeps_the_lock_and_a_later_deactivate_waits() {
    // The credential store gives up on a call after 300 ms; the backend takes 1.4 s for the
    // first write of the licence item. The caller stops waiting after 250 ms.
    let f = fixture_with(Duration::from_millis(300));
    let patient = f.client_with(|c| c.with_persist_wait(Duration::from_millis(250)));
    let other = f.client();
    let impatient =
        f.client_with(|c| c.with_waits(Duration::from_millis(150), Duration::from_millis(100)));
    f.backend.primary_set_delay_ms.store(1400, SeqCst);

    let started = Instant::now();
    let error = patient.activate(key()).unwrap_err();
    // The caller got an unresolved answer, long before the backend finished.
    assert_eq!(error, LicenseError::Unresolved);
    assert!(
        started.elapsed() < Duration::from_millis(1200),
        "{:?}",
        started.elapsed()
    );

    // The worker still owns the lock: other callers are busy, status is display-only busy.
    assert_eq!(impatient.activate(key()).unwrap_err(), LicenseError::Busy);
    assert!(impatient.status().busy);

    // A deactivate with the normal 5 s wait waits for the worker, then clears. It never
    // reports success against a state it did not see.
    let deactivated = other.deactivate();
    assert!(deactivated.is_ok(), "{deactivated:?}");
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
    assert!(f.backend.text(MARKER).is_none());

    // The worker has finished by now; nothing it did can land after the tombstone.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
    // The caller that gave up reconciles on its next call and finds a clean store.
    let view = patient.status();
    assert!(!view.unresolved, "{view:?}");
    assert_eq!(view.revision_state, RevisionState::Tombstone);
}

#[test]
fn two_activations_in_a_row_from_two_clients_are_ordered_not_interleaved() {
    let f = fixture();
    f.state.activate_delay_ms.store(300, SeqCst);
    let (a, b) = (f.client(), f.client());
    let first = std::thread::spawn(move || a.activate(key()));
    assert!(wait_for(Duration::from_secs(5), || {
        f.state.activate_calls.load(SeqCst) == 1
    }));
    let second = b.activate(key());
    assert!(first.join().unwrap().is_ok());
    assert!(second.is_ok(), "{second:?}");
    // Two writes, one after the other: exactly one active envelope, no marker.
    assert_eq!(f.backend.envelope().unwrap()["state"], "active");
    assert!(f.backend.text(MARKER).is_none());
    assert_eq!(f.state.activate_calls.load(SeqCst), 2);
}
