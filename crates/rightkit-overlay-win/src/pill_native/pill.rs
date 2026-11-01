//! The pill capsule: state model (ported from `PillModel.swift` / `PillPanel.swift`) and its
//! painter. The model is pure (explicit `Instant`s, no I/O) so it tests on every host; it only
//! reports `Effect`s that the driver in `mod.rs` executes.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use rightkit_overlay::pill::PillEvent;

use super::visuals::{
    self, capsule_size, intro_intensity, smoothstep, spring_state, ButtonId, Fade, Mode, PillScene,
    BAR_SLOTS, CAPSULE_RESIZE_MS, HOVER_EXIT_DELAY_MS, HOVER_EXIT_DURATION_MS, INTRO_SECONDS,
};

/// `scheduleStatusDismiss`: status holds 1.4 s, error 5 s.
pub const STATUS_HOLD_MS: u64 = 1400;
pub const ERROR_HOLD_MS: u64 = 5000;
/// `rightkit_overlay::HoverGate` reports a leave only after three consecutive outside samples,
/// 40 ms apart (`HOVER_TICK_MS`). That already costs about as much as the Swift 100 ms grace, so
/// the Windows driver subtracts it: observed leave-to-collapse stays near 100 ms instead of 220.
pub const HOVER_GATE_LEAVE_MS: u64 = 120;

/// `PillModel.setVisible(false)`: fade 0.22 s, then `orderOut` at 0.24 s.
pub const HIDE_FADE_MS: f32 = 220.0;
pub const HIDE_ORDER_OUT_MS: u64 = 240;
/// Button hover tint animation (`withAnimation(.easeOut(duration: 0.15))`).
pub const BUTTON_HOT_MS: f32 = 150.0;
/// `reveal()` entrance offset in points.
pub const REVEAL_OFFSET: f32 = 5.0;

/// Something the driver must do outside the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Status expired: collapse to idle through the app's reveal path so the
    /// `pill_visibility` gate still applies.
    RevealPill,
    /// Error expired: terminal-only recovery bound to the generation armed with that error.
    RecoverTerminalError(Option<u64>),
    /// The hide fade finished: take the window off screen.
    HideWindow,
    /// First reveal of the launch: play the swell that matches the bloom.
    PlayIntroSound,
}

#[derive(Clone, Debug)]
pub struct PillModel {
    t0: Instant,
    pub mode: Mode,
    pub label: String,
    pub cancellable: bool,
    /// `PillModel.isVisible` (target visibility; the window may still be fading out).
    pub visible: bool,
    /// `PillModel.isHovered`.
    pub hovered: bool,
    /// Raw pointer-over-capsule from overlay hover events.
    pub pointer_inside: bool,
    pub clip_visible: bool,
    pub hint_visible: bool,
    size_from: (f32, f32),
    size_to: (f32, f32),
    size_start_ms: f64,
    size_dur_ms: f32,
    /// `PillModel.presentedMode`: the mode whose capsule size is actually on screen.
    presented: Option<Mode>,
    pub rec_start: Option<Instant>,
    intro_start: Option<Instant>,
    intro_done: bool,
    reveal_start: Option<Instant>,
    opacity: Fade,
    hot: [Fade; 6],
    hover_exit_at: Option<Instant>,
    dismiss_at: Option<Instant>,
    error_generation: Option<u64>,
    hide_window_at: Option<Instant>,
    /// When the capsule's size morph and reveal entrance have finished: the hit regions were
    /// widened to cover the animation and must be tightened again at that moment.
    settle_at: Option<Instant>,
    /// Grace between the pointer leaving and the capsule collapsing. The Swift value
    /// (`HOVER_EXIT_DELAY_MS`, 100 ms) by default; the Windows driver lowers it because the
    /// overlay crate already debounces a leave by three 40 ms ticks (see `HOVER_GATE_LEAVE_MS`).
    pub exit_grace_ms: u64,
}

