#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{delivery_history::*, engine::*, engine_impl::*, engine_support::*};

#[cfg(any(test, target_os = "windows"))]
fn selected_text_matches_live(expected: Option<&str>, live: Option<&str>) -> bool {
    let Some(expected) = expected.map(str::trim).filter(|text| !text.is_empty()) else {
        return false;
    };
    live.map(str::trim).filter(|text| !text.is_empty()) == Some(expected)
}

#[cfg(test)]
mod tests {
    use super::selected_text_matches_live;

    #[test]
    fn selected_text_live_guard_accepts_same_nonempty_selection() {
        assert!(selected_text_matches_live(
            Some("  selected "),
            Some("selected")
        ));
    }

    #[test]
    fn selected_text_live_guard_rejects_replaced_selection() {
        assert!(!selected_text_matches_live(Some("first"), Some("second")));
    }

    #[test]
    fn selected_text_live_guard_rejects_empty_or_missing_selection() {
        assert!(!selected_text_matches_live(Some("  "), Some("value")));
        assert!(!selected_text_matches_live(Some("value"), Some("  ")));
        assert!(!selected_text_matches_live(None, Some("value")));
        assert!(!selected_text_matches_live(Some("value"), None));
    }
}

#[cfg(test)]
mod last_delivery_capture_tests {
    use super::*;

    fn target() -> rightkit_transcript_library::delivery::TargetSnapshot {
        rightkit_transcript_library::delivery::TargetSnapshot::empty()
    }

    fn capture_for(runtime: &mut EngineRuntime, transcript: &str) -> FinalizeCapture {
        runtime.begin_recording("s1", None, None).unwrap();
        runtime.begin_stop("s1", false, false).unwrap();
        let result = Ok(crate::worker::FinalTranscript {
            text: transcript.to_string(),
            tokens: Vec::new(),
            stop_evidence: crate::worker::StopEvidence::Manual,
        });
        match runtime.finalize_phase1_capture_typed("s1", result, None, None) {
            FinalizePhase1::Continue { capture, .. } => capture,
            _ => panic!("phase 1 should continue"),
        }
    }

    /// "delete that" and the casing commands read the last delivery's text from this snapshot.
    #[test]
    fn the_snapshot_carries_the_text_the_shell_last_pasted() {
        let mut runtime = EngineRuntime::new(std::path::PathBuf::from("."));
        runtime.set_last_delivery(DeliveryRecord::new(
            "d1",
            "hello world",
            rightkit_transcript_library::delivery::DeliveryOutcome::Pasted,
            target(),
        ));
        let capture = capture_for(&mut runtime, "delete that");
        assert_eq!(capture.last_delivery_text.as_deref(), Some("hello world"));
    }

    #[test]
    fn a_fresh_engine_has_no_last_delivery_text() {
        let mut runtime = EngineRuntime::new(std::path::PathBuf::from("."));
        let capture = capture_for(&mut runtime, "delete that");
        assert_eq!(capture.last_delivery_text, None);
    }
}

// Finalize — capture -> unlock -> commit.
//
// Finalize must not hold the `EngineRuntime` mutex across stop-time
// UIA/AX refresh, local + cloud (L1/L2/L3) polish HTTP (up to the l3_cleanup
// hard cap), or OS-level delivery/command dispatch: holding the lock there
// would queue `CancelDictation`/`GetState`/a new `StartDictation` behind
// whatever finalize happens to be running. So the work splits into three
// phases:
//   1. `finalize_phase1_capture` (locked, brief) — validates the stale
//      guard, resolves the cheap/pure control-command and ai-transform-tail
//      parsing, and snapshots everything phase 2 needs into an owned
//      `FinalizeCapture`. Short-circuit outcomes (stale no-op, transcribe
//      error, `zephyr cancel`) are resolved here and returned as `Done`.
//   2. `finalize_phase2_process` (a free function — NOT a method, so it
//      cannot accidentally borrow the runtime) — runs with no runtime lock
//      held at all: the stop-time field-context refresh, local + cloud
//      polish, standalone-command dispatch, and delivery. Polled via
//      `is_current` at the natural step boundaries (classify -> dispatch,
//      context -> polish, polish -> deliver) so a Cancel or a new Start that
//      raced ahead stops this finalize short of any further OS-visible
//      action.
//   3. `finalize_phase3_commit` (locked, brief) — re-validates the finalize
//      generation and commits `last_delivery` / `recent` / `state` / focus
//      reset, or discards the result if superseded.
//
// `finalize_transcript_with_audio_secs` composes all three back-to-back for
// direct callers (tests, any other `&mut EngineRuntime` caller) — same
// external signature, same observable behavior, since nothing else can run
// concurrently against an owned `&mut self`. `finalize_unlocked` is the
// `Arc<Mutex<EngineRuntime>>`-aware entry point the IPC worker-event-pump
// uses to actually release the lock across phase 2.

/// Everything phase 2 needs, snapshotted under the runtime lock in phase 1.
/// Owned/cloned so phase 2 can run without borrowing `EngineRuntime` at all.
pub(crate) struct FinalizeCapture {
    pub(crate) session_id: String,
    pub(crate) stop_started_at: Option<Instant>,
    pub(crate) transcript: String,
    pub(crate) stop_evidence: crate::worker::StopEvidence,
    pub(crate) standalone_action: Option<rightkit_voice_commands::command::CommandAction>,
    pub(crate) send_enter: bool,
    pub(crate) ai_transform: Option<rightkit_text_polish::text_pipeline::AiTransformIntent>,
    pub(crate) audio_secs: Option<f32>,
    pub(crate) focus: Arc<Mutex<FocusTracker>>,
    pub(crate) last_delivery_text: Option<String>,
    /// `StopKind::CancelToHistory` finalize (engine-side: `pending_local_only`,
    /// mirroring `send_enter`/`pending_send_enter`). Phase 2 runs ASR and L0
    /// local polish to completion exactly like a normal Stop, but skips the
    /// cloud L1/L2/L3 polish and ai-transform (prompt/summarize) lanes so a
    /// cancelled utterance's text is never sent to the cloud.
    /// Named `local_only_stop` (not `local_only`) to avoid colliding with
    /// the unrelated local `local_only` STRING binding phase 2 already uses
    /// for the deterministic L0-polished text (see `finalize_phase2_process`).
    pub(crate) local_only_stop: bool,
    pub(crate) recording_context: Option<rightkit_dictation_core::engine::RecordingContext>,
    pub(crate) context_request_attempted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) context_original_binding: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// What phase 3 must commit once phase 2 resolves an outcome, IF the
