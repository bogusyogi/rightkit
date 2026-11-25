//! The composed hint flows of one pill session: the pending kind and the three delay tokens kept
//! together as one value, showing the pending kind, the screenshot notice (a status word plus a
//! once-only prompt hint), the once-only "a cancelled recording is saved" hint and the feature-tip
//! cycle after each completed transcript. The building blocks are `hint_flow` (claim-once, queue,
//! retire, restart), `delayed`, `feature_tip` (the cadence) and `screenshot_notice` (the
//! classification); this module is the order in which a host calls them.
//!
//! What the host supplies ([`HintHost`]): the sink, the delays, the hint kinds (the crate has no
//! kind of its own), the two notice words, the tip cadence with its kind list and the per-platform
//! mapping of a tip kind. What shows a hint, writes the state file, claims a once-only flag and
//! shows a status word are closures at each call.
//!
//! Moved from HeardRight `pill_overlays.rs` (`PENDING_HINT` and the delay tokens,
//! `retire_delayed_hints`, `restart_delayed_hints`, `show_pill_hint_inner`, `set_pending_pill_hint`,
//! `show_screenshot_notice`, `screenshot_notice_text`, `maybe_show_cancel_saved_hint`,
//! `note_completed_transcript_for_hints`). Event names, delays and ordering are unchanged.

use std::path::Path;
use std::time::Duration;

use crate::delayed::Generation;
use crate::feature_tip::{self, Cadence};
use crate::hint_flow::{self, ClaimEvents, PendingEvents, PendingHint, TipEvents};
use crate::host_glue::Sink;
use crate::screenshot_notice::{classify, ScreenshotOutcome};

const PENDING_EVENTS: PendingEvents = PendingEvents {
    set: "pill_hint_pending_set",
    set_error: "pill_hint_pending_error",
};

const SCREENSHOT_EVENTS: ClaimEvents = ClaimEvents {
    claim_error: "screenshot_prompt_claim_error",
    show_error: "screenshot_prompt_show_error",
    shown: "screenshot_prompt_show",
};

const CANCEL_EVENTS: ClaimEvents = ClaimEvents {
    claim_error: "cancel_hint_claim_error",
    show_error: "cancel_hint_show_error",
    shown: "cancel_hint_show",
};

const TIP_EVENTS: TipEvents = TipEvents {
    show_error: "feature_hint_show_error",
    shown: "feature_hint_show",
};

/// The two words of a screenshot notice.
#[derive(Clone, Copy, Debug)]
pub struct NoticeLabels {
    /// Shown when the screenshot was written to a file.
    pub saved: &'static str,
    /// Shown when the image went to the clipboard.
    pub copied: &'static str,
}

impl NoticeLabels {
    /// The word for the outcome `detail` names (`screenshot_notice::classify`).
    pub fn text(&self, detail: &str) -> &'static str {
        match classify(detail) {
            ScreenshotOutcome::Saved => self.saved,
            ScreenshotOutcome::Copied => self.copied,
        }
    }
}

/// The host's inputs to the hint flows.
#[derive(Clone, Copy)]
pub struct HintHost {
    pub log: Sink,
    /// How long after a claim the once-only hints (screenshot prompt, cancel saved) show.
    pub claim_delay: Duration,
    /// How long after a completed transcript a feature tip shows.
    pub tip_delay: Duration,
    /// The kind the screenshot notice's prompt hint shows.
    pub screenshot_kind: &'static str,
    /// The kind of the once-only "a cancelled recording is saved" hint.
    pub cancel_kind: &'static str,
    pub notice_labels: NoticeLabels,
    /// The feature-tip cadence and the persisted keys.
    pub cadence: Cadence<'static>,
    /// The feature-tip kinds, in the order they are offered.
    pub tip_kinds: &'static [&'static str],
    /// Maps the kind picked from `tip_kinds` to the one this platform shows (a kind that has no
    /// card on a platform is replaced by one that has).
    pub resolve_tip_kind: fn(&'static str) -> &'static str,
}

/// The pending hint kind and the cancellation tokens of the delayed shows: a cancelled or
/// superseded delay shows nothing. Declare it as a `static`; the shutdown and the next install
/// retire and restart it together (`session_host`).
pub struct HintSet {
    /// The hint kind waiting to be shown.
    pub pending: PendingHint,
    screenshot: Generation,
    cancel: Generation,
    tip: Generation,
}

impl Default for HintSet {
    fn default() -> Self {
        Self::new()
    }
}

