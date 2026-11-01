use super::*;
use worker_clock::WorkerClock;

#[test]
fn worker_capture_reads_sample_timed_audio_and_queues_control() {
    let controller = ReplayCaptureController::default();
    controller.load_scripted(
        vec![0.0; 640],
        vec![ScriptedCaptureEvent {
            at_sample: 320,
            kind: wav_replay_source::ScriptedCaptureEventKind::Control("stop".into()),
        }],
    );
    let clock = controller.virtual_clock();
    let mut capture = ReplayCapture {
        controller: controller.clone(),
    };
    capture.resume().unwrap();

    assert_eq!(capture.read_f32(320).unwrap().len(), 320);
    assert_eq!(capture.read_f32(320).unwrap().len(), 320);
    assert!(matches!(
        controller.pop_scripted_control(),
        Some(CaptureEvent::Control { name, .. }) if name == "stop"
    ));
    assert_eq!(
        clock
            .now()
            .duration_since(worker_clock::WorkerInstant::ZERO),
        Duration::from_millis(20)
    );
}

#[test]
fn worker_capture_pause_stops_replay_reads() {
    let controller = ReplayCaptureController::default();
    controller.load(vec![0.0; 320]);
    let mut capture = ReplayCapture {
        controller: controller.clone(),
    };
    capture.resume().unwrap();
    capture.pause();

    assert!(capture.read_f32(320).unwrap().is_empty());
    assert!(controller.debug_state().contains("paused=true"));
}

#[test]
fn worker_capture_releases_lane_only_at_virtual_completion() {
    let controller = ReplayCaptureController::default();
    controller.load(vec![0.0; 640]);
    let clock = controller.virtual_clock();
    let mut capture = ReplayCapture {
        controller: controller.clone(),
    };
    capture.resume().unwrap();
    assert_eq!(capture.read_f32(320).unwrap().len(), 320);

    let submitted_at = clock.now();
    controller.mark_lane_in_flight(worker_clock::ExecutorLane::Kws, submitted_at);
    controller.schedule_lane_completion(LaneCompletion {
        lane: worker_clock::ExecutorLane::Kws,
        submitted_at,
        measured_decode_duration: Duration::from_millis(10),
    });
    assert!(capture.read_f32(320).unwrap().is_empty());
    assert!(controller.lane_completion_ready(worker_clock::ExecutorLane::Kws, submitted_at,));
    assert_eq!(capture.read_f32(320).unwrap().len(), 320);
    assert_eq!(
        clock.now().duration_since(submitted_at),
        Duration::from_millis(20)
    );
}

#[test]
fn capture_processes_audio_that_arrives_before_decoder_completion() {
    let controller = ReplayCaptureController::default();
    controller.load(vec![0.0; 640]);
    let clock = controller.virtual_clock();
    let mut capture = ReplayCapture {
        controller: controller.clone(),
    };
    capture.resume().unwrap();
    assert_eq!(capture.read_f32(320).unwrap().len(), 320);

    let submitted_at = clock.now();
    controller.mark_lane_in_flight(worker_clock::ExecutorLane::Kws, submitted_at);
    controller.schedule_lane_completion(LaneCompletion {
        lane: worker_clock::ExecutorLane::Kws,
        submitted_at,
        measured_decode_duration: Duration::from_millis(30),
    });

    assert_eq!(capture.read_f32(320).unwrap().len(), 320);
    assert!(capture.read_f32(320).unwrap().is_empty());
    assert!(controller.lane_completion_ready(worker_clock::ExecutorLane::Kws, submitted_at));
    assert_eq!(
        clock.now().duration_since(submitted_at),
        Duration::from_millis(30)
    );
}

#[test]
fn paused_capture_still_releases_scheduled_lane_completion() {
    let controller = ReplayCaptureController::default();
    controller.load(vec![0.0; 640]);
    let clock = controller.virtual_clock();
    let mut capture = ReplayCapture {
        controller: controller.clone(),
    };
    capture.resume().unwrap();
    assert_eq!(capture.read_f32(320).unwrap().len(), 320);

    let submitted_at = clock.now();
    controller.mark_lane_in_flight(worker_clock::ExecutorLane::Kws, submitted_at);
    controller.schedule_lane_completion(LaneCompletion {
        lane: worker_clock::ExecutorLane::Kws,
        submitted_at,
        measured_decode_duration: Duration::from_millis(10),
    });
    capture.pause();

    assert!(controller.lane_completion_ready(worker_clock::ExecutorLane::Kws, submitted_at,));
    assert_eq!(
        clock.now().duration_since(submitted_at),
        Duration::from_millis(10)
    );
}

#[test]
fn replay_eof_waits_for_worker_dispatch_after_executor_completion() {
    let controller = ReplayCaptureController::default();
    controller.load_scripted_unpadded(vec![0.0; 320], Vec::new());
    {
        let mut state = controller.state.0.lock().unwrap();
        state.cursor = 320;
        state.pumped_total = 320;
    }
    controller.note_submitted(320);
    controller.note_processed(320, 11);
    assert_eq!(controller.finished(), None);
    let _guard = ReplayProbeDispatchGuard {
        controller: Some(controller.clone()),
        total_samples: 320,
    };
    drop(_guard);
    assert_eq!(controller.finished(), Some(11));
}

#[test]
fn replay_eof_tracks_older_dispatch_when_newer_probe_is_replaced() {
    let controller = ReplayCaptureController::default();
    controller.load_scripted_unpadded(vec![0.0; 320], Vec::new());
    {
        let mut state = controller.state.0.lock().unwrap();
        state.cursor = 320;
        state.pumped_total = 320;
    }
    controller.note_submitted(100);
    controller.note_processed(100, 3);
    controller.note_submitted(200);
    controller.note_discarded(200);
    assert_eq!(controller.finished(), None);
    controller.note_dispatched(100);
    assert_eq!(controller.finished(), Some(3));
}
