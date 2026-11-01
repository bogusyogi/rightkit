//! Engine-side per-utterance timeline recorder (telemetry only).
//!
//! One dictation is in its stop phase at a time, so a single process-global
//! slot suffices. `begin_stop` fixes the stage-local origin; every `mark_*`
//! records milliseconds elapsed from it. Offsets are first-write-wins so a
//! repeated call site (idempotent stop, retried final) cannot move a stage.
//! Nothing here carries text content.

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::Instant;

use parking_lot::Mutex;
use rightkit_dictation_core::engine::{EnginePass, EngineTimeline};

struct Active {
    session_id: String,
    origin: Instant,
    timeline: EngineTimeline,
}

#[derive(Default)]
struct State {
    background_passes: u32,
    active: Option<Active>,
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State::default()))
}

thread_local! {
    static STAGE_DETAIL: Cell<Option<(f64, f64, f64)>> = const { Cell::new(None) };
}

/// A new recording began: drop any stale stop state and the background count.
pub fn session_started() {
    let mut state = state().lock();
    state.background_passes = 0;
    state.active = None;
}

/// The stop began at `origin`. Idempotent per session id (first origin wins).
pub fn begin_stop(session_id: &str, origin: Instant) {
    let mut state = state().lock();
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.session_id == session_id)
    {
        return;
    }
    state.active = Some(Active {
        session_id: session_id.to_string(),
        origin,
        timeline: EngineTimeline::default(),
    });
}

/// Record the elapsed offset for one stage (first write wins). No-op when no
/// stop is in progress.
pub fn mark(field: fn(&mut EngineTimeline) -> &mut Option<f64>) {
    let mut state = state().lock();
    if let Some(active) = state.active.as_mut() {
        let elapsed_ms = active.origin.elapsed().as_secs_f64() * 1_000.0;
        field(&mut active.timeline).get_or_insert(elapsed_ms);
    }
}

/// Add a measured DURATION (ms) to a stage field. No-op outside a stop.
pub fn add_duration_ms(field: fn(&mut EngineTimeline) -> &mut Option<f64>, duration_ms: f64) {
    let mut state = state().lock();
    if let Some(active) = state.active.as_mut() {
        let slot = field(&mut active.timeline);
        *slot = Some(slot.unwrap_or(0.0) + duration_ms);
    }
}

/// Backend-reported mel/encode/search split for the pass that is about to be
/// reported on this thread (see `record_pass`). Optional: backends that do
/// not call it report only samples and total.
pub fn note_stage_detail(mel_ms: f64, encode_ms: f64, search_ms: f64) {
    STAGE_DETAIL.with(|cell| cell.set(Some((mel_ms, encode_ms, search_ms))));
}

/// Report one decoded window. Counted as a final pass when the stop already
/// began, otherwise as a background pass.
pub fn record_pass(samples: usize, total_ms: f64) {
    let detail = STAGE_DETAIL.with(Cell::take);
    let mut guard = state().lock();
    let state = &mut *guard;
    match state.active.as_mut() {
        Some(active) => active.timeline.passes.push(EnginePass {
            samples: samples as u64,
            mel_ms: detail.map(|d| d.0),
            encode_ms: detail.map(|d| d.1),
            search_ms: detail.map(|d| d.2),
            total_ms,
        }),
        None => state.background_passes = state.background_passes.saturating_add(1),
    }
}

/// Like `record_pass` with an explicit split (macOS reports it directly).
pub fn record_pass_with_detail(
    samples: usize,
    mel_ms: f64,
    encode_ms: f64,
    search_ms: f64,
    total_ms: f64,
) {
    note_stage_detail(mel_ms, encode_ms, search_ms);
    record_pass(samples, total_ms);
}

/// Take the finished timeline for `session_id` (clears the slot). `None`
/// when no stop was recorded for that session.
pub fn take(session_id: &str) -> Option<EngineTimeline> {
    let mut state = state().lock();
    let background = state.background_passes;
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.session_id == session_id)
    {
        let mut timeline = state.active.take()?.timeline;
        timeline.background_passes = background;
        return Some(timeline);
    }
    None
}