impl PillModel {
    pub fn new(now: Instant) -> PillModel {
        PillModel {
            t0: now,
            mode: Mode::Idle,
            label: String::new(),
            cancellable: false,
            visible: false,
            hovered: false,
            pointer_inside: false,
            clip_visible: false,
            hint_visible: false,
            size_from: visuals::IDLE_COMPACT,
            size_to: visuals::IDLE_COMPACT,
            size_start_ms: 0.0,
            size_dur_ms: 0.0,
            presented: Some(Mode::Idle),
            rec_start: None,
            intro_start: None,
            intro_done: false,
            reveal_start: None,
            opacity: Fade::at(1.0),
            hot: [Fade::at(0.0); 6],
            hover_exit_at: None,
            dismiss_at: None,
            error_generation: None,
            hide_window_at: None,
            settle_at: None,
            exit_grace_ms: HOVER_EXIT_DELAY_MS as u64,
        }
    }

    /// Milliseconds since the model was created (the clock every `Fade` uses).
    pub fn ms(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.t0).as_secs_f64() * 1000.0
    }

    pub fn secs(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.t0).as_secs_f32()
    }

    // --- capsule size -----------------------------------------------------------------------

    /// Size the capsule is heading for: `capsuleSize(for: model)`.
    pub fn target_size(&self) -> (f32, f32) {
        // A visible hint keeps an idle capsule compact even while the pointer rests on it.
        let hovered = self.hovered && !(self.mode == Mode::Idle && self.hint_visible);
        capsule_size(self.mode, hovered, self.clip_visible, &self.label)
    }

    /// Presentation size at `now` (`model.presentationCapsuleSize`).
    pub fn size_at(&self, now: Instant) -> (f32, f32) {
        if self.size_dur_ms <= 0.0 {
            return self.size_to;
        }
        let l = ((self.ms(now) - self.size_start_ms) / f64::from(self.size_dur_ms)) as f32;
        let p = smoothstep(l);
        (
            self.size_from.0 + (self.size_to.0 - self.size_from.0) * p,
            self.size_from.1 + (self.size_to.1 - self.size_from.1) * p,
        )
    }

    fn size_running(&self, now: Instant) -> bool {
        self.size_dur_ms > 0.0 && self.ms(now) < self.size_start_ms + f64::from(self.size_dur_ms)
    }

    /// `animateCapsule(to:duration:)`: restarts from the current presentation size.
    fn start_resize(&mut self, to: (f32, f32), dur_ms: f32, now: Instant) {
        let from = self.size_at(now);
        self.size_from = from;
        self.size_to = to;
        self.size_start_ms = self.ms(now);
        self.size_dur_ms = if from == to { 0.0 } else { dur_ms };
        if self.size_dur_ms > 0.0 {
            self.note_settle(now + Duration::from_millis(dur_ms.ceil() as u64));
        }
    }

    fn note_settle(&mut self, at: Instant) {
        self.settle_at = Some(self.settle_at.map_or(at, |current| current.max(at)));
    }

    /// Largest extent the capsule paints at while its current animation runs, as
    /// `(width, height, extra_bottom)`: the union of the hit regions has to cover it or the
    /// window shape would clip the morph (a shrinking capsule is larger than its target).
    pub fn visual_extent(&self, now: Instant) -> (f32, f32, f32) {
        let (tw, th) = self.target_size();
        let (mut w, mut h) = (tw, th);
        if self.size_running(now) {
            w = w.max(self.size_from.0).max(self.size_to.0);
            h = h.max(self.size_from.1).max(self.size_to.1);
        }
        let extra = if self.reveal_running(now) {
            REVEAL_OFFSET
        } else {
            0.0
        };
        (w, h, extra)
    }

    /// Instant resize (`restoreCapsulePresentation`, `prepareClipboardPresentation`).
    fn set_size_now(&mut self, to: (f32, f32)) {
        self.size_from = to;
        self.size_to = to;
        self.size_dur_ms = 0.0;
    }

    // --- events -------------------------------------------------------------------------------

    /// `PillModel.push(kind:level:cancellable:label:)`. `error_generation` is read by the caller
    /// when the event is an error (the generation the expiry timer binds to).
    pub fn apply_event(
        &mut self,
        event: &PillEvent,
        now: Instant,
        error_generation: Option<u64>,
    ) -> Vec<Effect> {
        let (next, label, cancellable) = match event {
            PillEvent::Hide | PillEvent::Voice { .. } => return Vec::new(),
            PillEvent::Idle => (Mode::Idle, None, false),
            // The Swift pill renders `starting` as recording.
            PillEvent::Starting | PillEvent::Recording => (Mode::Recording, None, false),
            PillEvent::Processing { cancellable } => (Mode::Processing, None, *cancellable),
            PillEvent::Success => (Mode::Status, Some("Done".to_string()), false),
            PillEvent::Status { label } => {
                let trimmed = label.trim();
                let resolved = if trimmed.is_empty() {
                    "Saved".to_string()
                } else if trimmed.eq_ignore_ascii_case("canceled")
                    || trimmed.eq_ignore_ascii_case("cancelled")
                {
                    "Cancelled".to_string()
                } else {
                    label.clone()
                };
                (Mode::Status, Some(resolved), false)
            }
            PillEvent::Error { label } => {
                let resolved = if label.trim().is_empty() {
                    "Error".to_string()
                } else {
                    label.clone()
                };
                (Mode::Error, Some(resolved), false)
            }
        };
        let entering = next != self.mode;
        self.mode = next;
        self.cancellable = cancellable;
        if let Some(label) = label {
            self.label = label;
        }
        if entering {
            // A hover exit queued before the mode change must not resize the capsule over it.
            self.hover_exit_at = None;
        }
        let transition_needed =
            self.presented != Some(next) || matches!(next, Mode::Status | Mode::Error);
        if transition_needed && !self.clip_visible {
            let target = self.target_size();
            self.start_resize(target, CAPSULE_RESIZE_MS, now);
            self.presented = Some(next);
        }
        if next == Mode::Recording {
            if entering || self.rec_start.is_none() {
                self.rec_start = Some(now);
            }
        } else {
            self.rec_start = None;
        }
        self.error_generation = if next == Mode::Error {
            error_generation
        } else {
            None
        };
        self.arm_dismiss(now);
        Vec::new()
    }

    fn arm_dismiss(&mut self, now: Instant) {
        self.dismiss_at = match self.mode {
            Mode::Status => Some(now + Duration::from_millis(STATUS_HOLD_MS)),
            Mode::Error => Some(now + Duration::from_millis(ERROR_HOLD_MS)),
            _ => None,
        };
    }

    // --- visibility / launch --------------------------------------------------------------------

    /// `setVisible`: showing is immediate, hiding fades for 0.22 s and orders out at 0.24 s.
    pub fn set_visible(&mut self, visible: bool, now: Instant) {
        if visible {
            self.visible = true;
            self.opacity = Fade::at(1.0);
            self.hide_window_at = None;
        } else if self.visible {
            self.visible = false;
            let ms = self.ms(now);
            self.opacity = Fade {
                from: self.opacity.value(ms),
                to: 0.0,
                start_ms: ms,
                dur_ms: HIDE_FADE_MS,
            };
            self.hide_window_at = Some(now + Duration::from_millis(HIDE_ORDER_OUT_MS));
        }
    }

    /// `reveal()`: show, play the spring entrance, and run the one-time bloom when `intro`.
    pub fn reveal(&mut self, now: Instant, intro: bool) -> Vec<Effect> {
        self.set_visible(true, now);
        self.reveal_start = Some(now);
        self.note_settle(now + Duration::from_millis(700));
        let mut effects = Vec::new();
        if intro && !self.intro_done {
            self.intro_done = true;
            self.intro_start = Some(now);
            effects.push(Effect::PlayIntroSound);
        }
        effects
    }

    pub fn intro_value(&self, now: Instant) -> f32 {
        match self.intro_start {
            Some(start) => {
                let progress = now.saturating_duration_since(start).as_secs_f32() / INTRO_SECONDS;
                if progress >= 1.0 {
                    0.0
                } else {
                    intro_intensity(progress)
                }
            }
            None => 0.0,
        }
    }

    fn intro_running(&self, now: Instant) -> bool {
        self.intro_start
            .is_some_and(|s| now.saturating_duration_since(s).as_secs_f32() < INTRO_SECONDS)
    }

    fn reveal_offset(&self, now: Instant) -> f32 {
        match self.reveal_start {
            Some(start) => {
                let t = now.saturating_duration_since(start).as_secs_f32();
                (1.0 - spring_state(t)).max(0.0) * REVEAL_OFFSET
            }
            None => 0.0,
        }
    }

    fn reveal_running(&self, now: Instant) -> bool {
        self.reveal_start
            .is_some_and(|s| now.saturating_duration_since(s).as_secs_f32() < 0.7)
    }

    // --- pointer / hover ------------------------------------------------------------------------

    /// `setHover` driven by the raw pointer state (enter immediate, leave after 100 ms).
    pub fn set_pointer(&mut self, inside: bool, now: Instant) {
        if inside == self.pointer_inside {
            return;
        }
        self.pointer_inside = inside;
        if inside {
            self.hover_exit_at = None;
            self.hovered = true;
            if matches!(self.mode, Mode::Idle | Mode::Recording)
                && !(self.mode == Mode::Idle && self.hint_visible)
                && !self.clip_visible
            {
                let target = self.target_size();
                self.start_resize(target, CAPSULE_RESIZE_MS, now);
            }
        } else {
            self.hover_exit_at = Some(now + Duration::from_millis(self.exit_grace_ms));
        }
    }

    /// Per-button hover tint.
    pub fn set_button_hot(&mut self, button: ButtonId, hot: bool, now: Instant) {
        let ms = self.ms(now);
        self.hot[button.index()].retarget(if hot { 1.0 } else { 0.0 }, ms, BUTTON_HOT_MS);
    }

    pub fn clear_button_hot(&mut self, now: Instant) {
        for button in visuals::BUTTONS {
            self.set_button_hot(button, false, now);
        }
    }

    // --- clip / hint ----------------------------------------------------------------------------

    /// `prepareClipboardPresentation` / `dismissClipboard`.
    pub fn set_clip_visible(&mut self, visible: bool, now: Instant) {
        if visible == self.clip_visible {
            return;
        }
        let _ = now;
        self.clip_visible = visible;
        if visible {
            // Clip always attaches to the expanded idle shell.
            self.set_size_now(visuals::IDLE_EXPANDED);
        } else {
            let target = self.target_size();
            self.set_size_now(target);
            self.presented = Some(self.mode);
        }
    }

    /// `setHint`: a visible hint pins the capsule at the size of the mode it is showing.
    pub fn set_hint_visible(&mut self, visible: bool, now: Instant) {
        if visible == self.hint_visible {
            return;
        }
        self.hint_visible = visible;
        if visible {
            self.hover_exit_at = None;
            if self.mode == Mode::Idle {
                self.hovered = false;
            }
            if !self.clip_visible {
                let target = self.target_size();
                self.set_size_now(target);
                self.presented = Some(self.mode);
            }
        } else if !self.clip_visible {
            let target = self.target_size();
            if self.presented == Some(self.mode) {
                self.set_size_now(target);
            } else {
                self.start_resize(target, CAPSULE_RESIZE_MS, now);
                self.presented = Some(self.mode);
            }
        }
    }

    // --- timers ---------------------------------------------------------------------------------

    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.hover_exit_at,
            self.dismiss_at,
            self.hide_window_at,
            self.settle_at,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub fn on_timers(&mut self, now: Instant) -> Vec<Effect> {
        let mut effects = Vec::new();
        if self.settle_at.is_some_and(|at| now >= at) {
            self.settle_at = None;
        }
        if self.hide_window_at.is_some_and(|at| now >= at) {
            self.hide_window_at = None;
            effects.push(Effect::HideWindow);
        }
        if self.hover_exit_at.is_some_and(|at| now >= at) {
            self.hover_exit_at = None;
            self.hovered = false;
            if !self.clip_visible && matches!(self.mode, Mode::Idle | Mode::Recording) {
                let target = self.target_size();
                self.start_resize(target, HOVER_EXIT_DURATION_MS, now);
            }
            if self.mode == Mode::Error {
                // The expiry was dropped while the pointer held the error open.
                self.arm_dismiss(now);
            }
        }
        if self.dismiss_at.is_some_and(|at| now >= at) {
            self.dismiss_at = None;
            // Swift: `guard !(mode == .error && isHovered)`; the hover exit re-arms it.
            if !(self.mode == Mode::Error && self.hovered) {
                let was_error = self.mode == Mode::Error;
                let generation = self.error_generation.take();
                self.apply_event(&PillEvent::Idle, now, None);
                effects.push(if was_error {
                    Effect::RecoverTerminalError(generation)
                } else {
                    Effect::RevealPill
                });
            }
        }
        effects
    }

    // --- frames -----------------------------------------------------------------------------------

    /// True while something on the capsule is animating and the window needs frames.
    pub fn needs_frames(&self, now: Instant) -> bool {
        let shown = self.visible || self.hide_window_at.is_some();
        if !shown {
            return false;
        }
        let ms = self.ms(now);
        matches!(self.mode, Mode::Recording | Mode::Processing)
            || self.size_running(now)
            || self.intro_running(now)
            || self.reveal_running(now)
            || !self.opacity.done(ms)
            || self.hot.iter().any(|h| !h.done(ms))
    }

    /// Scene for the frame at `now`.
    pub fn scene(&self, now: Instant, bars: &[f32; BAR_SLOTS]) -> PillScene {
        let ms = self.ms(now);
        let (cap_w, cap_h) = self.size_at(now);
        let mut hot = [0.0f32; 6];
        for (slot, fade) in hot.iter_mut().zip(self.hot.iter()) {
            *slot = fade.value(ms);
        }
        PillScene {
            mode: self.mode,
            label: self.label.clone(),
            cap_w,
            cap_h,
            hovered: self.hovered,
            clipboard_visible: self.clip_visible,
            hint_visible: self.hint_visible,
            bars: *bars,
            elapsed_secs: self
                .rec_start
                .map(|s| now.saturating_duration_since(s).as_secs() as u32)
                .unwrap_or(0),
            intro: self.intro_value(now),
            hot,
            opacity: self.opacity.value(ms).clamp(0.0, 1.0),
            offset_y: self.reveal_offset(now),
        }
    }

    /// The scene the capsule settles into: used for hit regions and the hub-transition hitbox.
    pub fn settled_scene(&self, elapsed_secs: u32) -> PillScene {
        let (cap_w, cap_h) = self.target_size();
        PillScene {
            mode: self.mode,
            label: self.label.clone(),
            cap_w,
            cap_h,
            hovered: self.hovered,
            clipboard_visible: self.clip_visible,
            hint_visible: self.hint_visible,
            bars: [0.0; BAR_SLOTS],
            elapsed_secs,
            intro: 0.0,
            hot: [0.0; 6],
            opacity: 1.0,
            offset_y: 0.0,
        }
    }
}

