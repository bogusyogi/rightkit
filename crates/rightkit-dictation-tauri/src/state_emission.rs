//! Serialized, authority-checked emission of committed runtime states.
//!
//! Runtime state commits under the runtime lock, but UI emission happens after the lock drops, and
//! buffered `observed` snapshots can lag the authority by an arbitrary window. Every
//! runtime-state-driven emission therefore pairs the authority check with the emit inside one
//! critical section: a state emits only while the host's authority still says it is current, so a
//! stale buffered `Recording` can never land after the terminal state that replaced it. Nothing
//! inside the section may re-enter emission or take the runtime lock (pill dispatch must be
//! enqueue-only; renderer and menu-bar writes fire-and-forget), so the guard is a leaf mutex.
//!
//! The sinks are closures the host passes per call, so this module holds no application handle.

use std::sync::{Mutex, MutexGuard};

use rightkit_dictation_core::state::AppState;

use crate::state_decisions::{is_terminal_pill_state, state_tag};

static EMISSION_GUARD: Mutex<()> = Mutex::new(());

fn emission_serialization() -> MutexGuard<'static, ()> {
    EMISSION_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the emission rules read from the host.
pub struct EmissionHost {
    /// True while `state` is still the authoritative runtime state. With no authority yet
    /// (before the runtime is opened) no racing writer exists and emission proceeds.
    pub is_current: fn(&AppState) -> bool,
    /// Telemetry sink: event name and detail. This module passes only static tags and surface
    /// names; the sink must still treat the detail as diagnostics, never as a place for content.
    pub event_sink: fn(&str, &str),
    /// Forensic `state_changed` line; written for every committed transition, even one a newer
    /// state already superseded. The host owns what it records: `AppState` carries transcripts, so
    /// the implementation must record a content-free form ([`state_tag`], or a structured
    /// serialization that redacts content keys), never `{:?}` or the transcript fields.
    pub log_state: fn(&AppState),
}

impl EmissionHost {
    fn superseded(&self, surface: &str, state: &AppState) {
        (self.event_sink)(
            "state_emit_superseded",
            // The variant tag only. `AppState` derives `Debug` and its variants hold transcripts,
            // session and delivery ids and error text; none of that belongs in a diagnostic.
            &format!("surface={surface} state={}", state_tag(state)),
        );
    }
}

/// Mirror emission for a committed runtime state (renderer, menu bar, tray). Guarded: the state
/// emits only while it is still authoritative; a superseded state is dropped with telemetry.
pub fn emit_mirrors(host: &EmissionHost, state: &AppState, mirrors: impl FnOnce(&AppState)) {
    let _serialized = emission_serialization();
    if !(host.is_current)(state) {
        host.superseded("mirrors", state);
        return;
    }
    mirrors(state);
}

/// Pill-only emission for a committed runtime state, for command paths that already emitted
/// mirrors and for the sidecar handlers' deferred terminal pushes. Serialized and
/// authority-checked like [`emit_mirrors`], so a stale snapshot cannot cover a terminal pill.
pub fn emit_pill(host: &EmissionHost, state: &AppState, pill: impl FnOnce(&AppState)) {
    let _serialized = emission_serialization();
    if !(host.is_current)(state) {
        host.superseded("pill", state);
        return;
    }
    pill(state);
}

/// Full runtime-state emission: the forensic line stays unconditional, while mirrors and then the
/// pill emit only inside the serialized guard and only while the state is authoritative.
pub fn emit_full(
    host: &EmissionHost,
    state: &AppState,
    mirrors: impl FnOnce(&AppState),
    pill: impl FnOnce(&AppState),
) {
    (host.log_state)(state);
    let _serialized = emission_serialization();
    if !(host.is_current)(state) {
        host.superseded("full", state);
        return;
    }
    mirrors(state);
    pill(state);
}

/// Emit a batch of states observed while handling a sidecar event: each state is logged, then
/// mirrored (guarded); a non-terminal state also gets its pill emission immediately (guarded), a
/// terminal one is returned instead so the caller can push the pill's final look after the whole
/// batch. Returns the last terminal state of the batch.
pub fn emit_observed_sidecar(
    host: &EmissionHost,
    observed: &[AppState],
    mut mirrors: impl FnMut(&AppState),
    mut pill: impl FnMut(&AppState),
) -> Option<AppState> {
    let mut terminal_pill_state = None;
    for state in observed {
        (host.log_state)(state);
        emit_mirrors(host, state, &mut mirrors);
        if is_terminal_pill_state(state) {
            terminal_pill_state = Some(state.clone());
        } else {
            // Serialized + authority-checked: a superseded snapshot drops here instead of
            // covering the terminal state that replaced it.
            emit_pill(host, state, &mut pill);
        }
    }
    terminal_pill_state
}
