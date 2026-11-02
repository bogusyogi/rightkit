//! Ordinary unit tests for `unified_probe_lane`. Moved out of the
//! production file as a pure relocation; see `unified_probe_lane_owner_replays.rs`
//! for the ignored owner-recording experiments/replays.
#![cfg(test)]

use super::*;

#[test]
fn final_reply_wait_cancels_without_waiting_for_model_reply() {
    let (_reply_tx, reply_rx) = channel::<Result<u32, String>>();
    let cancel = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let waiter_cancel = Arc::clone(&cancel);
    let (done_tx, done_rx) = channel();
    let waiter = std::thread::spawn(move || {
        done_tx
            .send(wait_for_final_reply(
                reply_rx,
                Duration::from_secs(30),
                &waiter_cancel,
                0,
                "test final",
            ))
            .unwrap();
    });
    // Works whether Cancel arrives before or during the wait. No model reply
    // is ever sent; completing this receive proves cancellation releases it.
    cancel.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err("recording finalization cancelled".to_string()),
    );
    waiter.join().unwrap();
}

#[test]
fn final_reply_wait_preserves_result_and_fails_on_deadline_or_disconnect() {
    let cancel = std::sync::atomic::AtomicU64::new(3);
    let (tx, rx) = channel();
    tx.send(Ok(42_u32)).unwrap();
    assert_eq!(
        wait_for_final_reply(rx, Duration::from_secs(1), &cancel, 3, "final"),
        Ok(42)
    );
    let (_tx, rx) = channel::<Result<u32, String>>();
    assert_eq!(
        wait_for_final_reply(rx, Duration::ZERO, &cancel, 3, "final"),
        Err("final timed out".into())
    );
    let (tx, rx) = channel::<Result<u32, String>>();
    drop(tx);
    assert_eq!(
        wait_for_final_reply(rx, Duration::from_secs(1), &cancel, 3, "final"),
        Err("final reply dropped".into())
    );
}

#[test]
fn reload_coordinator_serializes_and_coalesces_queued_requests() {
    let (request_tx, request_rx) = channel();
    let (result_tx, result_rx) = channel();
    let (started_tx, started_rx) = channel();
    let (release_tx, release_rx) = channel();
    let (done_tx, done_rx) = channel();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_calls = Arc::clone(&calls);
    let coordinator = std::thread::spawn(move || {
        run_reload_coordinator(
            request_rx,
            result_tx,
            || {
                let call = observed_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if call == 0 {
                    started_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("reload barrier release");
                }
                Ok(true)
            },
            None,
        );
        done_tx.send(()).unwrap();
    });

    request_tx
        .send(ReloadCoordinatorCommand::Request { id: 1 })
        .unwrap();
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("reload coordinator started");
    request_tx
        .send(ReloadCoordinatorCommand::Request { id: 2 })
        .unwrap();
    request_tx
        .send(ReloadCoordinatorCommand::Request { id: 3 })
        .unwrap();
    release_tx.send(()).unwrap();

    assert_eq!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap().id,
        1
    );
    assert_eq!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap().id,
        2
    );
    assert_eq!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap().id,
        3
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);

    request_tx.send(ReloadCoordinatorCommand::Shutdown).unwrap();
    done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    coordinator.join().unwrap();
}

#[test]
fn reload_key_convergence_handles_fixed_probe_and_settings_return() {
    assert!(reload_keys_converged("A", "fixed", "A", false));
    assert!(!reload_keys_converged("B", "fixed", "A", false));
    for (configured, main, probe) in [("A", "A", "A"), ("B", "B", "B"), ("A", "A", "A")] {
        assert!(reload_keys_converged(main, probe, configured, true));
    }
    // Settings may move A -> B -> A while a reload is in flight; stale worker
    // replies must not declare convergence for returned setting A.
    assert!(!reload_keys_converged("B", "B", "A", true));
}

fn background_request(start_sample: usize, end_sample: usize) -> BackgroundRequest {
    BackgroundRequest {
        generation: 7,
        start_sample,
        end_sample,
        audio: vec![0.25; end_sample - start_sample],
    }
}

#[test]
fn cleanup_first_requires_native_kws_confirmation_and_onset() {
    use rightkit_text_polish::text_pipeline::ControlIntent;
    assert!(should_use_cleanup_first_control(
        true,
        ControlIntent::Send,
        Some(ControlIntent::Send),
        Some(16_000),
    ));
    assert!(!should_use_cleanup_first_control(
        true,
        ControlIntent::Send,
        None,
        Some(16_000),
    ));
    assert!(!should_use_cleanup_first_control(
        true,
        ControlIntent::Stop,
        Some(ControlIntent::Send),
        Some(16_000),
    ));
    assert!(!should_use_cleanup_first_control(
        false,
        ControlIntent::Send,
        Some(ControlIntent::Send),
        Some(16_000),
    ));
    assert!(!should_use_cleanup_first_control(
        true,
        ControlIntent::Cancel,
        Some(ControlIntent::Cancel),
        Some(16_000),
    ));
}

fn request(marker: u64) -> ProbeRequest {
    ProbeRequest {
        generation: 7,
        kws_stream_generation: 3,
        session_id: "session-7".into(),
        submitted_at: worker_clock::WorkerInstant::ZERO,
        created_at: Instant::now(),
        audio: vec![marker as f32],
        speech_start_sample: None,
        command_start: 0,
        stream_base_sample: 0,
        native_origin_sample: 0,
        total_samples: 1,
        pending_control_prefix: false,
        lane: "test",
        probe_sequence: marker,
        diagnostic_sample: marker == 1,
    }
}

fn opening_request(marker: u64) -> OpeningCommandRequest {
    OpeningCommandRequest {
        generation: 7,
        session_id: "session-7".into(),
        submitted_at: worker_clock::WorkerInstant::ZERO,
        created_at: Instant::now(),
        audio: vec![marker as f32],
        speech_start_sample: Some(0),
        command_start: 0,
        total_samples: marker as usize,
        pause_ready: false,
        pending_prefix: false,
    }
}

