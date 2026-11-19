//! The host's look: the one required input that decides how the pill, hint card and clip card
//! are drawn, sized and animated.
//!
//! The crate owns the controller (state, event mapping, timers, window lifecycle, hit-test
//! dispatch). Everything a user sees that is a design decision comes in through [`Painters`]:
//! the display lists, the sizes, the spring and fade tunings, the fonts. There is no default
//! implementation: a host without a look has no pill.
//!
//! Threading: the three surface methods (`pill`, `hint`, `clip`) run on the overlay threads
//! (paint) and on the driver thread (layout). They are never called while the crate's shared
//! state is locked: the crate takes a snapshot, releases the lock and then calls the look. An
//! implementation must still be quick (a slow look delays frames and every host call that waits
//! on the driver), must not block and must not call back into this crate. `capsule_size` is the
//! exception: the capsule model asks it while it holds the shared-state lock, so it must be a pure,
//! fast function of its arguments (no locks, no I/O, no calls into this crate). A panic in any
//! of them ends the generation (the overlays are destroyed; an explicit `ensure_installed`
//! restarts).
//!
//! Every number a look returns is sanitised by the crate before it reaches a window: sizes are
//! clamped to `1..=bound::MAX_SURFACE_PX`, non-finite values count as zero, at most
//! `bound::MAX_REGIONS` regions and `bound::MAX_DRAW_OPS` display-list operations are taken from
//! one frame.

use crate::anim::{AuxPose, Curve, Mode, SpringParams};
use crate::clipboard::ClipState;
use crate::draw::{DrawOp, VRect};
use crate::hint::HintContent;
use crate::regions::{HotSet, Region};
use crate::text::Fonts;

// ---------------------------------------------------------------------------------------------
// Metrics and motion (data the controller needs from the look)
// ---------------------------------------------------------------------------------------------

/// Sizes the controller needs to place and clamp the surfaces, in points (one logical pixel at
/// 100 % scaling). Per-surface layout (capsule sizes, card sizes) is the painter's own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Pill overlay window envelope: wide enough for the widest capsule, tall enough for the
    /// tallest plus the reveal offset (`pill_window_h`), plus the transparent strip under the
    /// capsule (`pill_window_bottom_pad`).
    pub pill_window_w: f32,
    pub pill_window_h: f32,
    pub pill_window_bottom_pad: f32,
    /// The capsule bottom sits this many points above the monitor work area.
    pub bottom_margin: f32,
    /// Gap between the capsule and the aux cards stacked above it.
    pub aux_spacing: f32,
    /// Slack around a morphing capsule's extent for a spring that overshoots after a retarget
    /// (the hit regions are widened by this much while the capsule animates).
    pub extent_slack: f32,
}

