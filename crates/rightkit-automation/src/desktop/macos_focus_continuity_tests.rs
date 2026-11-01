use super::*;

fn stable_observation() -> ObservationState {
    ObservationState {
        started_at: 1,
        coverage_started_at: Some(2),
        last_change_at: 2,
        sample: Some(FocusSample {
            pid: 7,
            window: 9,
            editor: 10,
            process_instance: 11,
        }),
        coverage_lost: false,
        last_poll_at: 99,
        recheck_from: None,
    }
}

#[test]
fn hotkey_anchor_uses_only_fresh_stable_observer_sample() {
    let observation = stable_observation();
    assert_eq!(
        hotkey_anchor_from_state(&observation, 100),
        Some(HotkeyFocusAnchor {
            captured_at: 100,
            process_id: 7,
            window_identity: 9,
            process_instance: 11,
            native_control: Some(10),
        }),
    );

    let mut missing = observation;
    missing.sample = None;
    assert!(hotkey_anchor_from_state(&missing, 100).is_none());

    let mut stale = observation;
    stale.last_poll_at = 0;
    let stale_at = nanos_to_ticks(250_000_000).saturating_add(1);
    assert!(hotkey_anchor_from_state(&stale, stale_at).is_none());

    let mut changed = observation;
    changed.last_change_at = 100;
    assert!(hotkey_anchor_from_state(&changed, 100).is_none());

    let mut recheck = observation;
    recheck.recheck_from = Some((observation.sample.unwrap(), 90));
    assert!(hotkey_anchor_from_state(&recheck, 100).is_none());
}

#[test]
fn hotkey_anchor_fails_closed_when_cache_is_busy_or_coverage_is_lost() {
    let held = state()
        .lock()
        .expect("observer state should not be poisoned");
    assert!(capture_hotkey_focus_anchor().is_none());
    drop(held);

    let mut lost = stable_observation();
    lost.coverage_lost = true;
    assert!(hotkey_anchor_from_state(&lost, 100).is_none());
}

#[test]
fn pending_progress_then_valid_keeps_and_reads_original_candidate() {
    let mut observation = stable_observation();
    let before = classify_continuity(&observation, 7, 9, 100, 11, 101, 250);
    assert_eq!(
        before,
        FocusContinuityStatus::Pending("awaiting_observer_progress")
    );
    let mut reads = 0;
    let (captured, after) = capture_continuous_candidate(
        before,
        true,
        || {
            reads += 1;
            "original-editor"
        },
        || {
            observation.last_poll_at = 600;
            classify_continuity(&observation, 7, 9, 100, 11, 601, 250)
        },
    );
    assert_eq!(reads, 1);
    assert_eq!(captured, Some("original-editor"));
    assert!(after.is_valid());
}

#[test]
fn changed_focus_cannot_publish_candidate_even_after_return() {
    let mut observation = stable_observation();
    observation.last_poll_at = 200;
    observation.last_change_at = 150;
    let changed = classify_continuity(&observation, 7, 9, 100, 11, 201, 250);
    assert_eq!(changed, FocusContinuityStatus::Invalid("focus_changed"));
    let (candidate, _) = capture_continuous_candidate(
        changed,
        true,
        || panic!("known changed focus must never read"),
        || FocusContinuityStatus::Valid,
    );
    assert_eq!(candidate, None::<()>);
    let (candidate, _) = capture_continuous_candidate(
        FocusContinuityStatus::Valid,
        true,
        || "candidate",
        || changed,
    );
    assert_eq!(candidate, None);
}

#[test]
fn pending_then_same_process_focus_change_never_reads_candidate() {
    let (candidate, after) = capture_continuous_candidate(
        FocusContinuityStatus::Pending("state_busy"),
        true,
        || panic!("must not read another editor"),
        || FocusContinuityStatus::Invalid("focus_changed"),
    );
    assert_eq!(candidate, None::<()>);
    assert_eq!(after, FocusContinuityStatus::Invalid("focus_changed"));
}

#[test]
fn pending_timeout_does_not_publish_candidate_or_busy_as_valid() {
    let status = settle_continuity_status(Duration::ZERO, || {
        FocusContinuityStatus::Pending("state_busy")
    });
    let (candidate, _) = capture_continuous_candidate(status, true, || "candidate", || status);
    assert_eq!(candidate, None);
    let mut calls = 0;
    let status = settle_continuity_status(Duration::from_millis(50), || {
        calls += 1;
        if calls == 1 {
            FocusContinuityStatus::Pending("state_busy")
        } else {
            FocusContinuityStatus::Valid
        }
    });
    assert!(status.is_valid());
    assert_eq!(calls, 2);
}

