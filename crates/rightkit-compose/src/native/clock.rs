//! Playback clock and A/V sync policy.
//!
//! The clock is pure: it maps `Instant`s to timeline [`Ticks`] under a playhead
//! state (playing/paused, speed, loop range). The audio path reports a
//! sample-exact cursor; [`AvSync`] turns the difference into a bounded
//! correction decision. Nothing here owns threads or devices.

use std::time::Instant;

use rightkit_timeline::{Fps, Ticks};

/// Inclusive-exclusive playback loop bounds, in timeline ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopRange {
    pub start: Ticks,
    pub end: Ticks,
}

#[derive(Debug, Clone)]
enum Base {
    /// Paused at an exact timeline position.
    Paused { at: Ticks },
    /// Playing: position = `at + (now - since) * speed`.
    Playing { at: Ticks, since: Instant },
}

/// One playback clock over a fixed-length timeline.
#[derive(Debug, Clone)]
pub struct TimelineClock {
    fps: Fps,
    total: Ticks,
    base: Base,
    /// Playback rate multiplier; must be finite and > 0.
    speed: f64,
    loop_range: Option<LoopRange>,
}

impl TimelineClock {
    /// A paused clock at frame 0.
    pub fn new(fps: Fps, total_frames: i64) -> Self {
        Self {
            fps,
            total: fps.frame_to_ticks(total_frames.max(0)),
            base: Base::Paused { at: Ticks(0) },
            speed: 1.0,
            loop_range: None,
        }
    }

    pub fn fps(&self) -> Fps {
        self.fps
    }

    pub fn total(&self) -> Ticks {
        self.total
    }

    pub fn playing(&self) -> bool {
        matches!(self.base, Base::Playing { .. })
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn loop_range(&self) -> Option<LoopRange> {
        self.loop_range
    }

    /// Maps a raw elapsed position through clamp + loop.
    fn normalize(&self, t: i64) -> Ticks {
        if let Some(l) = self.loop_range {
            let span = (l.end.0 - l.start.0).max(1);
            let mut v = t;
            while v >= l.end.0 {
                v -= span;
            }
            while v < l.start.0 {
                v += span;
            }
            return Ticks(v);
        }
        Ticks(t.clamp(0, self.total.0))
    }

    fn raw_position(&self, now: Instant) -> i64 {
        match self.base {
            Base::Paused { at } => at.0,
            Base::Playing { at, since } => {
                let elapsed = now.saturating_duration_since(since).as_secs_f64();
                at.0 + (elapsed * self.speed * rightkit_timeline::TICKS_PER_SECOND as f64) as i64
            }
        }
    }

    /// Current playhead position.
    pub fn position(&self, now: Instant) -> Ticks {
        self.normalize(self.raw_position(now))
    }

    /// Current output frame index (shared rounding rule via `fps`).
    pub fn frame(&self, now: Instant) -> i64 {
        self.fps
            .ticks_to_frame(self.position(now))
            .clamp(0, (self.fps.ticks_to_frame(self.total) - 1).max(0))
    }

    /// Restarts the playing anchor at `at` (or freezes there when paused).
    fn rebase(&mut self, at: Ticks, now: Instant) {
        self.base = match self.base {
            Base::Paused { .. } => Base::Paused { at },
            Base::Playing { .. } => Base::Playing { at, since: now },
        };
    }

    /// Start or resume playback. No-op when already at/past the end with no loop.
    pub fn play(&mut self, now: Instant) {
        let at = self.position(now);
        if !self.playing() {
            self.base = Base::Playing { at, since: now };
        }
        // Playback off the end restarts at 0 (or the loop start).
        let pos = self.position(now);
        if self.loop_range.is_none() && pos.0 >= self.total.0 && self.total.0 > 0 {
            self.rebase(Ticks(0), now);
        }
    }

    pub fn pause(&mut self, now: Instant) {
        let at = self.position(now);
        self.base = Base::Paused { at };
    }

    pub fn seek(&mut self, at: Ticks, now: Instant) {
        self.rebase(self.normalize(at.0), now);
    }

    /// Seek to an output frame index.
    pub fn seek_frame(&mut self, frame: i64, now: Instant) {
        let max = (self.fps.ticks_to_frame(self.total) - 1).max(0);
        self.seek(self.fps.frame_to_ticks(frame.clamp(0, max)), now);
    }

    /// Pause and move one output frame in `direction` (+1/-1).
    pub fn frame_step(&mut self, direction: i64, now: Instant) {
        self.pause(now);
        self.seek_frame(self.frame(now) + direction.signum(), now);
    }

    /// Sets the playback rate (clamped to 1/16x..16x).
    pub fn set_speed(&mut self, speed: f64, now: Instant) {
        if !speed.is_finite() || speed <= 0.0 {
            return;
        }
        let at = self.position(now);
        self.speed = speed.clamp(1.0 / 16.0, 16.0);
        self.rebase(at, now);
    }

    /// Sets (or clears) the playback loop. Ranges are clamped to the timeline.
    pub fn set_loop(&mut self, range: Option<LoopRange>) {
        self.loop_range = range.and_then(|l| {
            let start = l.start.0.clamp(0, self.total.0);
            let end = l.end.0.clamp(0, self.total.0);
            (end > start).then_some(LoopRange {
                start: Ticks(start),
                end: Ticks(end),
            })
        });
    }
}

/// The correction a video stage should take for the next frame, given the
/// audio (master) clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncAction {
    /// Frame is within tolerance; present on schedule.
    OnTime,
    /// Video is behind audio: drop `n` frames to catch up.
    Drop(i64),
    /// Video is ahead of audio by `ms`; present the current frame again.
    Hold { early_ms: f64 },
}

