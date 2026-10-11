//! Idle options: per-host timeout, minimise-started timer, controller hide timing, event gating.
//! Pure pieces only; the WebView2 calls are Windows-only and not exercised here.

use std::time::Duration;

use rightkit_shell::idle::{
    effective_delay, hide_plan, ControllerHide, HidePlan, IdleEvents, IdleHost, IdleOptions,
    IdleState, MinimizeStep, MinimizeTracker, DEFAULT_SUSPEND_AFTER, MAX_SUSPEND_AFTER,
};

#[test]
fn the_default_options_are_the_behaviour_hosts_already_have() {
    let o = IdleOptions::default();
    assert_eq!(o.suspend_after, DEFAULT_SUSPEND_AFTER);
    assert!(!o.start_on_minimize);
    assert_eq!(o.controller_hide, ControllerHide::Immediate);
    assert_eq!(o.events, IdleEvents::Always);
    // IdleHost::new keeps working and takes the same defaults with its own timeout.
    let host = IdleHost::new(Duration::from_secs(7));
    assert_eq!(host.suspend_after, Duration::from_secs(7));
    assert_eq!(host.options.suspend_after, Duration::from_secs(7));
    assert!(!host.options.start_on_minimize);
}

#[test]
fn the_timeout_is_per_host_with_a_per_hide_override_and_a_clamp() {
    let host = Duration::from_secs(90);
    assert_eq!(effective_delay(host, None), host);
    assert_eq!(
        effective_delay(host, Some(Duration::from_secs(5))),
        Duration::from_secs(5)
    );
    assert_eq!(effective_delay(host, Some(Duration::ZERO)), Duration::ZERO);
    assert_eq!(
        effective_delay(host, Some(Duration::from_secs(10 * 24 * 3600))),
        MAX_SUSPEND_AFTER
    );
    let configured = IdleHost::new(DEFAULT_SUSPEND_AFTER).with_options(IdleOptions {
        suspend_after: Duration::from_secs(120),
        ..IdleOptions::default()
    });
    assert_eq!(configured.suspend_after, Duration::from_secs(120));
    assert_eq!(configured.options.suspend_after, Duration::from_secs(120));
}

#[test]
fn the_controller_is_hidden_at_once_by_default_and_on_the_timer_when_asked() {
    assert_eq!(
        hide_plan(ControllerHide::Immediate, false),
        HidePlan {
            now: true,
            on_timer: false
        }
    );
    assert_eq!(
        hide_plan(ControllerHide::OnTimer, false),
        HidePlan {
            now: false,
            on_timer: true
        }
    );
    // The OS already hid the window (close-to-tray): no controller call in either mode.
    for mode in [ControllerHide::Immediate, ControllerHide::OnTimer] {
        assert_eq!(
            hide_plan(mode, true),
            HidePlan {
                now: false,
                on_timer: false
            }
        );
    }
}

#[test]
fn a_minimise_starts_the_timer_once_and_a_restore_ends_only_that() {
    let t = MinimizeTracker::default();
    assert_eq!(
        t.step(false),
        MinimizeStep::None,
        "restore with no minimise"
    );
    assert_eq!(t.step(true), MinimizeStep::Start);
    assert_eq!(
        t.step(true),
        MinimizeStep::None,
        "repeated resize while minimised"
    );
    assert_eq!(t.step(false), MinimizeStep::End);
    assert_eq!(t.step(false), MinimizeStep::None);
    // Visible by another route (focus) clears it, so the next minimise starts afresh.
    assert_eq!(t.step(true), MinimizeStep::Start);
    t.clear();
    assert_eq!(t.step(false), MinimizeStep::None);
    assert_eq!(t.step(true), MinimizeStep::Start);
}

#[test]
fn minimise_feeds_the_same_generation_stamped_machine() {
    let mut s = IdleState::default();
    let (generation, _) = s.on_hidden();
    // Restored before the timer: the stale timer must not suspend.
    s.on_visible();
    assert_eq!(
        s.on_timer(generation),
        rightkit_shell::idle::IdleAction::None
    );
    let (generation, _) = s.on_hidden();
    assert_eq!(
        s.on_timer(generation),
        rightkit_shell::idle::IdleAction::SuspendNow
    );
}

#[test]
fn idle_events_are_gated_by_whether_the_platform_really_suspends() {
    for native in [false, true] {
        assert!(IdleEvents::Always.emits(native));
    }
    assert!(!IdleEvents::NativeSuspendOnly.emits(false));
    assert!(IdleEvents::NativeSuspendOnly.emits(true));
}

#[test]
fn the_minimise_trigger_takes_the_per_hide_delay_note_hidden_after_takes() {
    use rightkit_shell::idle::{minimize_action, MinimizeAction};
    let host = Duration::from_secs(30);
    assert_eq!(
        minimize_action(host, MinimizeStep::Start, Some(Duration::from_secs(3))),
        MinimizeAction::Hide {
            delay: Duration::from_secs(3)
        }
    );
    assert_eq!(
        minimize_action(host, MinimizeStep::Start, None),
        MinimizeAction::Hide { delay: host }
    );
}

#[test]
fn a_failed_suspend_lets_the_next_hide_start_a_fresh_timer() {
    let mut s = IdleState::default();
    let (generation, _) = s.on_hidden();
    assert_eq!(
        s.on_timer(generation),
        rightkit_shell::idle::IdleAction::SuspendNow
    );
    s.on_suspend_failed();
    let (_, action) = s.on_hidden();
    assert_eq!(
        action,
        rightkit_shell::idle::IdleAction::HideControllerAndStartTimer
    );
}