#[test]
fn coverage_loss_and_process_change_are_terminal_not_retryable() {
    let mut observation = stable_observation();
    observation.coverage_lost = true;
    assert_eq!(
        classify_continuity(&observation, 7, 9, 100, 11, 101, 250),
        FocusContinuityStatus::Invalid("coverage_lost")
    );
    observation.coverage_lost = false;
    assert_eq!(
        classify_continuity(&observation, 7, 9, 100, 12, 101, 250),
        FocusContinuityStatus::Invalid("process_changed")
    );
}

#[test]
fn opaque_native_sample_recovers_after_loss_without_ax_registration() {
    let native = FocusSample {
        pid: 7,
        window: 9,
        editor: 0,
        process_instance: 11,
    };
    let mut observation = ObservationState {
        started_at: 1,
        coverage_started_at: None,
        last_change_at: 1,
        sample: None,
        coverage_lost: false,
        last_poll_at: 1,
        recheck_from: None,
    };
    apply_sample(&mut observation, Some(native), 10);
    assert_eq!(observation.coverage_started_at, Some(10));
    apply_sample(&mut observation, None, 20);
    assert_eq!(observation.coverage_started_at, None);
    apply_sample(&mut observation, Some(native), 30);
    assert_eq!(observation.coverage_started_at, Some(30));
    assert!(!crate::desktop::continuity_window_valid(
        15,
        35,
        30,
        observation.last_change_at,
        30,
        10
    ));
    assert!(crate::desktop::continuity_window_valid(
        31,
        35,
        30,
        observation.last_change_at,
        35,
        10
    ));
}

#[test]
fn heartbeat_keeps_retained_sample_fresh_but_never_revives_a_cleared_one() {
    let editor = FocusSample {
        pid: 7,
        window: 9,
        editor: 10,
        process_instance: 11,
    };
    let mut observation = ObservationState {
        started_at: 1,
        coverage_started_at: Some(2),
        last_change_at: 2,
        sample: Some(editor),
        coverage_lost: false,
        last_poll_at: 5,
        recheck_from: None,
    };
    // Stable focus, no resample: a token taken at 900 must still be fresh.
    assert!(!crate::desktop::continuity_window_valid(
        900,
        905,
        2,
        2,
        observation.last_poll_at,
        10
    ));
    heartbeat(&mut observation, 901);
    assert!(crate::desktop::continuity_window_valid(
        900,
        905,
        2,
        2,
        observation.last_poll_at,
        10
    ));
    // An AX/workspace notification cleared the sample: no liveness claim.
    observation.sample = None;
    heartbeat(&mut observation, 950);
    assert_eq!(observation.last_poll_at, 901);
    observation.sample = Some(editor);
    observation.coverage_lost = true;
    heartbeat(&mut observation, 960);
    assert_eq!(observation.last_poll_at, 901);
}

#[test]
fn readable_editor_change_and_loss_invalidate_previous_capture() {
    let editor = FocusSample {
        pid: 7,
        window: 9,
        editor: 10,
        process_instance: 11,
    };
    let mut observation = ObservationState {
        started_at: 1,
        coverage_started_at: Some(2),
        last_change_at: 2,
        sample: Some(editor),
        coverage_lost: false,
        last_poll_at: 5,
        recheck_from: None,
    };
    apply_sample(
        &mut observation,
        Some(FocusSample {
            editor: 12,
            ..editor
        }),
        20,
    );
    assert_eq!(observation.last_change_at, 20);
    apply_sample(&mut observation, Some(editor), 30);
    assert_eq!(observation.last_change_at, 30);
    apply_sample(
        &mut observation,
        Some(FocusSample {
            editor: 0,
            ..editor
        }),
        40,
    );
    assert_eq!(observation.last_change_at, 40);
}

#[test]
fn ax_notification_without_a_focus_move_does_not_invalidate_tokens() {
    let mut observation = stable_observation();
    let editor = observation.sample.expect("sample");
    // Token taken at 100; the app destroys some element at 150.
    note_ax_notification(&mut observation, 150);
    assert!(observation.sample.is_none(), "never Valid while unverified");
    assert_eq!(
        classify_continuity(&observation, 7, 9, 100, 11, 160, 1_000),
        FocusContinuityStatus::Pending("observation_updated")
    );
    // Re-read finds the same process, window and editor: no change.
    apply_sample(&mut observation, Some(editor), 170);
    assert_eq!(observation.last_change_at, 2);
    assert_eq!(
        classify_continuity(&observation, 7, 9, 100, 11, 180, 1_000),
        FocusContinuityStatus::Valid
    );
}

