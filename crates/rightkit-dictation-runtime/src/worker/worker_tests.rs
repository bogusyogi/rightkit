#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{
    capture_recovery::*, probe_decisions::*, transcript_text::*, unified_probe_lane::*,
    worker_api::*, worker_clock::*, worker_main::*,
};
#[allow(unused_imports)]
use super::{replay_driver::*, replay_effect_sink::*, wav_replay_source::*};

#[cfg(target_os = "macos")]
#[path = "worker_service_file_preemption_replay_integration_tests.rs"]
mod worker_service_file_preemption_replay_integration_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_handle_epoch_only_interrupts_terminal_commands() {
        let (cmd_tx, cmd_rx) = channel();
        let (_event_tx, event_rx) = channel();
        let handle = WorkerHandle {
            cmd_tx: Some(cmd_tx),
            event_rx: Arc::new(Mutex::new(event_rx)),
            cancel_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            wait_signal: Arc::new(WorkerWaitSignal::default()),
            file_jobs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };

        handle.send(WorkerCmd::ReloadModel { reply: None }).unwrap();
        handle
            .send(WorkerCmd::StartRecording {
                session_id: "epoch-test".into(),
            })
            .unwrap();
        assert_eq!(
            handle
                .cancel_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            0
        );
        handle.send(WorkerCmd::Cancel).unwrap();
        assert_eq!(
            handle
                .cancel_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            1
        );
        handle.send(WorkerCmd::Shutdown).unwrap();

