//! Applying the visibility policy: what the pill window does when an event arrives, when it is
//! revealed (with or without the launch bloom), when the visibility invariant runs and when a
//! full-screen app starts or stops suppressing the resting pill. The decisions are
//! `visibility_policy`'s; this module applies them to a surface and logs each transition.
//!
//! Two surfaces are supported through function tables the host fills with its own calls: an
//! overlay surface (`Overlay`: the controller's `apply_event`, `set_visibility`, `is_visible`,
//! `reassert_topmost`, `reveal`) and a native-panel surface (`Panel`: the host's bridge to a view
//! another framework draws). Nothing here reads a setting, a window or the OS: every fact is a
//! `Facts` reader the host supplies, and every log goes to the host's sink.
//!
//! Moved from HeardRight `pill/state.rs` (`enforce_visibility_invariant`,
//! `update_fullscreen_suppression`, `set_pill_active`, `dispatch_event`) and `pill/commands.rs`
//! (`webview_show_event`, `reveal_pill`, `reveal_pill_with_intro`, `reveal_pill_preserving_active`
//! on both platforms). Event names and detail text are unchanged.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::host_glue::Sink;
use crate::visibility::Visibility;
use crate::visibility_policy::{
    event_for_visibility, event_name, is_active_event, overlay_invariant, panel_invariant,
    plan_panel_dispatch, plan_show, suppression_plan, Invariant, InvariantInputs, PanelDispatch,
    PanelInvariant, PanelInvariantInputs, ShowReason, SuppressionInputs,
};
use crate::PillEvent;

/// "The pill is showing an active state" and "a full-screen app suppresses the resting pill",
/// kept as the host's `static`.
pub struct SessionFlags {
    active: AtomicBool,
    fullscreen_suppressed: AtomicBool,
}

impl SessionFlags {
    pub const fn new() -> SessionFlags {
        SessionFlags {
            active: AtomicBool::new(false),
            fullscreen_suppressed: AtomicBool::new(false),
        }
    }

    pub fn active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    pub fn fullscreen_suppressed(&self) -> bool {
        self.fullscreen_suppressed.load(Ordering::SeqCst)
    }

    /// Record whether `event` is an active state (`visibility_policy::is_active_event`).
    pub fn set_active_for(&self, event: &PillEvent) {
        self.active.store(is_active_event(event), Ordering::SeqCst);
    }

    /// Store the full-screen verdict and return the previous one.
    pub fn swap_fullscreen(&self, fullscreen: bool) -> bool {
        self.fullscreen_suppressed.swap(fullscreen, Ordering::SeqCst)
    }
}

/// What the host reads from its own state, each time it is needed.
#[derive(Clone, Copy)]
pub struct Facts {
    /// The "always show the resting pill" setting.
    pub always_show_resting: fn() -> bool,
    /// Whether onboarding is complete (nothing is repaired before it is).
    pub onboarding_complete: fn() -> bool,
    /// Whether a splash window is in front (`false` while the host has no app handle).
    pub splash_present: fn() -> bool,
    /// Whether the host's app handle exists (an overlay show before it is dropped).
    pub app_ready: fn() -> bool,
}

/// The overlay surface: the controller's own calls, behind the host's wrappers that install the
/// configuration first.
#[derive(Clone, Copy)]
pub struct Overlay {
    pub apply_event: fn(&PillEvent, Visibility),
    pub set_visibility: fn(Visibility),
    pub is_visible: fn() -> bool,
    pub reassert_topmost: fn(),
    pub reveal: fn(bool),
}

/// The native-panel surface: the host's bridge to a view another framework draws.
#[derive(Clone, Copy)]
pub struct Panel {
    pub push_event: fn(&PillEvent),
    pub set_visible: fn(bool),
    pub reveal: fn(),
    pub is_visible: fn() -> bool,
    /// Whether the native splash hand-off is still active.
    pub splash_handoff_active: fn() -> bool,
}

/// Everything the application functions need besides the surface.
#[derive(Clone, Copy)]
pub struct Policy {
    pub flags: &'static SessionFlags,
    pub facts: Facts,
    pub log: Sink,
    /// Schedule a reveal once the splash is gone (`reason`, `with_intro`).
    pub schedule_reveal_after_splash: fn(&'static str, bool),
}

impl Policy {
    fn always(&self) -> bool {
        (self.facts.always_show_resting)()
    }