/// Bar buffer driven once per painted frame (`PillModel.integrateLevels`).
#[derive(Clone, Debug)]
pub struct BarState {
    pub buf: [f32; BAR_SLOTS],
    mode: Option<Mode>,
    last: Option<Instant>,
}

impl BarState {
    pub fn new() -> BarState {
        BarState {
            buf: [0.0; BAR_SLOTS],
            mode: None,
            last: None,
        }
    }

    pub fn step(&mut self, mode: Mode, level: f32, now: Instant, secs: f32) {
        if self.mode != Some(mode) {
            self.mode = Some(mode);
            self.buf = [0.0; BAR_SLOTS];
            self.last = None;
        }
        let dt_ms = self
            .last
            .map(|l| now.saturating_duration_since(l).as_secs_f32() * 1000.0)
            .unwrap_or(1000.0 / 60.0)
            .min(100.0);
        match mode {
            Mode::Recording => visuals::integrate_recording(&mut self.buf, level, secs, dt_ms),
            Mode::Processing => self.buf = visuals::processing_levels(secs),
            _ => {
                if self.buf.iter().any(|v| *v != 0.0) {
                    self.buf = [0.0; BAR_SLOTS];
                }
            }
        }
        self.last = Some(now);
    }
}

impl Default for BarState {
    fn default() -> Self {
        BarState::new()
    }
}

