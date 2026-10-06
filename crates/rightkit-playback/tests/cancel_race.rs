//! Cancellation racing registration: whichever order `cancel` and `begin` land
//! in, the preparation is cancelled exactly once. Real threads, real registry,
//! no timing assumptions.
use rightkit_playback::{PlaybackError, PlaybackRegistry};
use std::sync::{Arc, Barrier};

#[test]
fn cancel_concurrent_with_begin_is_never_lost() {
    let registry = Arc::new(PlaybackRegistry::new());
    for i in 0..3000 {
        let id = format!("playback-race-{i}");
        let barrier = Arc::new(Barrier::new(2));
        let canceller = {
            let (registry, barrier, id) = (registry.clone(), barrier.clone(), id.clone());
            std::thread::spawn(move || {
                barrier.wait();
                registry.cancel(&id);
            })
        };
        barrier.wait();
        let began = registry.begin(&id);
        canceller.join().unwrap();
        match began {
            Err(PlaybackError::Cancelled) => {}
            Ok(active) => assert!(active.token().is_cancelled(), "cancel lost for {id}"),
            Err(other) => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn early_cancel_is_consumed_once() {
    let registry = Arc::new(PlaybackRegistry::new());
    registry.cancel("playback-early");
    assert!(matches!(
        registry.begin("playback-early"),
        Err(PlaybackError::Cancelled)
    ));
    let again = registry
        .begin("playback-early")
        .expect("a later request with the same id starts clean");
    assert!(!again.token().is_cancelled());
}
