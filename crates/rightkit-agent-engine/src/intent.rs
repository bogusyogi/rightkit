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
/// `direct`, `delegated`, `unavailable-pointer`, `revoked`, `out-of-scope`, `grant`.
///
/// Relay variants carry the quote and a pointer to the sending chat's message
/// (or the device it ran on). `Grant` carries no quote: a gateway call is
/// authorized by the owner's local grant, not by a message.
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
    /// work only; anything else asks the sending chat, or Astra per workspace rule.
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
    /// ChatGPT through the gateway. Tunnel authentication identifies the
    /// installation and is not requester identity.
    Gateway,
}