/// finalize generation is still current. Every non-short-circuited exit path
/// of the original function ended with the SAME two mutations (state ->
/// Idle, focus reset), optionally preceded by a `last_delivery` write and
/// (only for the normal in-app delivery path) a `recent` push — this struct
/// carries exactly that, so phase 3 doesn't need to re-inspect the outcome.
pub(crate) struct FinalizeCommit {
    pub(crate) outcome: FinalizeOutcome,
    pub(crate) delivery_record: Option<DeliveryRecord>,
    pub(crate) push_recent: bool,
}

impl FinalizeCommit {
    pub(crate) fn outcome_only(outcome: FinalizeOutcome) -> Self {
        Self {
            outcome,
            delivery_record: None,
            push_recent: false,
        }
    }

    pub(crate) fn with_record(
        outcome: FinalizeOutcome,
        record: DeliveryRecord,
        push_recent: bool,
    ) -> Self {
        Self {
            outcome,
            delivery_record: Some(record),
            push_recent,
        }
    }

    /// Superseded by a Cancel or a new Start mid-phase-2 (see `is_current`
    /// checkpoints below). No delivery/dispatch happened past this point;
    /// phase 3's own generation check will independently discard this too,
    /// so this is belt-and-suspenders, not the sole guard.
    pub(crate) fn discard() -> Self {
        Self {
            outcome: FinalizeOutcome::NoOp,
            delivery_record: None,
            push_recent: false,
        }
    }
}

pub(crate) enum FinalizePhase1 {
    /// Already resolved under the phase-1 lock — no phase 2/3 needed.
    Done(Result<FinalizeOutcome, String>),
    /// Proceed to phase 2 with this snapshot; `generation` is the
    /// `EngineRuntime::sequence` value captured at snapshot time, checked
    /// again at phase 3 (and polled mid-phase-2) to detect a superseding
    /// Cancel or new Start.
    Continue {
        capture: FinalizeCapture,
        generation: u64,
    },
}

/// Log-and-return-true helper for the phase-2 supersession checkpoints:
/// true means "stop here, this finalize was superseded" (a Cancel or a new
/// Start bumped the generation counter since phase 1 captured it).
pub(crate) fn discard_if_superseded(
    is_current: &dyn Fn() -> bool,
    session_id: &str,
    step: &'static str,
) -> bool {
    if is_current() {
        false
    } else {
        tracing::info!(
            session_id,
            step,
            "finalize_superseded_skipping_remaining_work"
        );
        true
    }
}

