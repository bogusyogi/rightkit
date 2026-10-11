//! Idle options: per-host timeout, minimise-started timer, controller hide timing, event gating.
//! Pure pieces only; the WebView2 calls are Windows-only and not exercised here.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
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
    let mut options = IdleOptions::default();
    options.suspend_after = Duration::from_secs(120);
    let configured = IdleHost::new(DEFAULT_SUSPEND_AFTER).with_options(options);
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

#[test]
fn the_provider_is_read_at_each_hide_and_again_at_the_next_hide() {
    let ms = Arc::new(AtomicU64::new(5_000));
    let calls = Arc::new(AtomicUsize::new(0));
    let (m, c) = (ms.clone(), calls.clone());
    let options = IdleOptions::default().suspend_after_with(move || {
        c.fetch_add(1, Ordering::SeqCst);
        Some(Duration::from_millis(m.load(Ordering::SeqCst)))
    });
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "not evaluated at build time"
    );
    let host = IdleHost::new(DEFAULT_SUSPEND_AFTER).with_options(options);
    assert_eq!(
        host.options.hide_delay(host.suspend_after, None),
        Duration::from_secs(5)
    );
    // The value changes between hides; the next hide sees it.
    ms.store(9_000, Ordering::SeqCst);
    assert_eq!(
        host.options.hide_delay(host.suspend_after, None),
        Duration::from_secs(9)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "once per hide");
}

#[test]
fn a_provider_returning_none_falls_back_to_the_fixed_suspend_after() {
    let mut options = IdleOptions::default();
    options.suspend_after = Duration::from_secs(120);
    let options = options.suspend_after_with(|| None);
    assert_eq!(
        options.hide_delay(Duration::from_secs(120), None),
        Duration::from_secs(120)
    );
    // The fixed value is what the host passes in, so it is used even if it differs from the field.
    assert_eq!(
        options.hide_delay(Duration::from_secs(7), None),
        Duration::from_secs(7)
    );
}

#[test]
fn a_panicking_provider_falls_back_and_the_next_hide_still_works() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let options = IdleOptions::default().suspend_after_with(move || {
        c.fetch_add(1, Ordering::SeqCst);
        panic!("provider failed on purpose (test)");
    });
    let fixed = Duration::from_secs(45);
    assert_eq!(options.hide_delay(fixed, None), fixed);
    assert_eq!(
        options.hide_delay(fixed, None),
        fixed,
        "later hides still run"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn an_explicit_per_hide_delay_wins_and_skips_the_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let options = IdleOptions::default().suspend_after_with(move || {
        c.fetch_add(1, Ordering::SeqCst);
        Some(Duration::from_secs(60))
    });
    assert_eq!(
        options.hide_delay(DEFAULT_SUSPEND_AFTER, Some(Duration::from_secs(3))),
        Duration::from_secs(3)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_provider_value_is_clamped_like_any_other_delay() {
    let options =
        IdleOptions::default().suspend_after_with(|| Some(Duration::from_secs(10 * 24 * 3600)));
    assert_eq!(
        options.hide_delay(DEFAULT_SUSPEND_AFTER, None),
        MAX_SUSPEND_AFTER
    );
}

#[test]
fn options_with_a_provider_clone_equal_and_differ_from_plain_options() {
    let a = IdleOptions::default().suspend_after_with(|| None);
    let b = a.clone();
    assert_eq!(a, b, "a clone shares the provider");
    assert_ne!(
        a,
        IdleOptions::default().suspend_after_with(|| None),
        "distinct closures are distinct providers"
    );
    assert_ne!(a, IdleOptions::default());
}
