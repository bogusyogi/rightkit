//! The pill capsule's controller state: mode, hover, visibility, timers, size spring, content
//! cross-fade, copy feedback, launch bloom and reveal. The model is pure (explicit `Instant`s, no
//! I/O) so it tests on every host; it only reports `Effect`s that the driver executes. Every
//! tuned value (spring parameters, fade lengths, sizes) comes from the host's look
//! (`look::Motion`, `Painters::capsule_size`); none lives here.

#![allow(dead_code)]

use std::time::Instant;

use rightkit_dictation_core::pill::PillEvent;

use crate::anim::{apply_curve, Fade, Mode, SpringAnim, SpringParams};
use crate::clock::{after, after_secs};
use crate::config::{self, limits, metrics, motion, status, timings};
use crate::look::{PillScene, PrevContent};
use crate::regions::{HotSet, RegionId};

/// What the capsule content shows, for cross-fading when it changes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ContentSnap {
    mode: Mode,
    label: String,
    /// Recording dock (hovered recording) only.
    hovered: bool,
    /// Idle controls visible.
    controls: bool,
}

/// The content that was replaced and when.
#[derive(Clone, Debug)]
struct PrevSnap {
    snap: ContentSnap,
    elapsed_secs: u32,
    start_ms: f64,
}

#[derive(Clone, Copy, Debug)]
struct CopyFlash {
    copied: bool,
    start_ms: f64,
}

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
    /// Target visibility (the window may still be fading out).
    pub visible: bool,
    /// The pointer counts as over the capsule (with the exit grace applied).
    pub hovered: bool,
    /// Raw pointer-over-capsule from overlay hover events.
    pub pointer_inside: bool,
    pub clip_visible: bool,
    pub hint_visible: bool,
    /// Capsule width and height, each on its own spring with shared parameters.
    size_w: SpringAnim,
    size_h: SpringAnim,
    /// The Windows "show animations" setting (`SPI_GETCLIENTAREAANIMATION`). When off there is
    /// no overshoot, no breathing and no scale, and changes are short fades.
    pub animations: bool,
    /// When the current processing state began (the animation stays hidden for a moment).
    proc_start: Option<Instant>,
    content: ContentSnap,
    prev_content: Option<PrevSnap>,
    copy_flash: Option<CopyFlash>,
    /// The mode whose capsule size is actually on screen.
    presented: Option<Mode>,
    pub rec_start: Option<Instant>,
    intro_start: Option<Instant>,
    intro_done: bool,
    reveal_start: Option<Instant>,
    opacity: Fade,
    /// Hover amount per region of the last layout (created on first hover).
    hot: Vec<(RegionId, Fade)>,
    hover_exit_at: Option<Instant>,
    dismiss_at: Option<Instant>,
    error_generation: Option<u64>,
    hide_window_at: Option<Instant>,
    /// When the capsule's size morph and reveal entrance have finished: the hit regions were
    /// widened to cover the animation and must be tightened again at that moment.
    settle_at: Option<Instant>,
    /// Grace between the pointer leaving and the capsule collapsing: the look's
    /// `Motion::hover_exit_delay_ms`, applied as given.
    pub exit_grace_ms: u64,
}

