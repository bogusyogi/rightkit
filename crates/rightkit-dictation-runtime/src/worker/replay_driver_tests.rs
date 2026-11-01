use super::super::wav_replay_source::ScriptedCaptureEventKind;
use super::*;

#[test]
fn pumps_audio_and_control_without_wall_clock_waits() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let scripted = vec![ScriptedCaptureEvent {
        at_sample: 320,
        kind: ScriptedCaptureEventKind::Control("stop".into()),
    }];
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 640], scripted, Arc::clone(&clock)).unwrap();

    assert_eq!(driver.read_audio(320).unwrap().unwrap().len(), 320);
    assert_eq!(clock.now(), sample_instant(0, 16_000));
    assert_eq!(driver.read_audio(320).unwrap().unwrap().len(), 320);
    assert!(
        matches!(driver.pop_control(), Some(CaptureEvent::Control { name, .. }) if name == "stop")
    );
    assert_eq!(clock.now(), sample_instant(320, 16_000));
    assert_eq!(driver.read_audio(320).unwrap(), None);
    assert!(driver.is_eof());
    assert_eq!(clock.now(), sample_instant(640, 16_000));
}

#[test]
fn preserves_capture_block_remainder_across_short_reads() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver = ReplayDriver::from_samples(16_000, vec![0.0; 320], Vec::new(), clock).unwrap();

    assert_eq!(driver.read_audio(100).unwrap().unwrap().len(), 100);
    assert_eq!(driver.read_audio(100).unwrap().unwrap().len(), 100);
    assert_eq!(driver.read_audio(200).unwrap().unwrap().len(), 120);
    assert_eq!(driver.read_audio(320).unwrap(), None);
}

#[test]
fn lane_completion_is_dispatched_before_later_audio() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 640], Vec::new(), Arc::clone(&clock)).unwrap();
    assert!(matches!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Audio(_)))
    ));
    driver
        .schedule_lane_completion(ExecutorLane::Kws, clock.now(), Duration::from_millis(10))
        .unwrap();

    assert_eq!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::LaneCompletion(
            ExecutorLane::Kws,
            WorkerInstant::ZERO,
        ))
    );
    assert_eq!(clock.now(), sample_instant(160, 16_000));
    assert!(driver.take_lane_completion(ExecutorLane::Kws, WorkerInstant::ZERO));
    assert!(matches!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Audio(_)))
    ));
    assert_eq!(clock.now(), sample_instant(320, 16_000));
}

#[test]
fn successive_rows_continue_monotonic_virtual_time() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut first =
        ReplayDriver::from_samples(16_000, vec![0.0; 320], Vec::new(), Arc::clone(&clock)).unwrap();
    assert_eq!(first.read_audio(320).unwrap().unwrap().len(), 320);
    assert_eq!(first.read_audio(320).unwrap(), None);
    let row_two_start = clock.now();

    let mut second =
        ReplayDriver::from_samples(16_000, vec![0.0; 320], Vec::new(), Arc::clone(&clock)).unwrap();
    assert_eq!(second.read_audio(320).unwrap().unwrap().len(), 320);
    assert_eq!(clock.now(), row_two_start);
    assert_eq!(second.read_audio(320).unwrap(), None);
    assert_eq!(
        clock.now().duration_since(row_two_start),
        Duration::from_millis(20)
    );
}

#[test]
fn unknown_lane_duration_yields_without_advancing_capture_time() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 640], Vec::new(), Arc::clone(&clock)).unwrap();
    let submitted_at = clock.now();
    driver
        .mark_lane_submitted(ExecutorLane::Kws, submitted_at)
        .unwrap();

    assert_eq!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::AwaitingLaneDuration(ExecutorLane::Kws))
    );
    assert_eq!(clock.now(), submitted_at);
    assert_eq!(driver.read_audio(320).unwrap(), None);
    assert_eq!(clock.now(), submitted_at);
    assert!(!driver.is_eof());
}

#[test]
fn capture_events_before_measured_completion_dispatch_first() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let scripted = vec![ScriptedCaptureEvent {
        at_sample: 320,
        kind: ScriptedCaptureEventKind::Control("stop".into()),
    }];
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 640], scripted, Arc::clone(&clock)).unwrap();
    let submitted_at = clock.now();
    driver
        .mark_lane_submitted(ExecutorLane::Kws, submitted_at)
        .unwrap();
    driver
        .supply_lane_duration(ExecutorLane::Kws, submitted_at, Duration::from_millis(50))
        .unwrap();

    assert!(matches!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Audio(_)))
    ));
    assert_eq!(clock.now(), sample_instant(0, 16_000));
    assert!(matches!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Control { name, .. })) if name == "stop"
    ));
    assert_eq!(clock.now(), sample_instant(320, 16_000));
    assert!(matches!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Audio(_)))
    ));
    assert_eq!(clock.now(), sample_instant(320, 16_000));
    assert_eq!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Eof {
            at_sample: 640
        }))
    );
    assert_eq!(clock.now(), sample_instant(640, 16_000));
    assert_eq!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::LaneCompletion(
            ExecutorLane::Kws,
            submitted_at,
        ))
    );
    assert_eq!(clock.now(), sample_instant(800, 16_000));
}