        assert_eq!(
            handle
                .cancel_epoch
                .load(std::sync::atomic::Ordering::Acquire),
            2
        );
        assert!(matches!(
            cmd_rx.recv_timeout(Duration::from_secs(1)),
            Ok(WorkerCmd::ReloadModel { reply: None })
        ));
        assert!(matches!(
            cmd_rx.recv_timeout(Duration::from_secs(1)),
            Ok(WorkerCmd::StartRecording { session_id }) if session_id == "epoch-test"
        ));
        assert!(matches!(
            cmd_rx.recv_timeout(Duration::from_secs(1)),
            Ok(WorkerCmd::Cancel)
        ));
        assert!(matches!(
            cmd_rx.recv_timeout(Duration::from_secs(1)),
            Ok(WorkerCmd::Shutdown)
        ));
    }

    #[test]
    fn idle_wait_wakes_for_commands_internal_work_and_disconnect() {
        let signal = Arc::new(WorkerWaitSignal::default());
        let (cmd_tx, cmd_rx) = channel();
        let signal_for_wait = Arc::clone(&signal);
        let waiting =
            std::thread::spawn(move || Worker::wait_for_idle_command(&cmd_rx, &signal_for_wait, 0));
        cmd_tx.send(WorkerCmd::Shutdown).unwrap();
        signal.notify();
        assert!(matches!(waiting.join().unwrap(), Ok(WorkerCmd::Shutdown)));

        let signal = Arc::new(WorkerWaitSignal::default());
        let (cmd_tx, cmd_rx) = channel();
        let signal_for_wait = Arc::clone(&signal);
        let observed = signal.snapshot();
        let waiting = std::thread::spawn(move || {
            Worker::wait_for_idle_command(&cmd_rx, &signal_for_wait, observed)
        });
        signal.notify();
        assert!(matches!(
            waiting.join().unwrap(),
            Err(RecvTimeoutError::Timeout)
        ));
        drop(cmd_tx);

        let (cmd_tx, cmd_rx) = channel();
        let (_event_tx, event_rx) = channel();
        let handle = WorkerHandle {
            cmd_tx: Some(cmd_tx),
            event_rx: Arc::new(Mutex::new(event_rx)),
            cancel_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            wait_signal: Arc::new(WorkerWaitSignal::default()),
            file_jobs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        let signal_for_wait = Arc::clone(&handle.wait_signal);
        let observed = handle.wait_signal.snapshot();
        let waiting = std::thread::spawn(move || {
            Worker::wait_for_idle_command(&cmd_rx, &signal_for_wait, observed)
        });
        drop(handle);
        assert!(matches!(
            waiting.join().unwrap(),
            Err(RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn idle_wait_does_not_miss_result_arriving_before_sleep() {
        let signal = WorkerWaitSignal::default();
        let (cmd_tx, cmd_rx) = channel();
        signal.notify();
        assert!(matches!(
            Worker::wait_for_idle_command(&cmd_rx, &signal, 0),
            Err(RecvTimeoutError::Timeout)
        ));
        drop(cmd_tx);
    }

    #[test]
    fn reload_failure_is_owned_by_matching_start_session() {
        assert!(reload_failure_applies(
            true,
            Some(7),
            Some("session-7"),
            7,
            Some("session-7"),
        ));
        assert!(!reload_failure_applies(
            true,
            Some(6),
            Some("session-6"),
            7,
            Some("session-7"),
        ));
        assert!(!reload_failure_applies(
            true,
            Some(7),
            Some("session-old"),
            7,
            Some("session-7"),
        ));
        assert!(!reload_failure_applies(
            false,
            Some(7),
            Some("session-7"),
            7,
            Some("session-7"),
        ));
    }

    #[test]
    fn capture_zero_guard_allows_startup_zeros_and_quiet_audio() {
        let mut guard = CaptureZeroGuard::default();
        guard.observe(&vec![0.0; CAPTURE_ZERO_GRACE_SAMPLES - 1]);
        assert!(!guard.needs_recovery());
        // Even very quiet finite audio is useful; this is not a VAD threshold.
        guard.observe(&[1.0e-12]);
        guard.observe(&vec![0.0; CAPTURE_ZERO_GRACE_SAMPLES - 1]);
        assert!(!guard.needs_recovery());
        guard.observe(&[0.0]);
        assert!(guard.needs_recovery());
    }

    #[test]
    fn capture_zero_guard_recovers_once_across_recording_then_fails() {
        let mut guard = CaptureZeroGuard::default();
        // A failure in the middle of a chunk must count only its zero suffix.
        let mut chunk = vec![0.03; 800];
        chunk.extend(vec![0.0; CAPTURE_ZERO_GRACE_SAMPLES]);
        for samples in chunk.chunks(137) {
            guard.observe(samples);
        }
        assert!(guard.needs_recovery());
        assert!(guard.begin_repair().is_ok());
        assert!(!guard.needs_recovery());
        guard.observe(&[0.04, -0.02]);
        assert!(!guard.needs_recovery());
        guard.observe(&vec![0.0; CAPTURE_ZERO_GRACE_SAMPLES]);
        assert_eq!(guard.begin_repair(), Err(CAPTURE_NO_AUDIO_MESSAGE));
        // A new recording receives its own bounded budget.
        assert!(CaptureZeroGuard::default().begin_repair().is_ok());
    }

    #[test]
    fn capture_zero_reopen_retains_first_syllable_and_remaining_audio() {
        let old = ReplayCaptureController::default();
        old.load(vec![0.0; 640]);
        let old_capture = WorkerCapture::Replay(ReplayCapture {
            controller: old.clone(),
        });
        old_capture.resume().unwrap();
        let fresh = ReplayCaptureController::default();
        let speech: Vec<f32> = (0..960).map(|i| (i as f32 + 1.0) / 1000.0).collect();
        fresh.load(speech.clone());
        let clock = fresh.virtual_clock();
        let mut capture = Some(old_capture);
        let mut route = None;
        let mut opens = 0;
        let mut retained = vec![0.5, -0.5];
        let seed = reopen_zero_capture_with(&mut capture, &mut route, clock.as_ref(), || {
            opens += 1;
            let cap = WorkerCapture::Replay(ReplayCapture {
                controller: fresh.clone(),
            });
            cap.resume().unwrap();
            Ok((cap, None))
        })
        .unwrap();
        assert_eq!(opens, 1);
        assert!(old.state.0.lock().unwrap().paused);
        retained.extend(seed);
        while let Ok(chunk) = capture.as_mut().unwrap().read_f32(usize::MAX) {
            if chunk.is_empty() {
                break;
            }
            retained.extend(chunk);
        }
        assert_eq!(&retained[..2], &[0.5, -0.5]);
        assert_eq!(&retained[2..2 + speech.len()], speech);
        // Replay appends a KWS confirmation tail; it must remain exact silence.
        assert_eq!(
            retained.len(),
            2 + speech.len() + crate::sherpa_kws::CONFIRM_TIMEOUT_SAMPLES
        );
        assert!(retained[2 + speech.len()..]
            .iter()
            .all(|sample| *sample == 0.0));
    }

    #[test]
    fn capture_zero_reopen_failure_drops_stale_stream_without_retry_loop() {
        let source = ReplayCaptureController::default();
        source.load(vec![0.0; 640]);
        let clock = source.virtual_clock();
        let old = WorkerCapture::Replay(ReplayCapture { controller: source });
        old.resume().unwrap();
        let mut capture = Some(old);
        let mut opens = 0;
        let result = reopen_zero_capture_with(&mut capture, &mut None, clock.as_ref(), || {
            opens += 1;
            Err("device unavailable".into())
        });
        assert_eq!(opens, 1);
        assert_eq!(result.unwrap_err(), "device unavailable");
        assert!(capture.is_none());
    }

    #[test]
    fn wake_diagnostics_are_numeric_only_and_identify_a_live_audio_kws_path() {
        let mut diagnostics = WakeDiagnostics::new(0.25, true);
        diagnostics.observe_audio(&[0.25, -0.25, 0.0, 0.0]);
        diagnostics.observe_decode(7);
        let value = diagnostics.payload("observing", "no_match", "info", "wake_observing");
        assert_eq!(value["event"], "wake_diagnostic");
        assert_eq!(value["mic_opened"], true);
        assert_eq!(value["audio_samples"], 4);
        assert_eq!(value["kws_decode_attempts"], 1);
        assert_eq!(value["score_available"], false);
        assert!(value.get("audio").is_none());
        assert!(value.get("transcript").is_none());
    }

    #[test]
    fn quiet_final_audio_still_reaches_decoder() {
        let audio = vec![0.001; SAMPLE_RATE as usize];
        let mut calls = 0;
        let result = transcribe_conditioned(&audio, BlankPolicy::Final, |_| {
            calls += 1;
            Ok("decoded".to_string())
        });
        assert_eq!(result, Ok("decoded".to_string()));
        assert_eq!(calls, 1);
    }

    #[test]
    fn blank_final_retries_same_route_and_recovers() {
        let audio = vec![0.001; SAMPLE_RATE as usize];
        let mut calls = 0;
        let result = transcribe_conditioned(&audio, BlankPolicy::Final, |_| {
            calls += 1;
            if calls == 1 {
                Ok(String::new())
            } else {
                Ok("recovered".to_string())
            }
        });
        assert_eq!(result, Ok("recovered".to_string()));
        assert_eq!(calls, 2);
    }

    #[test]
    fn guarded_blank_final_retries_same_route_and_recovers() {
        let audio = vec![0.001; SAMPLE_RATE as usize];
        let mut calls = 0;
        let result = transcribe_conditioned(&audio, BlankPolicy::Final, |_| {
            calls += 1;
            if calls == 1 {
                Err(crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR.to_string())
            } else {
                Ok("recovered".to_string())
            }
        });
        assert_eq!(result, Ok("recovered".to_string()));
        assert_eq!(calls, 2);
    }

    #[test]
    fn blank_final_returns_audible_error_after_one_retry() {
        let audio = vec![0.001; SAMPLE_RATE as usize];
        let mut calls = 0;
        let result = transcribe_conditioned(&audio, BlankPolicy::Final, |_| {
            calls += 1;
            Ok(String::new())
        });
        assert_eq!(
            result,
            Err(crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR.to_string())
        );
        assert_eq!(calls, 2);
    }

    #[test]
    fn guarded_blank_final_returns_audible_error_after_one_retry() {
        let audio = vec![0.001; SAMPLE_RATE as usize];
        let mut calls = 0;
        let result: Result<String, String> =
            transcribe_conditioned(&audio, BlankPolicy::Final, |_| {
                calls += 1;
                Err(crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR.to_string())
            });
        assert_eq!(
            result,
            Err(crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR.to_string())
        );
        assert_eq!(calls, 2);
    }

    #[test]
    fn silence_policy_discards_only_before_voice_then_stops_after_voice() {
        assert_eq!(
            silence_decision(false, Duration::from_millis(29_999)),
            SilenceDecision::KeepRecording
        );
        assert_eq!(
            silence_decision(false, Duration::from_millis(30_000)),
            SilenceDecision::DiscardInitialSilence
        );
        assert_eq!(
            silence_decision(true, Duration::from_millis(29_999)),
            SilenceDecision::KeepRecording
        );
        assert_eq!(
            silence_decision(true, Duration::from_millis(30_000)),
            SilenceDecision::KeepRecording
        );
        assert_eq!(
            silence_decision(true, Duration::from_millis(59_999)),
            SilenceDecision::KeepRecording
        );
        assert_eq!(
            silence_decision(true, Duration::from_millis(60_000)),
            SilenceDecision::StopAfterPostSpeechSilence
        );
    }

    #[test]
    fn capture_ingress_rejects_non_finite_pcm_before_any_policy() {
        let healthy = vec![0.0, 0.25, -0.25];
        let healthy_history = validate_capture_pcm(&healthy, None).unwrap().history();
        assert!(validate_capture_pcm(&[0.0, f32::NAN], None).is_err());
        assert!(validate_capture_pcm(&[f32::INFINITY], None).is_err());
        let constant = vec![0.0; CAPTURE_LIVENESS_MIN_SAMPLES];
        assert!(validate_capture_pcm(&constant, None).is_ok());
        let clipped = vec![1.0; CAPTURE_LIVENESS_MIN_SAMPLES];
        assert!(validate_capture_pcm(&clipped, None).is_ok());
        let constant_history = validate_capture_pcm(&constant, None).unwrap().history();
        assert!(validate_capture_pcm(&constant, Some(&constant_history)).is_ok());
        let repeated = validate_capture_pcm(&healthy, Some(&healthy_history)).unwrap();
        assert!(repeated.discarded_repeat());
        let repeated_history = repeated.history();
        assert!(validate_capture_pcm(&healthy, Some(&repeated_history)).is_err());

        let changed = vec![0.0, 0.5, -0.5];
        let changed_history = validate_capture_pcm(&changed, Some(&repeated_history))
            .unwrap()
            .history();
        assert!(validate_capture_pcm(&healthy, Some(&changed_history)).is_ok());
    }

    #[test]
    fn opening_command_rolls_after_silero_without_waiting_for_pause() {
        assert_eq!(CMD_RETRY_MS, 50);
        assert_eq!(CMD_NEW_AUDIO_PROBE_SAMPLES, SAMPLE_RATE as usize / 20);
        let now = Instant::now();
        assert!(opening_command_due(
            true,
            false,
            false,
            SAMPLE_RATE as usize / 2,
            0,
            now - Duration::from_millis(CMD_RETRY_MS),
        ));
        assert!(!opening_command_due(
            false,
            false,
            false,
            SAMPLE_RATE as usize / 2,
            0,
            now - Duration::from_millis(CMD_RETRY_MS),
        ));
    }

    #[test]
    fn opening_command_fires_immediately_when_silero_closes_the_phrase() {
        let now = Instant::now();
        assert!(opening_command_due(
            true,
            false,
            true,
            SAMPLE_RATE as usize,
            SAMPLE_RATE as usize,
            now,
        ));
    }

    #[test]
    fn opening_pause_probe_blocks_duplicate_while_in_flight() {
        let mut checked = false;
        mark_pause_probe_submitted(&mut checked, true);
        assert!(checked);
    }

    #[test]
    fn opening_prefix_result_reopens_pause_probe_for_fresh_audio() {
        let mut checked = true;
        let mut pending = None;
        mark_opening_prefix_result(&mut checked, &mut pending);
        assert!(!checked);
        assert!(pending.is_some());
    }

    #[test]
    fn opening_command_requires_fresh_audio_and_stays_bounded() {
        let now = Instant::now();
        assert!(!opening_command_due(
            true,
            false,
            false,
            CMD_MIN_PROBE_SAMPLES,
            CMD_MIN_PROBE_SAMPLES,
            now - Duration::from_millis(CMD_RETRY_MS),
        ));
        assert!(!opening_command_due(
            true,
            false,
            false,
            CMD_WINDOW_SAMPLES + 1,
            0,
            now - Duration::from_millis(CMD_RETRY_MS),
        ));
        assert!(opening_command_due(
            true,
            true,
            false,
            CMD_WINDOW_SAMPLES + CMD_NEW_AUDIO_PROBE_SAMPLES,
            CMD_WINDOW_SAMPLES,
            now - Duration::from_millis(CMD_RETRY_MS),
        ));
    }

    #[test]
    fn opening_prefix_retry_requires_fresh_post_pause_audio() {
        let now = Instant::now();
        assert!(!opening_command_due(
            true,
            true,
            true,
            SAMPLE_RATE as usize,
            SAMPLE_RATE as usize,
            now,
        ));
        assert!(opening_command_due(
            true,
            true,
            true,
            SAMPLE_RATE as usize + CMD_NEW_AUDIO_PROBE_SAMPLES,
            SAMPLE_RATE as usize,
            now,
        ));
    }

    #[test]
    fn ambiguous_opening_needs_same_action_on_later_pause_ready_probe() {
        let mut pending = None;

        // Field sequence: early ASR probes changed identity before one
        // pause-ready ambiguous result. No single result may stop recording.
        assert!(!opening_action_confirmation(
            &mut pending,
            "yeah",
            1_000,
            true,
            true,
        ));
        assert!(!opening_action_confirmation(
            &mut pending,
            "same",
            2_000,
            true,
            true,
        ));
        assert!(!opening_action_confirmation(
            &mut pending,
            "save",
            3_000,
            true,
            true,
        ));

        // Matching identity on a later probe can confirm only once the phrase
        // is pause-ready; equal sample counts are not a second observation.
        assert!(!opening_action_confirmation(
            &mut pending,
            "save",
            3_000,
            true,
            true,
        ));
        assert!(!opening_action_confirmation(
            &mut pending,
            "save",
            4_000,
            false,
            true,
        ));
        assert!(opening_action_confirmation(
            &mut pending,
            "save",
            5_000,
            true,
            true,
        ));
    }

    #[test]
    fn complete_opening_needs_same_action_on_later_probe() {
        let mut pending = None;

        // Acoustic hypotheses can still change even when each result is a
        // grammar-complete command. One result must not stop recording.
        assert!(!opening_action_confirmation(
            &mut pending,
            "key:ctrl+s",
            1_000,
            true,
            false,
        ));
        assert!(opening_action_confirmation(
            &mut pending,
            "key:ctrl+s",
            2_000,
            true,
            false,
        ));
    }

    #[test]
    fn blank_audible_probe_waits_for_more_audio_without_final_error() {
        let audio = vec![0.25; SAMPLE_RATE as usize];
        let mut calls = 0;
        let result = transcribe_conditioned(&audio, BlankPolicy::Probe, |_| {
            calls += 1;
            Ok(String::new())
        });
        assert_eq!(result, Ok(String::new()));
        assert_eq!(calls, 1);
    }

    #[test]
    fn command_probe_keeps_pre_speech_prefix() {
        let speech_start = SAMPLE_RATE as usize;
        let start = speech_start.saturating_sub(CMD_PREROLL_SAMPLES);
        assert_eq!(command_probe_start_sample(Some(start), 0), start);
        // With no speech start, fall back to the LAST CMD_WINDOW_SAMPLES of
        // audio so the standalone probe sees real speech (zephyr tail already
        // does this — without it, the pre-decode voice gate fails on the
        // leading silence in the full buffer; field bug 2026-07-06).
        assert_eq!(command_probe_start_sample(None, CMD_WINDOW_SAMPLES), 0);
        assert_eq!(
            command_probe_start_sample(None, CMD_WINDOW_SAMPLES + 4000),
            4000
        );
    }

    #[test]
    fn zephyr_tail_hit_is_decided_from_short_tail_text() {
        use rightkit_text_polish::text_pipeline::{
            has_trailing_control_wake, parse_control_command,
        };
        assert!(parse_control_command("please send this zephyr send").is_some());
        assert!(parse_control_command("zephyr stop").is_some());
        assert!(parse_control_command("please stop by later").is_none());
        assert!(has_trailing_control_wake("please send this zefyr"));
        assert!(!has_trailing_control_wake("the zephyr wind"));
    }

    #[test]
    fn runtime_capture_accepts_silent_pcm_from_a_live_device() {
        let silence = vec![0.0f32; SAMPLE_RATE as usize / 12];
        assert!(!capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::Data(silence.clone())
        ));
        assert!(validate_capture_pcm(&silence, None).is_ok());
        assert!(capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::NoCallbacks
        ));
        assert!(capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::NoSamples
        ));
    }

    #[test]
    fn runtime_capture_reports_digital_zero_but_reopens_nonfinite_readiness_pcm() {
        let silence = vec![0.0f32; SAMPLE_RATE as usize / 12];
        assert!(!capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::Data(silence.clone())
        ));
        // capture_first_buffer_outcome classifies digital-zero / non-finite
        // first buffers on every platform; requires_reopen only catches the
        // non-finite case, so digital-zero silence is a valid readiness seed.
        assert_eq!(
            capture_first_buffer_outcome(&CaptureFirstBuffer::Data(silence)),
            "capture_stream_digital_zero"
        );
        let non_finite = vec![f32::NAN; SAMPLE_RATE as usize / 12];
        assert!(capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::Data(non_finite.clone())
        ));
        assert_eq!(
            capture_first_buffer_outcome(&CaptureFirstBuffer::Data(non_finite)),
            "capture_stream_non_finite"
        );
        assert!(validate_capture_pcm(&[0.0; 16], None).is_ok());
    }

    #[test]
    fn main_asr_cannot_arm_or_parse_kws_controls() {
        assert!(confirmed_main_asr_control("Zephyr Sand", true).is_none());
        assert!(confirmed_main_asr_control("Zephyr Stop", true).is_none());
        assert!(confirmed_main_asr_control("Zephyr Cancel", true).is_none());
        assert!(confirmed_main_asr_control("Zephyr Sand", false).is_none());
        assert!(confirmed_main_asr_control("review them", true).is_none());
    }

    #[test]
    fn tail_probe_policy_uses_full_context_on_every_platform() {
        let total = SAMPLE_RATE as usize * 10;
        assert_eq!(
            tail_probe_start_sample(total, TailProbeLane::Full),
            total - TAIL_WINDOW_SAMPLES
        );

        assert_eq!(
            tail_probe_start_sample(total, TailProbeLane::Fast),
            total - FAST_TAIL_WINDOW_SAMPLES
        );
        assert_eq!(FAST_TAIL_WINDOW_SAMPLES, SAMPLE_RATE as usize * 3);
    }

    #[test]
    fn full_tail_fallback_requires_a_control_verb_and_fresh_audio() {
        // The measured 1.75-second Windows lane can retain the intent verb but
        // crop or distort the wake word. That requests the reliable 3s lane,
        // but never hot-loops on the same samples.
        assert!(should_run_full_tail_fallback(
            "enemies ever send.",
            false,
            TAIL_PREFIX_NEW_AUDIO_SAMPLES,
        ));
        assert!(should_run_full_tail_fallback(
            "and sephyr stop.",
            false,
            TAIL_PREFIX_NEW_AUDIO_SAMPLES,
        ));
        assert!(!should_run_full_tail_fallback(
            "enemies ever send.",
            false,
            TAIL_PREFIX_NEW_AUDIO_SAMPLES - 1,
        ));
        assert!(!should_run_full_tail_fallback(
            "enemies ever submit.",
            false,
            TAIL_PREFIX_NEW_AUDIO_SAMPLES,
        ));
        // Council revision: silence alone must never launch a blocking full
        // decode; the fallback requires transcript evidence of a control verb.
        assert!(!should_run_full_tail_fallback(
            "ordinary dictation",
            false,
            usize::MAX,
        ));
    }

    #[test]
    fn full_tail_fallback_retries_an_armed_candidate() {
        assert!(should_run_full_tail_fallback(
            "ordinary dictation",
            true,
            TAIL_PREFIX_NEW_AUDIO_SAMPLES,
        ));
        assert!(!should_run_full_tail_fallback(
            "ordinary dictation",
            false,
            TAIL_PREFIX_NEW_AUDIO_SAMPLES,
        ));
    }

    #[test]
    fn candidate_stays_fast_only_while_sherpa_still_sees_wake_or_verb() {
        assert!(should_keep_control_candidate_armed(false, true));
        assert!(!should_keep_control_candidate_armed(false, false));
        assert!(!should_keep_control_candidate_armed(true, true));
    }

    #[test]
    fn full_tail_schedule_obeys_audio_gate_and_1500ms_candidate_expiry() {
        assert!(!tail_probe_due(SAMPLE_RATE as usize / 4, true));
        assert!(!tail_probe_due(SAMPLE_RATE as usize / 4 + 1, false));
        assert!(tail_probe_due(SAMPLE_RATE as usize / 4 + 1, true));
        assert!(!tail_prefix_expired(Duration::from_millis(1_499)));
        assert!(tail_prefix_expired(Duration::from_millis(1_500)));
        assert!(!should_run_full_tail_fallback(
            "ordinary dictation",
            true,
            TAIL_PREFIX_NEW_AUDIO_SAMPLES - 1,
        ));
        assert!(should_run_full_tail_fallback(
            "ordinary dictation",
            true,
            TAIL_PREFIX_NEW_AUDIO_SAMPLES,
        ));
    }

    #[test]
    fn missed_vad_does_not_close_command_probing() {
        assert!(!pause_can_close_command_probe(false, true));
        assert!(!pause_can_close_command_probe(true, false));
        assert!(pause_can_close_command_probe(true, true));
    }

    #[test]
    fn low_energy_first_speech_is_still_sent_to_silero() {
        let chunk = vec![0.0001; 320];
        let mut last_voice_at = Instant::now();
        let mut heard_voice = false;
        let mut checked_this_pause = true;
        let mut speech_start_sample = None;
        let mut observed = false;

        observe_voice_chunk(
            &chunk,
            &mut last_voice_at,
            &mut heard_voice,
            &mut checked_this_pause,
            |samples| {
                observed = true;
                assert_eq!(samples, chunk);
                true
            },
            &mut speech_start_sample,
            8_000,
        );

        assert!(observed, "Silero must own first-speech detection");
        assert!(heard_voice);
        assert!(!checked_this_pause);
        assert_eq!(speech_start_sample, Some(8_000 - CMD_PREROLL_SAMPLES));
    }

    #[test]
    fn silero_silence_after_speech_does_not_refresh_voice_clock() {
        let chunk = vec![0.0; 320];
        let original_voice_at = Instant::now() - Duration::from_secs(2);
        let mut last_voice_at = original_voice_at;
        let mut heard_voice = true;
        let mut checked_this_pause = true;
        let mut speech_start_sample = Some(4_000);
        let mut observed = false;

        observe_voice_chunk(
            &chunk,
            &mut last_voice_at,
            &mut heard_voice,
            &mut checked_this_pause,
            |_| {
                observed = true;
                false
            },
            &mut speech_start_sample,
            12_000,
        );

        assert!(
            observed,
            "Silero must continue observing after first speech"
        );
        assert!(heard_voice, "silence must not erase prior speech evidence");
        assert_eq!(last_voice_at, original_voice_at);
        assert!(checked_this_pause);
        assert_eq!(speech_start_sample, Some(4_000));
    }

    #[test]
    fn speech_after_command_pause_does_not_move_kws_context() {
        let chunk = vec![0.1; 320];
        let mut last_voice_at = Instant::now() - Duration::from_millis(PAUSE_GATE_MS + 1);
        let mut heard_voice = true;
        let mut checked_this_pause = true;
        let mut speech_start_sample = Some(1_000);

        observe_voice_chunk(
            &chunk,
            &mut last_voice_at,
            &mut heard_voice,
            &mut checked_this_pause,
            |_| true,
            &mut speech_start_sample,
            40_000,
        );

        assert_eq!(speech_start_sample, Some(1_000));
    }

    #[test]
    fn speech_below_command_pause_keeps_current_sherpa_stream() {
        let chunk = vec![0.1; 320];
        let mut last_voice_at =
            Instant::now() - Duration::from_millis(PAUSE_GATE_MS.saturating_sub(1));
        let mut heard_voice = true;
        let mut checked_this_pause = true;
        let mut speech_start_sample = Some(1_000);

        observe_voice_chunk(
            &chunk,
            &mut last_voice_at,
            &mut heard_voice,
            &mut checked_this_pause,
            |_| true,
            &mut speech_start_sample,
            40_000,
        );

        assert_eq!(speech_start_sample, Some(1_000));
    }

    #[test]
    fn redacted_command_probe_keeps_counts_without_recognized_words() {
        let payload = command_probe_payload(CommandProbeLog {
            lane: "zephyr_tail_async",
            session_id: Some("session-1"),
            probe_ms: 2,
            recognized_text: "Zephyr send",
            classifier_result: "control_complete",
            speech_start_sample: Some(0),
            command_start: 0,
            command_samples: 16_000,
            total_samples: 16_000,
            pending_prefix: false,
            error: None,
        });
        let redacted = rightkit_dictation_core::redact_diagnostic_event(payload);
        assert_eq!(redacted["recognized_chars"], 11);
        assert_eq!(redacted["recognized_words"], 2);
        assert_eq!(redacted["recognized_empty"], false);
        assert_eq!(redacted["recognized_text"], "[redacted:diagnostics]");
        assert!(!redacted.to_string().contains("Zephyr"));
    }

    #[test]
    fn capture_reopen_policy_reuses_an_unchanged_saved_device() {
        assert!(!capture_should_reopen(false, false));
        assert!(capture_should_reopen(true, false));
        assert!(capture_should_reopen(false, true));
    }

    #[test]
    fn first_buffer_health_reopens_every_non_live_start_state() {
        use rightkit_audio::capture::{CaptureErrorKind, CaptureFirstBuffer};

        assert!(capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::NoCallbacks
        ));
        assert!(capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::NoSamples
        ));
        assert!(capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::StreamError(CaptureErrorKind::StreamInvalidated)
        ));
        assert!(!capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::Data(vec![0.0; CAPTURE_LIVENESS_MIN_SAMPLES])
        ));
        assert!(!capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::Data(vec![0.00001; CAPTURE_LIVENESS_MIN_SAMPLES - 1])
        ));
        assert!(!capture_first_buffer_requires_reopen(
            &CaptureFirstBuffer::Data(vec![0.00001; CAPTURE_LIVENESS_MIN_SAMPLES])
        ));
    }

    #[test]
    fn capture_route_signature_detects_native_format_changes() {
        let devices = [DeviceInfo {
            id: 4,
            name: "MacBook Microphone".to_string(),
            native_rate: 48_000,
            channels: 1,
            sample_format: "F32".to_string(),
            is_default: true,
            transport: CaptureTransport::BuiltIn,
            form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
            platform_id: Some("built-in".into()),
        }];
        let (_, baseline) = resolve_capture_route(None, &devices).unwrap();
        let mut changed = devices.clone();
        changed[0].native_rate = 24_000;
        let (_, call_route) = resolve_capture_route(None, &changed).unwrap();
        assert_ne!(baseline, call_route);
    }

    #[test]
    fn system_default_switch_detects_identical_duplicate_devices() {
        let devices = [
            DeviceInfo {
                id: 0,
                name: "USB mic".to_string(),
                native_rate: 48_000,
                channels: 1,
                sample_format: "F32".to_string(),
                is_default: true,
                transport: CaptureTransport::Usb,
                form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
                platform_id: Some("usb-a".into()),
            },
            DeviceInfo {
                id: 1,
                name: "USB mic".to_string(),
                native_rate: 48_000,
                channels: 1,
                sample_format: "F32".to_string(),
                is_default: false,
                transport: CaptureTransport::Usb,
                form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
                platform_id: Some("usb-b".into()),
            },
        ];
        let (baseline_id, baseline) = resolve_capture_route(None, &devices).unwrap();
        let mut switched = devices.clone();
        switched[0].is_default = false;
        switched[1].is_default = true;
        let (switched_id, current) = resolve_capture_route(None, &switched).unwrap();

        assert_eq!(baseline_id, None);
        assert_eq!(switched_id, None);
        assert_eq!(
            baseline.as_ref().unwrap().platform_id.as_deref(),
            Some("usb-a")
        );
        assert_eq!(
            current.as_ref().unwrap().platform_id.as_deref(),
            Some("usb-b")
        );
        assert!(baseline.as_ref().unwrap().uses_system_default);
        assert!(current.as_ref().unwrap().uses_system_default);
        assert!(capture_route_changed(&baseline, &current));
    }

    #[test]
    fn missing_saved_microphone_never_silently_switches_to_default() {
        let devices = [DeviceInfo {
            id: 0,
            name: "MacBook Microphone".to_string(),
            native_rate: 48_000,
            channels: 1,
            sample_format: "F32".to_string(),
            is_default: true,
            transport: CaptureTransport::BuiltIn,
            form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
            platform_id: Some("built-in".into()),
        }];
        let saved = r#"device:{"platform_id":"missing","name":"USB mic"}"#;
        let error = resolve_capture_route(Some(saved), &devices).unwrap_err();
        assert_eq!(error, "saved microphone is unavailable: USB mic");
    }

    #[test]
    fn duplicate_named_microphones_never_resolve_a_stale_platform_id() {
        let devices = [
            DeviceInfo {
                id: 1,
                name: "USB mic".to_string(),
                native_rate: 48_000,
                channels: 1,
                sample_format: "F32".to_string(),
                is_default: false,
                transport: CaptureTransport::Usb,
                form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
                platform_id: Some("usb-a".into()),
            },
            DeviceInfo {
                id: 2,
                name: "USB mic".to_string(),
                native_rate: 48_000,
                channels: 1,
                sample_format: "F32".to_string(),
                is_default: false,
                transport: CaptureTransport::Usb,
                form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
                platform_id: Some("usb-b".into()),
            },
        ];
        let saved = r#"device:{"platform_id":"missing","name":"USB mic"}"#;
        let error = resolve_capture_route(Some(saved), &devices).unwrap_err();
        assert_eq!(error, "saved microphone is unavailable: USB mic");
    }

    #[test]
    fn active_capture_route_change_requires_a_stream_rebuild() {
        let before = CaptureRouteSignature {
            device_id: 0,
            platform_id: Some("built-in".into()),
            uses_system_default: true,
            name: "Laptop Microphone".into(),
            native_rate: 48_000,
            channels: 2,
            sample_format: "F32".into(),
            transport: CaptureTransport::BuiltIn,
            form_factor: rightkit_audio::capture::CaptureFormFactor::Microphone,
        };
        let during_call = CaptureRouteSignature {
            device_id: 1,
            platform_id: Some("headset".into()),
            uses_system_default: true,
            name: "Headset Microphone (Hands-Free)".into(),
            native_rate: 16_000,
            channels: 1,
            sample_format: "F32".into(),
            transport: CaptureTransport::Bluetooth,
            form_factor: rightkit_audio::capture::CaptureFormFactor::Headset,
        };
        assert!(capture_route_changed(&Some(before), &Some(during_call)));
    }

    #[test]
    fn bluetooth_capture_gets_route_specific_settle_budget() {
        let bluetooth = CaptureRouteSignature {
            device_id: 1,
            platform_id: Some("headset".into()),
            uses_system_default: false,
            name: "Headset".into(),
            native_rate: 16_000,
            channels: 1,
            sample_format: "F32".into(),
            transport: CaptureTransport::Bluetooth,
            form_factor: rightkit_audio::capture::CaptureFormFactor::Headset,
        };
        assert_eq!(capture_settle_budget_ms(Some(&bluetooth), false), 1_500);
        assert_eq!(capture_settle_budget_ms(Some(&bluetooth), true), 1_500);
        assert_eq!(capture_settle_budget_ms(None, false), 250);
        assert_eq!(capture_settle_budget_ms(None, true), 500);
    }

    #[test]
    fn command_probe_queue_is_bounded_and_reports_failure_without_blocking() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            enqueue_command_probe(&tx, serde_json::json!({"event": "first"})),
            CommandProbeEnqueueResult::Queued
        );
        assert_eq!(
            enqueue_command_probe(&tx, serde_json::json!({"event": "overflow"})),
            CommandProbeEnqueueResult::Full
        );
        drop(rx);
        assert_eq!(
            enqueue_command_probe(&tx, serde_json::json!({"event": "disconnected"})),
            CommandProbeEnqueueResult::Disconnected
        );
    }

    /// A handle whose worker never answers, so a job stays in flight until it is cancelled.
    fn silent_worker_handle() -> (WorkerHandle, Receiver<WorkerCmd>) {
        let (cmd_tx, cmd_rx) = channel::<WorkerCmd>();
        let (_event_tx, event_rx) = channel::<WorkerEvent>();
        let handle = WorkerHandle {
            cmd_tx: Some(cmd_tx),
            event_rx: Arc::new(Mutex::new(event_rx)),
            cancel_epoch: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            wait_signal: Arc::new(WorkerWaitSignal::default()),
            file_jobs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        (handle, cmd_rx)
    }

    #[test]
    fn own_file_transcription_stops_when_its_cancel_flag_is_set() {
        let (handle, cmd_rx) = silent_worker_handle();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let job = {
            let handle = handle.clone();
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || {
                handle.transcribe_file_own(std::path::PathBuf::from("a.wav"), cancel)
            })
        };
        let command = cmd_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("job sent");
        assert!(
            matches!(&command, WorkerCmd::TranscribeFileJob { progress, .. } if progress.own),
            "the app's own job must be marked own"
        );
        assert!(handle.has_file_work());
        cancel.store(true, std::sync::atomic::Ordering::Release);
        let error = job.join().unwrap().unwrap_err();
        assert_eq!(error, "file transcription cancelled");
        assert!(!handle.has_file_work());
    }

    #[test]
    fn dummy_worker_reload_replies_ready() {
        let worker = spawn_dummy_worker(Arc::new(Mutex::new(FocusTracker::new()))).unwrap();
        assert!(!worker.reload_model().unwrap());
        let _ = worker.send(WorkerCmd::Shutdown);
    }

    #[test]
    fn duration_cap_trips_at_exactly_the_configured_ceiling() {
        assert!(!recording_duration_cap_exceeded(MAX_RECORDING_SAMPLES - 1));
        assert!(recording_duration_cap_exceeded(MAX_RECORDING_SAMPLES));
        assert!(recording_duration_cap_exceeded(MAX_RECORDING_SAMPLES + 1));
        // Sanity: cap is 30 minutes at 16kHz, not some other unit slip.
        assert_eq!(MAX_RECORDING_SAMPLES, 30 * 60 * SAMPLE_RATE as usize);
    }

    #[test]
    fn probe_gate_stashes_stop_without_dropping_it() {
        let (tx, rx) = channel::<WorkerCmd>();
        let mut stash: Option<WorkerCmd> = None;

        // Nothing queued yet — gate stays clear, nothing stashed.
        assert!(probe_gate_clear(&rx, &mut stash));
        assert!(stash.is_none());

        // Stop queued — gate closes, and the command is captured (not lost).
        tx.send(WorkerCmd::StopRecording { send_enter: true })
            .unwrap();
        assert!(!probe_gate_clear(&rx, &mut stash));
        assert!(stop_or_cancel_stashed(&stash));

        // A second peek must not pull another item off the channel (only one
        // command is ever in flight through the stash at a time).
        tx.send(WorkerCmd::Cancel).unwrap();
        assert!(!probe_gate_clear(&rx, &mut stash));
        assert!(matches!(
            stash,
            Some(WorkerCmd::StopRecording { send_enter: true })
        ));
        // The Cancel is still sitting in the channel, unconsumed.
        assert!(matches!(rx.try_recv(), Ok(WorkerCmd::Cancel)));
    }

    #[test]
    fn all_zero_live_capture_reports_no_microphone_audio_on_any_platform() {
        // Goal 1 (2026-09-14 mac/windows divergence fix): the Stop-time
        // all-zero-buffer short-circuit is a pure function of the samples,
        // not a macOS-only behavior — Windows must get the same explicit
        // error instead of an ASR call on a garbage all-zero buffer.
        let all_zero = vec![0.0f32; SAMPLE_RATE as usize / 4];
        assert!(is_no_microphone_audio_at_stop(&all_zero, true));
        // A replayed (fixture/bakeoff) capture is exempt even when all-zero.
        assert!(!is_no_microphone_audio_at_stop(&all_zero, false));
        // An empty buffer never trips it (nothing to misreport).
        assert!(!is_no_microphone_audio_at_stop(&[], true));
        // Real (non-zero) audio never trips it.
        let mut has_signal = vec![0.0f32; SAMPLE_RATE as usize / 4];
        has_signal[10] = 0.01;
        assert!(!is_no_microphone_audio_at_stop(&has_signal, true));
    }

    #[test]
    fn probe_gate_does_not_block_on_non_stop_commands() {
        let (tx, rx) = channel::<WorkerCmd>();
        let mut stash: Option<WorkerCmd> = None;
        tx.send(WorkerCmd::ReloadModel { reply: None }).unwrap();
        // A queued-but-unrelated command is stashed (not dropped) but does not
        // close the probe gate.
        assert!(probe_gate_clear(&rx, &mut stash));
        assert!(matches!(stash, Some(WorkerCmd::ReloadModel { .. })));
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "windows")))]
mod trigger_virtual_time_gate2 {
    use super::replay_effect_sink::{ReplayEffect, ReplayEffectSink};
    use super::*;
    use serde_json::{json, Value};
    use std::fs::File;
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    #[derive(Clone, Debug)]
    struct ReplayRow {
        pub(crate) id: String,
        pub(crate) audio_path: PathBuf,
        pub(crate) expected: Option<&'static str>,
        pub(crate) corpus: &'static str,
    }

