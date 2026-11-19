//! The clip card's controller state (`single` = the current item with copy and close, `recent` =
//! a short list of items, click a row to copy): items, the payload on show, "copied" and
//! "copy failed" feedback, auto-dismiss timers, the entrance and exit spring and the region hover
//! fades. Layout and drawing are the host's `Painters::clip`; which region does what is the
//! painter's `Region::action` list (`Action::CopyCurrent`, `CopyItem`, `ToggleAlternate`, ...).
//!
//! The crate knows an item as an opaque `id`, a `primary` payload and an optional `alternate`
//! payload. What the pair means (polished and raw text, say) and which one shows first is the
//! host's `config::PayloadPolicy`.
//!
//! Feedback is set only from a result: [`ClipState::set_copied`] after the host's copy reported
//! success, [`ClipState::set_copy_failed`] after it reported failure or did not answer in time.
//! Nothing here assumes a copy worked.
//!
//! Size: the item list and the payload on show are shared (`Arc`), so the snapshot a painter takes
//! on every frame copies two pointers, not the transcript. What enters the card is bounded by
//! [`validate_items`] against the host's `Limits` before a state is built.

#![allow(dead_code)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use crate::anim::{AuxPose, Fade, Presence};
use crate::clock::{after, after_secs};
use crate::config::{motion, payloads, timings, Limits, PayloadPolicy, PayloadSide};
use crate::regions::{HotSet, RegionId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipItem {
    /// Opaque identity of the item, echoed in the feedback state and used to key it.
    pub id: String,
    pub primary: String,
    pub alternate: Option<String>,
}

/// Why [`validate_items`] refused a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipRefusal {
    /// No items.
    Empty,
    /// An id is empty, longer than `config::bound::MAX_ID_BYTES` or contains a control character.
    BadId,
    /// Two items share an id (the feedback state is keyed by id).
    DuplicateId,
    /// A payload is longer than `Limits::max_clip_payload_bytes`. The card is refused rather
    /// than cut, because a cut payload would put a truncated transcript on the clipboard.
    PayloadTooLarge,
    /// All payloads together (primary and alternate of every kept item) are longer than
    /// `Limits::max_clip_total_bytes`. Refused, not cut, for the same reason.
    TotalTooLarge,
    /// The call was not accepted: no generation is running, the owner is gone or its command
    /// queue is full. Nothing was shown; the host must not act as if it were.
    NotRunning,
}

impl ClipRefusal {
    /// A fixed code for diagnostics.
    pub fn code(self) -> &'static str {
        match self {
            ClipRefusal::Empty => "empty",
            ClipRefusal::BadId => "bad_id",
            ClipRefusal::DuplicateId => "duplicate_id",
            ClipRefusal::PayloadTooLarge => "payload_too_large",
            ClipRefusal::TotalTooLarge => "total_too_large",
            ClipRefusal::NotRunning => "not_running",
        }
    }
}

/// The items a card may show: at most `Limits::max_clip_items` of them (the rest are dropped), each
/// with a valid unique id and payloads within `Limits::max_clip_payload_bytes`, and all payloads
/// together within `Limits::max_clip_total_bytes`.
pub fn validate_items(
    mut items: Vec<ClipItem>,
    limits: &Limits,
) -> Result<Vec<ClipItem>, ClipRefusal> {
    items.truncate(limits.max_clip_items);
    if items.is_empty() {
        return Err(ClipRefusal::Empty);
    }
    let mut seen: HashSet<&str> = HashSet::with_capacity(items.len());
    let mut total = 0usize;
    for item in &items {
        if item.id.is_empty()
            || item.id.len() > crate::config::bound::MAX_ID_BYTES
            || item.id.chars().any(char::is_control)
        {
            return Err(ClipRefusal::BadId);
        }
        if !seen.insert(item.id.as_str()) {
            return Err(ClipRefusal::DuplicateId);
        }
        let alternate_len = item.alternate.as_ref().map_or(0, String::len);
        if item.primary.len() > limits.max_clip_payload_bytes
            || alternate_len > limits.max_clip_payload_bytes
        {
            return Err(ClipRefusal::PayloadTooLarge);
        }
        total = total
            .saturating_add(item.primary.len())
            .saturating_add(alternate_len);
        if total > limits.max_clip_total_bytes {
            return Err(ClipRefusal::TotalTooLarge);
        }
    }
    Ok(items)
}