/// Final ASR text is the sole transform-routing authority. A trailing
/// `prompt` or `summarize` selects its layer and is removed in the same
/// operation; token timing, pauses, VAD, and KWS never participate.
pub(crate) fn take_final_ai_transform(
    transcript: &mut String,
) -> Option<rightkit_text_polish::text_pipeline::AiTransformIntent> {
    let command = rightkit_text_polish::text_pipeline::parse_ai_transform_command(transcript)?;
    *transcript = command.clean_text;
    Some(command.intent)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod deterministic_final_ai_transform_tests {
    use super::*;
    use rightkit_text_polish::text_pipeline::AiTransformIntent;

    #[test]
    fn trailing_prompt_always_routes_l2_and_is_removed_once() {
        let mut transcript = "Use the word prompt".to_string();
        assert_eq!(
            take_final_ai_transform(&mut transcript),
            Some(AiTransformIntent::Prompt)
        );
        assert_eq!(transcript, "Use the word");
        assert_eq!(take_final_ai_transform(&mut transcript), None);
    }

    #[test]
    fn trailing_summarize_always_routes_l3_and_is_removed_once() {
        let mut transcript = "Check recent recordings summarize".to_string();
        assert_eq!(
            take_final_ai_transform(&mut transcript),
            Some(AiTransformIntent::Summarize)
        );
        assert_eq!(transcript, "Check recent recordings");
        assert_eq!(take_final_ai_transform(&mut transcript), None);
    }

    #[test]
    fn ordinary_final_text_stays_l1_and_is_unchanged() {
        let mut transcript = "Prompt appears mid sentence then work continues".to_string();
        assert_eq!(take_final_ai_transform(&mut transcript), None);
        assert_eq!(
            transcript,
            "Prompt appears mid sentence then work continues"
        );
    }
}

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
#[allow(clippy::items_after_test_module)]
mod stale_selection_fallback_tests {
    use super::*;
    use rightkit_transcript_library::delivery::{DeliveryOutcome, TargetSnapshot};

    fn blank_target() -> TargetSnapshot {
        TargetSnapshot {
            process_id: None,
            process_name: None,
            window_title: None,
            window_handle: None,
            focused_control_handle: None,
            foreground_target: None,
            focused_text_input: None,
            is_elevated: None,
            stop_continuity: None,
            exe_path: None,
        }
    }

    /// Goal 3 (2026-09-14 mac/windows divergence fix): a selection that
    /// changed underneath a captured AI-transform must fall back to the
    /// clipboard on BOTH platforms, never lose the dictated text. This
    /// exercises the shared commit builder used by both platforms'
    /// stale-selection checks in `finalize_phase2_process` (a live AX/UIA
    /// probe cannot be driven deterministically in a unit test, so the
    /// probe outcome — "not live" — is asserted directly here rather than
    /// through a real focus tracker).
    #[test]
    fn stale_selection_falls_back_to_copy_instead_of_dropping_text() {
        let commit = stale_selection_fallback_commit(
            "the transformed text",
            None,
            Some(1200),
            blank_target(),
            false,
        );
        match commit.outcome {
            FinalizeOutcome::Delivery { record, send_enter } => {
                assert!(!send_enter);
                assert!(matches!(
                    record.outcome,
                    DeliveryOutcome::CopiedFallback {
                        reason: CopyFallbackReason::FocusChanged
                    }
                ));
            }
            other => panic!("expected Delivery/CopiedFallback, got {other:?}"),
        }
        assert!(commit.delivery_record.is_some());
        assert!(!commit.push_recent);
    }
}

impl EngineRuntime {
    /// Phase 1 (locked, brief) of finalize — see module doc comment.
    pub(crate) fn finalize_phase1_capture(
        &mut self,
        session_id: &str,
        result: Result<String, String>,
        audio_secs: Option<f32>,
    ) -> FinalizePhase1 {
        self.finalize_phase1_capture_typed(
            session_id,
            result.map(|text| crate::worker::FinalTranscript {
                text,
                tokens: Vec::new(),
                stop_evidence: crate::worker::StopEvidence::Manual,
            }),
            audio_secs,
            None,
        )
    }

    pub(crate) fn finalize_phase1_capture_typed(
        &mut self,
        session_id: &str,
        result: Result<crate::worker::FinalTranscript, String>,
        audio_secs: Option<f32>,
        standalone_action: Option<rightkit_voice_commands::command::CommandAction>,
    ) -> FinalizePhase1 {
        // Stale guard — if the active session moved on (cancel, new session,
        // watchdog-after-real), drop this final. The shell also enforces this
        // guard; defense in depth.
        let active = self.interaction_is_live(session_id)
            && matches!(&self.state, EngineState::Transcribing { session_id: s } if s == session_id);
        if !active {
            return FinalizePhase1::Done(Ok(FinalizeOutcome::NoOp));
        }
        let stop_started_at = self.stop_started_at.take();
        let send_enter = std::mem::take(&mut self.pending_send_enter);
        let local_only_stop = std::mem::take(&mut self.pending_local_only);
        let (mut transcript, stop_evidence) = match result {
            Ok(final_transcript) => (final_transcript.text, final_transcript.stop_evidence),
            Err(message) => {
                self.record_terminal_tombstone(session_id);
                self.state = EngineState::Error {
                    session_id: session_id.to_string(),
                    message: message.clone(),
                };
                trace_stop_to_engine_outcome(stop_started_at, "error");
                return FinalizePhase1::Done(Err(message));
            }
        };
        // Wake command authority arrives only from Sherpa through
        // `pending_send_enter` or the worker's cancel event. Final main-ASR
        // text remains dictation; it cannot select STOP, SEND, or CANCEL.
        let mut ai_transform = take_final_ai_transform(&mut transcript).inspect(|intent| {
            tracing::info!(
                intent = ?intent,
                parser = "final_asr_lexical",
                "ai_transform_tail_detected"
            );
        });
        // Field bug 2026-07-16: saying just "summarize" (ASR often adds a
        // leading filler the tail parser strips) matched the TAIL lane with
        // EMPTY content — L3 then summarized nothing and pasted the model's
        // "No content provided to summarize." An empty-content summarize tail
        // IS a bare trigger: normalize it back so phase 2's selection lanes
        // (captured UIA selection, then the Ctrl+C copy-fallback) handle it.
        if matches!(
            ai_transform,
            Some(rightkit_text_polish::text_pipeline::AiTransformIntent::Summarize)
        ) && transcript.trim().is_empty()
        {
            tracing::info!("ai_transform_empty_tail_treated_as_bare_trigger");
            transcript = "summarize".to_string();
            ai_transform = None;
        }
        let last_delivery_text = self.last_delivery.as_ref().map(|r| r.transcript.clone());
        let generation = self.sequence;
        FinalizePhase1::Continue {
            capture: FinalizeCapture {
                session_id: session_id.to_string(),
                stop_started_at,
                transcript,
                stop_evidence,
                standalone_action,
                send_enter,
                ai_transform,
                audio_secs,
                focus: self.focus.clone(),
                last_delivery_text,
                local_only_stop,
                recording_context: self.focus.lock().recording_start_context(),
                context_request_attempted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                    false,
                )),
                context_original_binding: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                    false,
                )),
            },
            generation,
        }
    }

    /// Phase 3 (locked, brief) of finalize — see module doc comment. Applies
    /// `commit` only if `expected_generation` still matches the live counter
    /// AND the runtime is still `Transcribing` this same session — either
    /// mismatching means a Cancel or a new Start raced ahead while phase 2
    /// ran unlocked, so this result is discarded instead of clobbering
    /// whatever is running now.
    pub(crate) fn finalize_phase3_commit(
        &mut self,
        session_id: &str,
        expected_generation: u64,
        commit: FinalizeCommit,
    ) -> FinalizeOutcome {
        let current = self.sequence == expected_generation
            && self.interaction_is_live(session_id)
            && matches!(&self.state, EngineState::Transcribing { session_id: s } if s == session_id);
        if !current {
            tracing::warn!(
                session_id,
                expected_generation,
                current_generation = self.sequence,
                "finalize_commit_discarded_stale_generation"
            );
            return FinalizeOutcome::NoOp;
        }
        if let Some(record) = commit.delivery_record {
            self.last_delivery = Some(record.clone());
            if commit.push_recent {
                self.push_recent(record);
            }
        }
        self.record_terminal_tombstone(session_id);
        self.state = EngineState::Idle;
        self.focus.lock().reset();
        commit.outcome
    }

    /// Apply a finalized transcript and return the sidecar-owned outcome for
    /// the shell to mirror into its UI state/history. Composes phase 1 + 2 +
    /// 3 back-to-back — for a direct `&mut EngineRuntime` caller (tests, any
    /// non-IPC caller) there is no concurrent lock holder to release the
    /// mutex FOR, so this stays a single synchronous call with identical
    /// external behavior to before the F3 split.
    pub fn finalize_transcript_with_audio_secs(
        &mut self,
        session_id: &str,
        result: Result<String, String>,
        audio_secs: Option<f32>,
    ) -> Result<FinalizeOutcome, String> {
        let finalize_started = Instant::now();
        let phase1_started = Instant::now();
        let (capture, generation) =
            match self.finalize_phase1_capture(session_id, result, audio_secs) {
                FinalizePhase1::Done(result) => return result,
                FinalizePhase1::Continue {
                    capture,
                    generation,
                } => (capture, generation),
            };
        let phase1_ms = phase1_started.elapsed().as_secs_f64() * 1_000.0;
        let phase2_started = Instant::now();
        let commit = finalize_phase2_process(capture, &|| true)?;
        let phase2_ms = phase2_started.elapsed().as_secs_f64() * 1_000.0;
        let phase3_started = Instant::now();
        let outcome = self.finalize_phase3_commit(session_id, generation, commit);
        let phase3_ms = phase3_started.elapsed().as_secs_f64() * 1_000.0;
        tracing::info!(
            target: "rightkit_finalize_timing",
            session_id,
            phase1_ms,
            phase2_ms,
            phase3_ms,
            finalize_total_ms = finalize_started.elapsed().as_secs_f64() * 1_000.0,
            "engine_finalize_stages"
        );
        Ok(outcome)
    }
}