#[test]
fn independent_unknown_lanes_hold_events_until_both_durations_arrive() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 640], Vec::new(), Arc::clone(&clock)).unwrap();
    let submitted_at = clock.now();
    driver
        .mark_lane_submitted(ExecutorLane::Kws, submitted_at)
        .unwrap();
    driver
        .mark_lane_submitted(ExecutorLane::MainAsr, submitted_at)
        .unwrap();
    driver
        .supply_lane_duration(ExecutorLane::Kws, submitted_at, Duration::from_millis(40))
        .unwrap();

    assert_eq!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::AwaitingLaneDuration(
            ExecutorLane::MainAsr
        ))
    );
    assert_eq!(clock.now(), submitted_at);

    driver
        .supply_lane_duration(
            ExecutorLane::MainAsr,
            submitted_at,
            Duration::from_millis(10),
        )
        .unwrap();
    assert!(matches!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Audio(_)))
    ));
    assert_eq!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::LaneCompletion(
            ExecutorLane::MainAsr,
            submitted_at,
        ))
    );
    assert_eq!(clock.now(), sample_instant(160, 16_000));
    assert!(driver.take_lane_completion(ExecutorLane::MainAsr, submitted_at));
    assert!(matches!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Audio(_)))
    ));
    assert_eq!(clock.now(), sample_instant(320, 16_000));
    assert_eq!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::LaneCompletion(
            ExecutorLane::Kws,
            submitted_at,
        ))
    );
    assert_eq!(clock.now(), sample_instant(640, 16_000));
    assert!(driver.take_lane_completion(ExecutorLane::Kws, submitted_at));
    assert_eq!(
        driver.next_event().unwrap(),
        Some(ReplayDriverEvent::Capture(CaptureEvent::Eof {
            at_sample: 640
        }))
    );
}

#[test]
fn lane_state_ignores_duplicate_submit_and_rejects_unmatched_duration() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, Vec::new(), Vec::new(), Arc::clone(&clock)).unwrap();
    let submitted_at = clock.now();

    assert_eq!(
        driver.supply_lane_duration(ExecutorLane::Kws, submitted_at, Duration::ZERO),
        Err(ReplayLaneError::UnexpectedCompletion(
            ExecutorLane::Kws,
            submitted_at,
        ))
    );
    driver
        .mark_lane_submitted(ExecutorLane::Kws, clock.now())
        .unwrap();
    assert_eq!(
        driver.mark_lane_submitted(ExecutorLane::Kws, clock.now()),
        Ok(())
    );
    assert!(driver.is_waiting_for_lane_duration());
}

#[test]
fn lane_completion_can_be_taken_by_lane_without_consuming_other_lane() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 640], Vec::new(), Arc::clone(&clock)).unwrap();
    let submitted_at = clock.now();
    driver
        .schedule_lane_completion(ExecutorLane::Kws, submitted_at, Duration::from_millis(5))
        .unwrap();
    driver
        .schedule_lane_completion(
            ExecutorLane::MainAsr,
            submitted_at,
            Duration::from_millis(10),
        )
        .unwrap();

    assert_eq!(driver.read_audio(320).unwrap().unwrap().len(), 320);
    assert_eq!(driver.read_audio(320).unwrap(), None);
    assert!(driver.lane_completion_ready(ExecutorLane::Kws, submitted_at));
    assert!(!driver.lane_completion_ready(ExecutorLane::MainAsr, submitted_at));
    assert!(driver.take_lane_completion(ExecutorLane::Kws, submitted_at));
    assert_eq!(driver.read_audio(320).unwrap(), None);
    assert!(driver.lane_completion_ready(ExecutorLane::MainAsr, submitted_at));
    assert!(driver.take_lane_completion(ExecutorLane::MainAsr, submitted_at));
    assert_eq!(driver.read_audio(320).unwrap().unwrap().len(), 320);
}

#[test]
fn pending_latest_request_blocks_capture_and_starts_after_prior_completion() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 960], Vec::new(), Arc::clone(&clock)).unwrap();
    assert_eq!(driver.read_audio(320).unwrap().unwrap().len(), 320);
    let first_at = clock.now();
    driver
        .schedule_lane_completion(ExecutorLane::Kws, first_at, Duration::from_millis(10))
        .unwrap();
    let pending_at = first_at.saturating_add(Duration::from_millis(5));
    driver
        .mark_lane_submitted(ExecutorLane::Kws, pending_at)
        .unwrap();

    assert_eq!(driver.read_audio(320).unwrap(), None);
    assert!(driver.lane_completion_ready(ExecutorLane::Kws, first_at));
    assert!(driver.take_lane_completion(ExecutorLane::Kws, first_at));
    let second_started_at = clock.now();
    driver
        .supply_lane_duration(ExecutorLane::Kws, pending_at, Duration::from_millis(10))
        .unwrap();
    assert_eq!(driver.read_audio(320).unwrap(), None);
    assert_eq!(
        clock.now().duration_since(second_started_at),
        Duration::from_millis(10)
    );
    assert!(driver.lane_completion_ready(ExecutorLane::Kws, pending_at));
    assert!(driver.take_lane_completion(ExecutorLane::Kws, pending_at));
    assert_eq!(driver.read_audio(320).unwrap().unwrap().len(), 320);
}

