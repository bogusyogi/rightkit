//! Opt-in quit drain: the host's decision logic, the bound after which it exits anyway, and the
//! capability probe the `@rightkit/tauri-shell` npm package reads. The Tauri event loop is not started
//! here; `QuitGate` is the whole decision, and `lifecycle` only wires it to `RunEvent`s.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use rightkit_tauri_shell::commands::{capabilities_for, ShellCapabilities};
use rightkit_tauri_shell::quit::{
    QuitGate, QuitStep, DEFAULT_QUIT_DRAIN_BOUND, MAX_QUIT_DRAIN_BOUND,
};

#[test]
fn a_gate_without_a_bound_quits_at_once_as_before() {
    let gate = QuitGate::new(None);
    let exiting = AtomicBool::new(false);
    assert!(!gate.enabled());
    assert_eq!(gate.request(&exiting, true), QuitStep::ExitNow);
    // Never latches into a draining state it was not asked for.
    assert_eq!(gate.request(&exiting, true), QuitStep::ExitNow);
}

#[test]
fn an_enabled_gate_drains_once_and_coalesces_repeat_requests() {
    let gate = QuitGate::new(Some(Duration::from_secs(3)));
    let exiting = AtomicBool::new(false);
    assert_eq!(gate.request(&exiting, true), QuitStep::Drain);
    // Tray Quit, Cmd+Q and a window close arriving while the renderer drains.
    assert_eq!(gate.request(&exiting, true), QuitStep::AlreadyDraining);
    assert_eq!(gate.request(&exiting, true), QuitStep::AlreadyDraining);
}

#[test]
fn once_exiting_is_set_the_exit_path_never_prompts_again() {
    let gate = QuitGate::new(Some(Duration::from_secs(3)));
    let exiting = AtomicBool::new(false);
    assert_eq!(gate.request(&exiting, true), QuitStep::Drain);
    // rk_shell_finish_quit sets exiting, then app.exit(0) raises ExitRequested again.
    exiting.store(true, Ordering::SeqCst);
    assert_eq!(gate.request(&exiting, true), QuitStep::ExitNow);
    // Also when no drain was ever in progress.
    let fresh = QuitGate::new(Some(Duration::from_secs(3)));
    assert_eq!(fresh.request(&exiting, true), QuitStep::ExitNow);
}

#[test]
fn with_no_renderer_left_there_is_nobody_to_drain() {
    let gate = QuitGate::new(Some(Duration::from_secs(3)));
    let exiting = AtomicBool::new(false);
    assert_eq!(gate.request(&exiting, false), QuitStep::ExitNow);
}

#[test]
fn the_bound_has_a_documented_default_and_a_ceiling() {
    assert_eq!(DEFAULT_QUIT_DRAIN_BOUND, Duration::from_secs(15));
    assert_eq!(MAX_QUIT_DRAIN_BOUND, Duration::from_secs(120));
    assert_eq!(
        QuitGate::new(Some(Duration::from_secs(7))).bound(),
        Some(Duration::from_secs(7))
    );
    assert_eq!(
        QuitGate::new(Some(Duration::from_secs(86_400))).bound(),
        Some(MAX_QUIT_DRAIN_BOUND)
    );
    assert_eq!(QuitGate::new(None).bound(), None);
}

#[test]
fn the_bound_exits_the_app_when_the_renderer_never_finishes() {
    let gate = Arc::new(QuitGate::new(Some(Duration::from_millis(40))));
    let exiting = AtomicBool::new(false);
    assert_eq!(gate.request(&exiting, true), QuitStep::Drain);
    let (tx, rx) = mpsc::channel();
    gate.arm_bound(move || tx.send(()).unwrap());
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the host must exit after its own bound");
    // One expiry per drain.
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
}

#[test]
fn finishing_before_the_bound_cancels_the_forced_exit() {
    let gate = Arc::new(QuitGate::new(Some(Duration::from_millis(120))));
    let exiting = AtomicBool::new(false);
    assert_eq!(gate.request(&exiting, true), QuitStep::Drain);
    let (tx, rx) = mpsc::channel();
    gate.arm_bound(move || tx.send(()).unwrap());
    gate.finish();
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
}

#[test]
fn capabilities_report_what_the_host_enabled_in_the_names_the_npm_package_reads() {
    let off = serde_json::to_value(capabilities_for(false)).unwrap();
    assert_eq!(
        off,
        serde_json::json!({"quitDrain": false, "notifyBypassDnd": true})
    );
    let on = serde_json::to_value(capabilities_for(true)).unwrap();
    assert_eq!(
        on,
        serde_json::json!({"quitDrain": true, "notifyBypassDnd": true})
    );
    let _: ShellCapabilities = capabilities_for(true);
}

#[test]
fn the_event_and_command_names_match_the_npm_contract() {
    assert_eq!(
        rightkit_tauri_shell::EVENT_QUIT_REQUESTED,
        "rightkit-shell://quit-requested"
    );
}