/// Run finalize using the capture -> unlock -> commit split (finding F3).
/// Phase 1 — and, for the `AutoStop` worker event, the atomic `begin_stop`
/// that must share its lock acquisition with phase 1's stale-check — runs
/// under a BRIEF runtime-mutex hold. Phase 2 (stop-time UIA/AX refresh,
/// local + cloud L1/L2/L3 polish, delivery/standalone-command dispatch) runs
/// with NO runtime lock held at all, so `CancelDictation`/`GetState`/a new
/// `StartDictation` are never blocked behind an in-flight cloud-polish HTTP
/// call or an OS paste/dispatch. Phase 3 re-acquires the lock only long
/// enough to verify the finalize generation is still current and commit; on
/// a mismatch the result is discarded (`Ok(FinalizeOutcome::NoOp)`) instead
/// of clobbering whatever ran after this finalize was captured.
///
/// `begin_stop_send_enter` is `Some(send_enter)` only for the final `AutoStop`
/// worker event. Its earlier `AutoStopBegin` normally moved
/// `Recording -> Transcribing` before final ASR; this idempotent call closes
/// the race if final result reaches pump before begin signal. Manual-stop
/// `TranscriptFinal` uses `None` because `StopDictation` already began stop.
pub(crate) fn finalize_unlocked_typed(
    runtime: &Arc<Mutex<EngineRuntime>>,
    session_id: &str,
    result: Result<crate::worker::FinalTranscript, String>,
    audio_secs: Option<f32>,
    begin_stop_send_enter: Option<bool>,
    standalone_action: Option<rightkit_voice_commands::command::CommandAction>,
) -> Result<FinalizeOutcome, String> {
    let phase1 = {
        let mut rt = runtime.lock();
        if let Some(send_enter) = begin_stop_send_enter {
            // AutoStop (hands-free zephyr stop/send tail) never carries the
            // cancel-to-history intent — that only originates from the
            // shell's explicit `Request::StopDictation { local_only: true }`
            // dispatch, which calls `begin_stop` directly (see
            // `ipc/dispatch.rs`).
            let _ = rt.begin_stop(session_id, send_enter, false);
        }
        rt.finalize_phase1_capture_typed(session_id, result, audio_secs, standalone_action)
    };
    let (capture, generation) = match phase1 {
        FinalizePhase1::Done(result) => return result,
        FinalizePhase1::Continue {
            capture,
            generation,
        } => (capture, generation),
    };
    let check_runtime = runtime.clone();
    let is_current = move || check_runtime.lock().sequence_matches(generation);
    let commit = finalize_phase2_process(capture, &is_current)?;
    let mut rt = runtime.lock();
    Ok(rt.finalize_phase3_commit(session_id, generation, commit))
}

/// Phase 2 (UNLOCKED) of finalize — see module doc comment. A free function,
/// not a method: it must not be able to borrow `EngineRuntime`, since the
/// whole point is that no runtime lock is held while this runs. Everything
/// it needs comes from `capture`; everything it calls is a free function
/// (text_polish / l3_cleanup / command_classify / command_dispatch / focus /
/// delivery) or a method on the separately-locked `FocusTracker`.
/// Build the copy-fallback commit for a selected-transform delivery whose
/// live selection no longer matches what was captured at stop. Pulled out
/// of `finalize_phase2_process` (shared by both platforms' stale-selection
/// checks) so it can be exercised directly without a live AX/UIA probe.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub(crate) fn stale_selection_fallback_commit(
    transcript: &str,
    raw_text: Option<String>,
    recording_ms: Option<u64>,
    target: rightkit_transcript_library::delivery::TargetSnapshot,
    send_enter: bool,
) -> FinalizeCommit {
    let record = copy_fallback_record(transcript, CopyFallbackReason::FocusChanged, target)
        .with_raw(raw_text, recording_ms);
    FinalizeCommit::with_record(
        FinalizeOutcome::Delivery {
            record: record.clone(),
            send_enter,
        },
        record,
        false,
    )
}