// ---------------------------------------------------------------------------------------------
// Painter (Windows only)
// ---------------------------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(super) mod painter {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::time::Instant;

    use super::super::visuals::{build_pill, PILL_WINDOW_BOTTOM_PAD, PILL_WINDOW_H, PILL_WINDOW_W};
    use super::super::{gfx, Cmd, Shared, Surf};
    use super::BarState;

    pub struct PillPainter {
        pub shared: Arc<Shared>,
        pub tx: Sender<Cmd>,
        pub bars: BarState,
    }

    impl gfx::PainterBody for PillPainter {
        fn body(&mut self, ctx: &mut gfx::Ctx) {
            let now = Instant::now();
            let (scene, needs) = {
                let visual = self.shared.visual.lock().unwrap_or_else(|e| e.into_inner());
                let level = f32::from_bits(self.shared.level_bits.load(Ordering::Relaxed));
                let secs = visual.pill.secs(now);
                self.bars.step(visual.pill.mode, level, now, secs);
                let scene = visual.pill.scene(now, &self.bars.buf);
                (scene, visual.pill.needs_frames(now))
            };
            let frame = build_pill(
                &scene,
                PILL_WINDOW_W,
                PILL_WINDOW_H + PILL_WINDOW_BOTTOM_PAD,
                PILL_WINDOW_BOTTOM_PAD,
            );
            gfx::replay(ctx, &frame.ops);
            if !needs && self.shared.pump[Surf::Pill as usize].swap(false, Ordering::SeqCst) {
                let _ = self.tx.send(Cmd::PumpIdle(Surf::Pill));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    fn model() -> (PillModel, Instant) {
        let t0 = Instant::now();
        (PillModel::new(t0), t0)
    }

    #[test]
    fn recording_grows_the_capsule_over_150ms_with_smoothstep() {
        let (mut m, t0) = model();
        m.set_visible(true, t0);
        m.apply_event(&PillEvent::Recording, t0, None);
        assert_eq!(m.mode, Mode::Recording);
        assert_eq!(m.target_size(), (106.0, 36.0));
        assert_eq!(m.size_at(t0), (64.0, 10.0));
        let mid = m.size_at(t0 + secs(0.075));
        assert!(
            (mid.0 - 85.0).abs() < 0.5,
            "smoothstep midpoint is the average: {mid:?}"
        );
        assert_eq!(m.size_at(t0 + secs(0.2)), (106.0, 36.0));
        assert!(
            m.needs_frames(t0 + secs(0.2)),
            "recording always animates its bars"
        );
    }

    #[test]
    fn the_visual_extent_covers_a_shrinking_morph_and_tightens_when_it_settles() {
        let (mut m, t0) = model();
        m.set_visible(true, t0);
        m.apply_event(&PillEvent::Recording, t0, None);
        let settled = t0 + secs(0.5);
        m.on_timers(settled);
        assert_eq!(m.visual_extent(settled), (106.0, 36.0, 0.0));
        assert_eq!(
            m.next_deadline(),
            None,
            "nothing is scheduled once the grow has settled"
        );

        m.apply_event(&PillEvent::Idle, settled, None);
        // The target is the small idle capsule, but the window shape must still cover the
        // recording-sized capsule that is shrinking.
        assert_eq!(m.target_size(), (64.0, 10.0));
        assert_eq!(m.visual_extent(settled + secs(0.01)), (106.0, 36.0, 0.0));
        assert!(
            m.next_deadline().is_some(),
            "a settle wake is armed to tighten the regions"
        );

        let later = settled + secs(1.0);
        m.on_timers(later);
        assert_eq!(m.visual_extent(later), (64.0, 10.0, 0.0));
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn the_reveal_entrance_extends_the_extent_below_the_capsule_until_it_settles() {
        let (mut m, t0) = model();
        m.reveal(t0, false);
        let (_, _, extra) = m.visual_extent(t0 + secs(0.05));
        assert_eq!(extra, REVEAL_OFFSET);
        let later = t0 + secs(1.0);
        m.on_timers(later);
        assert_eq!(m.visual_extent(later).2, 0.0);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn starting_renders_as_recording() {
        let (mut m, t0) = model();
        m.apply_event(&PillEvent::Starting, t0, None);
        assert_eq!(m.mode, Mode::Recording);
        assert!(m.rec_start.is_some());
        // A duplicate acknowledgement must not restart the timer.
        let first = m.rec_start;
        m.apply_event(&PillEvent::Recording, t0 + secs(1.0), None);
        assert_eq!(m.rec_start, first);
    }

    #[test]
    fn hover_enter_is_immediate_and_leave_waits_100ms_then_collapses_over_260ms() {
        let (mut m, t0) = model();
        m.set_visible(true, t0);
        m.set_pointer(true, t0);
        assert!(m.hovered);
        assert_eq!(m.target_size(), (124.0, 44.0));
        assert_eq!(m.size_at(t0 + secs(0.2)), (124.0, 44.0));

        let leave = t0 + secs(1.0);
        m.set_pointer(false, leave);
        assert!(m.hovered, "grace period keeps the dock open");
        assert_eq!(m.next_deadline(), Some(leave + Duration::from_millis(100)));
        assert!(m.on_timers(leave + secs(0.05)).is_empty());
        assert!(m.hovered);
        m.on_timers(leave + secs(0.11));
        assert!(!m.hovered);
        assert_eq!(m.target_size(), (64.0, 10.0));
        let mid = m.size_at(leave + secs(0.11) + secs(0.13));
        assert!(
            mid.0 < 124.0 && mid.0 > 64.0,
            "collapse is animating: {mid:?}"
        );
        assert_eq!(m.size_at(leave + secs(0.5)), (64.0, 10.0));
    }

    #[test]
    fn re_entering_during_the_grace_cancels_the_collapse() {
        let (mut m, t0) = model();
        m.set_pointer(true, t0);
        m.set_pointer(false, t0 + secs(1.0));
        m.set_pointer(true, t0 + secs(1.05));
        assert_eq!(m.next_deadline(), None);
        assert!(m.on_timers(t0 + secs(2.0)).is_empty());
        assert!(m.hovered);
    }

    #[test]
    fn status_expires_through_reveal_pill_after_1400ms() {
        let (mut m, t0) = model();
        m.apply_event(
            &PillEvent::Status {
                label: "Pasted".to_string(),
            },
            t0,
            None,
        );
        assert_eq!(m.mode, Mode::Status);
        assert_eq!(m.label, "Pasted");
        assert_eq!(m.next_deadline(), Some(t0 + Duration::from_millis(1400)));
        assert!(m.on_timers(t0 + secs(1.0)).is_empty());
        let effects = m.on_timers(t0 + secs(1.41));
        assert_eq!(effects, vec![Effect::RevealPill]);
        assert_eq!(m.mode, Mode::Idle);
        assert_eq!(m.next_deadline(), None);
    }

    #[test]
    fn error_expiry_is_terminal_recovery_bound_to_its_generation() {
        let (mut m, t0) = model();
        m.apply_event(
            &PillEvent::Error {
                label: "Mic unavailable".to_string(),
            },
            t0,
            Some(7),
        );
        assert_eq!(m.mode, Mode::Error);
        assert_eq!(m.next_deadline(), Some(t0 + Duration::from_millis(5000)));
        let effects = m.on_timers(t0 + secs(5.01));
        assert_eq!(effects, vec![Effect::RecoverTerminalError(Some(7))]);
        assert_eq!(m.mode, Mode::Idle);
    }

    #[test]
    fn a_hovered_error_is_held_open_until_the_pointer_leaves() {
        let (mut m, t0) = model();
        m.apply_event(
            &PillEvent::Error {
                label: "No mic".to_string(),
            },
            t0,
            Some(3),
        );
        m.set_pointer(true, t0 + secs(4.0));
        assert!(
            m.on_timers(t0 + secs(5.1)).is_empty(),
            "dropped while hovered"
        );
        assert_eq!(m.mode, Mode::Error);
        m.set_pointer(false, t0 + secs(6.0));
        m.on_timers(t0 + secs(6.2));
        // Re-armed from the hover exit.
        assert_eq!(
            m.next_deadline(),
            Some(t0 + secs(6.2) + Duration::from_millis(5000))
        );
        let effects = m.on_timers(t0 + secs(11.3));
        assert_eq!(effects, vec![Effect::RecoverTerminalError(Some(3))]);
    }

    #[test]
    fn cancel_labels_normalise_and_empty_labels_default() {
        let (mut m, t0) = model();
        m.apply_event(
            &PillEvent::Status {
                label: "Cancelled".to_string(),
            },
            t0,
            None,
        );
        assert_eq!(m.label, "Cancelled");
        m.apply_event(
            &PillEvent::Status {
                label: "  ".to_string(),
            },
            t0,
            None,
        );
        assert_eq!(m.label, "Saved");
        m.apply_event(&PillEvent::Success, t0, None);
        assert_eq!(m.label, "Done");
        m.apply_event(
            &PillEvent::Error {
                label: String::new(),
            },
            t0,
            None,
        );
        assert_eq!(m.label, "Error");
    }

    #[test]
    fn status_capsule_resizes_to_its_label_every_time() {
        let (mut m, t0) = model();
        m.apply_event(
            &PillEvent::Status {
                label: "Saved".to_string(),
            },
            t0,
            None,
        );
        let first = m.target_size();
        m.apply_event(
            &PillEvent::Status {
                label: "Pasted (unverified)".to_string(),
            },
            t0 + secs(0.1),
            None,
        );
        assert!(m.target_size().0 > first.0);
    }

    #[test]
    fn open_clip_pins_the_expanded_idle_shell_and_refuses_mode_resizes() {
        let (mut m, t0) = model();
        m.set_clip_visible(true, t0);
        assert_eq!(m.size_at(t0), (124.0, 44.0));
        m.apply_event(&PillEvent::Processing { cancellable: false }, t0, None);
        assert_eq!(
            m.size_at(t0 + secs(0.5)),
            (124.0, 44.0),
            "refused while the clip is open"
        );
        m.set_clip_visible(false, t0 + secs(1.0));
        assert_eq!(
            m.size_at(t0 + secs(1.0)),
            (72.0, 32.0),
            "restored instantly to the mode size"
        );
    }

    #[test]
    fn hint_pins_the_capsule_and_clears_idle_hover() {
        let (mut m, t0) = model();
        m.set_pointer(true, t0);
        assert!(m.hovered);
        m.set_hint_visible(true, t0 + secs(1.0));
        assert!(!m.hovered, "setHint clears idle hover");
        assert_eq!(m.size_at(t0 + secs(1.0)), (64.0, 10.0));
        m.set_hint_visible(false, t0 + secs(2.0));
        assert_eq!(m.size_at(t0 + secs(2.0)), (64.0, 10.0));
    }

    #[test]
    fn hiding_fades_for_220ms_and_orders_out_at_240ms() {
        let (mut m, t0) = model();
        m.set_visible(true, t0);
        m.set_visible(false, t0 + secs(1.0));
        assert!(m.needs_frames(t0 + secs(1.1)) || matches!(m.mode, Mode::Idle));
        let s = m.scene(t0 + secs(1.0) + secs(0.3), &[0.0; BAR_SLOTS]);
        assert!(s.opacity < 0.001);
        assert!(m.on_timers(t0 + secs(1.1)).is_empty());
        assert_eq!(m.on_timers(t0 + secs(1.25)), vec![Effect::HideWindow]);
        assert!(!m.needs_frames(t0 + secs(1.3)), "no frames once hidden");
    }

    #[test]
    fn idle_window_needs_no_frames() {
        let (mut m, t0) = model();
        m.set_visible(true, t0);
        assert!(
            !m.needs_frames(t0 + secs(5.0)),
            "a resting pill runs no animation"
        );
    }

    #[test]
    fn reveal_runs_the_bloom_exactly_once() {
        let (mut m, t0) = model();
        let first = m.reveal(t0, true);
        assert_eq!(first, vec![Effect::PlayIntroSound]);
        assert!(m.needs_frames(t0 + secs(1.0)));
        assert!(
            m.intro_value(t0 + secs(1.17)) > 0.9,
            "peak near 45 % of 2.6 s"
        );
        assert!(m.intro_value(t0 + secs(2.7)) == 0.0);
        let second = m.reveal(t0 + secs(10.0), true);
        assert!(second.is_empty());
        assert!(!m.needs_frames(t0 + secs(20.0)));
    }

    #[test]
    fn button_hot_fades_in_150ms() {
        let (mut m, t0) = model();
        m.set_visible(true, t0);
        m.set_button_hot(ButtonId::Start, true, t0);
        let half = m.scene(t0 + secs(0.075), &[0.0; BAR_SLOTS]).hot[ButtonId::Start.index()];
        assert!(half > 0.4 && half < 1.0);
        let full = m.scene(t0 + secs(0.3), &[0.0; BAR_SLOTS]).hot[ButtonId::Start.index()];
        assert!((full - 1.0).abs() < 1e-3);
        assert!(!m.needs_frames(t0 + secs(0.3)));
    }

    #[test]
    fn bars_reset_when_the_mode_changes() {
        let mut bars = BarState::new();
        let t0 = Instant::now();
        bars.step(Mode::Recording, 1.0, t0, 1.0);
        assert!(bars.buf.iter().any(|v| *v > 0.1));
        bars.step(Mode::Idle, 0.0, t0 + secs(0.016), 1.016);
        assert!(bars.buf.iter().all(|v| *v == 0.0));
        bars.step(Mode::Processing, 0.0, t0 + secs(0.033), 1.033);
        assert!(bars.buf.iter().all(|v| *v >= 0.18));
    }

    #[test]
    fn settled_scene_uses_the_target_size() {
        let (mut m, t0) = model();
        m.set_pointer(true, t0);
        let s = m.settled_scene(0);
        assert_eq!((s.cap_w, s.cap_h), (124.0, 44.0));
        assert!(s.idle_controls_visible());
    }
}
