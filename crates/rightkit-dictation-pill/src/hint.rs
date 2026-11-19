//! The hint card's controller state: what it shows (opaque to the crate), when it hides, its
//! entrance and exit spring and its region hover fades. Layout and drawing are the host's
//! `Painters::hint`. Which hints exist, what they say and when they are offered are the host's:
//! the crate has no tip catalog and no cadence.

#![allow(dead_code)]

use std::time::Instant;

use crate::anim::{AuxPose, Fade, Presence};
use crate::clock::{after_secs, MAX_DELAY_MS};
use crate::config::motion;
use crate::regions::{HotSet, RegionId};
use crate::text::cut_bytes;

/// What a hint card shows. The crate never interprets it: `kind` selects the card layout in the
/// painter, `title` and `body` carry text for kinds that need it. `hide_after_ms` is the
/// auto-hide delay (`None` keeps an interactive card up until dismissed). `show_hint` cuts each
/// text to `Limits::max_hint_text_bytes` and the delay to one day. None of the three texts is ever
/// written to a diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintContent {
    /// Selects the card layout in the host's painter. Never logged.
    pub kind: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub hide_after_ms: Option<u64>,
}

impl HintContent {
    pub fn new(kind: &str, hide_after_ms: Option<u64>) -> HintContent {
        HintContent {
            kind: kind.to_string(),
            title: None,
            body: None,
            hide_after_ms,
        }
    }

    pub fn with_text(mut self, title: &str, body: &str) -> HintContent {
        self.title = Some(title.to_string());
        self.body = Some(body.to_string());
        self
    }

    /// The content with `title` and `body` cut to `max_text_bytes`, `kind` cut to at most
    /// `config::bound::MAX_ID_BYTES` (and `max_text_bytes`), and the delay clamped to one day.
    pub fn bounded(self, max_text_bytes: usize) -> HintContent {
        let cut = |text: String| cut_bytes(&text, max_text_bytes).to_string();
        let kind_max = max_text_bytes.min(crate::config::bound::MAX_ID_BYTES);
        let kind = cut_bytes(&self.kind, kind_max).to_string();
        HintContent {
            kind,
            title: self.title.map(cut),
            body: self.body.map(cut),
            hide_after_ms: self.hide_after_ms.map(|ms| ms.min(MAX_DELAY_MS)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HintState {
    pub content: HintContent,
    pub kind: String,
    pub shown_ms: f64,
    hot: Vec<(RegionId, Fade)>,
    pub hide_at: Option<Instant>,
    /// Spring entrance and exit.
    pub presence: Presence,
    /// Fade-in of swapped content over the card shell.
    pub content_fade: Fade,
    /// When the exit animation ends and the window can be hidden.
    pub exit_at: Option<Instant>,
}

impl HintState {
    /// `motion`: the OS "show animations" setting.
    pub fn new(
        content: HintContent,
        kind: &str,
        shown_ms: f64,
        hide_at: Option<Instant>,
        motion: bool,
    ) -> Self {
        HintState {
            content,
            kind: kind.to_string(),
            shown_ms,
            hot: Vec::new(),
            hide_at,
            presence: Presence::entering(shown_ms, motion),
            content_fade: Fade::at(1.0),
            exit_at: None,
        }
    }

    /// This hint replaces `old` while it is still up: the card stays where it is and the new
    /// content fades in over the shell instead of the card entering again.
    pub fn take_over(&mut self, old: &HintState, now_ms: f64, animations: bool) {
        if old.presence.is_leaving() {
            return;
        }
        self.presence = old.presence;
        self.shown_ms = old.shown_ms;
        let m = motion();
        let dur = if animations {
            m.crossfade_ms
        } else {
            m.crossfade_reduced_ms
        };
        self.content_fade = Fade {
            from: 0.0,
            to: 1.0,
            start_ms: now_ms,
            dur_ms: dur,
        };
    }

    pub fn is_leaving(&self) -> bool {
        self.presence.is_leaving()
    }

    /// Start the exit: the card plays its entrance in reverse, then `exit_at` hides the window.
    pub fn begin_exit(&mut self, now_ms: f64, now: Instant) {
        self.presence.leave(now_ms);
        self.hide_at = None;
        // A technical margin (not a product value) so the timer fires after the last frame.
        let after = self.presence.exit_after_ms() / 1000.0 + 0.02;
        self.exit_at = Some(after_secs(now, after));
    }

    pub fn pose(&self, now_ms: f64) -> AuxPose {
        AuxPose {
            content_alpha: self.content_fade.value(now_ms).clamp(0.0, 1.0),
            ..self.presence.pose(now_ms)
        }
    }

    pub fn set_hot(&mut self, region: RegionId, hot: bool, now_ms: f64) {
        let dur = motion().aux_hot_ms;
        let target = if hot { 1.0 } else { 0.0 };
        match self.hot.iter_mut().find(|(id, _)| *id == region) {
            Some((_, fade)) => fade.retarget(target, now_ms, dur),
            None if hot && self.hot.len() < crate::config::bound::MAX_REGIONS => {
                let mut fade = Fade::at(0.0);
                fade.retarget(target, now_ms, dur);
                self.hot.push((region, fade));
            }
            None => {}
        }
    }

    pub fn hot_values(&self, now_ms: f64) -> HotSet {
        HotSet(
            self.hot
                .iter()
                .map(|(id, fade)| (*id, fade.value(now_ms)))
                .collect(),
        )
    }

    pub fn needs_frames(&self, now_ms: f64) -> bool {
        self.presence.animating(now_ms)
            || !self.content_fade.done(now_ms)
            || self.hot.iter().any(|(_, h)| !h.done(now_ms))
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

    use crate::look::HintView;
    use crate::{gfx, CmdTx, Shared, Surf};

    pub struct HintPainter {
        pub shared: Arc<Shared>,
        pub tx: CmdTx,
    }

    impl gfx::PainterBody for HintPainter {
        fn body(&mut self, ctx: &mut gfx::Ctx) {
            let now = Instant::now();
            let snapshot = {
                let visual = self.shared.visual.lock().unwrap_or_else(|e| e.into_inner());
                let now_ms = visual.pill.ms(now);
                visual.hint.as_ref().map(|hint| {
                    (
                        hint.content.clone(),
                        hint.hot_values(now_ms),
                        hint.pose(now_ms),
                        hint.needs_frames(now_ms),
                    )
                })
            };
            let needs = match snapshot {
                Some((content, hot, pose, needs)) => {
                    let frame = crate::config::get().painters.hint(&HintView {
                        content: &content,
                        hot: &hot,
                        pose,
                    });
                    gfx::replay(ctx, &frame.ops);
                    needs
                }
                None => false,
            };
            if !needs && self.shared.pump[Surf::Hint as usize].swap(false, Ordering::SeqCst) {
                self.tx.post_pump_idle(Surf::Hint);
            }
        }
    }
}