impl HintSet {
    pub const fn new() -> HintSet {
        HintSet {
            pending: PendingHint::new(),
            screenshot: Generation::new(),
            cancel: Generation::new(),
            tip: Generation::new(),
        }
    }

    fn delays(&'static self) -> [&'static Generation; 3] {
        [&self.screenshot, &self.cancel, &self.tip]
    }

    /// Shutdown: a delayed hint that has not shown yet never shows, the pending kind is dropped, a
    /// show already running finishes first, and a hint queued afterwards is refused until
    /// [`HintSet::restart`].
    pub fn retire(&'static self) {
        hint_flow::retire(&self.delays(), &self.pending);
    }

    /// Install: open the next lifecycle. The epoch of every delay token moves, so no show queued
    /// before the restart can run.
    pub fn restart(&'static self) {
        hint_flow::restart(&self.delays(), &self.pending);
    }

    /// Record `kind` as the hint to show next (`pill_hint_pending_set`, or
    /// `pill_hint_pending_error` when retired or the lock is poisoned).
    pub fn set_pending(&self, log: Sink, kind: &str) {
        self.pending
            .set(log, PENDING_EVENTS.set, PENDING_EVENTS.set_error, kind);
    }

    /// Take the pending kind (`None` when there is none) and hand it to `show`. An error only for a
    /// poisoned lock.
    pub fn show_pending(&self, show: impl FnOnce(Option<&str>)) -> Result<(), String> {
        let pending = self.pending.take()?;
        show(pending.as_deref());
        Ok(())
    }

    /// A screenshot finished: show its status word through `show_status`, log it
    /// (`screenshot_notice_show`), then claim the once-only prompt hint with `claim` and, when
    /// this call owns it, show it after `claim_delay`. `detail` is the outcome the screenshot
    /// command reported (its path is classified, never logged).
    pub fn show_screenshot_notice<E1, E2>(
        &'static self,
        host: &HintHost,
        detail: Option<&str>,
        show_status: impl FnOnce(&str),
        claim: impl FnOnce() -> Result<bool, E1>,
        show: impl FnOnce() -> Result<(), E2> + Send + 'static,
    ) {
        let text = host.notice_labels.text(detail.unwrap_or_default());
        show_status(text);
        (host.log)("screenshot_notice_show", text);
        hint_flow::claim_once_then_show(
            host.log,
            &self.screenshot,
            &self.pending,
            PENDING_EVENTS,
            SCREENSHOT_EVENTS,
            host.screenshot_kind,
            host.claim_delay,
            claim,
            show,
        );
    }

    /// The first time a cancel finalizes to history, show the "cancelled recordings are saved"
    /// hint once. `claim` is the atomic "have we shown this before" gate, so a second cancel never
    /// re-triggers it. The same claim-once pattern as [`HintSet::show_screenshot_notice`].
    pub fn show_cancel_saved<E1, E2>(
        &'static self,
        host: &HintHost,
        claim: impl FnOnce() -> Result<bool, E1>,
        show: impl FnOnce() -> Result<(), E2> + Send + 'static,
    ) {
        hint_flow::claim_once_then_show(
            host.log,
            &self.cancel,
            &self.pending,
            PENDING_EVENTS,
            CANCEL_EVENTS,
            host.cancel_kind,
            host.claim_delay,
            claim,
            show,
        );
    }

    /// One transcript completed: advance the cadence state at `path` (written with `replace`, the
    /// host's atomic writer) and, when a tip is due, queue the next kind of `tip_kinds` (mapped by
    /// `resolve_tip_kind`) to show after `tip_delay`.
    pub fn note_completed_transcript<E>(
        &'static self,
        host: &HintHost,
        path: &Path,
        replace: impl FnOnce(&Path, &[u8]) -> std::io::Result<()>,
        show: impl FnOnce() -> Result<(), E> + Send + 'static,
    ) {
        let step = feature_tip::note_completed(
            &host.cadence,
            path,
            host.tip_kinds.len(),
            feature_tip::now_epoch_ms(),
            replace,
        );
        let (completed, next_threshold) = (step.completed, step.next_at);
        let Some(index) = step.due_index else {
            return;
        };
        let kind = (host.resolve_tip_kind)(host.tip_kinds[index]);
        hint_flow::queue_tip(
            host.log,
            &self.tip,
            &self.pending,
            PENDING_EVENTS,
            TIP_EVENTS,
            kind,
            format!("kind={kind} completed={completed} next_at={next_threshold}"),
            host.tip_delay,
            show,
        );
    }
}
