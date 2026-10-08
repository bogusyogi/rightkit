//! Which visibility states each platform can record. Decided by the RK2 spikes:
//! only off-screen-visible (and its subset, occluded) yields frames on both
//! OSes. Anything else is reported as unsupported, never degraded silently.

use rightkit_contracts::types::record_session::{RecorderOs, VisibilityState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    Supported(&'static str),
    Unsupported(&'static str),
}

impl Support {
    pub fn is_supported(self) -> bool {
        matches!(self, Support::Supported(_))
    }
}

pub fn support(os: RecorderOs, os_build: &str, state: VisibilityState) -> Support {
    if os == RecorderOs::Windows && windows_build(os_build) < 22_000 {
        return Support::Unsupported(
            "Windows 10: the capture border cannot be suppressed (IsBorderRequired needs build 22000 or later)",
        );
    }
    match (os, state) {
        (_, VisibilityState::OnScreenVisible) => Support::Unsupported(
            "visible to the user; recording records only states the user cannot see",
        ),
        (RecorderOs::Macos, VisibilityState::OffScreenVisible) => Support::Supported(
            "window outside every display; needs the constrain override and WKWebView occlusion SPI (RK2 Spike A); rAF throttle defect open",
        ),
        (RecorderOs::Macos, VisibilityState::Occluded) => Support::Supported(
            "occluder also off-screen; subset of off-screen-visible (RK2 Spike A)",
        ),
        (RecorderOs::Windows, VisibilityState::OffScreenVisible) => Support::Supported(
            "HWND capture at off-screen coordinates (RK2 Spike B)",
        ),
        (RecorderOs::Windows, VisibilityState::Occluded) => Support::Supported(
            "occluder off-screen; no foreign pixels in any frame (RK2 Spike B)",
        ),
        (_, VisibilityState::AppHidden) => Support::Unsupported(
            "no frames for an app-hidden window (RK2 Spike A on macOS, Spike B on Windows)",
        ),
        (_, VisibilityState::Minimized) => Support::Unsupported(
            "no frames for a minimized window (RK2 Spike A on macOS, Spike B on Windows)",
        ),
    }
}

fn windows_build(os_build: &str) -> u32 {
    // `10.0.26300.9550`: the third component is the build number.
    os_build
        .split('.')
        .nth(2)
        .and_then(|b| b.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_off_screen_states_are_supported() {
        assert!(support(
            RecorderOs::Macos,
            "27.0.1",
            VisibilityState::OffScreenVisible
        )
        .is_supported());
        assert!(!support(RecorderOs::Macos, "27.0.1", VisibilityState::AppHidden).is_supported());
        assert!(!support(RecorderOs::Macos, "27.0.1", VisibilityState::Minimized).is_supported());
        assert!(!support(
            RecorderOs::Windows,
            "26300.9550",
            VisibilityState::OnScreenVisible
        )
        .is_supported());
    }

    #[test]
    fn windows_10_is_unsupported_everywhere() {
        assert!(!support(
            RecorderOs::Windows,
            "10.0.19045.1",
            VisibilityState::OffScreenVisible
        )
        .is_supported());
        assert!(support(
            RecorderOs::Windows,
            "10.0.26300.9550",
            VisibilityState::OffScreenVisible
        )
        .is_supported());
    }
}
