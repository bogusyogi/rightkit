use super::*;

#[test]
fn retry_gate_uses_worker_time() {
    let submitted =
        worker_clock::WorkerInstant::from_duration_since_start(Duration::from_millis(1_000));
    let early = worker_clock::WorkerInstant::from_duration_since_start(Duration::from_millis(
        1_000 + CMD_RETRY_MS - 1,
    ));
    let due = worker_clock::WorkerInstant::from_duration_since_start(Duration::from_millis(
        1_000 + CMD_RETRY_MS,
    ));

    assert!(!opening_command_due_at(
        true,
        false,
        false,
        CMD_MIN_PROBE_SAMPLES,
        0,
        Some(submitted),
        early,
    ));
    assert!(opening_command_due_at(
        true,
        false,
        false,
        CMD_MIN_PROBE_SAMPLES,
        0,
        Some(submitted),
        due,
    ));
}

#[test]
fn voice_clock_records_supplied_worker_time() {
    let now = worker_clock::WorkerInstant::from_duration_since_start(Duration::from_millis(42));
    let mut last_voice_at = worker_clock::WorkerInstant::ZERO;
    let mut heard_voice = false;
    let mut checked_this_pause = true;
    let mut speech_start_sample = None;

    observe_voice_chunk_at(
        &[0.25],
        &mut last_voice_at,
        now,
        &mut heard_voice,
        &mut checked_this_pause,
        |_| true,
        &mut speech_start_sample,
        CMD_PREROLL_SAMPLES,
    );

    assert_eq!(last_voice_at, now);
    assert!(heard_voice);
    assert!(!checked_this_pause);
    assert_eq!(speech_start_sample, Some(0));
}
