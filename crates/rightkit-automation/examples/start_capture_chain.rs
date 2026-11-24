//! Runs the production macOS start-capture chain against the live frontmost
//! app and prints content-free gate results. Read-only: no activation, input,
//! or AX writes beyond the observation lease the app itself takes.
//!
//! Run (macOS only): cargo run -p rightkit-automation --features desktop-context --example start_capture_chain

#[cfg(all(target_os = "macos", feature = "desktop-context"))]
fn main() {
    use std::time::Duration;
    rightkit_automation::desktop::start_focus_observer();
    // Observer is on demand: cover for the probe's whole run (no `end`; the process exits).
    rightkit_automation::desktop::begin_focus_coverage();
    std::thread::sleep(Duration::from_millis(1_500));
    for round in 0..5 {
        let Some(front) = rightkit_automation::desktop::macos::frontmost_fast() else {
            continue;
        };
        let window = rightkit_automation::desktop::macos::frontmost_window_identity(front.pid);
        let token = window.and_then(|w| {
            rightkit_automation::desktop::capture_focus_continuity(front.pid as u32, w)
                .map(|t| (w, t))
        });
        let settled = token.is_some_and(|(w, (at, inst))| {
            rightkit_automation::desktop::validate_focus_continuity_settled(
                front.pid as u32,
                w,
                at,
                inst,
                None,
                Duration::from_millis(150),
            )
        });
        let pin = rightkit_automation::desktop::macos::capture_stop_focus_pin();
        let pin_label = match &pin {
            Ok(p) if p.pid() == front.pid => "captured",
            Ok(_) => "pid_mismatch",
            Err(miss) => miss.label(),
        };
        let secure = pin
            .as_ref()
            .ok()
            .map(rightkit_automation::desktop::macos::focused_field_is_secure_for_pin);
        let read = pin.as_ref().ok().and_then(|p| {
            rightkit_automation::desktop::macos::focused_field_context_and_fingerprint_for_pin(
                p, 1_500,
            )
        });
        let after = token.is_some_and(|(w, (at, inst))| {
            rightkit_automation::desktop::validate_focus_continuity(
                front.pid as u32,
                w,
                at,
                inst,
                None,
            )
        });
        println!(
            "round={round} app={} token={} settled={settled} pin={pin_label} secure={secure:?} field_read={} has_text={} fingerprint={} caret_prev_known={} continuity_after_read={after}",
            front.bundle.unwrap_or_default(),
            token.is_some(),
            read.is_some(),
            read.as_ref().is_some_and(|(f, _)| f.field_text.as_deref().is_some_and(|t| !t.trim().is_empty())),
            read.as_ref().is_some_and(|(_, fp)| fp.is_some()),
            read.as_ref().is_some_and(|(f, _)| f.char_before_caret.is_some()),
        );
        if token.is_none() {
            println!(
                "  window_known={} pid={} observer: {}",
                window.is_some(),
                front.pid,
                rightkit_automation::desktop::focus_observer_debug_snapshot()
            );
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}

#[cfg(not(all(target_os = "macos", feature = "desktop-context")))]
fn main() {}
