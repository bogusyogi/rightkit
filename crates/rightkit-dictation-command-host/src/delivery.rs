// Delivery — OS side effects only: clipboard write, paste, Enter, foreground
// target snapshot, and target validation.
//
// The pure record shapes + id/time helpers live in `rightkit_transcript_library::delivery`
// (re-exported below). Module layout: `deliver` owns the public delivery
// entry points and outcome mapping, `insert` owns the per-platform transcript
// transaction (clipboard stage + paste keystroke + confirmation proof),
// `clipboard` owns raw clipboard read/write/clear + selection fetch,
// `keystrokes` owns the paste/Enter key posts, and `targets` owns the
// foreground-target snapshot / revalidation / restore helpers.

pub use rightkit_transcript_library::delivery::*;

mod clipboard;
mod deliver;
mod insert;
mod keystrokes;
mod targets;

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use clipboard::fetch_selected_text_via_copy;
pub use deliver::{
    copy_fallback_record, copy_text, deliver_text, deliver_text_with_enter, paste_text,
};
pub use targets::{restore_and_verify, snapshot_target};

pub(crate) fn test_mode() -> bool {
    // Switch class S1: the variable's name exists only in test and `test-mode` builds.
    {
        false
    }
}
