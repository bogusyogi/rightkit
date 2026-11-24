//! Capsule size rules: which size the recording capsule takes in each mode, how wide a status
//! capsule is for its label, how wide the hovered recording dock's content is, and whether a status
//! label is the host's "cancelled" word.
//!
//! The sizes, the face and font size of the status text and every spacing come from the host in
//! [`CapsuleSizes`] and [`DockSpacing`]; the crate fixes no number.

use crate::anim::Mode;
use crate::text::{text_width, Face};

/// The host's capsule sizes in points.
#[derive(Clone, Copy, Debug)]
pub struct CapsuleSizes {
    /// Idle capsule, compact (`width, height`).
    pub idle_compact: (f32, f32),
    /// Idle capsule when hovered or while the clip card is open.
    pub idle_expanded: (f32, f32),
    /// Recording capsule layout frame before `compact_recording_scale`.
    pub compact_recording_base: (f32, f32),
    pub compact_recording_scale: f32,
    pub recording_hover: (f32, f32),
    pub processing: (f32, f32),
    pub status_height: f32,
    pub status_min_width: f32,
    pub status_max_width: f32,
    /// Face and size the status label is measured in.
    pub status_face: Face,
    pub status_font_size: f32,
    /// Added to the measured label width: icon, spacing, padding and a rasterisation allowance.
    pub status_text_allowance: f32,
}

/// `ceil(base * scale)` of the compact recording capsule.
pub fn compact_recording_size(sizes: &CapsuleSizes) -> (f32, f32) {
    (
        (sizes.compact_recording_base.0 * sizes.compact_recording_scale).ceil(),
        (sizes.compact_recording_base.1 * sizes.compact_recording_scale).ceil(),
    )
}

/// Size of a status or error capsule for `label`: the measured text plus the allowance, clamped to
/// the host's width range.
pub fn status_capsule_size(sizes: &CapsuleSizes, label: &str) -> (f32, f32) {
    let text = text_width(sizes.status_face, sizes.status_font_size, label).ceil();
    (
        (text + sizes.status_text_allowance).clamp(sizes.status_min_width, sizes.status_max_width),
        sizes.status_height,
    )
}

/// The capsule size for a mode: idle and recording grow when hovered (idle also while the clip card
/// is open), processing is fixed, a status or error capsule follows its label.
pub fn capsule_size(
    sizes: &CapsuleSizes,
    mode: Mode,
    hovered: bool,
    clipboard_visible: bool,
    label: &str,
) -> (f32, f32) {
    match mode {
        Mode::Idle => {
            if hovered || clipboard_visible {
                sizes.idle_expanded
            } else {
                sizes.idle_compact
            }
        }
        Mode::Recording => {
            if hovered {
                sizes.recording_hover
            } else {
                compact_recording_size(sizes)
            }
        }
        Mode::Processing => sizes.processing,
        Mode::Status | Mode::Error => status_capsule_size(sizes, label),
    }
}

/// Whether `label` is `cancelled_word` (trimmed, ASCII case-insensitive).
pub fn is_cancelled_label(label: &str, cancelled_word: &str) -> bool {
    label.trim().eq_ignore_ascii_case(cancelled_word)
}

/// The hovered recording dock's layout numbers.
#[derive(Clone, Copy, Debug)]
pub struct DockSpacing {
    /// The dock capsule height (`CapsuleSizes::recording_hover.1`) and the button side: the padding
    /// at each end is half their difference.
    pub dock_height: f32,
    pub button_size: f32,
    /// Gap between the dock's items.
    pub gap: f32,
    /// Width of the level bars and the gap after them, before the timer.
    pub bars_width: f32,
    pub bars_gap: f32,
}

/// Natural width of a host-composed dock: `button_count` buttons plus one bars/timer group,
/// separated by `gap`, with symmetric outer padding. Button order remains the host's.
pub fn recording_dock_content_width(
    dock: &DockSpacing,
    timer_width: f32,
    button_count: usize,
) -> f32 {
    let pad = (dock.dock_height - dock.button_size) / 2.0;
    pad * 2.0
        + dock.button_size * button_count as f32
        + (dock.bars_width + dock.bars_gap + timer_width)
        + dock.gap * button_count as f32
}