/// Every tuned timing and curve the controller applies. The crate runs the springs and fades; the
/// values are the look's.
#[derive(Clone, Copy, Debug)]
pub struct Motion {
    /// Capsule size spring and the distance from rest under which it counts as settled.
    pub capsule_spring: SpringParams,
    pub capsule_spring_eps: f32,
    /// Spring used for the capsule and the cards when the OS "show animations" setting is off.
    pub reduced_spring: SpringParams,
    /// Card entrance and exit spring and its settle distance.
    pub card_spring: SpringParams,
    pub card_spring_eps: f32,
    /// Card pose while entering: `scale = card_scale_base + card_scale_gain * progress`.
    pub card_scale_base: f32,
    pub card_scale_gain: f32,
    /// How long after an entrance list rows may still stagger (`AuxPose::rows_ms`).
    pub card_rows_window_ms: f32,
    /// Cross-fade of swapped capsule or card content, normal and with animations off.
    pub crossfade_ms: f32,
    pub crossfade_reduced_ms: f32,
    /// The easing of every scalar fade (hide fade, hover amounts, copy feedback fades): progress
    /// `0..=1` in, eased progress out. Required, no default; `0 -> 0` and `1 -> 1` are checked at
    /// install (within 0.01).
    pub fade_curve: Curve,
    /// The easing of the content cross-fade and of the processing animation's fade-in. Same
    /// contract as `fade_curve`.
    pub crossfade_curve: Curve,
    /// Idle controls grow from `controls_scale_base` by `controls_scale_gain` during a cross-fade.
    pub controls_scale_base: f32,
    pub controls_scale_gain: f32,
    /// The processing animation stays hidden this long, so a short dictation never flashes it.
    pub processing_delay_ms: f32,
    /// Grace between the overlay reporting that the pointer left and the capsule collapsing. The
    /// overlay backend already debounces a leave by a few samples; a look that wants a total
    /// leave-to-collapse time subtracts that itself. The crate applies this value as given.
    pub hover_exit_delay_ms: u64,
    /// Hide: the capsule fades for `hide_fade_ms`, the window is taken off screen at
    /// `hide_order_out_ms`.
    pub hide_fade_ms: f32,
    pub hide_order_out_ms: u64,
    /// Hover amount fades: capsule buttons and aux-card regions.
    pub button_hot_ms: f32,
    pub aux_hot_ms: f32,
    /// Reveal entrance: the capsule starts `reveal_offset` points low and follows
    /// `reveal_curve(seconds since reveal)` (`0` to `1`) up; the entrance counts as running for
    /// `reveal_ms`.
    pub reveal_offset: f32,
    pub reveal_ms: f32,
    pub reveal_curve: fn(t_sec: f32) -> f32,
    /// Launch bloom: `intro_curve(progress)` for progress `0..=1` over `intro_seconds`.
    pub intro_seconds: f32,
    pub intro_curve: fn(progress: f32) -> f32,
    /// Copy feedback on a capsule button: the check mark holds `copy_check_ms` in total, the
    /// dimmed "empty" state `copy_empty_ms`; each fades in over its own `*_fade_in_ms` and every
    /// feedback fades out over `copy_fade_out_ms`.
    pub copy_check_ms: f64,
    pub copy_empty_ms: f64,
    pub copy_check_fade_in_ms: f64,
    pub copy_empty_fade_in_ms: f64,
    pub copy_fade_out_ms: f64,
    /// Clip card: a single card closes this long after a copy; a recent card resets its "copied"
    /// mark after `copied_reset_ms`.
    pub close_after_copy_ms: u64,
    pub copied_reset_ms: u64,
}

// ---------------------------------------------------------------------------------------------
// Scene and frames
// ---------------------------------------------------------------------------------------------

/// The outgoing content of a capsule cross-fade (a mode, hover or label change).
#[derive(Clone, Debug, PartialEq)]
pub struct PrevContent {
    pub mode: Mode,
    pub label: String,
    pub hovered: bool,
    pub controls: bool,
    pub elapsed_secs: u32,
    /// Opacity of the outgoing content.
    pub alpha: f32,
    /// Scale of the outgoing content about the capsule centre.
    pub scale: f32,
}

/// Everything the capsule painter needs for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct PillScene {
    pub mode: Mode,
    pub label: String,
    /// Presentation (possibly mid-animation) capsule size.
    pub cap_w: f32,
    pub cap_h: f32,
    pub hovered: bool,
    pub clipboard_visible: bool,
    pub hint_visible: bool,
    /// Latest microphone level `0.0..=1.0`. How it becomes bars is the painter's business.
    pub level: f32,
    /// Monotonic seconds since the controller started (the clock motion is computed from), and
    /// the same in milliseconds.
    pub now_secs: f32,
    pub now_ms: f64,
    /// The OS "show animations" setting is on (breathing and scale effects allowed).
    pub animations: bool,
    pub elapsed_secs: u32,
    /// Launch bloom intensity `0..=1` (`Motion::intro_curve`).
    pub intro: f32,
    /// Hover amount per region of the previous layout.
    pub hot: HotSet,
    /// Hide-fade opacity.
    pub opacity: f32,
    /// Reveal entrance offset in points.
    pub offset_y: f32,
    /// Opacity and scale of the incoming content during a cross-fade (1 at rest).
    pub content_alpha: f32,
    pub content_scale: f32,
    /// The content being replaced, while a cross-fade runs.
    pub prev: Option<PrevContent>,
    /// Opacity of the processing animation (0 for its first `processing_delay_ms`).
    pub proc_alpha: f32,
    /// Copy feedback: check-mark amount and disabled dimming, each 0..=1.
    pub copy_check: f32,
    pub copy_dim: f32,
}

impl PillScene {
    /// `PillCapsule.expanded`: idle controls are visible.
    pub fn idle_controls_visible(&self) -> bool {
        self.mode == Mode::Idle && (self.clipboard_visible || (!self.hint_visible && self.hovered))
    }
}

