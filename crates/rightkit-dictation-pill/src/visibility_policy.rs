//! The pill visibility policy as pure decisions: which event the pill shows for a host event,
//! whether a resting pill yields to a full-screen app or a splash, and what a visibility pass must
//! do. Moved verbatim from the app that kept it; the setting that turns the resting pill on or off
//! ("always show the resting pill"), whether a splash is present, whether onboarding is complete
//! and what the OS reports are inputs the host reads and passes in.
//!
//! Every input is a plain value read from the host's own state, and every answer is a value the
//! host applies with its own windows. Nothing here reads an OS or a setting.
//!
//! The rules:
//!
//! * Only the resting pill (the `Idle` sliver and a `Hide` that collapses to it) follows
//!   full-screen suppression. Every active state (recording, processing, status, error, success)
//!   stays up, on Windows and on macOS alike: `rightkit_dictation_core::pill::pill_should_be_visible`
//!   is `active || (always_show_resting && !fullscreen)`. A recording is therefore never invisible
//!   because a full-screen app is in front; it is invisible only when the resting-pill setting is
//!   off and the event is not an active one.
//! * With "always show the resting pill" off, an `Idle` event is shown as `Hide`; with it on, a
//!   `Hide` collapses to the resting pill.
//! * A splash window in front defers the pill: the model still learns the event, the pill stays
//!   hidden, and a reveal is rescheduled.
//! * Nothing is repaired before onboarding is complete.

use rightkit_dictation_core::pill::{pill_should_be_visible, state_for_event, PillEvent};

use crate::visibility::Visibility;

/// The event the pill shows for `event` under the "always show the resting pill" setting: with it
/// off, `Idle` becomes `Hide`.
pub fn event_for_visibility(event: PillEvent, always_show_resting: bool) -> PillEvent {
    if !always_show_resting && matches!(event, PillEvent::Idle) {
        PillEvent::Hide
    } else {
        event
    }
}

/// An ACTIVE state (recording, processing, status, error, success) rather than the resting or
/// hidden pill.
pub fn is_active_event(event: &PillEvent) -> bool {
    !matches!(event, PillEvent::Idle | PillEvent::Hide)
}

/// Log name of an event (never carries the label).
pub fn event_name(event: &PillEvent) -> &'static str {
    match event {
        PillEvent::Hide => "hide",
        PillEvent::Idle => "idle",
        PillEvent::Starting => "starting",
        PillEvent::Recording => "recording",
        PillEvent::Voice { .. } => "voice",
        PillEvent::Processing { .. } => "processing",
        PillEvent::Success => "success",
        PillEvent::Status { .. } => "status",
        PillEvent::Error { .. } => "error",
    }
}

// ---------------------------------------------------------------------------------------------
// Showing an event (native overlay host)
// ---------------------------------------------------------------------------------------------

/// Why [`plan_show`] decided what it did (for the log line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowReason {
    /// A splash is in front: the event is learned, the pill stays hidden, a reveal is retried.
    SplashActive,
    /// Full screen suppresses the resting pill (`Hide` and `Idle` only).
    FullscreenSuppressed,
    /// A `Hide` with the resting pill always on: shown as `Idle`.
    HideToIdle,
    /// A `Hide` with the resting pill off: the window hides.
    HideToHidden,
    /// An amplitude update: visibility untouched.
    Voice,
    /// An event that has a render state: the pill shows.
    Shown,
    /// Anything else: applied, visibility untouched.
    Kept,
}

/// What to apply: the event the pill should learn and what to do with its window.
#[derive(Clone, Debug, PartialEq)]
pub struct ShowDecision {
    pub event: PillEvent,
    pub visibility: Visibility,
    pub reason: ShowReason,
}

/// A `Hide` collapses to `Idle` when the resting pill is always on.
fn resting(event: &PillEvent, hide_means_idle: bool) -> PillEvent {
    if matches!(event, PillEvent::Hide) && hide_means_idle {
        PillEvent::Idle
    } else {
        event.clone()
    }
}

/// How the native overlay host applies `event`. `hide_means_idle` is "always show the resting
/// pill".
pub fn plan_show(
    event: &PillEvent,
    hide_means_idle: bool,
    splash_present: bool,
    fullscreen_suppressed: bool,
) -> ShowDecision {
    if splash_present {
        return ShowDecision {
            event: resting(event, hide_means_idle),
            visibility: Visibility::Hide,
            reason: ShowReason::SplashActive,
        };
    }
    if fullscreen_suppressed && matches!(event, PillEvent::Hide | PillEvent::Idle) {
        return ShowDecision {
            event: resting(event, hide_means_idle),
            visibility: Visibility::Hide,
            reason: ShowReason::FullscreenSuppressed,
        };
    }
    if matches!(event, PillEvent::Hide) {
        return if hide_means_idle {
            ShowDecision {
                event: PillEvent::Idle,
                visibility: Visibility::Show,
                reason: ShowReason::HideToIdle,
            }
        } else {
            ShowDecision {
                event: event.clone(),
                visibility: Visibility::Hide,
                reason: ShowReason::HideToHidden,
            }
        };
    }
    if matches!(event, PillEvent::Voice { .. }) {
        return ShowDecision {
            event: event.clone(),
            visibility: Visibility::Keep,
            reason: ShowReason::Voice,
        };
    }
    if state_for_event(event).is_some() {
        ShowDecision {
            event: event.clone(),
            visibility: Visibility::Show,
            reason: ShowReason::Shown,
        }
    } else {
        ShowDecision {
            event: event.clone(),
            visibility: Visibility::Keep,
            reason: ShowReason::Kept,
        }
    }
}