#[test]
#[cfg(target_os = "windows")]
fn idle_recording_dictation_gets_a_keep_warm_decode_after_the_idle_threshold() {
    let mailbox = MainAsrMailbox::default();
    mailbox.set_dictation_active(true);
    let started = Instant::now();
    assert!(matches!(mailbox.recv(), MainAsrJob::KeepWarm));
    let waited = started.elapsed();
    assert!(
        waited >= KEEP_WARM_IDLE,
        "keep-warm fired early: {waited:?}"
    );
    assert!(
        waited < KEEP_WARM_IDLE + Duration::from_millis(400),
        "keep-warm late: {waited:?}"
    );
}

#[test]
fn real_work_wins_over_keep_warm_during_dictation() {
    let mailbox = MainAsrMailbox::default();
    mailbox.set_dictation_active(true);
    mailbox.submit_opening(opening_request(7));
    assert!(matches!(mailbox.recv(), MainAsrJob::Opening(_)));
}

#[test]
fn silence_burst_fires_once_per_silence_run_and_rearms_on_speech() {
    let mut tracker = TerminalSilenceTracker::default();
    // 9 frames = 288 ms of silence: below the 300 ms threshold.
    let below = KEEP_WARM_BURST_SILENCE_SAMPLES / VAD_FRAME_SAMPLES;
    let mut at = 0;
    let mut feed = |tracker: &mut TerminalSilenceTracker, frames: &[bool]| {
        let len = frames.len() * VAD_FRAME_SAMPLES;
        let fired = tracker.observe(at, len, Some(frames));
        at += len;
        fired
    };
    assert!(!feed(&mut tracker, &[true; 3]), "speech never bursts");
    assert!(!feed(&mut tracker, &vec![false; below]), "below threshold");
    assert!(feed(&mut tracker, &[false]), "first crossing bursts");
    assert!(
        !feed(&mut tracker, &[false; 20]),
        "same silence run: no second burst"
    );
    assert!(
        !feed(&mut tracker, &[true]),
        "speech re-arms without bursting"
    );
    assert!(
        !feed(&mut tracker, &vec![false; below]),
        "new run below threshold"
    );
    assert!(
        feed(&mut tracker, &[false; 2]),
        "new run crossing bursts again"
    );
}

#[test]
#[cfg(target_os = "windows")]
fn silence_burst_queues_two_immediate_keep_warm_passes_behind_real_work() {
    let mailbox = MainAsrMailbox::default();
    mailbox.request_keep_warm_burst();
    assert_eq!(
        mailbox.state.lock().keep_warm_burst,
        0,
        "no burst outside dictation"
    );
    mailbox.set_dictation_active(true);
    mailbox.request_keep_warm_burst();
    mailbox.submit_opening(opening_request(7));
    if !keep_warm_enabled() {
        return; // battery or RIGHTKIT_ASR_KEEP_WARM_DISABLED: burst is gated off
    }
    assert!(
        matches!(mailbox.recv(), MainAsrJob::Opening(_)),
        "real work first"
    );
    let started = Instant::now();
    assert!(matches!(mailbox.recv(), MainAsrJob::KeepWarm));
    assert!(matches!(mailbox.recv(), MainAsrJob::KeepWarm));
    assert!(
        started.elapsed() < KEEP_WARM_IDLE,
        "burst passes do not wait for idle"
    );
    assert_eq!(mailbox.state.lock().keep_warm_burst, 0);
    mailbox.request_keep_warm_burst();
    mailbox.set_dictation_active(false);
    assert_eq!(
        mailbox.state.lock().keep_warm_burst,
        0,
        "dictation end drops a pending burst"
    );
}

#[test]
fn opening_command_latest_snapshot_replaces_pending_audio() {
    let mailbox = MainAsrMailbox::default();
    mailbox.submit_opening(opening_request(1));
    mailbox.submit_opening(opening_request(2));

    let MainAsrJob::Opening(request) = mailbox.recv() else {
        panic!("expected opening command");
    };
    assert_eq!(request.audio, vec![2.0]);
}

#[test]
fn opening_and_kws_are_exclusive_through_replacement_and_result_drain() {
    let exclusivity = Arc::new(ProbeLaneExclusivity::default());
    let opening = MainAsrMailbox {
        state: Mutex::default(),
        ready: parking_lot::Condvar::new(),
        exclusivity: Arc::clone(&exclusivity),
    };
    let probe = ProbeMailbox {
        state: Mutex::default(),
        ready: parking_lot::Condvar::new(),
        exclusivity,
    };

    assert!(opening.submit_opening(opening_request(1)));
    assert!(opening.submit_opening(opening_request(2)));
    assert!(!probe.submit_latest(request(1)));
    let MainAsrJob::Opening(_) = opening.recv() else {
        panic!("expected opening job");
    };
    opening.exclusivity.opening_finished();
    opening.exclusivity.opening_finished();
    assert!(!probe.submit_latest(request(2)));
    opening.exclusivity.opening_result_drained();
    assert!(!probe.submit_latest(request(3)));
    opening.exclusivity.opening_result_drained();
    assert!(probe.submit_latest(request(4)));
}

#[test]
fn kws_result_blocks_late_opening_until_result_dispatch_drains() {
    let exclusivity = Arc::new(ProbeLaneExclusivity::default());
    let opening = MainAsrMailbox {
        state: Mutex::default(),
        ready: parking_lot::Condvar::new(),
        exclusivity: Arc::clone(&exclusivity),
    };
    let probe = ProbeMailbox {
        state: Mutex::default(),
        ready: parking_lot::Condvar::new(),
        exclusivity,
    };
    assert!(probe.submit_latest(request(1)));
    let ProbeJob::Probe(_) = probe.recv() else {
        panic!("expected KWS job");
    };
    probe.exclusivity.probe_finished();
    probe.exclusivity.probe_finished();
    assert!(!opening.submit_opening(opening_request(2)));
    probe.exclusivity.probe_result_drained();
    assert!(!opening.submit_opening(opening_request(3)));
    probe.exclusivity.probe_result_drained();
    assert!(opening.submit_opening(opening_request(4)));
}