/// Which pass a painter is asked for. `Layout` is the settled scene, used to compute hit regions
/// and the hub-transition hitbox; it must not advance any animation state the painter keeps (for
/// example a bar buffer) and may return an empty `ops`. `Paint` is a real frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Paint,
    Layout,
}

/// One frame of the pill capsule.
#[derive(Clone, Debug, PartialEq)]
pub struct PillFrame {
    pub ops: Vec<DrawOp>,
    /// The capsule rectangle in window coordinates. The crate adds it as the capsule body region,
    /// anchors the aux cards to it and publishes it as the hitbox.
    pub capsule: VRect,
    /// The capsule's controls. Listed in paint order (the last one wins an overlap).
    pub regions: Vec<Region>,
}

/// One frame of an aux card (hint or clip).
#[derive(Clone, Debug, PartialEq)]
pub struct AuxFrame {
    pub ops: Vec<DrawOp>,
    /// Overlay window size in logical px.
    pub window: (f32, f32),
    /// The card rectangle in window coordinates: the card is centred on the pill and stacked on
    /// this rectangle's bottom edge.
    pub card: VRect,
    /// Regions in paint order. The painter lists the card body first (a `HoverRole::Body` region
    /// over `card`) so clicks on card text never fall through to the app below, then controls.
    pub regions: Vec<Region>,
}

/// What the hint painter draws.
#[derive(Clone, Copy, Debug)]
pub struct HintView<'a> {
    pub content: &'a HintContent,
    pub hot: &'a HotSet,
    pub pose: AuxPose,
}

/// What the clip painter draws.
#[derive(Clone, Copy, Debug)]
pub struct ClipView<'a> {
    pub state: &'a ClipState,
    pub hot: &'a HotSet,
    /// The item (`ClipItem::id`) whose "copied" feedback is fading in or out, and how far. Set
    /// only after the host's copy callback reported success.
    pub copied_id: Option<&'a str>,
    pub copied_amount: f32,
    /// The item whose copy failed (the host reported failure, or did not answer within
    /// `Timings::copy_deadline_ms`), and how far its failure feedback has faded in. The painter
    /// draws the host's failure label; the card stays open.
    pub failed_id: Option<&'a str>,
    pub failed_amount: f32,
    pub pose: AuxPose,
}

impl ClipView<'_> {
    /// The "copied" feedback amount of the item `id`.
    pub fn copied_amount_for(&self, id: &str) -> f32 {
        match self.copied_id {
            Some(current) if current == id => self.copied_amount,
            _ => 0.0,
        }
    }

    /// The copy-failure feedback amount of the item `id`.
    pub fn failed_amount_for(&self, id: &str) -> f32 {
        match self.failed_id {
            Some(current) if current == id => self.failed_amount,
            _ => 0.0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------------------------

/// The look. Required by `Config::new`; the crate has no stock implementation.
pub trait Painters: Send + Sync {
    /// The five font slots (bytes, GDI names, metrics). Read once at install.
    fn fonts(&self) -> Fonts;
    /// Window envelope, margins. Read once at install.
    fn metrics(&self) -> Metrics;
    /// Every spring, fade and curve the controller applies. Read once at install.
    fn motion(&self) -> Motion;
    /// The size the capsule is heading for in `mode`. `hovered` is true while the pointer rests on
    /// it (and not suppressed by a visible hint); `clipboard_visible` is true while the clip card
    /// is open. `label` is the status or error text (empty otherwise).
    fn capsule_size(
        &self,
        mode: Mode,
        hovered: bool,
        clipboard_visible: bool,
        label: &str,
    ) -> (f32, f32);
    /// Lay out and draw the capsule. `win_w`/`win_h` are the overlay window size and `bottom_pad`
    /// the transparent strip under the capsule's resting bottom edge (all from `metrics`).
    fn pill(
        &self,
        scene: &PillScene,
        win_w: f32,
        win_h: f32,
        bottom_pad: f32,
        pass: Pass,
    ) -> PillFrame;
    /// Lay out and draw the hint card.
    fn hint(&self, view: &HintView<'_>) -> AuxFrame;
    /// Lay out and draw the clip card (single or recent, per `view.state.single`).
    fn clip(&self, view: &ClipView<'_>) -> AuxFrame;
}