/// What the native panel host (the macOS view owned by another framework) does with an
/// already-visibility-resolved event.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelDispatch {
    /// Full screen suppresses the resting pill: hide the panel, send nothing.
    HidePanelFullscreen,
    /// A `Hide` with the resting pill off: hide the panel.
    HidePanel,
    /// A `Hide` with the resting pill on: send `Idle` and show the panel.
    IdleAndShow,
    /// Send the event and show the panel (amplitude updates do not show it).
    Push { event: PillEvent, show: bool },
}

/// Decide a native-panel dispatch. `event` must already have passed [`event_for_visibility`].
///
/// Full screen suppresses only the resting events (`Hide`, `Idle`), exactly as on Windows
/// ([`plan_show`]): an active event is pushed and shown. Before this was decided deliberately the
/// panel plan hid every event during full screen while the visibility pass re-showed an active
/// pill at its next run, so a recording could be invisible until then; a recording is never hidden
/// by a full-screen app now.
pub fn plan_panel_dispatch(
    event: PillEvent,
    always_show_resting: bool,
    fullscreen_suppressed: bool,
) -> PanelDispatch {
    if fullscreen_suppressed && !is_active_event(&event) {
        return PanelDispatch::HidePanelFullscreen;
    }
    match event {
        PillEvent::Hide if !always_show_resting => PanelDispatch::HidePanel,
        PillEvent::Hide => PanelDispatch::IdleAndShow,
        event => {
            let show = !matches!(event, PillEvent::Voice { .. });
            PanelDispatch::Push { event, show }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The invariant pass
// ---------------------------------------------------------------------------------------------

/// What an overlay host reads before a visibility pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvariantInputs {
    pub onboarding_complete: bool,
    /// An active pill state is showing.
    pub active: bool,
    pub always_show_resting: bool,
    /// A full-screen app on the pill's monitor suppresses the resting pill.
    pub fullscreen: bool,
    /// A splash window is in front.
    pub splash_present: bool,
    /// The pill window reports visible.
    pub shown: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invariant {
    /// Nothing to do.
    Skip,
    /// The pill should not show and a full-screen app is why: hide it.
    HideForFullscreen,
    /// The pill should show but a splash is in front: hide it and wait.
    DeferForSplash,
    /// The pill shows, but may have lost its topmost band: re-assert it.
    Reassert,
    /// The pill should show and does not: show it, then re-assert.
    ShowAndReassert,
}

/// The visibility pass for an overlay host that owns its windows.
pub fn overlay_invariant(i: &InvariantInputs) -> Invariant {
    if !i.onboarding_complete {
        return Invariant::Skip;
    }
    if !pill_should_be_visible(i.active, i.always_show_resting, i.fullscreen) {
        return if i.fullscreen {
            Invariant::HideForFullscreen
        } else {
            Invariant::Skip
        };
    }
    if i.splash_present {
        return Invariant::DeferForSplash;
    }
    if i.shown {
        Invariant::Reassert
    } else {
        Invariant::ShowAndReassert
    }
}

/// What a native panel host reads before a visibility pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelInvariantInputs {
    /// The splash hand-off owns the screen.
    pub splash_handoff_active: bool,
    pub onboarding_complete: bool,
    pub active: bool,
    pub always_show_resting: bool,
    pub fullscreen: bool,
    /// The panel reports visible.
    pub visible_before: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelInvariant {
    Skip,
    /// The hand-off is active: leave the panel alone.
    DeferForSplashHandoff,
    /// Make the panel visible or hidden (it differs from what the policy wants).
    Set { visible: bool },
}

/// The visibility pass for a native panel host.
pub fn panel_invariant(i: &PanelInvariantInputs) -> PanelInvariant {
    if i.splash_handoff_active {
        return PanelInvariant::DeferForSplashHandoff;
    }
    if !i.onboarding_complete {
        return PanelInvariant::Skip;
    }
    let visible = pill_should_be_visible(i.active, i.always_show_resting, i.fullscreen);
    if visible == i.visible_before {
        PanelInvariant::Skip
    } else {
        PanelInvariant::Set { visible }
    }
}

// ---------------------------------------------------------------------------------------------
// Full-screen suppression
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuppressionInputs {
    pub was_fullscreen: bool,
    pub fullscreen: bool,
    pub active: bool,
    pub onboarding_complete: bool,
    pub always_show_resting: bool,
}

/// What the app does when a guardian pass reports a full-screen verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuppressionPlan {
    /// Hide the resting pill now (an active pill stays up).
    pub hide_resting: bool,
    /// Leaving full screen: show the pill again where the policy would show it (overlay hosts).
    pub show_again: bool,
    /// The verdict changed: log it.
    pub changed: bool,
    /// Leaving full screen: run the visibility pass at once instead of waiting for the next one.
    pub enforce_on_exit: bool,
}

pub fn suppression_plan(i: &SuppressionInputs) -> SuppressionPlan {
    let leaving = i.was_fullscreen && !i.fullscreen;
    SuppressionPlan {
        hide_resting: i.fullscreen && !i.active,
        show_again: leaving
            && i.onboarding_complete
            && pill_should_be_visible(i.active, i.always_show_resting, false),
        changed: i.fullscreen != i.was_fullscreen,
        enforce_on_exit: leaving,
    }
}