impl PillModel {
    pub fn new(now: Instant) -> PillModel {
        let m = motion();
        let rest = config::capsule_size(Mode::Idle, false, false, "");
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
            size_w: SpringAnim::at(rest.0, m.capsule_spring, m.capsule_spring_eps),
            size_h: SpringAnim::at(rest.1, m.capsule_spring, m.capsule_spring_eps),
            animations: true,
            proc_start: None,
            content: ContentSnap {
                mode: Mode::Idle,
                label: String::new(),
                hovered: false,
                controls: false,
            },
            prev_content: None,
            copy_flash: None,
            presented: Some(Mode::Idle),
            rec_start: None,
            intro_start: None,
            intro_done: false,
            reveal_start: None,
            opacity: Fade::at(1.0),
            hot: Vec::new(),
            hover_exit_at: None,
            dismiss_at: None,
            error_generation: None,
            hide_window_at: None,
            settle_at: None,
            exit_grace_ms: m.hover_exit_delay_ms,
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

    /// Size the capsule is heading for in its current mode (sanitised, see
    /// `config::capsule_size`).
    pub fn target_size(&self) -> (f32, f32) {
        // A visible hint keeps an idle capsule compact even while the pointer rests on it.
        let hovered = self.hovered && !(self.mode == Mode::Idle && self.hint_visible);
        config::capsule_size(self.mode, hovered, self.clip_visible, &self.label)
    }

    fn capsule_params(&self) -> SpringParams {
        if self.animations {
            motion().capsule_spring
        } else {
            motion().reduced_spring
        }
    }

    /// Presentation size at `now`, clamped to the overlay window (`install` guarantees the
    /// window is at least one point each way, so the bounds are ordered; a NaN becomes 1).
    pub fn size_at(&self, now: Instant) -> (f32, f32) {
        let ms = self.ms(now);
        let m = metrics();
        (
            self.size_w.value(ms).max(1.0).min(m.pill_window_w),
            self.size_h.value(ms).max(1.0).min(m.pill_window_h),
        )
    }

    fn size_running(&self, now: Instant) -> bool {
        let ms = self.ms(now);
        !self.size_w.done(ms) || !self.size_h.done(ms)
    }

    /// Both axes chase `to` on the capsule spring from where they are, with the velocity they
    /// have, so a retarget mid-animation continues smoothly.
    fn start_resize(&mut self, to: (f32, f32), now: Instant) {
        let ms = self.ms(now);
        let params = self.capsule_params();
        self.size_w.retarget(to.0, ms, params);
        self.size_h.retarget(to.1, ms, params);
        let settle = self
            .size_w
            .settle_after_ms()
            .max(self.size_h.settle_after_ms());
        if settle > 0.0 {
            // A technical margin (not a product value) so the timer fires after the last frame.
            self.note_settle(after_secs(now, settle / 1000.0 + 0.03));
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
            let slack = metrics().extent_slack;
            w = (w.max(self.size_w.start_value()) + slack).min(metrics().pill_window_w);
            h = (h.max(self.size_h.start_value()) + slack).min(metrics().pill_window_h);
        }
        let extra = if self.reveal_running(now) {
            motion().reveal_offset
        } else {
            0.0
        };
        (w, h, extra)
    }

    /// Instant resize (no spring).
    fn set_size_now(&mut self, to: (f32, f32)) {
        self.size_w.snap(to.0);
        self.size_h.snap(to.1);
    }

    // --- content cross-fade, processing delay, copy feedback --------------------------------

    fn elapsed_secs(&self, now: Instant) -> u32 {
        self.rec_start
            .map(|s| u32::try_from(now.saturating_duration_since(s).as_secs()).unwrap_or(u32::MAX))
            .unwrap_or(0)
    }

    fn content_now(&self) -> ContentSnap {
        ContentSnap {
            mode: self.mode,
            label: if matches!(self.mode, Mode::Status | Mode::Error) {
                self.label.clone()
            } else {
                String::new()
            },
            hovered: self.mode == Mode::Recording && self.hovered,
            controls: self.mode == Mode::Idle
                && (self.clip_visible || (!self.hint_visible && self.hovered)),
        }
    }

    /// Start a cross-fade when what the capsule shows has changed. `elapsed_before` is the
    /// recording timer of the content being replaced.
    fn note_content(&mut self, now: Instant, elapsed_before: u32) {
        let snap = self.content_now();
        if snap == self.content {
            return;
        }
        let old = std::mem::replace(&mut self.content, snap);
        self.prev_content = Some(PrevSnap {
            snap: old,
            elapsed_secs: elapsed_before,
            start_ms: self.ms(now),
        });
    }

    fn crossfade_ms(&self) -> f32 {
        if self.animations {
            motion().crossfade_ms
        } else {
            motion().crossfade_reduced_ms
        }
    }

    fn crossfade_running(&self, now_ms: f64) -> bool {
        self.prev_content
            .as_ref()
            .is_some_and(|p| now_ms - p.start_ms < f64::from(self.crossfade_ms()))
    }

    /// Opacity of the processing animation: hidden for `Motion::processing_delay_ms`, then faded in.
    fn processing_alpha(&self, now: Instant) -> f32 {
        match (self.mode, self.proc_start) {
            (Mode::Processing, Some(start)) => {
                let t = now.saturating_duration_since(start).as_secs_f32() * 1000.0;
                apply_curve(
                    motion().crossfade_curve,
                    (t - motion().processing_delay_ms) / self.crossfade_ms(),
                )
            }
            _ => 1.0,
        }
    }

    /// Copy-last finished: flash a check mark (`copied`) or dim the button (empty history).
    pub fn flash_copy(&mut self, copied: bool, now: Instant) {
        self.copy_flash = Some(CopyFlash {
            copied,
            start_ms: self.ms(now),
        });
    }

    fn copy_flash_running(&self, now_ms: f64) -> bool {
        self.copy_flash.is_some_and(|f| {
            now_ms - f.start_ms
                < if f.copied {
                    motion().copy_check_ms
                } else {
                    motion().copy_empty_ms
                }
        })
    }

    /// The copy-last button ignores clicks while it shows the empty (disabled) state.
    pub fn copy_blocked(&self, now: Instant) -> bool {
        let ms = self.ms(now);
        self.copy_flash
            .is_some_and(|f| !f.copied && ms - f.start_ms < motion().copy_empty_ms)
    }

    /// `(check, dim)` of the copy-last button at `now_ms`: a fade in, a hold, a fade out.
    fn copy_amounts(&self, now_ms: f64) -> (f32, f32) {
        let Some(flash) = self.copy_flash else {
            return (0.0, 0.0);
        };
        let t = now_ms - flash.start_ms;
        let m = motion();
        let fade_out = m.copy_fade_out_ms;
        let (fade_in, total) = if flash.copied {
            (m.copy_check_fade_in_ms, m.copy_check_ms)
        } else {
            (m.copy_empty_fade_in_ms, m.copy_empty_ms)
        };
        let curve = m.fade_curve;
        let level = if t < 0.0 || t >= total {
            0.0
        } else if t < fade_in {
            apply_curve(curve, (t / fade_in) as f32)
        } else if t < total - fade_out {
            1.0
        } else {
            1.0 - apply_curve(curve, ((t - (total - fade_out)) / fade_out) as f32)
        };
        if flash.copied {
            (level, 0.0)
        } else {
            (0.0, level)
        }
    }

    // --- events -------------------------------------------------------------------------------

    /// Apply a pill event. `error_generation` is read by the caller when the event is an error
    /// (the generation the expiry timer binds to). Labels are cut to `Limits::max_label_bytes`.
    pub fn apply_event(
        &mut self,
        event: &PillEvent,
        now: Instant,
        error_generation: Option<u64>,
    ) -> Vec<Effect> {
        let (next, label, cancellable) = match event {
            PillEvent::Hide | PillEvent::Voice { .. } => return Vec::new(),
            PillEvent::Idle => (Mode::Idle, None, false),
            // What a `Starting` event shows is the host's choice (`HostInputs::starting_mode`,
            // validated at install); the crate has no default for it.
            PillEvent::Starting => (config::starting_mode(), None, false),
            PillEvent::Recording => (Mode::Recording, None, false),
            PillEvent::Processing { cancellable } => (Mode::Processing, None, *cancellable),
            PillEvent::Success => (Mode::Status, Some(status().done.to_string()), false),
            PillEvent::Status { label } => {
                let trimmed = label.trim();
                let resolved = if trimmed.is_empty() {
                    status().saved.to_string()
                } else if status()
                    .cancelled_aliases
                    .iter()
                    .any(|alias| trimmed.eq_ignore_ascii_case(alias))
                {
                    status().cancelled.to_string()
                } else {
                    bounded_label(label)
                };
                (Mode::Status, Some(resolved), false)
            }
            PillEvent::Error { label } => {
                let resolved = if label.trim().is_empty() {
                    status().error.to_string()
                } else {
                    bounded_label(label)
                };
                (Mode::Error, Some(resolved), false)
            }
        };
        let elapsed_before = self.elapsed_secs(now);
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
            self.start_resize(target, now);
            self.presented = Some(next);
        }
        if next == Mode::Processing {
            if entering || self.proc_start.is_none() {
                self.proc_start = Some(now);
            }
        } else {
            self.proc_start = None;
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
        self.note_content(now, elapsed_before);
        Vec::new()
    }

    fn arm_dismiss(&mut self, now: Instant) {
        self.dismiss_at = match self.mode {
            Mode::Status => Some(after(now, timings().status_hold_ms)),
            Mode::Error => Some(after(now, timings().error_hold_ms)),
            _ => None,
        };
    }

    // --- visibility / launch --------------------------------------------------------------------

    /// Showing is immediate; hiding fades for `Motion::hide_fade_ms` and takes the window off
    /// screen at `Motion::hide_order_out_ms`.
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
                dur_ms: motion().hide_fade_ms,
            };
            self.hide_window_at = Some(after(now, motion().hide_order_out_ms));
        }
    }

    /// Show, play the spring entrance, and run the one-time bloom when `intro`.
    pub fn reveal(&mut self, now: Instant, intro: bool) -> Vec<Effect> {
        self.set_visible(true, now);
        self.reveal_start = Some(now);
        self.note_settle(after(now, motion().reveal_ms as u64));
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
                let m = motion();
                // `install` guarantees `intro_seconds >= 0.001`.
                let progress = now.saturating_duration_since(start).as_secs_f32() / m.intro_seconds;
                if progress >= 1.0 {
                    0.0
                } else {
                    finite_or_zero((m.intro_curve)(progress))
                }
            }
            None => 0.0,
        }
    }

    fn intro_running(&self, now: Instant) -> bool {
        self.intro_start.is_some_and(|s| {
            now.saturating_duration_since(s).as_secs_f32() < motion().intro_seconds
        })
    }

    fn reveal_offset(&self, now: Instant) -> f32 {
        match self.reveal_start {
            Some(start) => {
                let t = now.saturating_duration_since(start).as_secs_f32();
                let m = motion();
                (1.0 - finite_or_zero((m.reveal_curve)(t))).max(0.0) * m.reveal_offset
            }
            None => 0.0,
        }
    }

    fn reveal_running(&self, now: Instant) -> bool {
        self.reveal_start.is_some_and(|s| {
            now.saturating_duration_since(s).as_secs_f32() < motion().reveal_ms / 1000.0
        })
    }

    // --- pointer / hover ------------------------------------------------------------------------

    /// Hover driven by the raw pointer state (enter immediate, leave after `exit_grace_ms`).
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
                self.start_resize(target, now);
            }
            self.note_content(now, self.elapsed_secs(now));
        } else {
            self.hover_exit_at = Some(after(now, self.exit_grace_ms));
        }
    }

    /// Per-region hover tint.
    pub fn set_hot(&mut self, region: RegionId, hot: bool, now: Instant) {
        let ms = self.ms(now);
        let dur = motion().button_hot_ms;
        let target = if hot { 1.0 } else { 0.0 };
        let can_add = hot && self.hot.len() < crate::config::bound::MAX_REGIONS;
        match self.hot.iter_mut().find(|(id, _)| *id == region) {
            Some((_, fade)) => fade.retarget(target, ms, dur),
            None if can_add => {
                let mut fade = Fade::at(0.0);
                fade.retarget(target, ms, dur);
                self.hot.push((region, fade));
            }
            None => {}
        }
    }

    pub fn clear_hot(&mut self, now: Instant) {
        let ms = self.ms(now);
        let dur = motion().button_hot_ms;
        for (_, fade) in self.hot.iter_mut() {
            fade.retarget(0.0, ms, dur);
        }
    }

    /// Hover amount of every region that was ever hovered, at `ms`.
    fn hot_set(&self, ms: f64) -> HotSet {
        HotSet(
            self.hot
                .iter()
                .map(|(id, fade)| (*id, fade.value(ms)))
                .collect(),
        )
    }

    // --- clip / hint ----------------------------------------------------------------------------

    /// The clip card opened or closed.
    pub fn set_clip_visible(&mut self, visible: bool, now: Instant) {
        if visible == self.clip_visible {
            return;
        }
        self.clip_visible = visible;
        if visible {
            // Clip always attaches to the expanded idle shell.
            let expanded = config::capsule_size(Mode::Idle, false, true, "");
            self.set_size_now(expanded);
        } else {
            let target = self.target_size();
            self.set_size_now(target);
            self.presented = Some(self.mode);
        }
        self.note_content(now, self.elapsed_secs(now));
    }

    /// A visible hint pins the capsule at the size of the mode it is showing.
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
                self.start_resize(target, now);
                self.presented = Some(self.mode);
            }
        }
        self.note_content(now, self.elapsed_secs(now));
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
                self.start_resize(target, now);
            }
            self.note_content(now, self.elapsed_secs(now));
            if self.mode == Mode::Error {
                // The expiry was dropped while the pointer held the error open.
                self.arm_dismiss(now);
            }
        }
        if self.dismiss_at.is_some_and(|at| now >= at) {
            self.dismiss_at = None;
            // An error stays up while the pointer holds it; the hover exit re-arms the expiry.
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
            || self.crossfade_running(ms)
            || self.copy_flash_running(ms)
            || self.intro_running(now)
            || self.reveal_running(now)
            || !self.opacity.done(ms)
            || self.hot.iter().any(|(_, h)| !h.done(ms))
    }

    /// Scene for the frame at `now`; `level` is the latest microphone level.
    pub fn scene(&self, now: Instant, level: f32) -> PillScene {
        let ms = self.ms(now);
        let (cap_w, cap_h) = self.size_at(now);
        let hot = self.hot_set(ms);
        // Content cross-fade: the new content fades in (idle controls also grow from
        // `controls_scale_base`) while the outgoing one fades out; both skip the scale when
        // animations are off.
        let (content_alpha, content_scale, prev) = match &self.prev_content {
            Some(p) if self.crossfade_running(ms) => {
                let e = apply_curve(
                    motion().crossfade_curve,
                    ((ms - p.start_ms) / f64::from(self.crossfade_ms())) as f32,
                );
                let grow = motion();
                let soft = |controls: bool, scale: f32| {
                    if self.animations && controls {
                        scale
                    } else {
                        1.0
                    }
                };
                (
                    e,
                    soft(
                        self.content.controls,
                        grow.controls_scale_base + grow.controls_scale_gain * e,
                    ),
                    Some(PrevContent {
                        mode: p.snap.mode,
                        label: p.snap.label.clone(),
                        hovered: p.snap.hovered,
                        controls: p.snap.controls,
                        elapsed_secs: p.elapsed_secs,
                        alpha: 1.0 - e,
                        scale: soft(p.snap.controls, 1.0 - grow.controls_scale_gain * e),
                    }),
                )
            }
            _ => (1.0, 1.0, None),
        };
        let (copy_check, copy_dim) = self.copy_amounts(ms);
        PillScene {
            mode: self.mode,
            label: self.label.clone(),
            cap_w,
            cap_h,
            hovered: self.hovered,
            clipboard_visible: self.clip_visible,
            hint_visible: self.hint_visible,
            level,
            now_secs: self.secs(now),
            now_ms: ms,
            animations: self.animations,
            elapsed_secs: self.elapsed_secs(now),
            intro: self.intro_value(now),
            hot,
            opacity: self.opacity.value(ms).clamp(0.0, 1.0),
            offset_y: self.reveal_offset(now),
            content_alpha,
            content_scale,
            prev,
            proc_alpha: self.processing_alpha(now),
            copy_check,
            copy_dim,
        }
    }

    /// The scene the capsule settles into: used for hit regions and the published hitbox.
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
            level: 0.0,
            now_secs: 0.0,
            now_ms: 0.0,
            animations: self.animations,
            elapsed_secs,
            intro: 0.0,
            hot: HotSet::empty(),
            opacity: 1.0,
            offset_y: 0.0,
            content_alpha: 1.0,
            content_scale: 1.0,
            prev: None,
            proc_alpha: 1.0,
            copy_check: 0.0,
            copy_dim: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Painter (Windows only)
