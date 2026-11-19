//! What a call into the controller asks of the pill window: show it, hide it, or leave it alone.
//!
//! This is the only visibility vocabulary the crate owns. Whether a pill shows at all (the
//! always-show-resting setting, a splash in front, onboarding, a full-screen application) is the
//! host's policy and lives in the host: the crate applies the [`Visibility`] it is handed and
//! decides nothing about when each is right.

/// Whether an applied event also shows or hides the pill window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    /// Show the window (the pill plays its entrance if it was hidden).
    Show,
    /// Hide the window (the pill fades, then the window leaves the screen).
    Hide,
    /// Leave the window as it is.
    Keep,
}