pub(crate) fn finalize_phase2_process(
    capture: FinalizeCapture,
    is_current: &dyn Fn() -> bool,
) -> Result<FinalizeCommit, String> {
    let FinalizeCapture {
        session_id,
        stop_started_at,
        mut transcript,
        stop_evidence: _stop_evidence,
        standalone_action,
        send_enter,
        mut ai_transform,
        audio_secs,
        focus,
        last_delivery_text,
        local_only_stop,
        recording_context,
        context_request_attempted,
        context_original_binding,
    } = capture;

    // Standalone voice commands are a Pro feature (root CLAUDE.md §6). The
    // SAME classifier the worker's streaming auto-fire uses decides command vs
    // dictation here — one source of truth (catalog command, app launch, or
    // macOS shortcut), so the two can't drift. KWS owns zephyr stop/send/cancel;
    // matching final-ASR phrases remain dictation here.
    let auto_fired_standalone = standalone_action.is_some();
    let mut standalone_summarize_requested = false;
    let mut classified_action = standalone_action.or_else(|| {
        crate::command_classify::classify_action(&transcript, crate::settings::is_pro())
    });
    // "summarize" is catalog-listed so the worker's streaming probe AUTO-FIRES
    // on the bare word like any other standalone command — but its action is
    // not a keystroke. Reroute the semantic token into the ai-transform
    // selection lanes below (captured UIA/AX selection, else the Ctrl+C
    // copy-fallback) by leaving the transcript as the bare trigger word.
    // The catalog maps a "__"-prefixed token to CommandAction::Special with
    // the prefix stripped (token_to_action) — match THAT, not a KeySequence
    // chord (field bug 2026-07-16: the KeySequence match never fired and the
    // raw dispatch errored "unknown special op", sticking the pill on error).
    if matches!(
        &classified_action,
        Some(rightkit_voice_commands::command::CommandAction::Special { op })
            if op == "summarize_selection"
    ) {
        tracing::info!("standalone_summarize_command_rerouted_to_selection_lane");
        standalone_summarize_requested = auto_fired_standalone;
        classified_action = None;
        transcript = "summarize".to_string();
        ai_transform = None;
    }
    if let Some(action) = classified_action {
        let last_text = last_delivery_text.as_deref();
        // A Cancel or a new Start that raced ahead of classification must stop
        // this command short of actually dispatching — the next step is the
        // OS-visible one. Delivery never restores or redirects focus.
        if discard_if_superseded(is_current, &session_id, "before_standalone_dispatch") {
            return Ok(FinalizeCommit::discard());
        }
        // A recognized command that fails to dispatch (no macOS equivalent,
        // unknown key, missing permission) must STILL return the engine to idle.
        // Propagating the error with `?` skipped the state reset, so no terminal
        // event fired and the pill hung in Processing forever (the "caps lock
        // kept processing" bug). Recover to armed either way — but carry the
        // failure so the shell can show it instead of faking success.
        //
        // macOS CGEvent dispatch silently no-ops without the Accessibility
        // grant — pre-check so "undo" without permissions is a visible
        // failure, not silence indistinguishable from success.
        #[cfg(target_os = "macos")]
        if !crate::macos_input::accessibility_trusted(false) {
            tracing::warn!("command dispatch blocked: accessibility not granted");
            trace_stop_to_engine_outcome(stop_started_at, "standalone_command_no_accessibility");
            return Ok(FinalizeCommit::outcome_only(
                FinalizeOutcome::CommandFailed {
                    message: "accessibility_not_granted".to_string(),
                },
            ));
        }
        match crate::command_dispatch::dispatch_with_last_text(&action, last_text) {
            Ok(outcome) => {
                tracing::info!(
                    action = outcome.action,
                    detail = %outcome.detail,
                    "command dispatched"
                );
                trace_stop_to_engine_outcome(stop_started_at, "standalone_command");
                return Ok(FinalizeCommit::outcome_only(
                    FinalizeOutcome::CommandDispatched {
                        action: outcome.action.to_string(),
                        detail: outcome.detail,
                    },
                ));
            }
            Err(err) => {
                tracing::warn!(
                    code = err.code,
                    message = %err.message,
                    "command dispatch failed; recovering to armed"
                );
                trace_stop_to_engine_outcome(stop_started_at, "standalone_command_failed");
                return Ok(FinalizeCommit::outcome_only(
                    FinalizeOutcome::CommandFailed {
                        message: err.message,
                    },
                ));
            }
        }
    }
    let mut polish_target = {
        let f = focus.lock();
        f.recording_start_target()
            .or_else(|| f.captured_target())
            .or_else(|| f.current_target())
    };
    // Context is captured at recording start and carried through this session;
    // never re-read a different field at stop.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let captured_field = recording_context.as_ref().and_then(|context| {
        if !crate::l3_cleanup::cleanup_enabled() {
            return None;
        }
        match context.state {
            rightkit_dictation_core::engine::RecordingContextState::Unreadable => None,
            rightkit_dictation_core::engine::RecordingContextState::ConfirmedEmpty
            | rightkit_dictation_core::engine::RecordingContextState::NonEmpty => {
                Some(crate::focus::FocusedFieldContext {
                    window_title: context.window_title.clone(),
                    field_text: context.field_text.clone(),
                    selected_text: context.selected_text.clone(),
                })
            }
        }
    });
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let polish_captured_field = captured_field.clone();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if let Some(field) = &polish_captured_field {
        if let Some(target) = &mut polish_target {
            if target.window_title.is_none() {
                target.window_title = field.window_title.clone();
            }
        }
        tracing::info!(
            has_window_title = field.window_title.is_some(),
            has_field_text = field.field_text.is_some(),
            has_selected_text = field.selected_text.is_some(),
            "macos_polish_context_ready"
        );
    } else {
        // Name the cause; these were indistinguishable in 0.1.170-0.1.175 logs.
        let reason = if !crate::l3_cleanup::cleanup_enabled() {
            "cleanup_disabled"
        } else {
            match recording_context.as_ref().map(|context| context.state) {
                None => "no_start_context_received",
                Some(rightkit_dictation_core::engine::RecordingContextState::Unreadable) => {
                    "start_context_unreadable"
                }
                Some(_) => "unexpected",
            }
        };
        tracing::info!(reason, "macos_polish_context_unavailable");
    }
    // A bare "prompt" / "summarize" is a transform command only when the
    // user has explicitly selected non-empty text. Resolve it after the
    // fresh stop-time UIA/AX capture so the selection—not the command word—
    // becomes the L2/L3 source and the active selection receives the paste.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let selected_transform_from_ax = if ai_transform.is_none() {
        captured_field
            .as_ref()
            .and_then(|field| {
                rightkit_text_polish::text_pipeline::parse_selected_text_ai_transform_command(
                    &transcript,
                    field.selected_text.as_deref(),
                )
            })
            .map(|command| {
                tracing::info!(intent = ?command.intent, "ai_transform_selection_detected");
                transcript = command.clean_text;
                ai_transform = Some(command.intent);
            })
            .is_some()
    } else {
        false
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let selected_transform_bound_to_source = selected_transform_from_ax;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let selected_transform_bound_to_source = false;
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let mut selected_transform_source = selected_transform_from_ax;
    #[cfg(target_os = "macos")]
    if selected_transform_from_ax {
        let live = captured_field
            .as_ref()
            .is_some_and(|expected| focus.lock().captured_selection_matches_live(expected));
        if !live {
            // Never fall through to ordinary delivery: that pasted the literal
            // command word over the user's selection (observed 2026-09-19).
            tracing::warn!("selected_text_changed_before_transform");
            trace_stop_to_engine_outcome(stop_started_at, "selected_transform_selection_changed");
            return Ok(FinalizeCommit::outcome_only(
                FinalizeOutcome::CommandFailed {
                    message: "selection_changed".to_string(),
                },
            ));
        }
    }
    // Copy-fallback (Adrian, 2026-07-16): a selection in a READ-ONLY region
    // (chat transcript, rendered web page) is invisible to the focused-control
    // UIA read — focused_text_input=false, selected_text=None — so a bare
    // "summarize" previously died as ordinary dictation. When the utterance IS
    // the bare trigger and no selection was captured, fetch the selection with
    // a clipboard-preserving synthetic Ctrl+C. Runs in the UNLOCKED phase 2,
    // so the fetch's ~300ms worst-case poll never holds the runtime mutex.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if !selected_transform_from_ax
        && ai_transform.is_none()
        && rightkit_text_polish::text_pipeline::is_bare_summarize_trigger(&transcript)
    {
        match crate::delivery::fetch_selected_text_via_copy() {
            Some(selection) => {
                tracing::info!("ai_transform_selection_via_copy_fallback");
                transcript = selection;
                ai_transform =
                    Some(rightkit_text_polish::text_pipeline::AiTransformIntent::Summarize);
                selected_transform_source = true;
            }
            None => {
                tracing::info!("ai_transform_copy_fallback_found_no_selection");
                selected_transform_source = false;
            }
        }
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if standalone_summarize_requested && !selected_transform_source {
        tracing::info!("standalone_summarize_no_selection");
        trace_stop_to_engine_outcome(stop_started_at, "standalone_summarize_no_selection");
        return Ok(FinalizeCommit::outcome_only(
            FinalizeOutcome::CommandFailed {
                message: "no_selection".to_string(),
            },
        ));
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let context_field_text = if selected_transform_source {
        captured_field.as_ref().and_then(|field| {
            crate::focus::field_text_without_selection(
                field.field_text.as_deref(),
                field.selected_text.as_deref(),
            )
        })
    } else {
        polish_captured_field
            .as_ref()
            .and_then(|field| field.field_text.clone())
    };
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let context_selected_text = if selected_transform_source {
        None
    } else {
        polish_captured_field
            .as_ref()
            .and_then(|field| field.selected_text.clone())
    };
    // Country/region context is auto-detected from the OS locale (e.g.
    // "en-IN"), never typed by the user — sent to the polish LLM for spelling
    // and tone only. Replaces the removed manual "Writing region" field.
    let writing_region = sys_locale::get_locale();
    let polish_context = crate::text_polish::DictationPolishContext {
        audio_secs,
        app_name: polish_target
            .as_ref()
            .and_then(|target| target.process_name.as_deref()),
        window_title: polish_target
            .as_ref()
            .and_then(|target| target.window_title.as_deref()),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        field_text: context_field_text.as_deref(),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        selected_text: context_selected_text.as_deref(),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        field_context_available: polish_captured_field.is_some(),
        writing_region: writing_region.as_deref(),
    };
    // L0 is mandatory input for every AI layer (L1/L2/L3) and is also the
    // fallback output. Raw ASR remains available only for history/telemetry.
    // Continuation casing: when the focused field already ends mid-sentence,
    // the dictation continues it — keep the first word lowercase. Applies to
    // the normal insertion lane only (transform output replaces a selection,
    // which is standalone text).
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let capitalize_start = ai_transform.is_some()
        || !crate::text_polish::continues_mid_sentence(context_field_text.as_deref());
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let capitalize_start = true;
    let local_finalize_started = Instant::now();
    let local_only = crate::text_polish::polish_local_only_with(&transcript, capitalize_start);
    tracing::info!(
        target: "rightkit_finalize_timing",
        session_id,
        local_fallback_ms = local_finalize_started.elapsed().as_secs_f64() * 1_000.0,
        "engine_finalize_local_fallback"
    );
    rightkit_dictation_inference::timeline_recorder::add_duration_ms(
        |t| &mut t.local_polish_ms,
        local_finalize_started.elapsed().as_secs_f64() * 1_000.0,
    );
    let snippets = crate::settings::snippets();
    let expand = |t: &str| {
        if snippets.is_empty() {
            t.to_string()
        } else {
            rightkit_text_polish::text_pipeline::expand_snippets(t, &snippets)
        }
    };
    let fallback_candidate = expand(&local_only);
    let semantic_source = transcript.clone();
    let ai_context = crate::l3_cleanup::PolishContext {
        app_name: polish_target
            .as_ref()
            .and_then(|target| target.process_name.clone()),
        window_title: polish_target
            .as_ref()
            .and_then(|target| target.window_title.clone()),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        field_text: context_field_text.clone(),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        selected_text: context_selected_text.clone(),
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        field_context_available: polish_captured_field.is_some(),
        vocabulary: crate::vocabulary::terms(),
        writing_region: writing_region.clone(),
        sound_alikes: crate::vocabulary::sound_alike_pairs(),
    };
    // Explicit dogfood evaluation mode: capture one production-real input,
    // fan it out to the frozen prompt/model matrix, and never type it into
    // the target app. This stays outside normal delivery and is inert unless
    // the operator sets RIGHTKIT_AI_EVAL_MODE.
    if crate::l3_cleanup::live_eval_suppresses_delivery() {
        let (lane, eval_input) = match ai_transform {
            Some(rightkit_text_polish::text_pipeline::AiTransformIntent::Prompt) => (
                crate::l3_cleanup::LiveEvalLane::L2,
                fallback_candidate.as_str(),
            ),
            Some(rightkit_text_polish::text_pipeline::AiTransformIntent::Summarize) => (
                crate::l3_cleanup::LiveEvalLane::L3,
                fallback_candidate.as_str(),
            ),
            None => (
                crate::l3_cleanup::LiveEvalLane::L1,
                fallback_candidate.as_str(),
            ),
        };
        match crate::l3_cleanup::run_live_evaluation(eval_input, &ai_context, lane) {
            Ok(path) => tracing::info!(output = %path.display(), "live_eval_case_complete"),
            Err(error) => tracing::error!(%error, "live_eval_case_failed"),
        }
        trace_stop_to_engine_outcome(stop_started_at, "live_eval_no_delivery");
        return Ok(FinalizeCommit::outcome_only(FinalizeOutcome::ResetToArmed));
    }

    // Cancel/new-Start check before the (potentially multi-second) cloud L2/L3
    // polish call — no point spending an HTTP round-trip on a session that's
    // already been superseded.
    if discard_if_superseded(is_current, &session_id, "before_polish") {
        return Ok(FinalizeCommit::discard());
    }

    // Cancel-to-history (`local_only_stop`) skips BOTH cloud lanes exactly
    // like a disabled/no-consent stop would, but unconditionally — the
    // deterministic local (L0) polish above stands as the final transcript,
    // and `polish_dictation` (L1 app-aware cloud polish) and the ai-transform
    // L2/L3 lanes below are never called, regardless of settings/consent or
    // whether the utterance carried a "prompt"/"summarize" tail. A cancelled
    // utterance's text must never leave the device.
    let mut transform_produced_output = true;
    let cloud_lane_started = Instant::now();
    let cloud_lane_ran =
        !local_only_stop && (ai_transform.is_some() || crate::l3_cleanup::cleanup_enabled());
    let transcript = if local_only_stop {
        fallback_candidate.clone()
    } else {
        match ai_transform {
            Some(rightkit_text_polish::text_pipeline::AiTransformIntent::Prompt) => {
                match crate::l3_cleanup::prompt_polish_outcome_with_receipts(
                    &fallback_candidate,
                    &ai_context,
                    Some(context_request_attempted.as_ref()),
                    Some(context_original_binding.as_ref()),
                ) {
                    crate::l3_cleanup::CleanupOutcome::Cleaned(prompt) => expand(&prompt),
                    crate::l3_cleanup::CleanupOutcome::Failed { error_class, .. } => {
                        crate::l3_cleanup::record_local_fallback();
                        tracing::warn!(error_class, "l3_prompt_fallback_to_cleaned_transcript");
                        transform_produced_output = false;
                        fallback_candidate.clone()
                    }
                    crate::l3_cleanup::CleanupOutcome::Skipped { reason, .. } => {
                        tracing::warn!(reason, "l3_prompt_skipped_using_cleaned_transcript");
                        transform_produced_output = false;
                        fallback_candidate.clone()
                    }
                }
            }
            Some(rightkit_text_polish::text_pipeline::AiTransformIntent::Summarize) => {
                match crate::l3_cleanup::summarize_outcome_with_receipts(
                    &fallback_candidate,
                    &ai_context,
                    Some(context_request_attempted.as_ref()),
                    Some(context_original_binding.as_ref()),
                ) {
                    crate::l3_cleanup::CleanupOutcome::Cleaned(summary) => expand(&summary),
                    crate::l3_cleanup::CleanupOutcome::Failed { error_class, .. } => {
                        crate::l3_cleanup::record_local_fallback();
                        tracing::warn!(error_class, "l3_summary_fallback_to_cleaned_transcript");
                        transform_produced_output = false;
                        fallback_candidate.clone()
                    }
                    crate::l3_cleanup::CleanupOutcome::Skipped { reason, .. } => {
                        tracing::warn!(reason, "l3_summary_skipped_using_cleaned_transcript");
                        transform_produced_output = false;
                        fallback_candidate.clone()
                    }
                }
            }
            None if !crate::l3_cleanup::cleanup_enabled() => {
                // L0 was already computed above as fallback_candidate. With
                // cleanup disabled the dictation-polish wrapper can only
                // recompute that same local result before returning it.
                // Reuse the identical expanded fallback; AI-on, transform,
                // and cancel-to-history paths keep their existing branches.
                tracing::info!(
                    reused_local_fallback = true,
                    "engine_finalize_ai_off_local_fallback_reused"
                );
                fallback_candidate.clone()
            }
            None => expand(&crate::text_polish::polish_dictation_with_receipts(
                &semantic_source,
                polish_context,
                Some(context_request_attempted.as_ref()),
                Some(context_original_binding.as_ref()),
            )),
        }
    };
    if cloud_lane_ran {
        rightkit_dictation_inference::timeline_recorder::add_duration_ms(
            |t| &mut t.cloud_polish_ms,
            cloud_lane_started.elapsed().as_secs_f64() * 1_000.0,
        );
    }
    // A selected-text transform whose provider failed or was skipped has only
    // the ORIGINAL SELECTION as "fallback". Pasting that back is a no-op in the
    // source editor and a wrong-app paste if focus moved (no provider request
    // means no binding receipt). Recover without pasting; the selection stays.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if selected_transform_source && !transform_produced_output {
        tracing::warn!("selected_transform_unavailable_nothing_pasted");
        trace_stop_to_engine_outcome(stop_started_at, "selected_transform_unavailable");
        return Ok(FinalizeCommit::outcome_only(
            FinalizeOutcome::CommandFailed {
                message: "transform_unavailable".to_string(),
            },
        ));
    }
    owner_diagnostics::record_event(serde_json::json!({
        "event": "delivery_transcript",
        "session_id": &session_id,
        "raw_transcript": &fallback_candidate,
        "delivered_transcript": &transcript,
        "send_enter": send_enter,
    }));
    if transcript.trim().is_empty() {
        let record = DeliveryRecord::new(
            delivery_id_for(""),
            "",
            DeliveryOutcome::CopiedFallback {
                reason: CopyFallbackReason::EmptyTranscript,
            },
            snapshot_target(),
        );
        trace_stop_to_engine_outcome(stop_started_at, "empty_transcript");
        return Ok(FinalizeCommit::with_record(
            FinalizeOutcome::Delivery {
                record: record.clone(),
                send_enter,
            },
            record,
            false,
        ));
    }
    let raw_text = if fallback_candidate != transcript {
        Some(fallback_candidate)
    } else {
        None
    };
    let recording_ms = audio_secs.map(|s| (s.max(0.0) * 1000.0) as u64);
    // Cancel/new-Start check before handing off to the shell — the last
    // engine-side point where stopping short avoids any OS-visible action.
    // Phase 3's generation check is the authoritative guard; this is belt-
    // and-suspenders so we don't emit a sidecar event for a dead session.
    if discard_if_superseded(is_current, &session_id, "before_shell_handoff") {
        return Ok(FinalizeCommit::discard());
    }
    // Selected-text safety (locked 2026-07-14): standalone "prompt" /
    // "summarize" only makes sense when the user had selected text and
    // that selection is still present at delivery. Re-read the focused
    // field right before handoff; if the live selection is empty, fall
    // back to copy instead of letting the shell type the transformed
    // result into a field the user changed.
    //
    // `!local_only_stop`: `copy_fallback_record` performs a REAL OS
    // clipboard write and returns `FinalizeOutcome::Delivery`, which the
    // shell routes to `accept_sidecar_delivery` — a path that does not
    // know about `cancel_to_history_session` at all. A cancel-to-history
    // finalize must never touch the clipboard or bypass the shell's
    // cancel-aware routing, so this safety-net branch is skipped entirely
    // for that stop; the plain `Transcript` outcome below already carries
    // the local-only text with no clipboard/paste side effect.
    // Stale-selection safety net (Adrian, 2026-07-16; ported to macOS
    // 2026-09-14): if the live selection no longer matches what was
    // captured at stop, the transform result must not be pasted into a
    // field the user has since changed. Both platforms fall back to
    // `copy_fallback_record` so the dictation lands on the clipboard
    // instead of being silently dropped.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if selected_transform_from_ax && ai_transform.is_some() && !local_only_stop {
        #[cfg(target_os = "macos")]
        let selection_still_live = captured_field
            .as_ref()
            .is_some_and(|expected| focus.lock().captured_selection_matches_live(expected));
        #[cfg(target_os = "windows")]
        let selection_still_live = captured_field
            .as_ref()
            .zip(focus.lock().focused_field_context_for_captured_target())
            .is_some_and(|(expected, live)| {
                selected_text_matches_live(
                    expected.selected_text.as_deref(),
                    live.selected_text.as_deref(),
                )
            });
        if !selection_still_live {
            tracing::warn!(
                intent = ?ai_transform,
                "selected_text_stale_falling_back_to_copy"
            );
            let target = focus
                .lock()
                .captured_target()
                .unwrap_or_else(snapshot_target);
            trace_stop_to_engine_outcome(stop_started_at, "stale_selection_copy_fallback");
            return Ok(stale_selection_fallback_commit(
                &transcript,
                raw_text,
                recording_ms,
                target,
                send_enter,
            ));
        }
    }
    // Both platforms hand the finished transcript to the shell for delivery
    // (clipboard + paste + optional Enter). The shell owns focus validation,
    // paste-settle timing, session generation, pill state, and history —
    // the engine's job ends at producing the final text.
    trace_stop_to_engine_outcome(stop_started_at, "shell_delivery");
    Ok(FinalizeCommit::outcome_only(FinalizeOutcome::Transcript {
        text: transcript,
        send_enter,
        raw_text,
        recording_ms,
        cancelled: local_only_stop,
        context_request_attempted: Some(
            context_request_attempted.load(std::sync::atomic::Ordering::Acquire),
        ),
        // A selected-text transform's SOURCE is the start editor's selection
        // (AX-captured, then re-proven live above), so its result belongs in
        // that editor even if focus moved before stop. Without this the shell
        // saw an unbound request and pasted into the stop-time editor.
        context_original_binding: Some(
            context_original_binding.load(std::sync::atomic::Ordering::Acquire)
                || (cfg!(any(target_os = "windows", target_os = "macos"))
                    && selected_transform_bound_to_source
                    && context_request_attempted.load(std::sync::atomic::Ordering::Acquire)),
        ),
    }))
}