#[test]
fn idle_wake_mailbox_merges_contiguous_arrivals_without_dropping_pcm() {
    let mailbox = ProbeMailbox::default();
    mailbox.submit_idle_wake(IdleWakeRequest {
        audio: vec![1.0, 2.0],
        start_sample: 0,
        total_samples: 2,
    });
    mailbox.submit_idle_wake(IdleWakeRequest {
        audio: vec![3.0, 4.0],
        start_sample: 2,
        total_samples: 4,
    });

    let ProbeJob::IdleWake(request) = mailbox.recv() else {
        panic!("expected idle wake request");
    };
    assert_eq!(request.audio, vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(request.start_sample, 0);
    assert_eq!(request.total_samples, 4);
}

#[test]
fn opening_command_precedes_static15_background() {
    let mailbox = MainAsrMailbox::default();
    mailbox.submit_background(BackgroundRequest {
        generation: 7,
        start_sample: 0,
        end_sample: 1,
        audio: vec![1.0],
    });
    mailbox.submit_opening(opening_request(2));

    assert!(matches!(mailbox.recv(), MainAsrJob::Opening(_)));
    assert!(matches!(mailbox.recv(), MainAsrJob::Background(_)));
}

fn file_work(
    samples: usize,
) -> (
    FileWork,
    Receiver<Result<FileTranscript, String>>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let (tx, rx) = channel();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let work = FileWork::new(
        vec![0.0; samples],
        tx,
        Arc::clone(&cancel),
        Arc::new(FileProgress::default()),
    );
    (work, rx, cancel)
}

fn own_file_work(
    samples: usize,
) -> (
    FileWork,
    Receiver<Result<FileTranscript, String>>,
    Arc<std::sync::atomic::AtomicBool>,
) {
    let (tx, rx) = channel();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let progress = Arc::new(FileProgress {
        own: true,
        ..FileProgress::default()
    });
    let work = FileWork::new(vec![0.0; samples], tx, Arc::clone(&cancel), progress);
    (work, rx, cancel)
}

#[test]
fn own_file_work_is_served_before_queued_peer_work() {
    let mailbox = MainAsrMailbox::default();
    let (peer, _peer_rx, _c1) = file_work(16);
    mailbox.push_file(peer);
    let (own, _own_rx, _c2) = own_file_work(32);
    mailbox.push_file(own);

    let MainAsrJob::File(first) = mailbox.recv() else {
        panic!("expected file work");
    };
    assert!(first.progress.own, "the app's own job must go first");
    let MainAsrJob::File(second) = mailbox.recv() else {
        panic!("expected file work");
    };
    assert!(!second.progress.own);
}

#[test]
fn own_file_work_waits_behind_a_started_job_and_earlier_own_work() {
    let mailbox = MainAsrMailbox::default();
    let (started, _started_rx, _c1) = file_work(16);
    started
        .progress
        .started
        .store(true, std::sync::atomic::Ordering::Release);
    mailbox.push_file(started);
    let (peer, _peer_rx, _c2) = file_work(16);
    mailbox.push_file(peer);
    let (own_a, _a_rx, _c3) = own_file_work(1);
    mailbox.push_file(own_a);
    let (own_b, _b_rx, _c4) = own_file_work(2);
    mailbox.push_file(own_b);

    let order: Vec<(bool, bool)> = mailbox
        .state
        .lock()
        .file_works
        .iter()
        .map(|work| {
            (
                work.progress.own,
                work.progress
                    .started
                    .load(std::sync::atomic::Ordering::Acquire),
            )
        })
        .collect();
    // started peer, own A, own B (arrival order), then the unstarted peer.
    assert_eq!(
        order,
        vec![(false, true), (true, false), (true, false), (false, false)]
    );
}

fn final_request() -> FinalRequest {
    let (reply, _rx) = channel();
    FinalRequest::Dictation {
        generation: 1,
        audio: vec![0.0],
        reply,
    }
}

#[test]
fn push_file_does_not_clear_opening_or_background() {
    let mailbox = MainAsrMailbox::default();
    mailbox.submit_background(background_request(0, 2));
    assert!(mailbox.submit_opening(opening_request(1)));
    let (work, _rx, _cancel) = file_work(16);
    mailbox.push_file(work);

    let state = mailbox.state.lock();
    assert!(state.latest_opening.is_some());
    assert_eq!(state.background_requests.len(), 1);
    assert_eq!(state.file_works.len(), 1);
}

#[test]
fn recv_serves_dictation_work_before_file_steps() {
    let mailbox = MainAsrMailbox::default();
    let (work, _rx, _cancel) = file_work(16);
    mailbox.push_file(work);
    mailbox.push_final(final_request());
    // push_final clears opening/background by design, so queue them after
    // it to cover all three dictation lanes ahead of the file.
    mailbox.submit_background(background_request(0, 2));
    assert!(mailbox.submit_opening(opening_request(2)));

    assert!(matches!(mailbox.recv(), MainAsrJob::Final(_)));
    assert!(matches!(mailbox.recv(), MainAsrJob::Opening(_)));
    assert!(matches!(mailbox.recv(), MainAsrJob::Background(_)));
    assert!(matches!(mailbox.recv(), MainAsrJob::File(_)));
}

fn work_is_paused(mailbox: &MainAsrMailbox) -> bool {
    mailbox.state.lock().file_works.iter().all(|work| {
        work.progress
            .paused
            .load(std::sync::atomic::Ordering::Acquire)
    })
}

#[test]
fn file_step_is_gated_while_dictation_is_active() {
    let mailbox = Arc::new(MainAsrMailbox::default());
    mailbox.set_dictation_active(true);
    let (work, _rx, _cancel) = file_work(16);
    mailbox.push_file(work);
    assert!(work_is_paused(&mailbox));
    mailbox.submit_background(background_request(0, 2));

    let (tx, rx) = channel();
    let worker_mailbox = Arc::clone(&mailbox);
    let worker = std::thread::spawn(move || {
        for _ in 0..2 {
            let kind = match worker_mailbox.recv() {
                MainAsrJob::Background(_) => "background",
                MainAsrJob::File(_) => "file",
                _ => "other",
            };
            tx.send(kind).unwrap();
        }
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        "background"
    );
    // Only the gated file job remains: the worker must sleep, not return.
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    mailbox.set_dictation_active(false);
    assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "file");
    worker.join().unwrap();
}

#[test]
fn fail_all_file_jobs_replies_with_code_and_fails_stale_epochs() {
    let mailbox = MainAsrMailbox::default();
    let (queued, queued_rx, _c1) = file_work(16);
    mailbox.push_file(queued);
    let (mut in_flight, _in_flight_rx, _c2) = file_work(16);
    in_flight.epoch = Some(mailbox.file_fail_epoch());

    mailbox.fail_all_file_jobs("engine_shutdown");

    assert_eq!(
        queued_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err(),
        "engine_shutdown"
    );
    assert!(!mailbox.has_file_work());
    assert_eq!(
        mailbox.file_failure_for(&in_flight).as_deref(),
        Some("engine_shutdown")
    );
    let (fresh, _rx, _c3) = file_work(16);
    mailbox.push_file(fresh);
    assert!(mailbox.has_file_work());
}

#[test]
fn cancelled_queued_file_is_swept_with_cancel_error() {
    let mailbox = Arc::new(MainAsrMailbox::default());
    mailbox.set_dictation_active(true);
    let (work, rx, cancel) = file_work(16);
    mailbox.push_file(work);
    cancel.store(true, std::sync::atomic::Ordering::Release);
    // With nothing else queued, recv sweeps the cancelled job and then
    // sleeps; Shutdown releases it.
    let worker_mailbox = Arc::clone(&mailbox);
    let worker = std::thread::spawn(move || {
        assert!(matches!(
            worker_mailbox.recv(),
            MainAsrJob::Control(MainAsrControl::Shutdown)
        ));
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err(),
        FILE_CANCELLED
    );
    mailbox.push_control(MainAsrControl::Shutdown);
    worker.join().unwrap();
}

#[test]
fn file_reply_wait_times_out_only_without_progress_or_pause() {
    let (_tx, rx) = channel::<Result<FileTranscript, String>>();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let progress = FileProgress::default();
    progress
        .paused
        .store(true, std::sync::atomic::Ordering::Release);
    let (still_waiting, result) = std::thread::scope(|scope| {
        let (rx, cancel, progress) = (rx, &cancel, &progress);
        let handle = scope
            .spawn(move || wait_for_file_reply(&rx, cancel, progress, Duration::from_millis(150)));
        std::thread::sleep(Duration::from_millis(500));
        let still_waiting = !handle.is_finished();
        progress
            .paused
            .store(false, std::sync::atomic::Ordering::Release);
        (still_waiting, handle.join().unwrap())
    });
    assert!(still_waiting, "paused job must not time out");
    assert_eq!(
        result.unwrap_err(),
        "file_queue_stall_timeout: no decode progress in 600 s"
    );
}

#[test]
fn failed_background_window_retries_before_later_window_can_commit() {
    let mut scheduled = ScheduledStatic15::default();
    let first = background_request(0, 2);
    assert!(commit_background_window(&mut scheduled, &first, |_| Err("blank".into())).is_err());
    assert_eq!(scheduled.committed_samples(), 0);

    // The capture worker holds this request after a failure. Recover it
    // before accepting window two, preserving a contiguous final tail.
    let mut deferred = Some(first);
    let retry = deferred.take().expect("failed window retained");
    commit_background_window(&mut scheduled, &retry, |_| {
        Ok(TranscriptionResult {
            text: "first".into(),
            tokens: Vec::new(),
        })
    })
    .expect("retry commits first window");
    let second = background_request(2, 4);
    commit_background_window(&mut scheduled, &second, |_| {
        Ok(TranscriptionResult {
            text: "second".into(),
            tokens: Vec::new(),
        })
    })
    .expect("later window commits after recovery");

    assert_eq!(scheduled.committed_samples(), 4);
    assert_eq!(scheduled.committed_text_counts().1, 2);
}

#[test]
fn opening_command_is_cleared_by_final_and_cancel() {
    let mailbox = MainAsrMailbox::default();
    mailbox.submit_opening(opening_request(1));
    let (reply, _rx) = channel();
    mailbox.push_final(FinalRequest::Dictation {
        generation: 7,
        audio: vec![2.0],
        reply,
    });
    assert!(matches!(mailbox.recv(), MainAsrJob::Final(_)));
    assert!(mailbox.state.lock().latest_opening.is_none());

    mailbox.submit_opening(opening_request(3));
    mailbox.push_control(MainAsrControl::Cancel { generation: 7 });
    assert!(matches!(
        mailbox.recv(),
        MainAsrJob::Control(MainAsrControl::Cancel { .. })
    ));
    assert!(mailbox.state.lock().latest_opening.is_none());
}

#[test]
fn latest_probe_replaces_older_pending_audio() {
    let mailbox = ProbeMailbox::default();
    mailbox.submit_latest(request(1));
    mailbox.submit_latest(request(2));

    let ProbeJob::Probe(probe) = mailbox.recv() else {
        panic!("expected probe");
    };
    assert_eq!(probe.audio, vec![2.0]);
}

#[test]
fn fresh_confirmation_fallback_requires_full_lane_and_armed_prefix() {
    assert!(should_use_fresh_confirmation_fallback(
        "zephyr_tail_full_async",
        true
    ));
    assert!(!should_use_fresh_confirmation_fallback(
        "zephyr_tail_async",
        true
    ));
    assert!(!should_use_fresh_confirmation_fallback(
        "zephyr_tail_full_async",
        false
    ));
}

#[test]
fn virtual_decode_barrier_preserves_latest_pending_probe() {
    let controller = ReplayCaptureController::default();
    controller.load(vec![0.0; 640]);
    let mut capture = ReplayCapture {
        controller: controller.clone(),
    };
    capture.resume().unwrap();
    let mailbox = ProbeMailbox::default();
    mailbox.submit_latest(request(1));
    let ProbeJob::Probe(in_flight) = mailbox.recv() else {
        panic!("expected in-flight probe");
    };

    controller.mark_lane_in_flight(worker_clock::ExecutorLane::Kws, in_flight.submitted_at);
    controller.schedule_lane_completion(LaneCompletion {
        lane: worker_clock::ExecutorLane::Kws,
        submitted_at: in_flight.submitted_at,
        measured_decode_duration: Duration::from_millis(10),
    });
    mailbox.submit_latest(request(2));
    mailbox.submit_latest(request(3));

    assert_eq!(capture.read_f32(320).unwrap().len(), 320);
    assert!(capture.read_f32(320).unwrap().is_empty());
    assert!(
        controller.lane_completion_ready(worker_clock::ExecutorLane::Kws, in_flight.submitted_at,)
    );
    let ProbeJob::Probe(next) = mailbox.recv() else {
        panic!("expected pending probe after virtual completion");
    };
    assert_eq!(next.probe_sequence, 3);
    assert_eq!(capture.read_f32(320).unwrap().len(), 320);
}

#[test]
fn lifecycle_control_preempts_pending_probe() {
    let mailbox = ProbeMailbox::default();
    mailbox.submit_latest(request(1));
    let (reply, _rx) = channel();
    mailbox.push_control(ProbeControl::Reload { reply });

    assert!(matches!(
        mailbox.recv(),
        ProbeJob::Control(ProbeControl::Reload { .. })
    ));
    assert!(matches!(mailbox.recv(), ProbeJob::Probe(_)));
}

#[test]
fn final_request_clears_pending_background_and_dequeues_first() {
    let mailbox = MainAsrMailbox::default();
    mailbox.submit_background(BackgroundRequest {
        generation: 7,
        start_sample: 0,
        end_sample: 1,
        audio: vec![1.0],
    });
    let (reply, _rx) = channel();
    mailbox.push_final(FinalRequest::Dictation {
        generation: 7,
        audio: vec![2.0],
        reply,
    });

    assert!(matches!(
        mailbox.recv(),
        MainAsrJob::Final(FinalRequest::Dictation { .. })
    ));
    assert!(mailbox.state.lock().background_requests.is_empty());
}

#[test]
fn generation_and_session_reject_stale_results() {
    let result = ProbeResult {
        request: request(1),
        transcript: Ok("zephyr stop".into()),
        probe_ms: 10,
        queue_wait_ms: 0,
        completed_at: Instant::now(),
        measured_decode_duration: Duration::from_millis(10),
        native_origin_sample: 0,
        native_control_intent: None,
        trigger_onset_sample: None,
        trigger_onset_skip_reason: None,
    };
    assert!(probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    ));
    assert!(!probe_result_is_current(
        &result,
        8,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    ));
    assert!(!probe_result_is_current(
        &result,
        7,
        Some("session-8"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    ));
    assert!(!probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        4,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    ));

    let opening = OpeningCommandResult {
        request: opening_request(1),
        transcript: Ok("open safari".into()),
        decode_ms: 10,
        measured_decode_duration: Duration::from_millis(10),
    };
    assert!(opening_result_is_current(&opening, 7, Some("session-7")));
    assert!(!opening_result_is_current(&opening, 8, Some("session-7")));
    assert!(!opening_result_is_current(&opening, 7, Some("session-8")));

    assert_eq!(
        result.lane_completion(),
        LaneCompletion {
            lane: worker_clock::ExecutorLane::Kws,
            submitted_at: worker_clock::WorkerInstant::ZERO,
            measured_decode_duration: Duration::from_millis(10),
        }
    );
    assert_eq!(
        opening.lane_completion(),
        LaneCompletion {
            lane: worker_clock::ExecutorLane::MainAsr,
            submitted_at: worker_clock::WorkerInstant::ZERO,
            measured_decode_duration: Duration::from_millis(10),
        }
    );

    let mut schedule = worker_clock::ReplayEventSchedule::default();
    result.lane_completion().schedule_on(&mut schedule);
    assert_eq!(
        schedule.next_event(),
        Some(worker_clock::ReplayEvent {
            at: worker_clock::WorkerInstant::ZERO.saturating_add(Duration::from_millis(10)),
            kind: worker_clock::ReplayEventKind::LaneCompletion(worker_clock::ExecutorLane::Kws,),
        })
    );
}

#[test]
fn probe_staleness_uses_worker_decision_time() {
    let result = ProbeResult {
        request: request(1),
        transcript: Ok("zephyr stop".into()),
        probe_ms: 10,
        queue_wait_ms: 0,
        completed_at: Instant::now(),
        measured_decode_duration: Duration::from_millis(10),
        native_origin_sample: 0,
        native_control_intent: None,
        trigger_onset_sample: None,
        trigger_onset_skip_reason: None,
    };
    let virtual_now =
        worker_clock::WorkerInstant::ZERO.saturating_add(Duration::from_millis(2_001));
    assert!(!probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        virtual_now,
    ));
    assert!(probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO.saturating_add(Duration::from_secs(2)),
    ));
    assert!(!probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO
            .saturating_add(Duration::from_secs(2))
            .saturating_add(Duration::from_nanos(1)),
    ));
}

