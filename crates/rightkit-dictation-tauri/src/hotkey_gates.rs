//! Whether a dictation or shortcut-test hotkey may act now.
//!
//! A dictation hotkey pressed while the launch splash is still up is DISCARDED, not queued: the
//! hotkeys register at setup but the engine is not warm, so a press would block in the supervisor
//! and fire the instant the model loaded. The onboarding mic and shortcut test runs before the
//! launch finishes by design, so it keeps its own path.

pub fn dictation_hotkeys_enabled(
    onboarded: bool,
    onboarding_test_active: bool,
    launch_finished: bool,
) -> bool {
    if !onboarding_test_active && !launch_finished {
        return false;
    }
    onboarded && !onboarding_test_active
}

pub fn shortcut_test_hotkeys_enabled(onboarding_test_active: bool) -> bool {
    onboarding_test_active
}
