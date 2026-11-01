//! Mid-turn interjection: what a user types while an agent is still working.
//!
//! Users type over a running agent constantly. The buffer is strict FIFO and
//! never merges entries -- two quick messages must arrive as two synthetic
//! turns, not one concatenated blob that loses the boundary between them.
//! Entries are drained only at safe points (the host decides when that is;
//! this module never guesses).
//!
//! Every drained entry becomes a [`Directive`] -- the engine-visible outcome
//! the host must perform -- and steer and re-prompt entries are additionally
//! reframed as synthetic user turns, tagged so the model knows it arrived
//! mid-work, plus a trailing reminder to finish unfinished work -- so being
//! steered does not read as being told to abandon the current turn. The three
//! kinds are three different user intents with three different outcomes:
//! a **steer** continues the running turn informed by new input; a **cancel**
//! stops it; a **re-prompt** abandons it and replaces it with a fresh turn.
//! Flattening all three into injected text collapses cancel and re-prompt
//! into "steer", which is exactly the bug this module exists to prevent.
//!
//! Modelled after `xai-interjection-core` (`repos/cli/grok-build/crates/common/xai-interjection-core`),
//! trimmed to what this crate needs: no serde (nothing here crosses a process
//! boundary yet) and one buffer type instead of a generic event queue.

use std::sync::{Arc, Mutex, MutexGuard};

/// Wrapping the raw string in this size costs a few hundred bytes; keep the
/// budget clear of that overhead.
const LARGE_INTERJECTION_THRESHOLD: usize = 25_000;

const STEER_NOTE: &str = "The user sent a message while you were working:";
const REPROMPT_NOTE: &str = "The user replaced this turn with a new request:";
const UNFINISHED_TASKS_REMINDER: &str =
    "Make sure to complete any unfinished tasks from previous turns.";

/// Why this text is waiting in the buffer: which of the three mid-turn
/// intents the user meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Typed while a turn was actively streaming; that turn should continue,
    /// informed by this text.
    Steer,
    /// Typed to stop the in-flight turn. The turn stops; the text is for the
    /// operator transcript only and is never framed into a model turn -- a
    /// stopped turn has no reader for framing.
    Cancel,
    /// Typed to abandon the in-flight turn and replace it with a fresh turn
    /// whose prompt is this text.
    Reprompt,
}

/// One message waiting in the FIFO buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInterjection {
    pub text: String,
    pub kind: Kind,
}

/// What the host must do with one drained interjection: the engine-visible
/// outcome, not just text. One variant per [`Kind`], so kind survives the
/// whole trip to the engine -- a cancel can never be delivered as a steer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// The in-flight turn continues, informed by this already-framed synthetic
    /// user turn.
    Steer(String),
    /// Stop the in-flight turn now. Nothing is delivered to the model for it;
    /// there is no in-flight reader left to deliver to.
    Cancel,
    /// Abandon the in-flight turn and start a fresh turn whose prompt is this
    /// already-framed synthetic user turn.
    Reprompt(String),
}

impl Directive {
    /// Whether carrying out this directive ends the in-flight turn.
    pub fn stops_turn(&self) -> bool {
        matches!(self, Directive::Cancel | Directive::Reprompt(_))
    }
}

/// FIFO buffer of pending interjections. Cheaply cloneable; clones share one
/// underlying queue, so the composer (producer) and the turn driver
/// (consumer) can each hold their own handle.
#[derive(Debug, Clone)]
pub struct InterjectionBuffer {
    entries: Arc<Mutex<Vec<PendingInterjection>>>,
}

