//! The Windows full-screen verdict for the pill's monitor: whether the foreground window covers the
//! monitor the pill sits on. A thin layer over `rightkit_overlay::windows::foreground_fullscreen_on`
//! that turns the host's monitor rectangle and probe policy into the probe the overlay crate takes.
//! (The macOS verdict is `fullscreen_mac`.)
//!
//! Without a resolvable monitor nothing is judged full screen (never a guess). Every choice of the
//! probe is the host's ([`Probe`]); the shell classes the overlay crate already ignores stay
//! ignored, and the host's extra classes are added to them.
//!
//! Moved from HeardRight `pill/state.rs` (`platform_frontmost_is_fullscreen`).

use rightkit_overlay::{FullScreenProbe, WorkArea};

/// The host's policy for the full-screen probe.
#[derive(Clone, Copy, Debug)]
pub struct Probe<'a> {
    /// Count only a foreground window without a caption and without a sizing border (a maximised
    /// ordinary window keeps its caption, so it never counts).
    pub require_borderless: bool,
    /// Also count what the shell notification state reports as an exclusive full-screen, a
    /// presentation or a Store app.
    pub use_shell_notification_state: bool,
    /// How far, in pixels, an edge of the window may fall short of the monitor edge and still
    /// count as covering it.
    pub edge_tolerance_px: i32,
    /// Window classes that never count, on top of the overlay crate's own list (transient system
    /// chrome rather than a full-screen app).
    pub extra_ignore_classes: &'a [&'a str],
}

/// Whether the foreground window is full screen on `monitor`, the physical-pixel
/// `[left, top, right, bottom]` of the monitor the pill sits on (`None` when it cannot be
/// resolved: not full screen).
pub fn frontmost_is_fullscreen(monitor: Option<[f64; 4]>, probe: &Probe<'_>) -> bool {
    let Some([left, top, right, bottom]) = monitor else {
        return false;
    };
    let mut native = FullScreenProbe {
        require_borderless: probe.require_borderless,
        use_shell_notification_state: probe.use_shell_notification_state,
        edge_tolerance_px: probe.edge_tolerance_px,
        ..FullScreenProbe::default()
    };
    native.ignore_classes.extend(
        probe
            .extra_ignore_classes
            .iter()
            .map(|class| (*class).to_string()),
    );
    rightkit_overlay::windows::foreground_fullscreen_on(
        WorkArea {
            left,
            top,
            right,
            bottom,
        },
        &native,
    )
}
