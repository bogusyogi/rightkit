#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{
    capture_recovery::*, transcript_text::*, unified_probe_lane::*, worker_api::*, worker_clock::*,
    worker_main::*,
};

/// Condition final dictation audio while retaining per-token timing so static-15
/// window commits and final-tail decode share one timed result contract.
pub(crate) fn transcribe_buffer_result(
    model: &mut AsrRuntime,
    buffer: &[f32],
    locked_lang: &mut Option<i32>,
) -> Result<TranscriptionResult, String> {
    transcribe_conditioned(buffer, BlankPolicy::Final, |conditioned| {
        model.with_inference_lease("final_asr", |model| {
            crate::asr::apply_utterance_bias(model);
            model.transcribe_final_result_under_lease_locked(conditioned, locked_lang)
        })
    })
}

pub(crate) fn transcribe_opening_buffer(
    model: &mut AsrRuntime,
    buffer: &[f32],
) -> Result<String, String> {
    transcribe_conditioned(buffer, BlankPolicy::Probe, |conditioned| {
        model.with_inference_lease("opening_command_asr", |model| {
            crate::asr::apply_utterance_bias(model);
            model.transcribe_under_lease(conditioned)
        })
    })
}

pub(crate) fn warm_main_asr(model: &mut AsrRuntime) -> Result<(), String> {
    let silence = vec![0.0; SAMPLE_RATE as usize];
    model.with_inference_lease("main_asr_warmup", |model| {
        crate::asr::apply_utterance_bias(model);
        model.transcribe_under_lease(&silence).map(|_| ())
    })
}

/// One keep-warm pass while a dictation records (Windows/DirectML). The single seam for keep-warm
/// pass content: today a full warm-up decode of 1 s of silence, as at startup. A cheaper pass
/// (ORT-18 `encoder`-only, or a static-length pass for ORT-01) belongs behind its own switch here,
/// so the worker's scheduling (`MainAsrMailbox::recv`) stays unchanged.
pub(crate) fn keep_warm_main_asr(model: &mut AsrRuntime) -> Result<(), String> {
    warm_main_asr(model)
}

pub(crate) fn finish_recording_transcript(
    model: &mut AsrRuntime,
    scheduled: &mut ScheduledStatic15,
    buffer: &[f32],
    generation: u64,
    locked_lang: &mut Option<i32>,
) -> Result<TranscriptionResult, String> {
    let stop_started = Instant::now();
    let scheduled_path = model.uses_scheduled_static15();
    let committed_samples_before = scheduled.committed_samples();
    let tail_samples = buffer.len().saturating_sub(committed_samples_before);
    let result = if scheduled_path {
        // Every uncommitted sample through the trigger cut is decoded here.
        // Scheduled windows remain the only source of already-committed text.
        scheduled.finish_result(buffer, |tail| {
            transcribe_buffer_result(model, tail, locked_lang)
        })
    } else {
        transcribe_buffer_result(model, buffer, locked_lang)
    };
    // Latency KPI: stop -> final transcript. `tail_samples` is what actually
    // had to be decoded synchronously after stop; on the scheduled path that
    // should be a small quiet-cut remainder regardless of total recording
    // length, which is the point of decoding the rest in the background.
    tracing::info!(
        generation,
        stop_to_final_ms = stop_started.elapsed().as_millis() as u64,
        buffer_secs = audio_secs(buffer),
        scheduled_path,
        committed_samples = committed_samples_before,
        tail_samples,
        ok = result.is_ok(),
        "stop-to-final latency"
    );
    match &result {
        Ok(result) => tracing::info!(
            generation,
            total_samples = buffer.len(),
            final_chars = result.text.chars().count(),
            final_words = result.text.split_whitespace().count(),
            final_empty = result.text.trim().is_empty(),
            timed_tokens = result.tokens.len(),
            "recording transcript assembly complete"
        ),
        Err(error) => tracing::error!(
            generation,
            total_samples = buffer.len(),
            error,
            "recording transcript assembly failed"
        ),
    }
    result
}

pub(crate) fn audio_secs(buffer: &[f32]) -> f32 {
    buffer.len() as f32 / SAMPLE_RATE as f32
}

