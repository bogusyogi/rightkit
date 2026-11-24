//! Compares the focus observer's editor identity with the identity of a
//! freshly captured focus pin for the live frontmost app. Read-only.
//!
//! Run (macOS only): cargo run -p rightkit-automation --features desktop-context --example editor_identity_probe

#[cfg(all(target_os = "macos", feature = "desktop-context"))]
fn main() {
    use std::time::Duration;
    rightkit_automation::desktop::start_focus_observer();
    // Observer is on demand: cover for the probe's whole run (no `end`; the process exits).
    rightkit_automation::desktop::begin_focus_coverage();
    std::thread::sleep(Duration::from_millis(1_500));
    for round in 0..20 {
        let Some(front) = rightkit_automation::desktop::macos::frontmost_fast() else {
            continue;
        };
        let Some(window) = rightkit_automation::desktop::macos::frontmost_window_identity(front.pid) else {
            continue;
        };
        let proof =
            rightkit_automation::desktop::capture_focus_continuity_with_editor(front.pid as u32, window)
                .map(|(_, _, editor)| editor);
        let pin_full = rightkit_automation::desktop::macos::capture_stop_focus_pin().ok();
        let pin = pin_full.as_ref().map(|p| p.editor_identity());
        let within = match (proof, pin_full.as_ref()) {
            (Some(Some(editor)), Some(p)) => Some(p.editor_identity_is_self_or_ancestor(editor)),
            _ => None,
        };
        println!(
            "round={round} app={} proof_editor={proof:?} pin_editor={pin:?} same={} self_or_ancestor={within:?}",
            front.bundle.unwrap_or_default(),
            matches!((proof, pin), (Some(Some(a)), Some(b)) if a == b),
        );
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(not(all(target_os = "macos", feature = "desktop-context")))]
fn main() {}