#[derive(Clone, Debug)]
pub struct ClipState {
    /// The items, shared with every snapshot of this state.
    pub items: Arc<[ClipItem]>,
    pub single: bool,
    /// The payload the single card shows (and copies); shared like `items`.
    pub shown: Arc<str>,
    /// The item whose copy was last confirmed by the host.
    pub copied_id: Option<String>,
    /// The row or button whose "copied" feedback is fading in or out, and how far.
    copied_fade_id: Option<String>,
    copied_fade: Fade,
    /// The item whose copy failed (the host said so, or did not answer in time), shown until
    /// `failed_reset_at`.
    pub failed_id: Option<String>,
    failed_fade_id: Option<String>,
    failed_fade: Fade,
    pub shown_ms: f64,
    hot: Vec<(RegionId, Fade)>,
    pub autodismiss_at: Option<Instant>,
    pub copied_reset_at: Option<Instant>,
    pub failed_reset_at: Option<Instant>,
    pub close_at: Option<Instant>,
    /// Spring entrance and exit.
    pub presence: Presence,
    /// Fade-in of swapped content over the card shell.
    pub content_fade: Fade,
    /// When the exit animation ends and the window can be hidden.
    pub exit_at: Option<Instant>,
    /// The OS "show animations" setting.
    motion: bool,
}

/// The payload a single card opens on, per the host's policy.
pub fn initial_payload(item: &ClipItem, policy: &PayloadPolicy) -> String {
    match (policy.initial, alternate_of(item, policy)) {
        (PayloadSide::Alternate, Some(alternate)) => alternate.to_string(),
        _ => item.primary.clone(),
    }
}

/// The item's alternate payload when the policy counts it as different from the primary one.
pub fn alternate_of<'a>(item: &'a ClipItem, policy: &PayloadPolicy) -> Option<&'a str> {
    let alternate = item.alternate.as_deref()?;
    if (policy.distinct)(&item.primary, alternate) {
        Some(alternate)
    } else {
        None
    }
}

impl ClipState {
    pub fn new(
        items: Vec<ClipItem>,
        single: bool,
        shown_ms: f64,
        now: Instant,
        motion: bool,
    ) -> ClipState {
        let shown: Arc<str> = if single {
            items
                .first()
                .map(|item| initial_payload(item, payloads()))
                .unwrap_or_default()
                .into()
        } else {
            Arc::from("")
        };
        ClipState {
            items: items.into(),
            single,
            shown,
            copied_id: None,
            copied_fade_id: None,
            copied_fade: Fade::at(0.0),
            failed_id: None,
            failed_fade_id: None,
            failed_fade: Fade::at(0.0),
            shown_ms,
            hot: Vec::new(),
            autodismiss_at: if single {
                Some(after(now, timings().single_clip_autodismiss_ms))
            } else {
                None
            },
            copied_reset_at: None,
            failed_reset_at: None,
            close_at: None,
            presence: Presence::entering(shown_ms, motion),
            content_fade: Fade::at(1.0),
            exit_at: None,
            motion,
        }
    }

    fn crossfade_ms(&self) -> f32 {
        if self.motion {
            motion().crossfade_ms
        } else {
            motion().crossfade_reduced_ms
        }
    }

    /// This card replaces `old` while it is still up: it stays where it is and the new content
    /// fades in over the shell instead of the card entering again.
    pub fn take_over(&mut self, old: &ClipState, now_ms: f64) {
        if old.presence.is_leaving() {
            return;
        }
        self.presence = old.presence;
        self.shown_ms = old.shown_ms;
        self.content_fade = Fade {
            from: 0.0,
            to: 1.0,
            start_ms: now_ms,
            dur_ms: self.crossfade_ms(),
        };
    }

    pub fn is_leaving(&self) -> bool {
        self.presence.is_leaving()
    }

