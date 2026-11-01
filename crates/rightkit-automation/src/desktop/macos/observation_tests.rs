use super::*;

#[test]
fn running_application_identity_miss_labels_are_specific_and_content_free() {
    assert_eq!(
        RunningApplicationIdentityMiss::InvalidPid.label(),
        "invalid_pid"
    );
    assert_eq!(
        RunningApplicationIdentityMiss::NotFound.label(),
        "running_app_not_found"
    );
    assert_eq!(
        RunningApplicationIdentityMiss::Terminated.label(),
        "running_app_terminated"
    );
    assert_eq!(
        RunningApplicationIdentityMiss::MissingBundleIdentifier.label(),
        "missing_bundle_identifier"
    );
    assert_eq!(
        RunningApplicationIdentityMiss::MissingBundleUrl.label(),
        "missing_bundle_url"
    );
    assert_eq!(
        RunningApplicationIdentityMiss::MissingBundlePath.label(),
        "missing_bundle_path"
    );
}

/// Lease stand-in that counts its own drops.
struct CountedLease(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for CountedLease {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn drops() -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0))
}

fn dropped(counter: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> usize {
    counter.load(std::sync::atomic::Ordering::SeqCst)
}

#[test]
fn prewake_holds_the_lease_for_the_same_pid_and_never_rewakes_it() {
    let counter = drops();
    let mut state = PrewakeState::new();
    let first = state.activate(10);
    let seq = first.wake_seq.expect("first activation wakes");
    assert!(first.released.is_none());
    assert!(state
        .finish(seq, 10, Some(CountedLease(counter.clone())))
        .is_none());

    // Re-announcing the held pid neither wakes nor releases anything.
    let again = state.activate(10);
    assert!(again.wake_seq.is_none());
    assert!(again.released.is_none());
    assert_eq!(dropped(&counter), 0);
}

#[test]
fn prewake_drops_the_held_lease_when_a_different_pid_activates() {
    let counter = drops();
    let mut state = PrewakeState::new();
    let seq = state.activate(10).wake_seq.unwrap();
    assert!(state
        .finish(seq, 10, Some(CountedLease(counter.clone())))
        .is_none());

    let next = state.activate(20);
    assert!(next.wake_seq.is_some());
    assert_eq!(
        dropped(&counter),
        0,
        "released lease is handed back, not dropped under the lock"
    );
    drop(next.released);
    assert_eq!(dropped(&counter), 1);

    // The old pid is no longer held, so activating it again wakes it again.
    let seq_20 = next.wake_seq.unwrap();
    assert!(state.finish(seq_20, 20, None).is_none());
    assert!(state.activate(10).wake_seq.is_some());
}

#[test]
fn prewake_stale_wake_hands_its_lease_back_instead_of_holding_it() {
    let counter = drops();
    let mut state = PrewakeState::new();
    let slow = state.activate(10).wake_seq.unwrap();
    // Another app activates while pid 10's wake is still running.
    let newer = state.activate(20).wake_seq.unwrap();
    assert!(!state.is_current(slow));
    assert!(state.is_current(newer));

    let stale = state.finish(slow, 10, Some(CountedLease(counter.clone())));
    assert!(stale.is_some(), "stale lease must be returned for dropping");
    drop(stale);
    assert_eq!(dropped(&counter), 1);
    // The stale finish must not clear the newer wake's pending marker.
    assert!(state.activate(20).wake_seq.is_none());
}

#[test]
fn prewake_skips_a_pid_whose_wake_is_already_in_flight() {
    let mut state: PrewakeState<CountedLease> = PrewakeState::new();
    let seq = state.activate(10).wake_seq.unwrap();
    assert!(state.activate(10).wake_seq.is_none());
    // A wake that ends without a lease (non-Chromium, already awake) frees
    // the pid so its next activation is evaluated again.
    assert!(state.finish(seq, 10, None).is_none());
    assert!(state.activate(10).wake_seq.is_some());
}