pub(crate) struct CommandProbeLog<'a> {
    pub(crate) lane: &'static str,
    pub(crate) session_id: Option<&'a str>,
    pub(crate) probe_ms: u64,
    pub(crate) recognized_text: &'a str,
    pub(crate) classifier_result: &'static str,
    pub(crate) speech_start_sample: Option<usize>,
    pub(crate) command_start: usize,
    pub(crate) command_samples: usize,
    pub(crate) total_samples: usize,
    pub(crate) pending_prefix: bool,
    pub(crate) error: Option<&'a str>,
}

pub(crate) fn command_probe_payload(log: CommandProbeLog<'_>) -> serde_json::Value {
    let recognized = log.recognized_text.trim();
    json!({
        "event": "command_probe",
        "lane": log.lane,
        "session_id": log.session_id,
        "probe_ms": log.probe_ms,
        "recognized_text": log.recognized_text,
        "recognized_chars": recognized.chars().count(),
        "recognized_words": recognized.split_whitespace().count(),
        "recognized_empty": recognized.is_empty(),
        "classifier_result": log.classifier_result,
        "speech_start_sample": log.speech_start_sample,
        "command_start": log.command_start,
        "command_samples": log.command_samples,
        "total_samples": log.total_samples,
        "pending_prefix": log.pending_prefix,
        "error": log.error,
    })
}

pub(crate) fn trace_command_probe(log: CommandProbeLog<'_>) {
    emit_command_probe(command_probe_payload(log));
}

pub(crate) fn trace_recording_stop(
    session_id: Option<&str>,
    reason: &'static str,
    total_samples: usize,
    heard_voice: bool,
    speech_start_sample: Option<usize>,
    silence_for_ms: Option<u64>,
    send_enter: bool,
) {
    let payload = json!({
        "event": "recording_stop",
        "session_id": session_id,
        "reason": reason,
        "total_samples": total_samples,
        "audio_secs": audio_secs_from_samples(total_samples),
        "heard_voice": heard_voice,
        "speech_start_sample": speech_start_sample,
        "silence_for_ms": silence_for_ms,
        "send_enter": send_enter,
    });
    emit_command_probe(payload);
}

pub(crate) fn trace_recording_start(session_id: &str, phase: &'static str, elapsed_ms: u64) {
    let payload = json!({
        "event": "recording_start",
        "session_id": session_id,
        "phase": phase,
        "elapsed_ms": elapsed_ms,
    });
    emit_command_probe(payload);
}

pub(crate) fn audio_secs_from_samples(samples: usize) -> f32 {
    samples as f32 / SAMPLE_RATE as f32
}

pub(crate) fn emit_command_probe(payload: serde_json::Value) {
    if !crate::settings::diagnostics_enabled() {
        return;
    }
    let mut payload = payload;
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "ts_ms".to_string(),
            json!(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64),
        );
    }
    let redacted = rightkit_dictation_core::redact_diagnostic_event(payload);
    append_command_probe_event(&redacted);
}

// `append_command_probe_event` fires ~3-4x/sec while recording (every tail
// probe + standalone-command probe + start/stop trace). A disk stall (AV
// scan, slow/contended disk) must never backpressure live capture or grow
// memory without bound. Keep one FIFO writer with a bounded queue; telemetry
// is best-effort and drops when storage cannot keep up.
pub(crate) const COMMAND_PROBE_QUEUE_CAPACITY: usize = 256;
pub(crate) static COMMAND_PROBE_TX: OnceLock<std::sync::mpsc::SyncSender<serde_json::Value>> =
    OnceLock::new();
pub(crate) static COMMAND_PROBE_DROPPED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static COMMAND_PROBE_DISCONNECTED_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandProbeEnqueueResult {
    Queued,
    Full,
    Disconnected,
}

pub(crate) fn command_probe_writer() -> &'static std::sync::mpsc::SyncSender<serde_json::Value> {
    COMMAND_PROBE_TX.get_or_init(|| {
        let (tx, rx) =
            std::sync::mpsc::sync_channel::<serde_json::Value>(COMMAND_PROBE_QUEUE_CAPACITY);
        let spawned = std::thread::Builder::new()
            .name("hr-engine-command-probe-log".to_string())
            .spawn(move || {
                command_probe_writer_loop(rx);
            })
            .is_ok();
        if !spawned {
            tracing::warn!("command probe telemetry writer thread failed to spawn");
        }
        tx
    })
}