impl Default for InterjectionBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl InterjectionBuffer {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Enqueue one message. Text is truncated at a UTF-8-safe boundary here,
    /// not at drain time, so `len()` always reflects what will actually be
    /// sent.
    pub fn push(&self, text: String, kind: Kind) {
        let text = truncate_utf8_safe(text, LARGE_INTERJECTION_THRESHOLD);
        self.lock().push(PendingInterjection { text, kind });
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Drain every pending entry as [`Directive`]s, in FIFO order. Never
    /// merges two entries into one -- the caller gets one directive per
    /// `push`, always, and each directive's kind is the kind that was pushed.
    pub fn drain(&self) -> Vec<Directive> {
        std::mem::take(&mut *self.lock())
            .into_iter()
            .map(|entry| match entry.kind {
                Kind::Steer => Directive::Steer(frame(STEER_NOTE, &entry.text)),
                Kind::Cancel => Directive::Cancel,
                Kind::Reprompt => Directive::Reprompt(frame(REPROMPT_NOTE, &entry.text)),
            })
            .collect()
    }

    /// Drop every pending entry. P1-7 immediate-interrupt semantics: a stop
    /// directive (cancel / re-prompt) takes effect at enqueue time and
    /// discards everything queued around it, because once the user has said
    /// "stop" or "replace", nothing else in the queue is a request they still
    /// want carried out.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Frame `text` as the synthetic replacement turn for a re-prompt, using
    /// the exact envelope [`Self::drain`] produces for [`Kind::Reprompt`] —
    /// distinguishing note, `<user_query>` body, and the trailing
    /// unfinished-work reminder — with the same UTF-8-boundary truncation
    /// applied at push time.
    ///
    /// Public for hosts applying P1-7 semantics: a re-prompt aborts the
    /// in-flight turn and spawns its replacement IMMEDIATELY, so the host
    /// frames the payload at enqueue time instead of waiting for a drain that
    /// would only run after the (now-aborted) turn finished.
    pub fn frame_reprompt(text: &str) -> String {
        let bounded = truncate_utf8_safe(text.to_string(), LARGE_INTERJECTION_THRESHOLD);
        frame(REPROMPT_NOTE, &bounded)
    }

    /// Frame `text` as a steer, exactly as [`Self::drain`] does.
    pub fn frame_steer(text: &str) -> String {
        let bounded = truncate_utf8_safe(text.to_string(), LARGE_INTERJECTION_THRESHOLD);
        frame(STEER_NOTE, &bounded)
    }

    /// Raw text of every pending entry, oldest first.
    pub fn texts(&self) -> Vec<String> {
        self.lock().iter().map(|entry| entry.text.clone()).collect()
    }

    /// Raw text of the oldest entry, removed (FIFO send order).
    pub fn pop_first(&self) -> Option<String> {
        let mut guard = self.lock();
        if guard.is_empty() {
            None
        } else {
            Some(guard.remove(0).text)
        }
    }

    /// Raw text of the newest entry, removed (edit / remove last queued).
    pub fn pop_last(&self) -> Option<String> {
        self.lock().pop().map(|entry| entry.text)
    }

    fn lock(&self) -> MutexGuard<'_, Vec<PendingInterjection>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Truncate `text` to at most `max_bytes`, at the last UTF-8 character
/// boundary at or before that limit, so a multi-byte character is never split.
fn truncate_utf8_safe(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let end = text
        .char_indices()
        .take_while(|(i, _)| *i < max_bytes)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let mut truncated = text[..end].to_string();
    truncated.push_str("... [truncated]");
    truncated
}

/// Wrap `text` in the envelope for one framed kind: a distinguishing note,
/// the message itself, and the trailing reminder that unfinished work from
/// earlier turns still needs finishing. Cancel is never framed -- a stopped
/// turn has no model left to read it -- so only steer and re-prompt notes
/// ever reach this.
fn frame(note: &str, text: &str) -> String {
    format!("{note}\n<user_query>\n{text}\n</user_query>\n{UNFINISHED_TASKS_REMINDER}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_interjections_stay_ordered_and_unmerged() {
        let buf = InterjectionBuffer::new();
        buf.push("one".into(), Kind::Steer);
        buf.push("two".into(), Kind::Steer);
        buf.push("three".into(), Kind::Steer);

        let drained = buf.drain();
        assert_eq!(drained.len(), 3, "one message per entry, never merged");
        assert!(matches!(drained[0], Directive::Steer(ref t) if t.contains("one")));
        assert!(matches!(drained[1], Directive::Steer(ref t) if t.contains("two")));
        assert!(matches!(drained[2], Directive::Steer(ref t) if t.contains("three")));
        assert!(buf.is_empty(), "drain must empty the buffer");
    }

    #[test]
    fn a_mid_turn_message_carries_the_unfinished_work_reminder() {
        let buf = InterjectionBuffer::new();
        buf.push("stop and check the other file too".into(), Kind::Steer);
        let out = buf.drain();
        let Directive::Steer(text) = &out[0] else {
            panic!("expected a steer directive")
        };
        assert!(
            text.ends_with(UNFINISHED_TASKS_REMINDER),
            "a steer must not read as licence to abandon the current turn"
        );
    }

    // Steer, cancel, and re-prompt must not collapse into one outcome: they
    // are three different user intents ("keep going, also this" vs. "stop"
    // vs. "drop that, do this instead"). One test per kind, each asserting
    // the engine-visible directive -- not just that text arrived.
    #[test]
    fn a_steer_yields_a_framed_message_the_running_turn_continues_with() {
        let buf = InterjectionBuffer::new();
        buf.push("same text".into(), Kind::Steer);
        let out = buf.drain();
        assert_eq!(
            out,
            vec![Directive::Steer(format!(
                "{STEER_NOTE}\n<user_query>\nsame text\n</user_query>\n{UNFINISHED_TASKS_REMINDER}"
            ))]
        );
        assert!(
            !out[0].stops_turn(),
            "a steer must not end the running turn"
        );
    }

    #[test]
    fn a_cancel_yields_a_stop_and_no_synthetic_turn() {
        let buf = InterjectionBuffer::new();
        buf.push("actually stop".into(), Kind::Cancel);
        let out = buf.drain();
        assert_eq!(out, vec![Directive::Cancel]);
        assert!(out[0].stops_turn(), "a cancel must end the running turn");
    }

    #[test]
    fn a_reprompt_yields_a_replacement_turn_not_a_steer() {
        let buf = InterjectionBuffer::new();
        buf.push("same text".into(), Kind::Steer);
        buf.push("same text".into(), Kind::Reprompt);
        let out = buf.drain();
        assert_eq!(
            out[1],
            Directive::Reprompt(format!(
                "{REPROMPT_NOTE}\n<user_query>\nsame text\n</user_query>\n{UNFINISHED_TASKS_REMINDER}"
            ))
        );
        assert_ne!(
            out[0], out[1],
            "steer and reprompt must not collapse into one outcome"
        );
        assert!(
            out[1].stops_turn(),
            "a reprompt must abandon the running turn"
        );
    }

    #[test]
    fn mixed_kinds_stay_in_fifo_order_one_directive_per_entry() {
        let buf = InterjectionBuffer::new();
        buf.push("first".into(), Kind::Cancel);
        buf.push("second".into(), Kind::Steer);
        buf.push("third".into(), Kind::Reprompt);

        let out = buf.drain();
        assert_eq!(out.len(), 3, "one directive per entry, never merged");
        assert_eq!(out[0], Directive::Cancel);
        assert!(matches!(out[1], Directive::Steer(ref t) if t.contains("second")));
        assert!(matches!(out[2], Directive::Reprompt(ref t) if t.contains("third")));
        assert!(buf.is_empty(), "drain must empty the buffer");
    }

    #[test]
    fn oversized_interjections_truncate_at_a_utf8_boundary() {
        let buf = InterjectionBuffer::new();
        // Multi-byte characters throughout, well past the threshold, so a
        // naive byte-index cut would land mid-character and panic on
        // slicing.
        let huge = "é".repeat(LARGE_INTERJECTION_THRESHOLD);
        buf.push(huge, Kind::Steer);
        let out = buf.drain();
        let Directive::Steer(text) = &out[0] else {
            panic!("expected a steer directive")
        };
        assert!(text.contains("... [truncated]"));
        assert!(text.len() < LARGE_INTERJECTION_THRESHOLD + 200);
    }

    #[test]
    fn short_interjections_are_untouched_by_truncation() {
        let buf = InterjectionBuffer::new();
        buf.push("hi".into(), Kind::Steer);
        let out = buf.drain();
        let Directive::Steer(text) = &out[0] else {
            panic!("expected a steer directive")
        };
        assert!(!text.contains("[truncated]"));
        assert_eq!(
            text,
            &format!("{STEER_NOTE}\n<user_query>\nhi\n</user_query>\n{UNFINISHED_TASKS_REMINDER}")
        );
    }

    #[test]
    fn draining_an_empty_buffer_yields_nothing() {
        let buf = InterjectionBuffer::new();
        assert!(buf.drain().is_empty());
    }

    /// P1-7: a stop discards everything queued around it, so the host can
    /// empty the FIFO the moment a cancel/re-prompt takes effect.
    #[test]
    fn clear_discards_every_pending_entry() {
        let buf = InterjectionBuffer::new();
        buf.push("one".into(), Kind::Steer);
        buf.push("two".into(), Kind::Cancel);
        buf.clear();
        assert!(buf.is_empty(), "a stop must discard the whole queue");
        assert_eq!(buf.drain(), Vec::<Directive>::new());
    }

    /// P1-7 immediate re-prompt: `frame_reprompt` produces the exact envelope
    /// drain() would — same note, same reminder — so spawning the replacement
    /// turn at enqueue time is indistinguishable from the drained path.
    #[test]
    fn frame_reprompt_matches_the_drained_reprompt_envelope() {
        let framed = InterjectionBuffer::frame_reprompt("do this instead");
        assert_eq!(
            framed,
            format!(
                "{REPROMPT_NOTE}\n<user_query>\ndo this instead\n</user_query>\n{UNFINISHED_TASKS_REMINDER}"
            )
        );

        // Cross-check against an actual drain of a Reprompt entry.
        let buf = InterjectionBuffer::new();
        buf.push("do this instead".into(), Kind::Reprompt);
        let Directive::Reprompt(via_drain) = buf.drain().remove(0) else {
            panic!("expected a reprompt directive");
        };
        assert_eq!(framed, via_drain);
    }

    /// The UTF-8-boundary truncation applied at push time applies equally to
    /// enqueue-time framing, and never splits a multi-byte character.
    #[test]
    fn frame_reprompt_truncates_at_a_utf8_boundary() {
        let huge = "é".repeat(LARGE_INTERJECTION_THRESHOLD * 2);
        let framed = InterjectionBuffer::frame_reprompt(&huge);
        assert!(framed.contains("... [truncated]"));
        // Must not panic on slicing: the frame is valid UTF-8 by construction
        // (it was built from a String), and bounded well past the threshold.
        assert!(framed.len() < LARGE_INTERJECTION_THRESHOLD + 200);

        let short = InterjectionBuffer::frame_reprompt("hi");
        assert!(!short.contains("[truncated]"));
    }

    #[test]
    fn clones_share_one_underlying_queue() {
        let buf = InterjectionBuffer::new();
        let producer = buf.clone();
        producer.push("from the clone".into(), Kind::Steer);
        assert_eq!(buf.len(), 1);
    }
}