    /// Start the exit: the card plays its entrance in reverse, then `exit_at` hides the window.
    pub fn begin_exit(&mut self, now_ms: f64, now: Instant) {
        self.presence.leave(now_ms);
        self.autodismiss_at = None;
        self.close_at = None;
        self.copied_reset_at = None;
        self.failed_reset_at = None;
        // A technical margin (not a product value) so the timer fires after the last frame.
        let after = self.presence.exit_after_ms() / 1000.0 + 0.02;
        self.exit_at = Some(after_secs(now, after));
    }

    /// What a copy of the single card (`index` is `None`) or of item `index` would copy: the
    /// item's id and the payload on show (single) or its primary payload (a recent row).
    pub fn copy_target(&self, index: Option<usize>) -> Option<(String, String)> {
        match index {
            None => self
                .current()
                .map(|item| (item.id.clone(), self.shown.to_string())),
            Some(i) => self
                .items
                .get(i)
                .map(|item| (item.id.clone(), item.primary.clone())),
        }
    }

    /// The host confirmed the copy of `id`: mark it copied (`Some`) or clear the mark (`None`);
    /// the label and icon cross-fade. Any failure feedback clears.
    pub fn set_copied(&mut self, id: Option<String>, now_ms: f64) {
        let dur = self.crossfade_ms();
        match id {
            Some(id) => {
                self.set_copy_failed(None, now_ms);
                self.copied_fade_id = Some(id.clone());
                self.copied_id = Some(id);
                self.copied_fade.retarget(1.0, now_ms, dur);
            }
            None => {
                self.copied_id = None;
                self.copied_fade.retarget(0.0, now_ms, dur);
            }
        }
    }

    /// The copy of `id` failed (`Some`) or the failure feedback ends (`None`). Any "copied" mark
    /// clears: the card never shows both.
    pub fn set_copy_failed(&mut self, id: Option<String>, now_ms: f64) {
        let dur = self.crossfade_ms();
        match id {
            Some(id) => {
                if self.copied_id.is_some() {
                    self.set_copied(None, now_ms);
                }
                self.failed_fade_id = Some(id.clone());
                self.failed_id = Some(id);
                self.failed_fade.retarget(1.0, now_ms, dur);
            }
            None => {
                self.failed_id = None;
                self.failed_fade.retarget(0.0, now_ms, dur);
            }
        }
    }

    /// The item (`ClipItem::id`) whose "copied" feedback is fading in or out, and how far
    /// (`0.0..=1.0`). The painter decides which control shows it.
    pub fn copied_state(&self, now_ms: f64) -> (Option<&str>, f32) {
        match self.copied_fade_id.as_deref() {
            Some(id) => (Some(id), self.copied_fade.value(now_ms).clamp(0.0, 1.0)),
            None => (None, 0.0),
        }
    }

    /// The item whose copy-failure feedback is fading in or out, and how far.
    pub fn failed_state(&self, now_ms: f64) -> (Option<&str>, f32) {
        match self.failed_fade_id.as_deref() {
            Some(id) => (Some(id), self.failed_fade.value(now_ms).clamp(0.0, 1.0)),
            None => (None, 0.0),
        }
    }

    pub fn pose(&self, now_ms: f64) -> AuxPose {
        AuxPose {
            content_alpha: self.content_fade.value(now_ms).clamp(0.0, 1.0),
            ..self.presence.pose(now_ms)
        }
    }

    pub fn current(&self) -> Option<&ClipItem> {
        if self.single {
            self.items.first()
        } else {
            None
        }
    }

    /// The single card's alternate payload when the host's policy counts it as different from the
    /// primary one (the card then offers a toggle).
    pub fn alternate_payload(&self) -> Option<&str> {
        alternate_of(self.current()?, payloads())
    }