fn command_probe_writer_loop(rx: std::sync::mpsc::Receiver<serde_json::Value>) {
    while let Ok(payload) = rx.recv() {
        if crate::settings::diagnostics_enabled() {
            if let Err(err) = write_command_probe_event(&payload) {
                tracing::warn!(error = %err, "command probe telemetry write failed");
            }
        }
    }
}

pub(crate) fn enqueue_command_probe(
    sender: &std::sync::mpsc::SyncSender<serde_json::Value>,
    payload: serde_json::Value,
) -> CommandProbeEnqueueResult {
    match sender.try_send(payload) {
        Ok(()) => CommandProbeEnqueueResult::Queued,
        Err(std::sync::mpsc::TrySendError::Full(_)) => CommandProbeEnqueueResult::Full,
        Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
            CommandProbeEnqueueResult::Disconnected
        }
    }
}

pub(crate) fn append_command_probe_event(payload: &serde_json::Value) {
    if !crate::settings::diagnostics_enabled() {
        return;
    }
    match enqueue_command_probe(command_probe_writer(), payload.clone()) {
        CommandProbeEnqueueResult::Queued => {}
        CommandProbeEnqueueResult::Full => {
            let dropped =
                COMMAND_PROBE_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if dropped.is_power_of_two() {
                tracing::warn!(
                    dropped_events = dropped,
                    queue_capacity = COMMAND_PROBE_QUEUE_CAPACITY,
                    "command probe telemetry queue full; dropping diagnostic event"
                );
            }
        }
        CommandProbeEnqueueResult::Disconnected => {
            if !COMMAND_PROBE_DISCONNECTED_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("command probe telemetry writer unavailable; dropping events");
            }
        }
    }
}

fn write_command_probe_event(payload: &serde_json::Value) -> std::io::Result<()> {
    let path = crate::settings::app_data_root().join("engine-events.jsonl");
    write_event_at(&path, payload)
}

fn write_event_at(path: &Path, payload: &serde_json::Value) -> std::io::Result<()> {
    let payload = rightkit_dictation_core::redact_diagnostic_event(payload.clone());
    // rightkit-logs owns rotation: rotate before the incoming line would overflow the
    // limit (one `.1` generation, `<name>.1`), and reopen the current path per event so
    // clear/export can unlink it between events.
    rightkit_logs::JsonlStore::new(path, COMMAND_EVENTS_JSONL_MAX_BYTES)
        .with_rotate_before_overflow(true)
        .append_bounded(&payload)
}

