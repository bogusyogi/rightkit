//! `intent.source`: where the authority for a task came from.
//! Covers the provenance table, recording, delegation binding and gateway-origin requests.
//!
//! A relay never mints authority. The agent records only the provenance it
//! received. Agent output, transcript text and classifier scores are not
//! provenance.

use serde::{Deserialize, Serialize};

/// Version of the [`Intent`] record.
pub const INTENT_SCHEMA_VERSION: u32 = 1;
/// Vocabulary version for [`IntentSource`] and [`GrantOrigin`].
pub const INTENT_SOURCE_SCHEMA_VERSION: u32 = 1;
/// Version of the [`DelegationRef`] record.
pub const DELEGATION_REF_SCHEMA_VERSION: u32 = 1;

/// The `intent` block of a task envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub schema_version: u32,
    pub source: IntentSource,
}

/// Provenance kinds. Serialized as `intent.source.kind`:
/// `direct`, `delegated`, `unavailable-pointer`, `revoked`, `out-of-scope`, `grant`, `engine`.
///
/// Relay variants carry the quote and a pointer to the sending chat's message
/// (or the device it ran on). `Grant` carries no quote: a gateway call is
/// authorized by the owner's local grant, not by a message. `Engine` carries none either: a run the engine
/// starts has nothing the owner said this time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum IntentSource {
    /// the owner's words, quoted, with a pointer. He asked for this; the receiver acts.
    Direct {
        chat: String,
        device: String,
        message_id: String,
        quote: String,
    },
    /// His delegation, quoted and bound to the reference and revision that
    /// hold it. `decision` is what the relaying chat decided under it. The
    /// receiver rechecks `delegation` before acting and after any wait.
    Delegated {
        chat: String,
        device: String,
        message_id: String,
        quote: String,
        delegation: DelegationRef,
        decision: String,
    },
    /// A quote whose source cannot be read (chat closed, link down). Reversible
    /// work only; anything else asks the sending chat, or whoever the workspace rule names.
    UnavailablePointer {
        chat: String,
        device: String,
        message_id: String,
        quote: String,
    },
    /// The delegation was revoked or revised after it was quoted. Not covered:
    /// the receiver refuses with "not covered by the relayed request".
    Revoked {
        chat: String,
        device: String,
        message_id: String,
        quote: String,
        delegation: DelegationRef,
    },
    /// The act falls outside the quoted request or delegation. Not covered.
    OutOfScope {
        chat: String,
        device: String,
        message_id: String,
        quote: String,
    },
    /// Authority from a local grant the owner set (for example a gateway call).
    /// The agent never writes a quote, message id or intent it did not receive.
    Grant {
        origin: GrantOrigin,
        grant_id: String,
        grant_revision: u64,
        /// Only when the request itself carries a conversation reference.
        conversation_ref: Option<String>,
    },
    /// A run the engine started from a schedule or from a standing instruction the owner approved. Nothing
    /// the owner said this time: there is no quote, message id or chat, and none may be written into it.
    ///
    /// Only the engine builds this variant, from its own store, never from a caller's request. The receiver
    /// accepts it only while the rule `rule_id` is active at exactly `rule_revision`, and runs it with the
    /// unattended tool set; every hard-to-undo step still asks the owner. `occurrence_key` names this one
    /// occurrence (a digest of the rule, its revision and the trigger), so the same firing twice is one task.
    Engine {
        trigger: EngineTrigger,
        rule_id: String,
        rule_revision: u64,
        occurrence_key: String,
    },
}

/// What started an engine run. Serialized in kebab-case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineTrigger {
    /// A schedule came due.
    Schedule,
    /// A signal (a mail, a check's finding) matched a standing instruction.
    Signal,
}

/// Longest `rule_id` or `occurrence_key`, in bytes.
pub const MAX_ENGINE_ID_BYTES: usize = 256;

/// Why an [`IntentSource`] is not well formed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IntentSourceError {
    #[error("engine intent field {0} is empty")]
    Empty(&'static str),
    #[error("engine intent field {0} is longer than 256 bytes or holds a control character")]
    Malformed(&'static str),
    #[error("engine intent rule_revision must be at least 1")]
    Revision,
}

impl IntentSource {
    /// Checks what can be checked from the record alone. Only [`IntentSource::Engine`] has rules here:
    /// `rule_id` and `occurrence_key` are non-empty, at most [`MAX_ENGINE_ID_BYTES`] bytes with no control
    /// character, and `rule_revision` is at least 1. That an `occurrence_key` is unique per occurrence is
    /// the store's rule (the task's idempotency key is derived from it), not something one record shows.
    /// Every other source is accepted as it was before.
    pub fn validate(&self) -> Result<(), IntentSourceError> {
        let IntentSource::Engine {
            rule_id,
            rule_revision,
            occurrence_key,
            ..
        } = self
        else {
            return Ok(());
        };
        for (name, value) in [("rule_id", rule_id), ("occurrence_key", occurrence_key)] {
            if value.trim().is_empty() {
                return Err(IntentSourceError::Empty(name));
            }
            if value.len() > MAX_ENGINE_ID_BYTES || value.chars().any(char::is_control) {
                return Err(IntentSourceError::Malformed(name));
            }
        }
        if *rule_revision == 0 {
            return Err(IntentSourceError::Revision);
        }
        Ok(())
    }
}

/// Pointer to the authoritative delegation: a chat message id, or a
/// contract rule identifier, with the revision that holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRef {
    pub schema_version: u32,
    pub reference: String,
    pub revision: u64,
}

/// Where a grant-origin request came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantOrigin {
    /// A remote chat client through the gateway. Tunnel authentication identifies the
    /// installation and is not requester identity.
    Gateway,
}