#[test]
fn stale_empty_result_preserves_stream_for_fresh_complete_result() {
    let mut stale = ProbeResult {
        request: request(1),
        transcript: Ok("zephyr".into()),
        probe_ms: 10,
        queue_wait_ms: 0,
        completed_at: Instant::now(),
        measured_decode_duration: Duration::from_millis(10),
        native_origin_sample: 0,
        native_control_intent: None,
        trigger_onset_sample: None,
        trigger_onset_skip_reason: None,
    };
    let stale_decision = probe_result_decision(
        &stale,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO.saturating_add(Duration::from_millis(2_001)),
    );
    assert_eq!(stale_decision.reason, Some(ProbeStaleReason::Age));
    assert!(!stale_decision.reset_stream);

    stale.transcript = Ok("zephyr stop".into());
    stale.native_control_intent = Some(rightkit_text_polish::text_pipeline::ControlIntent::Stop);
    let fresh_decision = probe_result_decision(
        &stale,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    );
    assert!(fresh_decision.current);
    assert!(!fresh_decision.reset_stream);
}

#[test]
fn stale_complete_resets_once_and_retired_results_cannot_cascade() {
    let mut result = ProbeResult {
        request: request(1),
        transcript: Ok("zephyr stop".into()),
        probe_ms: 10,
        queue_wait_ms: 0,
        completed_at: Instant::now(),
        measured_decode_duration: Duration::from_millis(10),
        native_origin_sample: 0,
        native_control_intent: Some(rightkit_text_polish::text_pipeline::ControlIntent::Stop),
        trigger_onset_sample: None,
        trigger_onset_skip_reason: None,
    };
    let expired = probe_result_decision(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO.saturating_add(Duration::from_millis(2_001)),
    );
    assert_eq!(expired.reason, Some(ProbeStaleReason::Age));
    assert!(expired.reset_stream);

    result.request.kws_stream_generation = 2;
    let retired_stream = probe_result_decision(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO.saturating_add(Duration::from_millis(2_001)),
    );
    assert_eq!(
        retired_stream.reason,
        Some(ProbeStaleReason::KwsStreamGeneration)
    );
    assert!(!retired_stream.reset_stream);

    result.request.generation = 6;
    let retired_recording = probe_result_decision(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    );
    assert_eq!(
        retired_recording.reason,
        Some(ProbeStaleReason::RecordingGeneration)
    );
    assert!(!retired_recording.reset_stream);
}

