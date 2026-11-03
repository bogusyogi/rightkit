//! macOS permission-dialog evidence: positive-only detection of a system permission prompt.
//!
//! (This module replaces the dialog half of the removed `userdata` module, rightkit#1437. The
//! other half, `userdata::Guard`, refused real user data; QA now runs on the owner's real data
//! under the snapshot and restore in [`crate::snapshot`].)

/// Names of the macOS agents that show system permission prompts.
const SYSTEM_DIALOG_OWNERS: &[&str] = &[
    "usernotificationcenter",
    "coreservicesuiagent",
    "universalaccessauthwarn",
    "securityagent",
    "tccd",
];

/// True when `name` is a macOS process that owns permission prompts.
pub fn is_system_dialog_owner(name: &str) -> bool {
    let n = name.to_lowercase();
    SYSTEM_DIALOG_OWNERS.iter().any(|o| n == *o)
}

/// Outcome of a permission-dialog check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogCheck {
    /// Evidence shows no dialog.
    Clear,
    /// A dialog owner was seen.
    Dialog(String),
    /// This build cannot observe system dialogs; the string says why. Not evidence of absence.
    Unsupported(&'static str),
}

/// Positive evidence only: the first sampled foreground name that belongs to a system dialog
/// agent, from `StopReport::frontmost_states`. An empty answer proves nothing, because these
/// agents rarely become frontmost.
pub fn permission_dialog_in(frontmost_states: &[String]) -> Option<String> {
    frontmost_states
        .iter()
        .find(|n| is_system_dialog_owner(n))
        .cloned()
}

/// TODO: observe live TCC prompts. `rightkit_automation::macos::background::windows(pid)` lists windows of one pid
/// only; there is no enumeration of windows by owner name (`UserNotificationCenter`,
/// `CoreServicesUIAgent`), so no reliable detector exists in this crate yet. This hook reports
/// [`DialogCheck::Unsupported`] instead of guessing. Replace the body once the automation layer
/// can list windows by owner without clicking them.
pub fn live_permission_dialog_check(_app_pid: u32) -> DialogCheck {
    DialogCheck::Unsupported(
        "no window-by-owner enumeration in rightkit-qa-driver; permission dialogs are not observable",
    )
}