    /// The event the pill shows for `event` under the resting-pill setting.
    pub fn event_for_visibility(&self, event: PillEvent) -> PillEvent {
        event_for_visibility(event, self.always())
    }
}

// ---------------------------------------------------------------------------------------------
// Overlay surface
// ---------------------------------------------------------------------------------------------

/// Translate a state-machine event into the overlay pill. The policy is
/// `visibility_policy::plan_show` (splash defers, full screen suppresses only the resting pill, a
/// Hide collapses to the resting pill when `hide_means_idle`, active events always show, and
/// amplitude updates leave visibility alone); this applies the decision and logs it.
pub fn show_event_overlay(
    policy: &Policy,
    overlay: &Overlay,
    event: &PillEvent,
    hide_means_idle: bool,
) {
    if !(policy.facts.app_ready)() {
        return;
    }
    let log = policy.log;
    let visible_before = (overlay.is_visible)();
    let is_voice = matches!(event, PillEvent::Voice { .. });
    let decision = plan_show(
        event,
        hide_means_idle,
        (policy.facts.splash_present)(),
        policy.flags.fullscreen_suppressed(),
    );
    if decision.reason == ShowReason::SplashActive {
        // The model still learns the event so the pill is current when the splash hands off.
        (overlay.apply_event)(&decision.event, decision.visibility);
        if !is_voice {
            log(
                "pill_reveal_deferred",
                &format!(
                    "reason=splash_active event={} visible_before={visible_before}",
                    event_name(event)
                ),
            );
        }
        (policy.schedule_reveal_after_splash)("webview_show_event", false);
        return;
    }
    if !is_voice {
        log(
            "pill_event",
            &format!(
                "event={} hide_means_idle={} visible_before={} path=native",
                event_name(event),
                hide_means_idle,
                visible_before
            ),
        );
    }
    (overlay.apply_event)(&decision.event, decision.visibility);
    match decision.reason {
        ShowReason::HideToIdle | ShowReason::HideToHidden => log(
            "pill_visibility_transition",
            &format!(
                "requested=hide resolved={} visible_before={visible_before} path=native",
                if hide_means_idle { "idle" } else { "hidden" }
            ),
        ),
        ShowReason::Shown => log(
            "pill_visibility_transition",
            &format!(
                "requested={} resolved=shown visible_before={visible_before} path=native",
                event_name(event)
            ),
        ),
        // Suppressed resting events, amplitude updates and the rest log nothing more.
        _ => {}
    }
}

/// One queued event reaching the overlay pill on the main thread (the part after the host's own
/// main-thread assertion).
pub fn dispatch_overlay(policy: &Policy, overlay: &Overlay, generation: u64, event: PillEvent) {
    let should_log = !matches!(event, PillEvent::Voice { .. });
    if should_log {
        (policy.log)(
            "pill_dispatch_main_begin",
            &format!("generation={generation} event={}", event_name(&event)),
        );
    }
    let event = policy.event_for_visibility(event);
    policy.flags.set_active_for(&event);
    // hide_means_idle follows the setting: "always" collapses a Hide to the resting pill;
    // otherwise a Hide actually hides the window.
    show_event_overlay(policy, overlay, &event, policy.always());
}

/// Reveal the resting pill, respecting the resting-pill setting, and retry once the splash is gone.
pub fn reveal_overlay(policy: &Policy, overlay: &Overlay) {
    let event = policy.event_for_visibility(PillEvent::Idle);
    policy.flags.set_active_for(&event);
    show_event_overlay(policy, overlay, &event, policy.always());
    if (policy.facts.splash_present)() {
        (policy.schedule_reveal_after_splash)("intro_deferred", false);
    }
}

/// Reveal the resting pill, or keep an active (recording, processing, error) pill up.
pub fn reveal_preserving_active_overlay(policy: &Policy, overlay: &Overlay) {
    if (policy.facts.splash_present)() {
        (overlay.set_visibility)(Visibility::Hide);
        (policy.log)(
            "pill_reveal_deferred",
            "reason=splash_active path=preserve_active",
        );
        (policy.schedule_reveal_after_splash)("preserve_active", false);
        return;
    }
    // A late launch re-reveal can happen while dictation is already recording (post-crash or
    // prewarm recovery). Revealing must not emit Idle in that case: doing so makes the pill look
    // stopped while the engine keeps recording. This is separate from `reveal_overlay`: the
    // frontend intentionally reveals after success or error to collapse back to Idle.
    if policy.flags.active() {
        (overlay.set_visibility)(Visibility::Show);
        (policy.log)("pill_reveal_preserved_active", "active=true path=native");
        return;
    }
    reveal_overlay(policy, overlay);
}

/// Reveal after the splash has completed its hand-off, then play the launch bloom and swell once.
/// `settle` is how long the reveal settles on the driver thread before the bloom is decided; the
/// bloom plays only if the pill really is up (a hidden resting pill must stay silent). `thread_name`
/// names the short-lived thread.
pub fn reveal_with_intro_overlay(
    policy: &'static Policy,
    overlay: &'static Overlay,
    settle: Duration,
    thread_name: &str,
) {
    if (policy.facts.splash_present)() {
        (policy.schedule_reveal_after_splash)("intro_handoff", true);
        return;
    }
    reveal_preserving_active_overlay(policy, overlay);
    let delay_ms = settle.as_millis();
    let spawned = std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            std::thread::sleep(settle);
            if (overlay.is_visible)() {
                (policy.log)(
                    "pill_intro_emit",
                    &format!("delay_ms={delay_ms} path=native"),
                );
                (overlay.reveal)(true);
            } else {
                (policy.log)(
                    "pill_intro_skipped",
                    "reason=pill_not_visible_after_handoff",
                );
            }
        });
    if let Err(error) = spawned {
        (policy.log)(
            "pill_intro_skipped",
            &format!("reason=thread_spawn_error error={error}"),
        );
    }
}