#[test]
fn current_origin_mismatch_resets_but_session_and_lag_rejections_do_not() {
    let mut result = ProbeResult {
        request: request(1),
        transcript: Ok("zephyr".into()),
        probe_ms: 10,
        queue_wait_ms: 0,
        completed_at: Instant::now(),
        measured_decode_duration: Duration::from_millis(10),
        native_origin_sample: 1,
        native_control_intent: None,
        trigger_onset_sample: None,
        trigger_onset_skip_reason: None,
    };
    let origin = probe_result_decision(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    );
    assert_eq!(origin.reason, Some(ProbeStaleReason::NativeOrigin));
    assert!(origin.reset_stream);

    result.native_origin_sample = 0;
    let lag = probe_result_decision(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        1 + SAMPLE_RATE as usize * 2 + 1,
        worker_clock::WorkerInstant::ZERO,
    );
    assert_eq!(lag.reason, Some(ProbeStaleReason::SampleLag));
    assert!(!lag.reset_stream);

    let session = probe_result_decision(
        &result,
        7,
        Some("other-session"),
        3,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    );
    assert_eq!(session.reason, Some(ProbeStaleReason::Session));
    assert!(!session.reset_stream);
}

#[test]
fn rolling_snapshot_feeds_only_audio_after_last_processed_sample() {
    assert_eq!(
        incremental_probe_offset(160_000, 40_000, 124_000),
        Some(120_000)
    );
    assert_eq!(incremental_probe_offset(160_000, 164_000, 8_000), None);
    assert_eq!(
        incremental_probe_offset(160_000, 40_000, 100_000),
        Some(100_000)
    );
    assert_eq!(probe_reset_stream_base(40_000, 164_000, None), 164_000);
    assert_eq!(
        probe_reset_stream_base(40_000, 40_000, Some(120_000)),
        40_000
    );
}