#[test]
fn ax_notification_with_a_real_focus_move_invalidates_from_notification_time() {
    let mut observation = stable_observation();
    let editor = observation.sample.expect("sample");
    note_ax_notification(&mut observation, 150);
    note_ax_notification(&mut observation, 155);
    apply_sample(
        &mut observation,
        Some(FocusSample {
            editor: 99,
            ..editor
        }),
        170,
    );
    assert!(observation.last_change_at >= 150);
    assert!(matches!(
        classify_continuity(&observation, 7, 9, 100, 11, 180, 1_000),
        FocusContinuityStatus::Invalid(_)
    ));
    // Focus lost entirely after a notification is also a change.
    let mut lost = stable_observation();
    note_ax_notification(&mut lost, 150);
    apply_sample(&mut lost, None, 170);
    assert!(lost.last_change_at >= 150);
}

#[test]
fn learning_the_editor_of_the_same_target_is_not_a_focus_change() {
    let mut observation = stable_observation();
    let known = observation.sample.expect("sample");
    observation.sample = Some(FocusSample { editor: 0, ..known });
    apply_sample(&mut observation, Some(known), 500);
    assert_eq!(observation.sample, Some(known));
    assert_eq!(
        observation.last_change_at, 2,
        "0 -> known editor must not invalidate tokens"
    );
    // A different KNOWN editor is still a change.
    apply_sample(
        &mut observation,
        Some(FocusSample {
            editor: 77,
            ..known
        }),
        600,
    );
    assert_eq!(observation.last_change_at, 600);
    // So is an unknown editor in a different window.
    let mut other = stable_observation();
    other.sample = Some(FocusSample { editor: 0, ..known });
    apply_sample(
        &mut other,
        Some(FocusSample {
            window: 123,
            ..known
        }),
        700,
    );
    assert_eq!(other.last_change_at, 700);
}

#[test]
fn missing_sample_retries_are_paced_after_a_slow_observation() {
    let last = Instant::now();
    assert!(!sample_due(
        last,
        last + Duration::from_millis(40),
        false,
        true,
        false,
        false,
        true,
    ));
    assert!(sample_due(
        last,
        last + Duration::from_millis(150),
        false,
        true,
        false,
        false,
        true,
    ));
    // PID changes remain an immediate resampling boundary.
    assert!(sample_due(
        last,
        last + Duration::from_millis(1),
        true,
        true,
        false,
        false,
        true,
    ));
}

#[test]
fn notification_invalidation_rechecks_immediately() {
    let mut observation = stable_observation();
    note_ax_notification(&mut observation, 150);
    assert!(observation.sample.is_none());
    assert!(observation.recheck_from.is_some());

    let last = Instant::now();
    assert!(sample_due(
        last,
        last + Duration::from_millis(1),
        false,
        true,
        observation.recheck_from.is_some(),
        false,
        true,
    ));
}

#[test]
fn failed_sample_retry_count_is_bounded_over_ten_cycles() {
    let clock_start = Instant::now();
    let mut last_sampled = clock_start;
    let mut retries = 0;
    for cycle in 1..=10 {
        let now = clock_start + Duration::from_millis(40 * cycle);
        let due = sample_due(last_sampled, now, false, true, false, false, true);
        retries += due as u8;
        if due {
            last_sampled = now;
        }
    }
    // Ten old 40 ms cycles issued ten provider walks; paced retries issue two.
    assert_eq!(retries, 2);
}

// ---- On-demand coverage lifecycle (pure state machine; no AppKit, no thread) ----

#[test]
fn lifecycle_starts_idle_and_first_arm_requests_the_thread() {
    let mut lifecycle = CoverageLifecycle::IDLE;
    assert!(!lifecycle.wanted && !lifecycle.is_ready());
    assert_eq!(lifecycle.arm(), ArmOutcome::Armed { spawn: true });
    assert!(lifecycle.wanted);
    assert_eq!(lifecycle.epoch, 1);
    // Armed but the first observer cycle has not completed: not ready.
    assert!(!lifecycle.is_ready());
}