    fn intent(value: &str) -> &'static str {
        match value.trim().to_ascii_uppercase().as_str() {
            "STOP" | "ZEPHYR_STOP" | "ZEPHYR STOP" => "STOP",
            "SEND" | "ZEPHYR_SEND" | "ZEPHYR SEND" => "SEND",
            "CANCEL" | "ZEPHYR_CANCEL" | "ZEPHYR CANCEL" => "CANCEL",
            other => panic!("unsupported trigger intent {other}"),
        }
    }

    #[cfg(target_os = "macos")]
    const DEFAULT_CORPUS_ROOT: &str = "/Volumes/D/kwstest";
    #[cfg(target_os = "windows")]
    const DEFAULT_CORPUS_ROOT: &str = r"\\192.168.1.7\d\kwstest";

    #[cfg(target_os = "macos")]
    fn localize_network_path(value: &str) -> PathBuf {
        let normalized = value.replace('\\', "/");
        let suffix = normalized
            .strip_prefix("//192.168.1.7/d/kwstest/")
            .unwrap_or_else(|| panic!("unexpected corpus path {value}"));
        PathBuf::from(DEFAULT_CORPUS_ROOT).join(suffix)
    }

    #[cfg(target_os = "windows")]
    fn localize_network_path(value: &str) -> PathBuf {
        let normalized = value.replace('/', "\\");
        let prefix = format!("{}\\", DEFAULT_CORPUS_ROOT);
        assert!(
            normalized.starts_with(&prefix),
            "unexpected corpus path {value}"
        );
        PathBuf::from(normalized)
    }

    #[cfg(target_os = "macos")]
    fn owner_audio_path(clip: &Value) -> PathBuf {
        clip["mac_path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| localize_network_path(clip["path"].as_str().expect("owner path")))
    }

    #[cfg(target_os = "windows")]
    fn owner_audio_path(clip: &Value) -> PathBuf {
        localize_network_path(clip["path"].as_str().expect("owner path"))
    }

    fn default_output_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(".audit/2026-08-04-event-driven-trigger-runtime")
    }

    fn load_rows(root: &Path) -> Vec<ReplayRow> {
        if let Ok(manifest_path) = std::env::var("RIGHTKIT_KWS_GATE2_MANIFEST") {
            let manifest: Value = serde_json::from_slice(
                &std::fs::read(&manifest_path).expect("read external KWS manifest"),
            )
            .expect("parse external KWS manifest");
            return manifest["rows"]
                .as_array()
                .expect("external manifest rows")
                .iter()
                .map(|clip| ReplayRow {
                    id: clip["id"].as_str().expect("external row id").to_string(),
                    audio_path: PathBuf::from(clip["path"].as_str().expect("external row path")),
                    expected: clip["expected"].as_str().map(intent),
                    corpus: "external_manifest",
                })
                .collect();
        }
        let mut rows = Vec::new();
        let positive: Value = serde_json::from_slice(
            &std::fs::read(root.join("canonical/positive14/qualification-raw.json"))
                .expect("read canonical positive truth"),
        )
        .expect("parse canonical positive truth");
        for clip in positive["results"][0]["clips"]
            .as_array()
            .expect("canonical positive clips")
        {
            let id = clip["audio"]["id"].as_str().expect("positive id");
            rows.push(ReplayRow {
                id: id.to_string(),
                audio_path: root
                    .join("canonical/positive14/audio")
                    .join(format!("{id}.wav")),
                expected: Some(intent(
                    clip["parser_result"]["intent"]
                        .as_str()
                        .expect("positive intent"),
                )),
                corpus: "canonical_positive14",
            });
        }

        let negative: Vec<Value> = serde_json::from_slice(
            &std::fs::read(root.join("canonical/negative238/candidate_manifest.json"))
                .expect("read canonical negative truth"),
        )
        .expect("parse canonical negative truth");
        for clip in negative {
            rows.push(ReplayRow {
                id: clip["id"].as_str().expect("negative id").to_string(),
                audio_path: localize_network_path(clip["path"].as_str().expect("negative path")),
                expected: None,
                corpus: "canonical_negative238",
            });
        }

        for (relative, corpus) in [
            ("owner/windows/owner-kws-manifest.json", "owner_windows"),
            ("owner/mac/owner-kws-manifest.json", "owner_mac"),
        ] {
            let manifest: Value = serde_json::from_slice(
                &std::fs::read(root.join(relative)).expect("read owner truth"),
            )
            .expect("parse owner truth");
            for clip in manifest["rows"].as_array().expect("owner rows") {
                let audio_path = owner_audio_path(clip);
                rows.push(ReplayRow {
                    id: clip["id"].as_str().expect("owner id").to_string(),
                    audio_path,
                    expected: clip["expected_detection"].as_str().map(intent),
                    corpus,
                });
            }
        }
        rows
    }

    fn read_wav(path: &Path) -> Vec<f32> {
        let wav = rightkit_audio::wav::read_wav_mono_f32(path)
            .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
        assert_eq!(
            wav.sample_rate,
            SAMPLE_RATE,
            "{} sample rate",
            path.display()
        );
        wav.samples
    }

    fn scripted_control_cmd(event: CaptureEvent) -> Result<Option<WorkerCmd>, String> {
        match event {
            CaptureEvent::Control { name, .. } => match name.trim().to_ascii_lowercase().as_str() {
                "stop" => Ok(Some(WorkerCmd::StopRecording { send_enter: false })),
                "send" => Ok(Some(WorkerCmd::StopRecording { send_enter: true })),
                "cancel" => Ok(Some(WorkerCmd::Cancel)),
                other => Err(format!("unsupported scripted replay control {other}")),
            },
            CaptureEvent::CaptureError { message, .. } => {
                Err(format!("scripted replay capture error: {message}"))
            }
            CaptureEvent::Disconnect { .. } => Err("scripted replay disconnect".into()),
            CaptureEvent::Audio(_) | CaptureEvent::Eof { .. } => Ok(None),
        }
    }

    fn dispatch_scripted_controls(controller: &ReplayCaptureController, handle: &WorkerHandle) {
        while let Some(event) = controller.pop_scripted_control() {
            if let Some(cmd) = scripted_control_cmd(event).expect("valid scripted replay control") {
                handle.send(cmd).expect("dispatch scripted replay control");
            }
        }
    }

    fn drain_action_events(
        handle: &WorkerHandle,
        session_id: &str,
        effects: &mut ReplayEffectSink,
    ) -> Vec<&'static str> {
        let mut actions = Vec::new();
        let events = handle.event_rx.lock();
        loop {
            match events.try_recv() {
                Ok(WorkerEvent::AutoStopBegin {
                    session_id: event_session,
                    send_enter,
                    ..
                }) if event_session == session_id => {
                    let (action, effect) = if send_enter {
                        ("SEND", ReplayEffect::Send)
                    } else {
                        ("STOP", ReplayEffect::Stop)
                    };
                    effects
                        .record_terminal(session_id, effect)
                        .unwrap_or_else(|duplicate| {
                            panic!("duplicate replay effect: {duplicate:?}")
                        });
                    actions.push(action);
                }
                Ok(WorkerEvent::RunawayDiscard {
                    session_id: event_session,
                }) if event_session == session_id => {
                    effects
                        .record_terminal(session_id, ReplayEffect::Cancel)
                        .unwrap_or_else(|duplicate| {
                            panic!("duplicate replay effect: {duplicate:?}")
                        });
                    actions.push("CANCEL");
                }
                // Spoken `zephyr cancel` now transcribes-to-history instead of
                // discarding: the begin marker is the terminal cancel effect
                // (the follow-up TranscriptFinal carries the saved text).
                Ok(WorkerEvent::AutoCancelBegin {
                    session_id: event_session,
                }) if event_session == session_id => {
                    effects
                        .record_terminal(session_id, ReplayEffect::Cancel)
                        .unwrap_or_else(|duplicate| {
                            panic!("duplicate replay effect: {duplicate:?}")
                        });
                    actions.push("CANCEL");
                }
                Ok(WorkerEvent::CaptureFailed {
                    session_id: event_session,
                    message,
                }) if event_session == session_id => {
                    panic!("{session_id}: capture failed: {message}")
                }
                // This replay loads no main model, so the cancel-to-history
                // decode that follows AutoCancelBegin returns the audible-blank
                // error. Stop/Send carry the same error on AutoStop, which this
                // gate already ignores; it scores trigger actions only.
                Ok(WorkerEvent::TranscriptFinal {
                    session_id: event_session,
                    result: Err(message),
                    ..
                }) if event_session == session_id
                    && message == crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR
                    && effects
                        .effects_for(session_id)
                        .iter()
                        .any(|recorded| recorded.effect == ReplayEffect::Cancel) => {}
                Ok(WorkerEvent::TranscriptFinal {
                    session_id: event_session,
                    result: Err(message),
                    ..
                }) if event_session == session_id => {
                    panic!("{session_id}: recording failed: {message}")
                }
                Ok(WorkerEvent::StartFailed { message }) => {
                    panic!("worker failed during replay: {message}")
                }
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("worker event channel disconnected")
                }
            }
        }
        actions
    }

    #[test]
    fn scripted_controls_map_to_real_worker_commands() {
        for (name, expected) in [("stop", "stop"), ("send", "send"), ("cancel", "cancel")] {
            let cmd = scripted_control_cmd(CaptureEvent::Control {
                at_sample: 320,
                name: name.into(),
            })
            .unwrap()
            .unwrap();
            assert!(matches!(
                (expected, cmd),
                ("stop", WorkerCmd::StopRecording { send_enter: false })
                    | ("send", WorkerCmd::StopRecording { send_enter: true })
                    | ("cancel", WorkerCmd::Cancel)
            ));
        }
    }

    #[test]
    fn replay_eof_waits_for_final_capture_pump_and_probe_result() {
        let controller = ReplayCaptureController::default();
        {
            let mut state = controller.state.0.lock().unwrap();
            state.total_samples = 320;
            state.cursor = 320;
        }
        assert_eq!(controller.finished(), None);
        {
            let mut state = controller.state.0.lock().unwrap();
            state.pumped_total = 320;
        }
        controller.note_submitted(320);
        assert_eq!(controller.finished(), None);
        controller.note_processed(320, 7);
        assert_eq!(controller.finished(), None);
        controller.note_dispatched(320);
        assert_eq!(controller.finished(), Some(7));
    }

    #[test]
    fn gate2_defaults_use_host_corpus_without_copying() {
        #[cfg(target_os = "macos")]
        assert_eq!(
            localize_network_path(r"\\192.168.1.7\d\kwstest\owner\mac\sample.wav"),
            PathBuf::from("/Volumes/D/kwstest/owner/mac/sample.wav")
        );
        #[cfg(target_os = "windows")]
        assert_eq!(
            localize_network_path(r"\\192.168.1.7\d\kwstest\owner\windows\sample.wav"),
            PathBuf::from(r"\\192.168.1.7\d\kwstest\owner\windows\sample.wav")
        );
    }

    #[test]
    #[ignore = "explicit 373-row production-worker virtual-time trigger corpus"]
    fn production_worker_gate2_replays_full_trigger_corpus() {
        let corpus_root = PathBuf::from(
            std::env::var("RIGHTKIT_KWS_CORPUS_ROOT")
                .unwrap_or_else(|_| DEFAULT_CORPUS_ROOT.to_string()),
        );
        let output_root = std::env::var("RIGHTKIT_KWS_RUNTIME_OUTPUT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_output_root());
        std::fs::create_dir_all(&output_root).expect("create runtime output root");
        let external_manifest = std::env::var_os("RIGHTKIT_KWS_GATE2_MANIFEST").is_some();
        let positives_only =
            std::env::var("RIGHTKIT_KWS_RUNTIME_POSITIVES_ONLY").as_deref() == Ok("1");
        let row_filter = std::env::var("RIGHTKIT_KWS_RUNTIME_ROW_ID").ok();
        let mut rows = load_rows(&corpus_root);
        if positives_only {
            rows.retain(|row| row.expected.is_some());
        }
        if let Some(row_id) = row_filter.as_deref() {
            rows.retain(|row| row.id == row_id);
        }
        if external_manifest {
            assert_eq!(
                rows.len(),
                1,
                "external Gate2 manifest requires one row per process; set RIGHTKIT_KWS_RUNTIME_ROW_ID"
            );
        }
        let positives = rows.iter().filter(|row| row.expected.is_some()).count();
        if row_filter.is_none() && !external_manifest {
            assert_eq!(
                (rows.len(), positives),
                if positives_only { (56, 56) } else { (373, 56) }
            );
        } else {
            assert!(!rows.is_empty());
        }
        assert!(rows.iter().all(|row| row.audio_path.is_file()));

        std::env::set_var(
            "RIGHTKIT_SHERPA_KWS_DIR",
            std::env::var_os("RIGHTKIT_KWS_GATE2_MODEL_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws")
                }),
        );
        std::env::set_var("RIGHTKIT_APP_DATA_DIR", output_root.join("app-data"));
        std::env::set_var("RIGHTKIT_SCHEDULED_STATIC15", "0");
        std::env::remove_var("RIGHTKIT_DISABLE_KWS");
        #[cfg(target_os = "windows")]
        let _ort = unsafe {
            libloading::Library::new(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../src-tauri/resources/runtime/onnxruntime.dll"),
            )
        }
        .expect("preload bundled ONNX Runtime");

        let rows_path = output_root.join("rows.jsonl");
        let mut output = BufWriter::new(File::create(&rows_path).expect("create rows output"));
        let started = Instant::now();
        let mut passed = 0usize;
        let mut misses = Vec::new();
        let mut false_actions = Vec::new();
        let mut wrong_actions = Vec::new();
        let mut max_probe_ms = 0u64;
        let mut effects = ReplayEffectSink::default();

        let controller = install_trigger_replay();
        let worker = spawn_worker_with_clock(
            output_root.join("unused-main-model"),
            AsrEp::Cpu,
            Arc::new(Mutex::new(FocusTracker::new())),
            controller.virtual_clock(),
        )
        .expect("spawn production worker for trigger replay");

        for (index, row) in rows.iter().enumerate() {
            let audio = read_wav(&row.audio_path);
            let audio_samples = audio.len();
            let row_timeout =
                Duration::from_secs_f64(audio.len() as f64 / SAMPLE_RATE as f64 + 60.0);
            if external_manifest {
                controller.load_scripted_unpadded(audio, Vec::new());
            } else {
                controller.load(audio);
            }
            let session_id = format!("virtual-time-gate2-{index:03}-{}", row.id);
            worker
                .send(WorkerCmd::StartRecording {
                    session_id: session_id.clone(),
                })
                .unwrap_or_else(|error| panic!("{} start: {error}", row.id));

            let row_started = Instant::now();
            let mut actions = Vec::new();
            let row_max_probe_ms = loop {
                dispatch_scripted_controls(&controller, &worker);
                actions.extend(drain_action_events(&worker, &session_id, &mut effects));
                if !actions.is_empty() {
                    let state = controller
                        .state
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    break state.max_probe_ms;
                }
                if let Some(max_probe_ms) = controller.finished() {
                    // Executor EOF now acknowledges only after worker main has
                    // dispatched each result. Re-drain terminal events before
                    // synthetic negative cleanup so Cancel cannot interrupt a
                    // just-completed spoken control.
                    dispatch_scripted_controls(&controller, &worker);
                    actions.extend(drain_action_events(&worker, &session_id, &mut effects));
                    if !actions.is_empty() {
                        break max_probe_ms;
                    }
                    worker
                        .send(WorkerCmd::Cancel)
                        .expect("cancel negative replay");
                    worker.reload_model().expect("negative replay barrier");
                    actions.extend(drain_action_events(&worker, &session_id, &mut effects));
                    break max_probe_ms;
                }
                assert!(
                    row_started.elapsed() < row_timeout,
                    "{} replay timed out: {}",
                    row.id,
                    controller.debug_state()
                );
                std::thread::sleep(Duration::from_millis(1));
            };

            if !actions.is_empty() {
                worker.reload_model().expect("positive replay barrier");
                actions.extend(drain_action_events(&worker, &session_id, &mut effects));
            }
            max_probe_ms = max_probe_ms.max(row_max_probe_ms);
            let actual = match actions.as_slice() {
                [] => None,
                [single] => Some(*single),
                _ => panic!("{} emitted multiple actions: {actions:?}", row.id),
            };
            let expected_effects = match actual {
                Some("STOP") => vec![ReplayEffect::Stop],
                Some("SEND") => vec![ReplayEffect::Send],
                Some("CANCEL") => vec![ReplayEffect::Cancel],
                None => Vec::new(),
                Some(other) => unreachable!("unknown replay action {other}"),
            };
            effects
                .assert_exactly_once(&session_id, &expected_effects)
                .unwrap_or_else(|error| panic!("{}: {error}", row.id));
            let pass = actual == row.expected;
            if pass {
                passed += 1;
            } else {
                match (row.expected, actual) {
                    (Some(_), None) => misses.push(row.id.clone()),
                    (None, Some(_)) => false_actions.push(row.id.clone()),
                    (Some(_), Some(_)) => wrong_actions.push(row.id.clone()),
                    (None, None) => unreachable!(),
                }
            }
            let (onset_sample, confirmation_sample, stop_buffer_samples, observed_probe_ms) = {
                let state = controller
                    .state
                    .0
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                (
                    state.confirmed_onset_sample,
                    state.confirmation_sample,
                    state.stop_buffer_samples,
                    state.max_probe_ms,
                )
            };
            max_probe_ms = max_probe_ms.max(observed_probe_ms);
            serde_json::to_writer(
                &mut output,
                &json!({
                    "row_id": row.id,
                    "corpus": row.corpus,
                    "audio_path": row.audio_path,
                    "expected": row.expected,
                    "actual": actual,
                    "actual_intent": actual,
                    "pass": pass,
                    "max_probe_ms": observed_probe_ms,
                    "onset_sample": onset_sample,
                    "confirmation_sample": confirmation_sample,
                    "stop_buffer_samples": stop_buffer_samples,
                    "audio_samples": audio_samples,
                    "wall_ms": row_started.elapsed().as_millis() as u64,
                }),
            )
            .expect("write row result");
            output.write_all(b"\n").expect("finish row result");
            output.flush().expect("checkpoint row result");
        }
        worker
            .send(WorkerCmd::Shutdown)
            .expect("shutdown replay worker");
        TRIGGER_REPLAY_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
        *REPLAY_CAPTURE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;

        let summary = json!({
            "schema": "heardright.kws_runtime_gate2.trigger_only.result.v1",
            "evidence_class": "gate2_acceptance",
            "rows": rows.len(),
            "positives": positives,
            "negatives": rows.len() - positives,
            "passed": passed,
            "misses": misses,
            "false_actions": false_actions,
            "wrong_actions": wrong_actions,
            "max_probe_ms": max_probe_ms,
            "wall_ms": started.elapsed().as_millis() as u64,
        });
        std::fs::write(
            output_root.join("summary.json"),
            serde_json::to_vec_pretty(&summary).expect("serialize summary"),
        )
        .expect("write summary");
        assert_eq!(passed, rows.len(), "{summary}");
    }

    /// Gate2 bypasses the silence backstops (TRIGGER_REPLAY_ACTIVE), so this
    /// drives the same production worker through both of them: dictation
    /// followed by a minute of silence must auto-stop as a silence stop, and a
    /// recording that never hears speech must be discarded.
    #[test]
    #[ignore = "explicit production-worker virtual-time silence backstops; needs the trigger corpus"]
    fn production_worker_replays_silence_backstops() {
        let corpus_root = PathBuf::from(
            std::env::var("RIGHTKIT_KWS_CORPUS_ROOT")
                .unwrap_or_else(|_| DEFAULT_CORPUS_ROOT.to_string()),
        );
        let output_root = std::env::var("RIGHTKIT_KWS_RUNTIME_OUTPUT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_output_root());
        std::fs::create_dir_all(&output_root).expect("create runtime output root");
        let dictation = load_rows(&corpus_root)
            .into_iter()
            .find(|row| row.expected.is_none())
            .expect("a negative (dictation-only) corpus row");

        std::env::set_var(
            "RIGHTKIT_SHERPA_KWS_DIR",
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src-tauri/resources/kws"),
        );
        std::env::set_var("RIGHTKIT_APP_DATA_DIR", output_root.join("app-data"));
        std::env::set_var("RIGHTKIT_SCHEDULED_STATIC15", "0");
        std::env::remove_var("RIGHTKIT_DISABLE_KWS");
        let controller = install_trigger_replay();
        let worker = spawn_worker_with_clock(
            output_root.join("unused-main-model"),
            AsrEp::Cpu,
            Arc::new(Mutex::new(FocusTracker::new())),
            controller.virtual_clock(),
        )
        .expect("spawn production worker for silence replay");
        // The executor reads the flag only at spawn, to skip the main model.
        // Clearing it afterwards re-enables the silence backstops.
        TRIGGER_REPLAY_ACTIVE.store(false, std::sync::atomic::Ordering::Release);

        let run = |session_id: &str, audio: Vec<f32>| -> Vec<WorkerEvent> {
            let timeout = Duration::from_secs_f64(audio.len() as f64 / SAMPLE_RATE as f64 + 60.0);
            controller.load(audio);
            worker
                .send(WorkerCmd::StartRecording {
                    session_id: session_id.to_string(),
                })
                .expect("start silence replay");
            let started = Instant::now();
            let mut terminal = Vec::new();
            loop {
                let events = worker.event_rx.lock();
                while let Ok(event) = events.try_recv() {
                    match &event {
                        WorkerEvent::AutoStopBegin { session_id: id, .. }
                        | WorkerEvent::AutoStop { session_id: id, .. }
                        | WorkerEvent::AutoCancelBegin { session_id: id }
                        | WorkerEvent::RunawayDiscard { session_id: id }
                        | WorkerEvent::CaptureFailed { session_id: id, .. }
                        | WorkerEvent::TranscriptFinal { session_id: id, .. }
                            if id == session_id =>
                        {
                            terminal.push(event)
                        }
                        _ => {}
                    }
                }
                drop(events);
                let done = terminal.iter().any(|event| {
                    matches!(
                        event,
                        WorkerEvent::AutoStop { .. }
                            | WorkerEvent::RunawayDiscard { .. }
                            | WorkerEvent::CaptureFailed { .. }
                            | WorkerEvent::TranscriptFinal { .. }
                    )
                });
                if done {
                    return terminal;
                }
                assert!(
                    controller.finished().is_none(),
                    "{session_id}: replay reached EOF without a backstop: {}",
                    controller.debug_state()
                );
                assert!(
                    started.elapsed() < timeout,
                    "{session_id}: silence replay timed out"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        };

        let mut speech_then_silence = read_wav(&dictation.audio_path);
        speech_then_silence.resize(speech_then_silence.len() + SAMPLE_RATE as usize * 75, 0.0);
        let events = run("silence-backstop-post-speech", speech_then_silence);
        assert!(
            matches!(
                events.as_slice(),
                [
                    WorkerEvent::AutoStopBegin {
                        send_enter: false,
                        standalone_action: None,
                        ..
                    },
                    WorkerEvent::AutoStop {
                        send_enter: false,
                        standalone_action: None,
                        ..
                    },
                ]
            ),
            "{}: post-speech silence must auto-stop once: {events:?}",
            dictation.id
        );

        let events = run(
            "silence-backstop-initial",
            vec![0.0; SAMPLE_RATE as usize * 40],
        );
        assert!(
            matches!(events.as_slice(), [WorkerEvent::RunawayDiscard { .. }]),
            "a recording that never hears speech must be discarded: {events:?}"
        );

        worker
            .send(WorkerCmd::Shutdown)
            .expect("shutdown silence replay worker");
        *REPLAY_CAPTURE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}