#[test]
fn dead_probe_worker_is_reported_instead_of_hanging_reload() {
    let main_mailbox = Arc::new(MainAsrMailbox::default());
    let probe_mailbox = Arc::new(ProbeMailbox::default());
    let (result_tx, result_rx) = channel();
    drop(result_tx);
    let (idle_wake_result_tx, idle_wake_result_rx) = channel();
    drop(idle_wake_result_tx);
    let (opening_result_tx, opening_result_rx) = channel();
    drop(opening_result_tx);
    let (stream_result_tx, stream_result_rx) = channel();
    drop(stream_result_tx);
    let (reload_tx, _reload_rx) = channel();
    let (_reload_result_tx, reload_result_rx) = channel();
    let lane = AsrExecutor {
        main_mailbox,
        probe_mailbox,
        result_rx,
        idle_wake_result_rx,
        opening_result_rx,
        stream_result_rx,
        main_alive: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        probe_alive: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        scheduled_static15_enabled: false,
        control_cleanup_supported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        reload_tx,
        reload_result_rx,
        reload_next_id: std::sync::atomic::AtomicU64::new(1),
        reload_lock: Arc::new(std::sync::Mutex::new(())),
    };

    assert!(lane.reload_model().unwrap_err().contains("not running"));
    assert!(lane.try_recv().unwrap_err().contains("disconnected"));
}