#[test]
fn released_pending_request_can_be_replaced_before_decode_starts() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 960], Vec::new(), Arc::clone(&clock)).unwrap();
    let first_at = clock.now();
    driver
        .schedule_lane_completion(ExecutorLane::Kws, first_at, Duration::from_millis(10))
        .unwrap();
    let superseded_at = first_at.saturating_add(Duration::from_millis(5));
    driver
        .mark_lane_submitted(ExecutorLane::Kws, superseded_at)
        .unwrap();
    assert!(driver
        .advance_to_lane_completion(ExecutorLane::Kws, first_at)
        .unwrap());

    let replacement_at = first_at.saturating_add(Duration::from_millis(8));
    driver
        .mark_lane_submitted(ExecutorLane::Kws, replacement_at)
        .unwrap();
    driver
        .supply_lane_duration(ExecutorLane::Kws, replacement_at, Duration::from_millis(10))
        .unwrap();
    assert!(driver
        .advance_to_lane_completion(ExecutorLane::Kws, replacement_at)
        .unwrap());
}

#[test]
fn actual_dequeue_survives_a_newer_pending_replacement() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 960], Vec::new(), Arc::clone(&clock)).unwrap();
    let a = clock.now();
    let b = a.saturating_add(Duration::from_millis(5));
    let c = a.saturating_add(Duration::from_millis(11));
    let d = a.saturating_add(Duration::from_millis(12));
    driver.mark_lane_submitted(ExecutorLane::Kws, a).unwrap();
    driver.mark_lane_started(ExecutorLane::Kws, a).unwrap();
    driver.mark_lane_submitted(ExecutorLane::Kws, b).unwrap();
    driver
        .supply_lane_duration(ExecutorLane::Kws, a, Duration::from_millis(10))
        .unwrap();
    assert!(driver
        .advance_to_lane_completion(ExecutorLane::Kws, a)
        .unwrap());
    driver.mark_lane_submitted(ExecutorLane::Kws, c).unwrap();
    driver.mark_lane_submitted(ExecutorLane::Kws, d).unwrap();
    driver.mark_lane_started(ExecutorLane::Kws, c).unwrap();
    driver
        .supply_lane_duration(ExecutorLane::Kws, c, Duration::from_millis(20))
        .unwrap();
    assert!(driver
        .advance_to_lane_completion(ExecutorLane::Kws, c)
        .unwrap());
    assert_eq!(clock.now().duration_since(a), Duration::from_millis(31));
    assert!(matches!(driver.lane_flight(ExecutorLane::Kws),
        LaneFlight::AwaitingDuration { request_at, .. } if request_at == d));
    driver.mark_lane_started(ExecutorLane::Kws, d).unwrap();
    driver
        .supply_lane_duration(ExecutorLane::Kws, d, Duration::from_millis(5))
        .unwrap();
    assert!(driver
        .advance_to_lane_completion(ExecutorLane::Kws, d)
        .unwrap());
    assert_eq!(clock.now().duration_since(a), Duration::from_millis(36));
}

#[test]
fn actual_dequeue_can_follow_a_completed_request_with_same_timestamp() {
    let clock = Arc::new(VirtualWorkerClock::default());
    let mut driver =
        ReplayDriver::from_samples(16_000, vec![0.0; 960], Vec::new(), Arc::clone(&clock)).unwrap();
    let submitted_at = clock.now();
    driver
        .mark_lane_submitted(ExecutorLane::Kws, submitted_at)
        .unwrap();
    driver
        .mark_lane_started(ExecutorLane::Kws, submitted_at)
        .unwrap();
    driver
        .mark_lane_submitted(ExecutorLane::Kws, submitted_at)
        .unwrap();
    driver
        .supply_lane_duration(ExecutorLane::Kws, submitted_at, Duration::from_millis(10))
        .unwrap();
    assert_eq!(
        driver.mark_lane_started(ExecutorLane::Kws, submitted_at),
        Err(ReplayLaneError::UnexpectedStart(
            ExecutorLane::Kws,
            submitted_at
        ))
    );
    assert!(driver
        .advance_to_lane_completion(ExecutorLane::Kws, submitted_at)
        .unwrap());
    driver
        .mark_lane_started(ExecutorLane::Kws, submitted_at)
        .unwrap();
    driver
        .supply_lane_duration(ExecutorLane::Kws, submitted_at, Duration::from_millis(5))
        .unwrap();
    assert!(driver
        .advance_to_lane_completion(ExecutorLane::Kws, submitted_at)
        .unwrap());
    assert_eq!(
        clock.now().duration_since(submitted_at),
        Duration::from_millis(15)
    );
}