/// Enforce the product invariant at the overlay boundary: once onboarding is complete an active
/// pill is always visible, and the resting pill is visible when the setting says so. `reason` is
/// logged on a deferral or a repair.
pub fn enforce_invariant_overlay(policy: &Policy, overlay: &Overlay, reason: &str) {
    let active = policy.flags.active();
    let fullscreen = policy.flags.fullscreen_suppressed();
    let plan = overlay_invariant(&InvariantInputs {
        onboarding_complete: (policy.facts.onboarding_complete)(),
        active,
        always_show_resting: policy.always(),
        fullscreen,
        splash_present: (policy.facts.splash_present)(),
        shown: (overlay.is_visible)(),
    });
    match plan {
        Invariant::Skip => {}
        Invariant::HideForFullscreen => (overlay.set_visibility)(Visibility::Hide),
        Invariant::DeferForSplash => {
            (overlay.set_visibility)(Visibility::Hide);
            (policy.log)(
                "pill_visibility_invariant_deferred",
                &format!("reason={reason} splash_active=true active={active}"),
            );
        }
        // The pill reports visible but may have lost its topmost z-position and be buried under
        // maximized windows. Re-assert it on every guardian pass: the self-heal for the "pill
        // disappeared" failure.
        Invariant::Reassert => (overlay.reassert_topmost)(),
        Invariant::ShowAndReassert => {
            (overlay.set_visibility)(Visibility::Show);
            (overlay.reassert_topmost)();
            // Show is queued to the owner thread, so the visible state is read back on its next
            // pass.
            (policy.log)(
                "pill_visibility_invariant_repair",
                &format!("reason={reason} active={active} show_ok=queued path=native"),
            );
        }
    }
}