/// Opt-in production-entrypoint smoke. Set model/KWS/WAV paths when
/// running ignored tests against a native macOS TDT bundle; this keeps
/// model loading out of ordinary unit-test runs.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires native TDT + KWS model directories and a 16 kHz mono WAV"]
fn confirmed_control_executor_native_entrypoint_smoke() {
    // Fixture schema: model_dir (models parent), coreml_model_dir, kws_dir,
    // wav, onset, expected_text, expected_method. RightKit strips arbitrary
    // request environment, so only model-selection vars below are set.
    let fixture_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.audit/kws-control-smoke.json");
    let fixture_raw = std::fs::read_to_string(&fixture_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", fixture_path.display()));
    let fixture: serde_json::Value =
        serde_json::from_str(&fixture_raw).expect("parse control smoke fixture");
    let field = |name: &str| {
        fixture
            .get(name)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("fixture field {name} must be a string"))
    };
    let path_field = |name: &str| {
        let path = std::path::PathBuf::from(field(name));
        if path.is_absolute() {
            path
        } else {
            fixture_path.parent().expect("fixture parent").join(path)
        }
    };
    let model_dir = path_field("model_dir");
    let coreml_model_dir = path_field("coreml_model_dir");
    let kws_dir = path_field("kws_dir");
    let wav_path = path_field("wav");
    let expected_text = field("expected_text").to_string();
    let expected_method = field("expected_method").to_string();
    let onset_sample = fixture
        .get("onset")
        .and_then(serde_json::Value::as_u64)
        .expect("fixture field onset must be an integer") as usize;
    std::env::set_var("RIGHTKIT_ASR_BACKEND", "parakeet-tdt");
    std::env::set_var("RIGHTKIT_COREML_MODEL_DIR", &coreml_model_dir);
    std::env::set_var("RIGHTKIT_KWS_DIR", &kws_dir);
    let wav = rightkit_audio::wav::read_wav_mono_f32(&wav_path).expect("read control WAV");
    assert_eq!(wav.sample_rate, SAMPLE_RATE);
    let audio: Vec<f32> = wav.samples;
    let executor = AsrExecutor::spawn(model_dir, AsrEp::Cpu).expect("spawn native ASR executor");
    assert!(executor.supports_control_cleanup());
    let outcome = executor
        .transcribe_final_control_result(
            1,
            audio,
            onset_sample,
            rightkit_text_polish::text_pipeline::ControlIntent::Send,
            &std::sync::atomic::AtomicU64::new(0),
            0,
        )
        .expect("confirmed control production entrypoint");
    assert_ne!(outcome.method, "legacy_cut");
    assert!(
        outcome.error.is_none(),
        "cleanup error: {:?}",
        outcome.error
    );
    let normalize = |text: &str| {
        text.split_whitespace()
            .map(|word| {
                word.chars()
                    .filter(|ch| ch.is_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>()
            })
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        normalize(&outcome.transcript.text),
        normalize(&expected_text)
    );
    assert_eq!(outcome.method, expected_method);
    executor.shutdown();
    // Shutdown enqueues controls. Keep the test process alive until model
    // owners have dropped native runtimes; exiting first races C++ teardown.
    let deadline = Instant::now() + Duration::from_secs(10);
    while executor
        .main_alive
        .load(std::sync::atomic::Ordering::Acquire)
        || executor
            .probe_alive
            .load(std::sync::atomic::Ordering::Acquire)
    {
        assert!(
            Instant::now() < deadline,
            "native ASR owners did not shut down"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn probe_worker_failure_is_reported_only_once() {
    let mut failed = false;
    assert!(latch_probe_lane_failure(&mut failed));
    assert!(!latch_probe_lane_failure(&mut failed));
}

#[test]
fn probe_lifecycle_is_session_correlated_and_content_free() {
    let request = request(3);
    let payload = probe_lifecycle_payload(&request, "queued", None, None);
    assert_eq!(payload["session_id"], "session-7");
    assert_eq!(payload["generation"], 7);
    assert_eq!(payload["total_samples"], 1);
    assert_eq!(payload["phase"], "queued");
    assert!(payload.get("recognized_text").is_none());
    assert!(payload.get("transcript").is_none());
}

#[test]
fn routine_probe_lifecycle_is_sampled_but_anomalies_are_not() {
    let routine = request(3);
    assert!(!should_trace_probe_lifecycle(
        &routine,
        "inference_finished",
        Some(true)
    ));
    assert!(should_trace_probe_lifecycle(
        &routine,
        "inference_finished",
        Some(false)
    ));
    assert!(should_trace_probe_lifecycle(
        &routine,
        "pending_replaced",
        None
    ));
    assert!(should_trace_probe_lifecycle(
        &request(1),
        "inference_finished",
        Some(true)
    ));
}

// --- trigger onset location + audio-cut arithmetic (2026-07-27 fix) ---

fn request_at(command_start: usize) -> ProbeRequest {
    let mut req = request(1);
    req.command_start = command_start;
    req.stream_base_sample = command_start;
    req
}

fn tok(text: &str, start: f32, end: f32) -> rightkit_dictation_inference::TimedToken {
    rightkit_dictation_inference::TimedToken {
        text: text.to_string(),
        start,
        end,
    }
}

#[test]
fn finds_trigger_onset_and_offsets_by_command_start() {
    // " please zephyr stop" split into pieces the way both decoders emit
    // them: a leading-space piece opens each new word.
    let tokens = vec![
        tok(" please", 0.0, 0.4),
        tok(" zephyr", 0.48, 0.9),
        tok(" stop", 0.96, 1.2),
    ];
    let request = request_at(16_000); // 1.0s into the full recording buffer
    let onset = find_trigger_onset_sample(request.command_start, &tokens);
    // 0.48s @ 16kHz = 7680 samples, offset by command_start.
    assert_eq!(onset, Some(16_000 + 7_680));
}

#[test]
fn explicit_acoustic_wake_start_wins_over_late_confirmation_tokens() {
    let tokens = vec![tok(" zephyr", 1.4, 1.8), tok(" stop", 1.9, 2.1)];
    assert_eq!(
        resolve_trigger_onset_sample(Some(24_000), 16_000, &tokens),
        Some(24_000)
    );
}

#[test]
fn no_trigger_shaped_word_returns_none() {
    let tokens = vec![tok(" please", 0.0, 0.4), tok(" stop", 0.48, 0.9)];
    let request = request_at(0);
    assert_eq!(
        find_trigger_onset_sample(request.command_start, &tokens),
        None
    );
}

#[test]
fn empty_tokens_return_none() {
    let request = request_at(0);
    assert_eq!(find_trigger_onset_sample(request.command_start, &[]), None);
}

#[test]
fn picks_the_last_trigger_shaped_word_not_the_first() {
    // "zephyr finds zephyr stop" — only the SECOND "zephyr" is the real
    // trigger (anchored right before the verb); an earlier incidental
    // occurrence must not win.
    let tokens = vec![
        tok(" zephyr", 0.0, 0.4),
        tok(" finds", 0.48, 0.9),
        tok(" zephyr", 0.96, 1.4),
        tok(" stop", 1.44, 1.7),
    ];
    let request = request_at(0);
    let onset = find_trigger_onset_sample(request.command_start, &tokens);
    assert_eq!(onset, Some((0.96 * SAMPLE_RATE as f32).round() as usize));
}

#[test]
fn multi_piece_word_uses_first_piece_start() {
    // A trigger word split across subword pieces ("▁ze" + "pher", no
    // leading space on the continuation piece) must still be timed from
    // its FIRST piece, not its last.
    let tokens = vec![tok(" ze", 0.5, 0.6), tok("pher", 0.6, 0.9)];
    let request = request_at(0);
    let onset = find_trigger_onset_sample(request.command_start, &tokens);
    assert_eq!(onset, Some((0.5 * SAMPLE_RATE as f32).round() as usize));
}

#[test]
fn terminal_cut_uses_confident_static15_seam() {
    let onset = 72_000;
    let pause = 60_000..71_700;
    let mut pcm = vec![0.4; 80_000];
    pcm[pause.clone()].fill(0.0);
    pcm[onset..].fill(0.9);
    let cut = resolve_trigger_audio_cut(Some(onset), None, Some(1_000), &pcm);
    let TriggerAudioCut::Cut {
        onset_sample,
        cut_at,
        cut_source,
        ..
    } = cut
    else {
        panic!("expected confident terminal seam");
    };
    assert_eq!(onset_sample, onset);
    assert_eq!(cut_source, "static15_seam");
    assert!(pause.contains(&cut_at));
    assert!(pause.end < onset);
    assert!(pcm[..onset].iter().all(|sample| *sample != 0.9));
}

#[test]
fn terminal_cut_without_a_pause_still_cuts_at_the_kws_onset() {
    // Continuous speech into the command: no seam, so the command audio is
    // removed at the spotter's onset and never reaches the recognizer.
    let onset = 70_000;
    let mut pcm = vec![0.3; 80_000];
    pcm[20_000..24_000].fill(0.0);
    assert_eq!(
        resolve_trigger_audio_cut(Some(onset), None, Some(1_000), &pcm),
        TriggerAudioCut::Cut {
            onset_sample: onset,
            cut_at: onset,
            cut_source: rightkit_dictation_inference::KWS_ONSET_CUT_SOURCE,
            seam_rms: None,
            noise_floor_rms: None,
        }
    );
}

#[test]
fn terminal_cut_on_flat_audio_cuts_at_the_kws_onset() {
    let pcm = vec![0.3; 40_000];
    assert_eq!(
        resolve_trigger_audio_cut(Some(40_000), None, Some(1_000), &pcm),
        TriggerAudioCut::Cut {
            onset_sample: 40_000,
            cut_at: 40_000,
            cut_source: rightkit_dictation_inference::KWS_ONSET_CUT_SOURCE,
            seam_rms: None,
            noise_floor_rms: None,
        }
    );
}

#[test]
fn a_command_with_no_dictation_before_it_keeps_the_full_buffer() {
    let pcm = vec![0.3; 40_000];
    assert_eq!(
        resolve_trigger_audio_cut(Some(1_000), None, Some(1_000), &pcm),
        TriggerAudioCut::FullBuffer {
            reason: "no_precommand_speech"
        }
    );
}

#[test]
fn missing_or_implausible_onset_uses_typed_full_buffer_fallback() {
    let pcm = vec![0.0; 30_000];
    assert!(matches!(
        resolve_trigger_audio_cut(None, None, Some(1_000), &pcm),
        TriggerAudioCut::FullBuffer {
            reason: "no_timestamps"
        }
    ));
    assert!(matches!(
        resolve_trigger_audio_cut(Some(40_000), None, Some(1_000), &pcm),
        TriggerAudioCut::FullBuffer {
            reason: "implausible"
        }
    ));
    assert!(matches!(
        resolve_trigger_audio_cut(Some(0), None, None, &pcm),
        TriggerAudioCut::FullBuffer {
            reason: "implausible"
        }
    ));
}

#[test]
fn current_result_requires_generation_snapshot_origin_and_live_lag() {
    let mut result = ProbeResult {
        request: request(1),
        transcript: Ok("zephyr stop".into()),
        probe_ms: 10,
        queue_wait_ms: 0,
        completed_at: Instant::now(),
        measured_decode_duration: Duration::from_millis(10),
        native_origin_sample: 0,
        native_control_intent: None,
        trigger_onset_sample: Some(100),
        trigger_onset_skip_reason: None,
    };
    assert!(probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        32_000,
        worker_clock::WorkerInstant::ZERO,
    ));
    assert!(!probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        2,
        Some(0),
        1,
        worker_clock::WorkerInstant::ZERO,
    ));
    // Fresh snapshots legitimately move scheduler origin while an older
    // result is in flight. Result must match its own snapshot start; latest
    // scheduler origin is not command authority.
    assert!(probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        3,
        Some(1),
        1,
        worker_clock::WorkerInstant::ZERO,
    ));
    result.native_origin_sample = 1;
    assert!(!probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        3,
        Some(1),
        1,
        worker_clock::WorkerInstant::ZERO,
    ));
    result.native_origin_sample = 0;
    assert!(!probe_result_is_current(
        &result,
        7,
        Some("session-7"),
        3,
        Some(0),
        SAMPLE_RATE as usize * 2 + 2,
        worker_clock::WorkerInstant::ZERO,
    ));
}

#[test]
fn native_graph_result_maps_only_canonical_typed_intents() {
    use rightkit_text_polish::text_pipeline::ControlIntent;
    assert_eq!(
        native_control_intent("zephyr send"),
        Some(ControlIntent::Send)
    );
    assert_eq!(native_control_intent("zephyr zipper"), None);
}

#[test]
fn reset_origin_is_first_sample_actually_fed_not_old_stream_base() {
    assert_eq!(probe_reset_stream_base(6_559, 32_000, Some(4_000)), 6_559);
    // A recovery snapshot forces a reset; timestamp origin is snapshot
    // start, never stale requested stream base.
    assert_eq!(probe_reset_stream_base(6_559, 32_000, None), 32_000);
}
