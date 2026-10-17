//! `rk-relay/1`: the blind relay and mailbox for devices that cannot reach each other directly
//! (issue #133; decisions "Pulse device connectivity" and its "relay-first" amendment,
//! 2026-10-16).
//!
//! The relay carries ciphertext it cannot read. It sees who is talking to whom (mailbox ids,
//! circle ids), when, and how much; it never sees content, labels, or keys. Small traffic
//! (messages, commands, receipts, revocations, push-wake) goes through it, with store-and-forward
//! for a device that is asleep; bulk goes direct, and what is relayed is capped.
//!
//! This module holds the transport-free pieces of the protocol, all testable in process:
//!
//! * the vocabulary and wire frames ([`ClientFrame`], [`ServerFrame`], [`MailboxId`],
//!   [`CircleId`], [`Class`]) and the exact bytes that are signed ([`ticket_message`],
//!   [`auth_message`]);
//! * the relay's state machine ([`RelayCore`]): challenge and stateless authentication, a
//!   per-(mailbox, circle) store-and-forward queue with a time to live, live delivery to a
//!   connected recipient, acknowledgement, per-class size caps, and a per-circle monthly byte
//!   quota. It owns no sockets and no clock;
//! * the device side ([`RelaySession`]): authenticate, seal and send, receive, de-duplicate and
//!   acknowledge. The end-to-end cipher is a [`Sealer`] the caller supplies; the relay never
//!   sees a key.
//!
//! **Authority without accounts.** An owner key (the "circle") signs a ticket naming a device
//! key and an expiry ([`issue_ticket`]). The relay verifies the ticket and the device's
//! signature over its challenge; it learns two pseudonyms and keeps nothing about a person.
//! A message is queued under (recipient mailbox, sender circle), and a device reads only the
//! queue of its own circle, so a stranger who knows a mailbox id can neither read nor block that
//! mailbox's queue.
//!
//! Not here (see CHANGELOG 0.2.6, "open"): a socket or WebSocket adapter and deployment, the
//! concrete end-to-end cipher and its key agreement, direct connection and hole punching,
//! push-wake, and persistence of the queue.

mod client;
mod hub;

pub use self::client::{
    issue_ticket, ClientError, Delivery, DeviceCredential, Event, RelaySession, SealError, Sealer,
};
pub use self::hub::{ConnId, Outgoing, RelayCore, RelayError, RelayLimits};

use crate::remote::util::{hex, unhex};
use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};

/// Protocol name, bound into every signed message and id.
pub const PROTOCOL: &str = "rk-relay/1";

/// A per-device address: the SHA-256 of the device's public signing key, domain separated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct MailboxId(pub [u8; 32]);

/// A per-owner pseudonym: the SHA-256 of the owner's public signing key, domain separated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct CircleId(pub [u8; 32]);

fn domain_hash(domain: &str, key: &[u8; 32]) -> [u8; 32] {
    let mut input = Vec::with_capacity(domain.len() + 32);
    input.extend_from_slice(domain.as_bytes());
    input.extend_from_slice(key);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest(&SHA256, &input).as_ref());
    out
}

impl MailboxId {
    /// The mailbox of the device whose public signing key is `device_pub`.
    pub fn of_device(device_pub: &[u8; 32]) -> Self {
        Self(domain_hash("rk-relay/1 mailbox", device_pub))
    }
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
    pub fn from_hex(text: &str) -> Option<Self> {
        unhex_array::<32>(text).map(Self)
    }
}

impl CircleId {
    /// The circle owned by the key `owner_pub`.
    pub fn of_owner(owner_pub: &[u8; 32]) -> Self {
        Self(domain_hash("rk-relay/1 circle", owner_pub))
    }
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
    pub fn from_hex(text: &str) -> Option<Self> {
        unhex_array::<32>(text).map(Self)
    }
}

pub(crate) fn unhex_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = unhex(text)?;
    <[u8; N]>::try_from(bytes.as_slice()).ok()
}