/// Apply a full-screen verdict to the overlay pill. `enforce` runs the invariant (the host's
/// `enforce_invariant_overlay` wrapper) right after leaving full screen.
pub fn update_fullscreen_overlay(
    policy: &Policy,
    overlay: &Overlay,
    fullscreen: bool,
    enforce: impl FnOnce(&str),
) {
    let was_fullscreen = policy.flags.swap_fullscreen(fullscreen);
    let plan = suppression_plan(&SuppressionInputs {
        was_fullscreen,
        fullscreen,
        active: policy.flags.active(),
        // Read only when leaving fullscreen, the one case that uses it.
        onboarding_complete: was_fullscreen && !fullscreen && (policy.facts.onboarding_complete)(),
        always_show_resting: policy.always(),
    });
    // Only the resting pill is hidden; an active pill stays up.
    if plan.hide_resting {
        (overlay.set_visibility)(Visibility::Hide);
    }
    // Leaving fullscreen shows the pill again, but only where the visibility setting would show it
    // (the same decision as the invariant).
    if plan.show_again {
        (overlay.set_visibility)(Visibility::Show);
    }
    if plan.changed {
        (policy.log)(
            "pill_fullscreen_suppression",
            &format!("fullscreen={fullscreen}"),
        );
        // Restore immediately after exiting fullscreen instead of waiting for the next pass.
        if plan.enforce_on_exit {
            enforce("fullscreen_exit");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Native-panel surface
// ---------------------------------------------------------------------------------------------

/// One queued event reaching the native panel on the main thread (the part after the host's own
/// main-thread assertion).
pub fn dispatch_panel(policy: &Policy, panel: &Panel, generation: u64, event: PillEvent) {
    let should_log = !matches!(event, PillEvent::Voice { .. });
    let main_started = Instant::now();
    if should_log {
        (policy.log)(
            "pill_dispatch_main_begin",
            &format!("generation={generation} event={}", event_name(&event)),
        );
    }
    let event = policy.event_for_visibility(event);
    let visible_event_name = event_name(&event);
    policy.flags.set_active_for(&event);
    let fullscreen = policy.flags.fullscreen_suppressed();
    match plan_panel_dispatch(event, policy.always(), fullscreen) {
        PanelDispatch::HidePanelFullscreen => {
            (panel.set_visible)(false);
            if should_log {
                (policy.log)(
                    "pill_dispatch_main_end",
                    &format!(
                        "generation={generation} event={} elapsed_ms={} suppressed=fullscreen",
                        visible_event_name,
                        main_started.elapsed().as_millis()
                    ),
                );
            }
            return;
        }
        PanelDispatch::HidePanel => (panel.set_visible)(false),
        PanelDispatch::IdleAndShow => {
            (panel.push_event)(&PillEvent::Idle);
            (panel.set_visible)(true);
        }
        PanelDispatch::Push { event, show } => {
            (panel.push_event)(&event);
            if show {
                (panel.set_visible)(true);
            }
        }
    }
    if should_log {
        (policy.log)(
            "pill_dispatch_main_end",
            &format!(
                "generation={generation} event={} elapsed_ms={}",
                visible_event_name,
                main_started.elapsed().as_millis()
            ),
        );
    }
}

/// Reveal the resting pill on the native panel.
pub fn reveal_panel(policy: &Policy, panel: &Panel) {
    let event = policy.event_for_visibility(PillEvent::Idle);
    policy.flags.set_active_for(&event);
    (panel.push_event)(&event);
    if policy.always() {
        (panel.set_visible)(true);
        (panel.reveal)();
    }
    (policy.log)("pill_reveal_native", "path=swiftui");
}

/// Reveal the resting pill, or keep an active pill up, on the native panel.
pub fn reveal_preserving_active_panel(policy: &Policy, panel: &Panel) {
    if (panel.splash_handoff_active)() {
        (policy.log)("pill_reveal_deferred", "reason=splash_handoff");
        return;
    }
    if policy.flags.fullscreen_suppressed() && !policy.flags.active() {
        (panel.set_visible)(false);
        return;
    }
    if policy.flags.active() {
        (panel.set_visible)(true);
        (panel.reveal)();
    } else {
        reveal_panel(policy, panel);
    }
}

/// Enforce the product invariant at the native-panel boundary.
pub fn enforce_invariant_panel(policy: &Policy, panel: &Panel, reason: &str) {
    let active = policy.flags.active();
    let fullscreen = policy.flags.fullscreen_suppressed();
    let visible_before = (panel.is_visible)();
    let plan = panel_invariant(&PanelInvariantInputs {
        splash_handoff_active: (panel.splash_handoff_active)(),
        onboarding_complete: (policy.facts.onboarding_complete)(),
        active,
        always_show_resting: policy.always(),
        fullscreen,
        visible_before,
    });
    match plan {
        PanelInvariant::Skip => {}
        PanelInvariant::DeferForSplashHandoff => (policy.log)(
            "pill_visibility_invariant_deferred",
            &format!("reason={reason} splash_handoff=true"),
        ),
        PanelInvariant::Set { visible } => {
            (panel.set_visible)(visible);
            (policy.log)(
                "pill_visibility_invariant_native",
                &format!(
                    "reason={reason} active={active} fullscreen={fullscreen} visible_before={visible_before} visible_after={visible}"
                ),
            );
        }
    }
}

/// Apply a full-screen verdict to the native panel. `enforce` runs the invariant right after
/// leaving full screen.
pub fn update_fullscreen_panel(
    policy: &Policy,
    panel: &Panel,
    fullscreen: bool,
    enforce: impl FnOnce(&str),
) {
    let was_fullscreen = policy.flags.swap_fullscreen(fullscreen);
    let plan = suppression_plan(&SuppressionInputs {
        was_fullscreen,
        fullscreen,
        active: policy.flags.active(),
        onboarding_complete: was_fullscreen && !fullscreen && (policy.facts.onboarding_complete)(),
        always_show_resting: policy.always(),
    });
    // Only the resting pill is hidden; an active pill stays up.
    if plan.hide_resting {
        (panel.set_visible)(false);
    }
    if plan.changed {
        (policy.log)(
            "pill_fullscreen_suppression",
            &format!("fullscreen={fullscreen}"),
        );
        if plan.enforce_on_exit {
            enforce("fullscreen_exit");
        }
    }
}