    pub fn set_hot(&mut self, region: RegionId, hot: bool, now_ms: f64) {
        let dur = motion().aux_hot_ms;
        let target = if hot { 1.0 } else { 0.0 };
        let can_add = hot && self.hot.len() < crate::config::bound::MAX_REGIONS;
        match self.hot.iter_mut().find(|(id, _)| *id == region) {
            Some((_, fade)) => fade.retarget(target, now_ms, dur),
            None if can_add => {
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
            || !self.copied_fade.done(now_ms)
            || !self.failed_fade.done(now_ms)
            || self.hot.iter().any(|(_, h)| !h.done(now_ms))
    }

    /// Toggle the single card between its primary and alternate payloads. Does nothing unless
    /// the policy counts the alternate as different.
    pub fn toggle_alternate(&mut self) {
        let Some(item) = self.current() else { return };
        let Some(alternate) = alternate_of(item, payloads()).map(str::to_string) else {
            return;
        };
        let primary = item.primary.clone();
        self.shown = if *self.shown == *alternate {
            primary.into()
        } else {
            alternate.into()
        };
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.autodismiss_at,
            self.copied_reset_at,
            self.failed_reset_at,
            self.close_at,
            self.exit_at,
        ]
        .into_iter()
        .flatten()
        .min()
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

    use crate::look::ClipView;
    use crate::{gfx, CmdTx, Shared, Surf};

    pub struct ClipPainter {
        pub shared: Arc<Shared>,
        pub tx: CmdTx,
    }

    impl gfx::PainterBody for ClipPainter {
        fn body(&mut self, ctx: &mut gfx::Ctx) {
            let now = Instant::now();
            let snapshot = {
                let visual = self.shared.visual.lock().unwrap_or_else(|e| e.into_inner());
                let now_ms = visual.pill.ms(now);
                visual.clip.as_ref().map(|clip| {
                    let (copied_id, copied_amount) = clip.copied_state(now_ms);
                    let (failed_id, failed_amount) = clip.failed_state(now_ms);
                    (
                        clip.clone(),
                        clip.hot_values(now_ms),
                        copied_id.map(str::to_string),
                        copied_amount,
                        failed_id.map(str::to_string),
                        failed_amount,
                        clip.pose(now_ms),
                        clip.needs_frames(now_ms),
                    )
                })
            };
            let needs = match snapshot {
                Some((
                    clip,
                    hot,
                    copied_id,
                    copied_amount,
                    failed_id,
                    failed_amount,
                    pose,
                    needs,
                )) => {
                    let frame = crate::config::get().painters.clip(&ClipView {
                        state: &clip,
                        hot: &hot,
                        copied_id: copied_id.as_deref(),
                        copied_amount,
                        failed_id: failed_id.as_deref(),
                        failed_amount,
                        pose,
                    });
                    gfx::replay(ctx, &frame.ops);
                    needs
                }
                None => false,
            };
            if !needs && self.shared.pump[Surf::Clip as usize].swap(false, Ordering::SeqCst) {
                self.tx.post_pump_idle(Surf::Clip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trimmed_differs(primary: &str, alternate: &str) -> bool {
        primary.trim() != alternate.trim()
    }

    fn policy(initial: PayloadSide) -> PayloadPolicy {
        PayloadPolicy {
            initial,
            distinct: trimmed_differs,
        }
    }

    fn item(primary: &str, alternate: Option<&str>) -> ClipItem {
        ClipItem {
            id: "a".to_string(),
            primary: primary.to_string(),
            alternate: alternate.map(str::to_string),
        }
    }

    #[test]
    fn the_initial_payload_follows_the_policy() {
        let it = item("polished", Some("raw"));
        assert_eq!(
            initial_payload(&it, &policy(PayloadSide::Primary)),
            "polished"
        );
        assert_eq!(initial_payload(&it, &policy(PayloadSide::Alternate)), "raw");
    }

    #[test]
    fn an_alternate_that_is_not_distinct_is_not_offered() {
        let same = item("text", Some("  text "));
        assert_eq!(alternate_of(&same, &policy(PayloadSide::Primary)), None);
        // An alternate initial side falls back to the primary payload when there is no
        // distinct alternate.
        assert_eq!(
            initial_payload(&same, &policy(PayloadSide::Alternate)),
            "text"
        );
        assert_eq!(
            alternate_of(&item("a", None), &policy(PayloadSide::Primary)),
            None
        );
    }
}