pub(crate) fn command_probe_start_sample(
    speech_start_sample: Option<usize>,
    buffer_samples: usize,
) -> usize {
    // With a VAD-confirmed speech start, probe from there backward (covers
    // the word that armed the gesture + the trigger word itself). Otherwise,
    // fall back to the LAST CMD_WINDOW_SAMPLES of audio — same window the
    // zephyr_tail probe uses — so the standalone lane sees the same speech
    // and gets a chance to classify. Without this, VAD misses (common on
    // Windows WASAPI shared-mode where the VAD CNN was trained on cleaner
    // audio) leave the standalone probe staring at the full buffer's
    // pre-roll silence and failing the pre-decode voice gate.
    match speech_start_sample {
        Some(start) => start,
        None => buffer_samples.saturating_sub(CMD_WINDOW_SAMPLES),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TailProbeLane {
    Fast,
    Full,
}

pub(crate) fn tail_probe_start_sample(buffer_samples: usize, lane: TailProbeLane) -> usize {
    let window_samples = match lane {
        TailProbeLane::Fast => FAST_TAIL_WINDOW_SAMPLES,
        TailProbeLane::Full => TAIL_WINDOW_SAMPLES,
    };
    buffer_samples.saturating_sub(window_samples)
}

pub(crate) fn tail_text_ends_with_control_verb(text: &str) -> bool {
    let Some(last) = text.split_whitespace().last() else {
        return false;
    };
    // Reuse the canonical fuzzy verb vocabulary rather than maintaining a
    // second list in the worker. This only requests the reliable probe; it can
    // never fire a command by itself.
    rightkit_text_polish::text_pipeline::parse_control_command(&format!("zephyr {last}")).is_some()
}

/// A standalone command is candidate-only until a later opening probe resolves
/// to the same normalized action identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingOpeningAction {
    pub(crate) action_identity: String,
    pub(crate) total_samples: usize,
}

pub(crate) fn normalize_opening_action_text(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn normalized_opening_action_identity(
    action: &rightkit_voice_commands::command::CommandAction,
) -> String {
    use rightkit_voice_commands::command::CommandAction;

    match action {
        CommandAction::KeySequence { chords, .. } => format!(
            "key:{}",
            chords
                .iter()
                .map(|chord| normalize_opening_action_text(chord))
                .collect::<Vec<_>>()
                .join("|")
        ),
        CommandAction::LaunchApp { name } => {
            format!("launch:{}", normalize_opening_action_text(name))
        }
        CommandAction::RunShortcut { name } => {
            format!("shortcut:{}", normalize_opening_action_text(name))
        }
        CommandAction::Mouse {
            action,
            button,
            clicks,
            direction,
            page,
        } => format!(
            "mouse:{}:{}:{:?}:{}:{}",
            normalize_opening_action_text(action),
            button
                .as_deref()
                .map(normalize_opening_action_text)
                .unwrap_or_default(),
            clicks,
            direction
                .as_deref()
                .map(normalize_opening_action_text)
                .unwrap_or_default(),
            page,
        ),
        CommandAction::Power {
            op,
            requires_confirm,
        } => format!(
            "power:{}:{}",
            normalize_opening_action_text(op),
            requires_confirm
        ),
        CommandAction::LastPasteTransform { transform } => {
            format!("transform:{}", normalize_opening_action_text(transform))
        }
        CommandAction::Special { op } => format!("special:{}", normalize_opening_action_text(op)),
    }
}

/// Returns true only when two consecutive opening probes resolve to the same
/// action & their total sample count strictly advances. Ambiguous grammar
/// matches additionally require a pause-ready second observation.
pub(crate) fn opening_action_confirmation(
    pending: &mut Option<PendingOpeningAction>,
    action_identity: &str,
    total_samples: usize,
    pause_ready: bool,
    requires_pause_ready: bool,
) -> bool {
    let confirmed = pending.as_ref().is_some_and(|prior| {
        prior.action_identity == action_identity
            && total_samples > prior.total_samples
            && (!requires_pause_ready || pause_ready)
    });
    if confirmed {
        *pending = None;
        return true;
    }

    *pending = Some(PendingOpeningAction {
        action_identity: action_identity.to_owned(),
        total_samples,
    });
    false
}

pub(crate) fn should_run_full_tail_fallback(
    fast_text: &str,
    candidate_armed: bool,
    new_audio_samples: usize,
) -> bool {
    (candidate_armed || tail_text_ends_with_control_verb(fast_text))
        && new_audio_samples >= TAIL_PREFIX_NEW_AUDIO_SAMPLES
}

pub(crate) fn tail_probe_due(total_samples: usize, audio_arrived: bool) -> bool {
    total_samples > SAMPLE_RATE as usize / 4 && audio_arrived
}

pub(crate) fn tail_prefix_expired(elapsed: Duration) -> bool {
    elapsed >= Duration::from_millis(TAIL_PREFIX_GRACE_MS)
}

pub(crate) fn should_keep_control_candidate_armed(
    control_found: bool,
    saw_control_candidate: bool,
) -> bool {
    !control_found && saw_control_candidate
}

pub(crate) fn opening_command_due_at(
    heard_voice: bool,
    pending_prefix: bool,
    pause_ready: bool,
    command_audio_samples: usize,
    last_submitted_samples: usize,
    last_check: Option<worker_clock::WorkerInstant>,
    now: worker_clock::WorkerInstant,
) -> bool {
    heard_voice
        && (command_audio_samples <= CMD_WINDOW_SAMPLES || pending_prefix)
        && command_audio_samples >= CMD_MIN_PROBE_SAMPLES
        && (pause_ready
            || last_check.is_none_or(|last_check| {
                now.duration_since(last_check) >= Duration::from_millis(CMD_RETRY_MS)
            }))
        && (command_audio_samples
            >= last_submitted_samples.saturating_add(CMD_NEW_AUDIO_PROBE_SAMPLES)
            || (pause_ready && !pending_prefix))
}

pub(crate) fn observe_voice_chunk_at(
    chunk: &[f32],
    last_voice_at: &mut worker_clock::WorkerInstant,
    now: worker_clock::WorkerInstant,
    heard_voice: &mut bool,
    checked_this_pause: &mut bool,
    mut speech_observer: impl FnMut(&[f32]) -> bool,
    speech_start_sample: &mut Option<usize>,
    chunk_start_sample: usize,
) {
    if chunk.is_empty() {
        return;
    }
    let confirmed = speech_observer(chunk);
    if confirmed {
        if speech_start_sample.is_none() {
            *speech_start_sample = Some(chunk_start_sample.saturating_sub(CMD_PREROLL_SAMPLES));
        }
        *last_voice_at = now;
        *heard_voice = true;
        *checked_this_pause = false;
    }
}

pub(crate) fn observe_voice_chunk_with_terminal_silence(
    chunk: &[f32],
    last_voice_at: &mut worker_clock::WorkerInstant,
    now: worker_clock::WorkerInstant,
    heard_voice: &mut bool,
    checked_this_pause: &mut bool,
    speech_vad: &mut crate::vad::SpeechVad,
    speech_start_sample: &mut Option<usize>,
    chunk_start_sample: usize,
    terminal_silences: &mut TerminalSilenceTracker,
) -> bool {
    let verdicts = speech_vad.observe_frames(chunk);
    let confirmed = verdicts
        .as_ref()
        .is_some_and(|frames| frames.iter().any(|speech| *speech));
    observe_voice_chunk_at(
        chunk,
        last_voice_at,
        now,
        heard_voice,
        checked_this_pause,
        |_| confirmed,
        speech_start_sample,
        chunk_start_sample,
    );
    terminal_silences.observe(chunk_start_sample, chunk.len(), verdicts.as_deref())
}

pub(crate) fn pause_can_close_command_probe(heard_voice: bool, pause_ready: bool) -> bool {
    heard_voice && pause_ready
}

pub(crate) fn mark_pause_probe_submitted(checked_this_pause: &mut bool, pause_ready: bool) {
    if pause_ready {
        *checked_this_pause = true;
    }
}

pub(crate) fn mark_opening_prefix_result_at(
    checked_this_pause: &mut bool,
    pending_command_prefix_since: &mut Option<worker_clock::WorkerInstant>,
    now: worker_clock::WorkerInstant,
) {
    *checked_this_pause = false;
    pending_command_prefix_since.get_or_insert(now);
}

// mpsc has no "peek and put back", so a command
// pulled here via `try_recv` (to gate a probe launch — see `probe_gate_clear`
// below) is stashed in `stash` rather than dropped. worker/worker_main.rs's
// dispatcher checks `stash` FIRST, before calling `recv_timeout` again, so
// the stashed command is handled on the very next loop iteration — no
// command is ever lost, and since this is the only call site that pulls from
// `cmd_rx` outside worker/worker_main.rs, at most one command is ever in flight
// through the stash at a time (ordering relative to the channel is preserved).
pub(crate) fn stash_pending_command(cmd_rx: &Receiver<WorkerCmd>, stash: &mut Option<WorkerCmd>) {
    if stash.is_none() {
        if let Ok(cmd) = cmd_rx.try_recv() {
            *stash = Some(cmd);
        }
    }
}

pub(crate) fn stop_or_cancel_stashed(stash: &Option<WorkerCmd>) -> bool {
    matches!(
        stash,
        Some(WorkerCmd::StopRecording { .. }) | Some(WorkerCmd::Cancel)
    )
}

/// True when it's safe to pay for a probe decode: peeks the command channel
/// and returns false if a Stop/Cancel is already queued, so a probe never
/// launches into a session that's about to end (the worker is single-threaded,
/// so a probe decode — bounded per F4(a) for the Windows Whisper CLI lane, but
/// still real cost for any backend — would otherwise delay Stop/Cancel
/// handling until it returns). A queued command of any OTHER kind is also
/// stashed (never dropped) but does not block the probe.
pub(crate) fn probe_gate_clear(
    cmd_rx: &Receiver<WorkerCmd>,
    stash: &mut Option<WorkerCmd>,
) -> bool {
    stash_pending_command(cmd_rx, stash);
    !stop_or_cancel_stashed(stash)
}

pub(crate) fn transcribe_probe_buffer(
    model: &mut AsrRuntime,
    buffer: &[f32],
) -> Result<String, String> {
    transcribe_conditioned(buffer, BlankPolicy::Probe, |conditioned| {
        model.with_inference_lease("command_trigger_probe", |model| {
            crate::asr::apply_probe_context_bias(model);
            model.transcribe_under_lease(conditioned)
        })
    })
}

/// File audio gets the same conditioning authority as dictation, applied once
/// when a file job is created (the job then steps over the conditioned samples).
pub(crate) fn condition_file_audio(buffer: &[f32]) -> Vec<f32> {
    let audio_policy = "default";
    rightkit_dictation_core::audio_conditioning::condition_for_asr(
        buffer,
        SAMPLE_RATE,
        audio_policy,
    )
    .to_vec()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlankPolicy {
    Final,
    Probe,
}

pub(crate) fn transcribe_conditioned<T, F>(
    buffer: &[f32],
    blank_policy: BlankPolicy,
    mut transcribe: F,
) -> Result<T, String>
where
    F: FnMut(&[f32]) -> Result<T, String>,
    T: TranscriptText,
{
    #[cfg(target_os = "macos")]
    let _asr_window_input = crate::coreml_asr::install_asr_window_input(
        crate::coreml_asr::AsrWindowInput::from_raw_pcm(
            buffer,
            Some(match blank_policy {
                BlankPolicy::Final => "final",
                BlankPolicy::Probe => "opening_probe",
            }),
        ),
    );
    let t = Instant::now();
    let audio_policy = "default";
    let conditioned = rightkit_dictation_core::audio_conditioning::condition_for_asr(
        buffer,
        SAMPLE_RATE,
        audio_policy,
    );
    let raw_rms = rightkit_dictation_core::audio_conditioning::rms(buffer);

    let conditioned_rms = rightkit_dictation_core::audio_conditioning::rms(&conditioned);

    let result = transcribe(&conditioned);
    let (text_chars, text_words, text_empty) = result
        .as_ref()
        .map(|value| {
            let text = value.transcript_text();
            (
                text.chars().count(),
                text.split_whitespace().count(),
                text.trim().is_empty(),
            )
        })
        .unwrap_or((0, 0, true));
    tracing::info!(
        kind = ?blank_policy,
        decode_ms = t.elapsed().as_millis() as u64,
        samples = buffer.len(),
        audio_secs = buffer.len() as f32 / SAMPLE_RATE as f32,
        raw_rms,
        conditioned_rms,
        text_chars,
        text_words,
        text_empty,
        ok = result.is_ok(),
        "transcribe finished"
    );

    // Opening probes may remain blank. Final dictation retries the same
    // conditioned audio on the same route once, recovering transient blank
    // decodes without changing models, stages, or device selection.
    let blank_result = matches!(result, Ok(ref value) if value.transcript_text().trim().is_empty())
        || (blank_policy == BlankPolicy::Final
            && matches!(
                result,
                Err(ref error)
                    if error == crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR
            ));
    if blank_result {
        if blank_policy == BlankPolicy::Probe {
            return Ok(T::empty());
        }
        tracing::warn!("[hr-asr] blank final transcript; retrying once");
        let retry = transcribe(&conditioned);
        let (retry_chars, retry_words, retry_empty) = retry
            .as_ref()
            .map(|value| {
                let text = value.transcript_text();
                (
                    text.chars().count(),
                    text.split_whitespace().count(),
                    text.trim().is_empty(),
                )
            })
            .unwrap_or((0, 0, true));
        tracing::info!(
            kind = ?blank_policy,
            raw_rms,
            conditioned_rms,
            retry_chars,
            retry_words,
            retry_empty,
            ok = retry.is_ok(),
            "retry transcribe finished"
        );
        if matches!(retry, Ok(ref value) if value.transcript_text().trim().is_empty())
            || matches!(
                retry,
                Err(ref error) if error == crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR
            )
        {
            return Err(crate::asr::AUDIBLE_BLANK_TRANSCRIPTION_ERROR.to_string());
        }
        return retry;
    }
    match result {
        Ok(value) if value.transcript_text().trim().is_empty() => Ok(value),
        other => other,
    }
}
