//! Adaptive stream pacing (A219): the live answer reaches the transcript line by line instead of a
//! whole burst in one frame, and a big backlog catches up at once.
//!
//! Two gears with hysteresis:
//!
//! * **Smooth**: one queued line is released per tick ([`TICK`], 40 ms), so a small burst reads as
//!   a steady stream.
//! * **CatchUp**: the whole queue is released at once. Entered when more than [`CATCH_UP_LINES`]
//!   lines are queued or the oldest queued line has waited more than [`CATCH_UP_AGE`]; left only
//!   after the backlog has stayed under [`SMOOTH_LINES`] lines for [`SMOOTH_AFTER`].
//!
//! Only whole lines are paced. A delta without a newline passes straight through when nothing is
//! queued (token streaming keeps its latency); with lines queued it joins the queue, so order never
//! changes. Any other event must call [`StreamPacer::flush`] first.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Time between releases in the Smooth gear.
pub const TICK: Duration = Duration::from_millis(40);
/// More queued lines than this, and the pacer catches up.
pub const CATCH_UP_LINES: usize = 24;
/// An oldest queued line older than this, and the pacer catches up.
pub const CATCH_UP_AGE: Duration = Duration::from_millis(250);
/// Back to Smooth once fewer lines than this stay queued...
pub const SMOOTH_LINES: usize = 4;
/// ...for this long.
pub const SMOOTH_AFTER: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gear {
    Smooth,
    CatchUp,
}

#[derive(Debug)]
pub struct StreamPacer {
    /// Queued chunks: each is one whole line (ending in `\n`) or a trailing partial line.
    queue: VecDeque<(String, Instant)>,
    gear: Gear,
    /// Since when the backlog has been under [`SMOOTH_LINES`] while in CatchUp.
    calm_since: Option<Instant>,
}

impl Default for StreamPacer {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            gear: Gear::Smooth,
            calm_since: None,
        }
    }
}

impl StreamPacer {
    /// Whether anything waits for a tick.
    pub fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    fn queued_lines(&self) -> usize {
        self.queue
            .iter()
            .filter(|(chunk, _)| chunk.ends_with('\n'))
            .count()
    }

    /// Take in one delta. Returns the text to apply now (empty when it was queued).
    pub fn push(&mut self, delta: &str, now: Instant) -> String {
        if delta.is_empty() {
            return String::new();
        }
        // The calm spell may have ended while nothing was streaming.
        self.reassess(now);
        if self.queue.is_empty() && !delta.contains('\n') {
            return delta.to_string();
        }
        for chunk in delta.split_inclusive('\n') {
            self.queue.push_back((chunk.to_string(), now));
        }
        self.reassess(now);
        if self.gear == Gear::CatchUp {
            self.calm_since = Some(now);
            return self.flush();
        }
        String::new()
    }

    /// One tick: the text to apply now (one line in Smooth, everything in CatchUp).
    pub fn tick(&mut self, now: Instant) -> String {
        self.reassess(now);
        if self.queue.is_empty() {
            return String::new();
        }
        if self.gear == Gear::CatchUp {
            self.calm_since = Some(now);
            return self.flush();
        }
        let mut out = String::new();
        while let Some((chunk, _)) = self.queue.pop_front() {
            let line_end = chunk.ends_with('\n');
            out.push_str(&chunk);
            if line_end {
                break;
            }
        }
        out
    }

    /// Everything queued, now. Call before any other event is applied so order holds.
    pub fn flush(&mut self) -> String {
        self.queue.drain(..).map(|(chunk, _)| chunk).collect()
    }

    fn reassess(&mut self, now: Instant) {
        let lines = self.queued_lines();
        let oldest = self
            .queue
            .front()
            .map(|(_, at)| now.saturating_duration_since(*at));
        match self.gear {
            Gear::Smooth => {
                if lines > CATCH_UP_LINES || oldest.is_some_and(|age| age > CATCH_UP_AGE) {
                    self.gear = Gear::CatchUp;
                    self.calm_since = None;
                }
            }
            Gear::CatchUp => {
                if lines < SMOOTH_LINES {
                    let since = *self.calm_since.get_or_insert(now);
                    if now.saturating_duration_since(since) >= SMOOTH_AFTER {
                        self.gear = Gear::Smooth;
                        self.calm_since = None;
                    }
                } else {
                    // A big backlog just arrived: the calm spell starts when it has been drained.
                    self.calm_since = None;
                }
            }
        }
    }
}
