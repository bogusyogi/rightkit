//! Real macOS event-path checks. Run on a host with Accessibility and Input Monitoring granted to
//! the test process. Synthetic sources are private so no user modifier state is changed.

// `harness = false`: every host needs a `main`, so the macOS gate lives in a module.
#[cfg(target_os = "macos")]
mod mac {

use std::sync::{mpsc, Arc, atomic::{AtomicUsize, Ordering}};
use std::time::Duration;

use rightkit_hotkeys::macos::{
    EventTap, FnHotkeyCallbacks, GlobalHotkeys, HotkeyState, MiddleMouseTap,
    TapConfig, TapDecision, TapKind, TapLocation,
};
use rightkit_hotkeys::macos::ffi::{FLAG_SECONDARY_FN, KEYCODE_FN, KEYCODE_SPACE};
use rightkit_hotkeys::macos::synth::{
    accessibility_trusted, can_listen, can_post, pump_run_loop, post_flags_changed, post_key,
    post_other_mouse, SYNTHETIC_EVENT_USER_DATA,
};
use rightkit_hotkeys::parse_chord;

fn require_access() {
    assert!(accessibility_trusted(), "Accessibility grant missing");
    assert!(can_listen(), "Input Monitoring/listen grant missing");
    assert!(can_post(), "Post Events grant missing");
}

fn wait<T>(rx: &mpsc::Receiver<T>) -> T {
    rx.recv_timeout(Duration::from_secs(3)).expect("event callback timed out")
}

pub fn main() {
    require_access();
    assert!(rightkit_hotkeys::macos::synth::init_background_app(), "background app host failed");
    carbon_press_release();
    tap_pass_swallow_and_listen_only();
    fn_space_is_swallowed();
    middle_button_filters_non_middle();
    println!("rightkit-hotkeys macOS E2E: PASS");
}

fn carbon_press_release() {
    let (tx, rx) = mpsc::channel();
    let mut hotkeys = GlobalHotkeys::new(move |_, state| tx.send(state).unwrap()).unwrap();
    let chord = parse_chord("Ctrl+Alt+Meta+Shift+F18").unwrap();
    let id = hotkeys.register(&chord).unwrap();
    assert!(matches!(hotkeys.register(&chord), Err(rightkit_hotkeys::macos::RegisterError::Conflict { .. })));
    assert_eq!(hotkeys.registered(), 1);
    let flags = rightkit_hotkeys::macos::ffi::FLAG_CONTROL
        | rightkit_hotkeys::macos::ffi::FLAG_ALTERNATE
        | rightkit_hotkeys::macos::ffi::FLAG_COMMAND
        | rightkit_hotkeys::macos::ffi::FLAG_SHIFT;
    assert!(post_key(TapLocation::Session, 79, true, flags));
    assert!(post_key(TapLocation::Session, 79, false, flags));
    pump_run_loop(0.2);
    assert_eq!(wait(&rx), HotkeyState::Pressed);
    assert_eq!(wait(&rx), HotkeyState::Released);
    assert!(hotkeys.unregister(id));
    assert_eq!(hotkeys.registered(), 0);
}

fn tap_pass_swallow_and_listen_only() {
    let seen = Arc::new(AtomicUsize::new(0));
    let observer_seen = seen.clone();
    let observer = EventTap::spawn(TapConfig {
        name: "rightkit-e2e-observer".into(), location: TapLocation::AnnotatedSession,
        listen_only: true, kinds: vec![TapKind::KeyDown],
    }, move |event| {
        if event.user_data == SYNTHETIC_EVENT_USER_DATA {
            observer_seen.fetch_add(1, Ordering::SeqCst);
        }
        TapDecision::Pass
    }).unwrap();
    let swallow = EventTap::spawn(TapConfig {
        name: "rightkit-e2e-swallow".into(), location: TapLocation::Session,
        listen_only: false, kinds: vec![TapKind::KeyDown],
    }, |event| {
        if event.user_data == SYNTHETIC_EVENT_USER_DATA {
            TapDecision::Swallow
        } else {
            TapDecision::Pass
        }
    }).unwrap();
    assert!(post_key(TapLocation::Session, 79, true, 0));
    pump_run_loop(0.2);
    assert_eq!(seen.load(Ordering::SeqCst), 0, "swallowed key reached annotated observer");
    drop(swallow);
    assert!(post_key(TapLocation::Session, 79, true, 0));
    pump_run_loop(0.2);
    assert_eq!(seen.load(Ordering::SeqCst), 1, "listen-only observer did not see passed key");
    drop(observer);
}

fn fn_space_is_swallowed() {
    let (tx, rx) = mpsc::channel();
    let cb = FnHotkeyCallbacks { on_toggle: Arc::new(move || tx.send(()).unwrap()), ..FnHotkeyCallbacks::noop() };
    let tap = rightkit_hotkeys::macos::FnHotkeyTap::spawn(TapLocation::Session, cb).unwrap();
    assert!(post_flags_changed(TapLocation::Session, KEYCODE_FN, FLAG_SECONDARY_FN));
    assert!(post_key(TapLocation::Session, KEYCODE_SPACE, true, FLAG_SECONDARY_FN));
    assert!(post_key(TapLocation::Session, KEYCODE_SPACE, false, FLAG_SECONDARY_FN));
    assert!(post_flags_changed(TapLocation::Session, KEYCODE_FN, 0));
    pump_run_loop(0.25);
    wait(&rx);
    drop(tap);
}

fn middle_button_filters_non_middle() {
    let (tx, rx) = mpsc::channel();
    let tap = MiddleMouseTap::spawn(TapLocation::Session, move || tx.send(()).unwrap(), |_, _| false).unwrap();
    assert!(post_other_mouse(TapLocation::Session, 3, 10.0, 10.0, true));
    pump_run_loop(0.1);
    assert!(rx.try_recv().is_err(), "button 3 must pass through");
    assert!(post_other_mouse(TapLocation::Session, 2, 10.0, 10.0, true));
    pump_run_loop(0.2);
    wait(&rx);
    drop(tap);
}
}

#[cfg(target_os = "macos")]
fn main() {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macos_e2e: macOS-only end-to-end gate; nothing to run on this host");
}