// ---------------------------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(super) mod painter {
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::Instant;

    use crate::config::metrics;
    use crate::look::Pass;
    use crate::{gfx, CmdTx, Shared, Surf};

    pub struct PillPainter {
        pub shared: Arc<Shared>,
        pub tx: CmdTx,
    }

    impl gfx::PainterBody for PillPainter {
        fn body(&mut self, ctx: &mut gfx::Ctx) {
            let now = Instant::now();
            let (scene, needs) = {
                let visual = self.shared.visual.lock().unwrap_or_else(|e| e.into_inner());
                let level = f32::from_bits(self.shared.level_bits.load(Ordering::Relaxed));
                let scene = visual.pill.scene(now, level);
                (scene, visual.pill.needs_frames(now))
            };
            let m = metrics();
            let frame = crate::config::get().painters.pill(
                &scene,
                m.pill_window_w,
                m.pill_window_h + m.pill_window_bottom_pad,
                m.pill_window_bottom_pad,
                Pass::Paint,
            );
            gfx::replay(ctx, &frame.ops);
            if !needs && self.shared.pump[Surf::Pill as usize].swap(false, Ordering::SeqCst) {
                self.tx.post_pump_idle(Surf::Pill);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

/// An incoming label cut to `Limits::max_label_bytes` at a character boundary.
fn bounded_label(label: &str) -> String {
    crate::text::cut_bytes(label, limits().max_label_bytes).to_string()
}
