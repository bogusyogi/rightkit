//! The mailbox wire protocol, ALPN `right/mbox/1`.
//!
//! One bidirectional stream per request, frames as in `frame.rs`. The server sees the endpoint id
//! of whoever connects (iroh proves it) and nothing inside an item: the body is sealed
//! (`seal.rs`). The protocol follows the shape Happy (MIT) uses for its blind relay: the client
//! deposits opaque ciphertext for a recipient, the recipient pulls and acknowledges, the server
//! deletes on acknowledgement. Happy's source was not read for this crate (see the notes in
//! `docs/relay-hetzner.md`).
//!
//! * `deposit`: any ALLOWED sender puts one sealed item in a recipient's mailbox.
//! * `collect`: only the mailbox owner (the connecting endpoint) lists items after a cursor.
//! * `ack`: only the owner; the acknowledged items are deleted.
//! * `set_allow`: only the owner; replaces the list of endpoints allowed to deposit.
//!
//! A mailbox is named by its owner's endpoint id, so there is nothing to register: the owner
//! always reaches its own mailbox and nobody else's.

use serde::{Deserialize, Serialize};

pub const ALPN_MAILBOX: &[u8] = b"right/mbox/1";

/// Largest sealed item the server accepts by default (the small path, `caps::SMALL_ITEM_MAX` plus
/// the seal's header, tag and padding to the next 64 KB step).
pub const DEFAULT_MAX_ITEM_BYTES: usize = 128 * 1024;
/// Largest `collect` batch.
pub const MAX_COLLECT: u32 = 100;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Body: the sealed item.
    Deposit {
        /// Recipient endpoint id, 64 hex characters.
        to: String,
        /// The item's id, 32 hex characters; unique per sender and bound into the seal.
        record_id: String,
    },
    Collect {
        /// Return items with a server sequence greater than this.
        after: u64,
        limit: u32,
    },
    Ack {
        seqs: Vec<u64>,
    },
    SetAllow {
        /// Endpoint ids (64 hex) allowed to deposit into the caller's mailbox.
        senders: Vec<String>,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    /// Machine word when `ok` is false: `not_allowed`, `too_large`, `queue_full`, `quota`,
    /// `rate_limited`, `malformed`.
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Deposit: the server sequence given to the item.
    #[serde(default)]
    pub seq: Option<u64>,
    /// Collect: how many item frames follow. Ack: how many items were deleted.
    #[serde(default)]
    pub count: Option<u32>,
}

impl Reply {
    pub fn ok() -> Self {
        Self {
            ok: true,
            ..Self::default()
        }
    }

    pub fn error(code: &str, message: &str) -> Self {
        Self {
            ok: false,
            code: Some(code.to_string()),
            message: Some(message.to_string()),
            ..Self::default()
        }
    }
}

/// Header of one collected item; the sealed item is the frame body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemHeader {
    pub seq: u64,
    /// The depositing endpoint, 64 hex characters.
    pub from: String,
    pub record_id: String,
    pub deposited_ms: u64,
}
