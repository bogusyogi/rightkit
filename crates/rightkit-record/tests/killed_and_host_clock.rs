mod common;

use common::{read_manifest, recorder, request, Scratch};
use rightkit_contracts::types::record_session::BundleStatus;
use rightkit_record::fake::{frame, FakeBackend};
use rightkit_record::{HelperEvent, Phase, StopReason, VerifyOptions};

const T0: u64 = 1_000_000;

fn ready_and_frames(n: u64) -> Vec<HelperEvent> {
    (0..n)
        .map(|i| {
            let sample = frame(i, 5_000 + i * 16_666_667, i * 16_666_667);
            if i == 0 {
                HelperEvent::Ready(sample)
            } else {
                HelperEvent::Frame(sample)
            }
        })
        .collect()
}

#[test]
fn a_killed_stop_never_seals_complete_and_says_killed_in_the_bundle() {
    let scratch = Scratch::new("killed");
    let mut backend = FakeBackend::macos();
    backend.push(ready_and_frames(3));
    let out = scratch.bundle("take");
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out.clone())).expect("start");

    let outcome = rec.record_stop_with(StopReason::Killed).expect("stop");
    assert_eq!(outcome.status, BundleStatus::PartialQuarantined, "{outcome:?}");
    assert_eq!(outcome.reason.as_deref(), Some("killed"));
    assert_eq!(rec.backend().stops, 1, "the media is still finished through the helper");
    assert_eq!(rec.status().unwrap().phase, Phase::Quarantined);

    let manifest = read_manifest(&out);
    assert_eq!(manifest["status"], "partial_quarantined");
    assert_eq!(manifest["reason"], "killed");
    assert_eq!(manifest["frames"]["count"], 3);
    assert!(rec
        .bundle_verify(&out, &VerifyOptions::default())
        .unwrap()
        .ok());

    // Idempotent: a later plain stop returns the same outcome, not a complete one.
    assert_eq!(rec.record_stop().unwrap(), outcome);
    assert_eq!(rec.backend().stops, 1);
}

#[test]
fn a_killed_stop_before_any_frame_is_invalid_and_says_killed() {
    let scratch = Scratch::new("killed-no-frames");
    let out = scratch.bundle("take");
    let mut rec = recorder(FakeBackend::macos(), T0);
    rec.record_start(request(out)).expect("start");
    let outcome = rec.record_stop_with(StopReason::Killed).unwrap();
    assert_eq!(outcome.status, BundleStatus::Invalid);
    assert_eq!(outcome.reason.as_deref(), Some("killed: no_accepted_frames"));
}

#[test]
fn a_requested_stop_is_unchanged_and_still_seals_complete() {
    let scratch = Scratch::new("requested");
    let mut backend = FakeBackend::macos();
    backend.push(ready_and_frames(2));
    let out = scratch.bundle("take");
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out)).expect("start");
    let outcome = rec.record_stop_with(StopReason::Requested).unwrap();
    assert_eq!(outcome.status, BundleStatus::Complete);
    assert_eq!(outcome.reason, None);
}

#[test]
fn a_take_that_already_ended_keeps_its_outcome_whatever_the_stop_reason() {
    let scratch = Scratch::new("killed-after-complete");
    let mut backend = FakeBackend::macos();
    backend.push(ready_and_frames(2));
    let out = scratch.bundle("take");
    let mut rec = recorder(backend, T0);
    rec.record_start(request(out)).expect("start");
    let first = rec.record_stop().unwrap();
    assert_eq!(first.status, BundleStatus::Complete);
    assert_eq!(rec.record_stop_with(StopReason::Killed).unwrap(), first);
}

#[test]
fn host_ns_reads_the_backends_host_clock_and_is_none_without_one() {
    let mut backend = FakeBackend::windows();
    backend.host_now_ns = Some(123_456_789);
    let rec = recorder(backend, T0);
    assert_eq!(rec.host_ns(), Some(123_456_789));

    let rec = recorder(FakeBackend::windows(), T0);
    assert_eq!(rec.host_ns(), None);
}
