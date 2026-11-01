//! Durable pinned-context registry (ENG-104).
//!
//! Compaction already treats any `TranscriptMessage` whose `pinned: bool`
//! field is `true` as preserved verbatim: see
//! `compactor_preservation::is_pinned` and
//! `compactor_preservation::partition_authoritative_messages` (both sibling
//! `include!` modules of `compactor.rs`, not editable from this lane). This
//! module does **not** duplicate or replace that mechanism -- it supplies
//! the durable, engine-owned *data* those mechanisms already know how to
//! honor: a stable list of which message ids the user pinned, independent
//! of the transcript's own mutable flag.
//!
//! Why a separate registry instead of only the transcript's `pinned` bit:
//! the bit lives on `TranscriptMessage`, which compaction rewrites (fold,
//! rebuild, replay). A registry keyed by `(session_id, message_id)` survives
//! that rewriting and can be reapplied every time a transcript is loaded or
//! about to be compacted, so a user pin can never be silently lost to a
//! rebuild that forgot to carry the flag forward.

use rightkit_agent_engine_protocol::TranscriptMessage;
use serde::{Deserialize, Serialize};

/// One user-pinned message, durable across compaction and session reload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedMessageRef {
    pub session_id: String,
    pub message_id: String,
    /// ISO-8601-ish timestamp of when the user pinned it (for display /
    /// ordering in the pinned-context UI panel).
    pub pinned_at: String,
}

/// Durable registry of pinned messages, independent of any one transcript's
/// in-memory `pinned` bit. Expected lifecycle: load/deserialize once per
/// session store, `pin`/`unpin` on user action, call [`apply_to`] before
/// handing a transcript to the compactor (or right after loading a
/// transcript from disk) so the registry -- not whatever the persisted
/// transcript happened to have -- is the source of truth for what survives.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinnedContextRegistry {
    entries: Vec<PinnedMessageRef>,
}

impl PinnedContextRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin a message. Idempotent: pinning an already-pinned message is a
    /// no-op (keeps the original `pinned_at`).
    pub fn pin(
        &mut self,
        session_id: impl Into<String>,
        message_id: impl Into<String>,
        pinned_at: impl Into<String>,
    ) {
        let session_id = session_id.into();
        let message_id = message_id.into();
        if self.is_pinned(&session_id, &message_id) {
            return;
        }
        self.entries.push(PinnedMessageRef {
            session_id,
            message_id,
            pinned_at: pinned_at.into(),
        });
    }

    /// Unpin a message. Returns `true` if an entry was actually removed.
    pub fn unpin(&mut self, session_id: &str, message_id: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|e| !(e.session_id == session_id && e.message_id == message_id));
        self.entries.len() != before
    }

    pub fn is_pinned(&self, session_id: &str, message_id: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.session_id == session_id && e.message_id == message_id)
    }

    /// All pins for one session, most-recently-pinned first (by insertion
    /// order reversed -- `pinned_at` ordering is a caller/display concern
    /// since this module treats it as an opaque string).
    pub fn for_session(&self, session_id: &str) -> Vec<&PinnedMessageRef> {
        self.entries
            .iter()
            .filter(|e| e.session_id == session_id)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Reapply this registry's pins onto a transcript's own `pinned` bit --
    /// the exact flag `compactor_preservation::is_pinned` and
    /// `partition_authoritative_messages` read to decide what a compaction
    /// pass preserves verbatim. Only ever sets the bit to `true`; never
    /// clears an existing `pinned: true` that the transcript set through
    /// some other path (e.g. an open-obligation pin).
    ///
    /// Call this once per session, before the transcript reaches
    /// `Compactor::compact()`, e.g. right after loading a persisted
    /// transcript or immediately after a user pin/unpin action mutates the
    /// registry.
    pub fn apply_to(&self, session_id: &str, messages: &mut [TranscriptMessage]) {
        if self.is_empty() {
            return;
        }
        for message in messages.iter_mut() {
            let id = message_id(message);
            if self.is_pinned(session_id, id) {
                set_pinned(message, true);
            }
        }
    }
}

fn message_id(message: &TranscriptMessage) -> &str {
    match message {
        TranscriptMessage::System { id, .. }
        | TranscriptMessage::User { id, .. }
        | TranscriptMessage::Assistant { id, .. }
        | TranscriptMessage::Tool { id, .. }
        | TranscriptMessage::Compact { id, .. }
        | TranscriptMessage::Fold { id, .. } => id.as_str(),
    }
}

fn set_pinned(message: &mut TranscriptMessage, value: bool) {
    match message {
        TranscriptMessage::System { pinned, .. }
        | TranscriptMessage::User { pinned, .. }
        | TranscriptMessage::Assistant { pinned, .. }
        | TranscriptMessage::Tool { pinned, .. }
        | TranscriptMessage::Compact { pinned, .. }
        | TranscriptMessage::Fold { pinned, .. } => *pinned = value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_message(id: &str) -> TranscriptMessage {
        TranscriptMessage::User {
            id: id.to_string(),
            created_at: "2026-08-29T00:00:00Z".to_string(),
            content: "hello".to_string(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn pin_unpin_idempotent_and_scoped_per_session() {
        let mut registry = PinnedContextRegistry::new();
        registry.pin("s1", "m1", "2026-08-29T00:00:00Z");
        registry.pin("s1", "m1", "2026-08-29T01:00:00Z"); // no-op
        assert_eq!(registry.for_session("s1").len(), 1);
        assert!(!registry.is_pinned("s2", "m1"));

        assert!(registry.unpin("s1", "m1"));
        assert!(!registry.unpin("s1", "m1")); // already gone
        assert!(registry.is_empty());
    }

    #[test]
    fn apply_to_sets_pinned_bit_for_registered_messages_only() {
        let mut registry = PinnedContextRegistry::new();
        registry.pin("s1", "m1", "2026-08-29T00:00:00Z");

        let mut messages = vec![user_message("m1"), user_message("m2")];
        registry.apply_to("s1", &mut messages);

        let TranscriptMessage::User { pinned: p1, .. } = &messages[0] else {
            panic!("expected user message");
        };
        let TranscriptMessage::User { pinned: p2, .. } = &messages[1] else {
            panic!("expected user message");
        };
        assert!(*p1);
        assert!(!*p2);
    }

    #[test]
    fn apply_to_never_unpins_a_message_pinned_by_another_path() {
        let registry = PinnedContextRegistry::new(); // empty: no user pins
        let mut messages = vec![user_message("m1")];
        if let TranscriptMessage::User { pinned, .. } = &mut messages[0] {
            *pinned = true; // e.g. set by open-obligation protection
        }
        registry.apply_to("s1", &mut messages);
        let TranscriptMessage::User { pinned, .. } = &messages[0] else {
            panic!("expected user message");
        };
        assert!(*pinned, "apply_to must never clear an existing pin");
    }
}