#[test]
fn lifecycle_arm_is_idempotent_while_wanted() {
    let mut lifecycle = CoverageLifecycle::IDLE;
    lifecycle.arm();
    lifecycle.thread_spawned = true;
    lifecycle.mark_ready(1);
    // A second begin during an active interval must not restart it or reset readiness.
    assert_eq!(lifecycle.arm(), ArmOutcome::AlreadyWanted);
    assert_eq!(lifecycle.epoch, 1);
    assert!(lifecycle.is_ready());
}

#[test]
fn lifecycle_ready_requires_the_current_epoch() {
    let mut lifecycle = CoverageLifecycle::IDLE;
    lifecycle.arm();
    lifecycle.mark_ready(1);
    assert!(lifecycle.is_ready());

    // End then begin again before the thread noticed: the old session's epoch is dead
    // and its late readiness announcement cannot satisfy the new interval.
    assert!(lifecycle.disarm());
    lifecycle.thread_spawned = true;
    assert_eq!(lifecycle.arm(), ArmOutcome::Armed { spawn: false });
    assert_eq!(lifecycle.epoch, 2);
    assert!(!lifecycle.session_live(1));
    lifecycle.mark_ready(1);
    assert!(!lifecycle.is_ready());
    lifecycle.mark_ready(2);
    assert!(lifecycle.is_ready());
}

#[test]
fn lifecycle_disarm_reports_whether_coverage_was_active_and_clears_readiness() {
    let mut lifecycle = CoverageLifecycle::IDLE;
    assert!(!lifecycle.disarm(), "ending idle coverage is a no-op");
    lifecycle.arm();
    lifecycle.mark_ready(1);
    assert!(lifecycle.disarm());
    assert!(!lifecycle.wanted && !lifecycle.is_ready());
    assert!(!lifecycle.session_live(1));
    assert!(!lifecycle.disarm(), "double end is a no-op");
    // A readiness announcement racing in after the end is ignored.
    lifecycle.mark_ready(1);
    assert!(!lifecycle.is_ready());
}

#[test]
fn lifecycle_rearm_after_spawn_failure_requests_a_new_thread() {
    let mut lifecycle = CoverageLifecycle::IDLE;
    assert_eq!(lifecycle.arm(), ArmOutcome::Armed { spawn: true });
    // Spawn failed: the caller disarms and leaves `thread_spawned` false.
    lifecycle.disarm();
    assert_eq!(lifecycle.arm(), ArmOutcome::Armed { spawn: true });
}

#[test]
fn session_reset_starts_a_fresh_interval_no_earlier_token_can_fall_inside() {
    let mut observation = stable_observation();
    observation.coverage_lost = true;
    observation.recheck_from = Some((observation.sample.unwrap(), 5));
    reset_observation_for_session(&mut observation, 1_000);
    assert_eq!(observation.started_at, 1_000);
    assert_eq!(observation.last_change_at, 1_000);
    assert_eq!(observation.last_poll_at, 1_000);
    assert!(observation.coverage_started_at.is_none());
    assert!(observation.sample.is_none());
    assert!(observation.recheck_from.is_none());
    assert!(!observation.coverage_lost);
    // A token captured in the previous interval is never valid against the new one.
    assert!(matches!(
        classify_continuity(&observation, 7, 9, 100, 11, 1_001, 250),
        FocusContinuityStatus::Invalid(_)
    ));
}

#[test]
fn ending_coverage_leaves_nothing_that_captures_or_validates_as_live() {
    let mut observation = stable_observation();
    assert!(hotkey_anchor_from_state(&observation, 100).is_some());
    clear_observation_after_session(&mut observation, 100);
    assert!(observation.coverage_started_at.is_none());
    assert!(observation.sample.is_none());
    assert!(hotkey_anchor_from_state(&observation, 101).is_none());
    assert_eq!(
        classify_continuity(&observation, 7, 9, 99, 11, 101, 250),
        FocusContinuityStatus::Invalid("focus_changed"),
    );
}

#[test]
fn begin_wait_returns_false_without_blocking_when_coverage_was_never_wanted() {
    // `wait_for_focus_coverage` must not sleep through its timeout when no
    // session is armed (the lifecycle is idle in this process unless another
    // test armed it; arming needs AppKit, which pure unit tests avoid).
    if focus_coverage_wanted() {
        return;
    }
    let started = Instant::now();
    assert!(!wait_for_focus_coverage(Duration::from_secs(5)));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn start_focus_observer_shim_spawns_no_thread() {
    start_focus_observer();
    start_focus_observer();
    assert!(!focus_coverage_wanted());
    assert!(!lock_coverage().thread_spawned);
}