/// A/V sync policy: the audio sample cursor is the master clock; video frames
/// are chosen from it and drift beyond `tolerance` collapses to a hard resync
/// rather than creeping lip-sync error.
#[derive(Debug, Clone, Copy)]
pub struct AvSync {
    pub sample_rate: i64,
    /// Frames within this window present normally (ms).
    pub tolerance_ms: f64,
}

impl AvSync {
    pub const fn new(sample_rate: i64) -> Self {
        Self {
            sample_rate,
            tolerance_ms: 45.0,
        }
    }

    /// Timeline position of the audio write cursor.
    pub fn audio_ticks(&self, samples_played: i64) -> Ticks {
        Ticks(rightkit_timeline::time::round_div_half_down(
            samples_played as i128 * rightkit_timeline::TICKS_PER_SECOND as i128,
            self.sample_rate as i128,
        ) as i64)
    }

    /// Signed video-minus-audio error for presenting `frame` now, in ms.
    /// Positive = video ahead of audio.
    pub fn drift_ms(&self, audio_samples: i64, frame: i64, fps: Fps) -> f64 {
        let video_ticks = fps.frame_to_ticks(frame).0 as f64;
        let audio_ticks = self.audio_ticks(audio_samples).0 as f64;
        (video_ticks - audio_ticks) / rightkit_timeline::TICKS_PER_SECOND as f64 * 1000.0
    }

    /// Decide what to do about `frame` given the audio cursor. `frame` is the
    /// frame the video stage was about to present; the correct frame under the
    /// audio clock is `audio_ticks -> frame`.
    pub fn correct(&self, audio_samples: i64, frame: i64, fps: Fps) -> SyncAction {
        let drift = self.drift_ms(audio_samples, frame, fps);
        if drift.abs() <= self.tolerance_ms {
            return SyncAction::OnTime;
        }
        if drift > 0.0 {
            return SyncAction::Hold { early_ms: drift };
        }
        let due = fps.ticks_to_frame(self.audio_ticks(audio_samples));
        SyncAction::Drop((due - frame).max(1))
    }
}