/// What a message is for. The relay applies the size cap of the class a sender declares, so the
/// class is a claim: it bounds honest senders, while the largest cap bounds any sender (the
/// relay checks the body length against the declared class and against the overall maximum).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// Messages, commands, receipts, revocations, push-wake.
    Small,
    /// A clipboard item or attachment (relayed cap 10 MiB).
    Clipboard,
    /// A file or other bulk transfer that had to be relayed (relayed cap 25 MiB).
    Transfer,
    /// Screen sharing: direct only. The relay refuses it and a [`RelaySession`] never sends it.
    Screen,
}

/// The bytes a circle owner signs to authorise `device_pub` until `exp` (unix seconds).
pub fn ticket_message(owner_pub: &[u8; 32], device_pub: &[u8; 32], exp: u64) -> Vec<u8> {
    let mut m = Vec::with_capacity(PROTOCOL.len() + 8 + 32 + 32 + 8);
    m.extend_from_slice(PROTOCOL.as_bytes());
    m.extend_from_slice(b" ticket");
    m.extend_from_slice(owner_pub);
    m.extend_from_slice(device_pub);
    m.extend_from_slice(&exp.to_be_bytes());
    m
}

/// The bytes a device signs to prove it holds `device_pub` for this connection's `nonce`.
pub fn auth_message(nonce: &[u8; 32], device_pub: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(PROTOCOL.len() + 6 + 32 + 32);
    m.extend_from_slice(PROTOCOL.as_bytes());
    m.extend_from_slice(b" auth");
    m.extend_from_slice(nonce);
    m.extend_from_slice(device_pub);
    m
}

/// Error codes carried in [`ServerFrame::Error`].
pub mod code {
    pub const BAD_FRAME: &str = "bad_frame";
    pub const UNAUTHENTICATED: &str = "unauthenticated";
    pub const AUTH_FAILED: &str = "auth_failed";
    pub const EXPIRED: &str = "expired";
    pub const TOO_LARGE: &str = "too_large";
    pub const DIRECT_ONLY: &str = "direct_only";
    pub const QUOTA_EXCEEDED: &str = "quota_exceeded";
    pub const QUEUE_FULL: &str = "queue_full";
    pub const BUSY: &str = "busy";
}

/// A frame from a device to the relay. One JSON object per line on the wire; the transport that
/// carries it (a WebSocket, a TLS stream) is outside this module.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Answer to the relay's challenge. All binary fields are lowercase hex.
    Auth {
        owner_pub: String,
        device_pub: String,
        exp: u64,
        ticket_sig: String,
        challenge_sig: String,
    },
    /// Deposit one sealed message for `to`. `id` (32 hex digits, random) makes a retry
    /// idempotent: the same id from the same sender is stored once.
    Put {
        id: String,
        to: String,
        class: Class,
        /// Standard base64 of the sealed bytes.
        body: String,
    },
    /// Ask for up to `limit` queued messages that were not yet delivered on this connection.
    Fetch {
        limit: u32,
    },
    /// The device has processed these ids; the relay may delete them.
    Ack {
        ids: Vec<String>,
    },
    Ping,
}

/// A frame from the relay to a device.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    Challenge {
        nonce: String,
    },
    Authed {
        mailbox: String,
        circle: String,
    },
    Stored {
        id: String,
        seq: u64,
    },
    Deliver {
        id: String,
        seq: u64,
        /// The sender's mailbox. Routing information the relay saw; the sealed body is what
        /// authenticates the sender.
        from: String,
        class: Class,
        body: String,
    },
    Acked {
        count: u32,
    },
    Error {
        code: String,
        message: String,
        retry_after: Option<u64>,
    },
    Pong,
}

impl ServerFrame {
    pub(crate) fn error(code: &str, message: &str) -> Self {
        ServerFrame::Error {
            code: code.into(),
            message: message.into(),
            retry_after: None,
        }
    }
}
