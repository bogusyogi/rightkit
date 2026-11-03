//! Reliable, ordered message delivery between linked devices over a relay (rightkit#1244).
//!
//! The layer is sans-IO: the caller moves [`Frame`]s over whatever transport it has (the relay in
//! rightkit#133) and drives two state machines.
//!
//! - [`SendQueue`] numbers messages per stream, splits large ones into UTF-8-safe fragments, keeps
//!   unacknowledged messages for replay under their original sequence numbers, and emits skip
//!   markers for a permanent failure or timeout so a stream never hangs on one sequence number.
//! - [`Receiver`] delivers messages in order, holds out-of-order and partial messages in bounded
//!   buffers, and acknowledges (cumulatively) only after the handler accepted the message.
//!
//! A device that only sends commands uses a `SendQueue` and no `Receiver`.
//!
//! Version mismatch fails closed ([`DecodeError::UnsupportedVersion`]); bytes that are not a
//! device-link frame at all ([`DecodeError::NotAFrame`]) are the caller's plain payload, which is
//! how old peers keep working.

mod receive;
mod send;
mod wire;

pub use receive::{Delivery, ReceiveError, Receiver};
pub use send::{EnqueueError, SendError, SendQueue};
pub use wire::{decode, encode, DecodeError};

/// Wire protocol version carried in every frame.
pub const PROTOCOL_VERSION: u8 = 1;

/// Limits shared by both sides. The defaults are the protocol's: 4 MiB messages, 128 KiB
/// fragments, at most 64 fragments per message, at most 64 unacknowledged messages and 16 MiB
/// pending per peer, and 16 MiB of reassembly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_message_bytes: usize,
    pub fragment_bytes: usize,
    pub max_fragments: usize,
    pub max_unacked: usize,
    pub max_pending_bytes: usize,
    pub max_reassembly_bytes: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_message_bytes: 4 * 1024 * 1024,
            fragment_bytes: 128 * 1024,
            max_fragments: 64,
            max_unacked: 64,
            max_pending_bytes: 16 * 1024 * 1024,
            max_reassembly_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Why a sequence number was skipped instead of delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The message failed permanently.
    Failed,
    /// The message was not acknowledged within the caller's timeout.
    TimedOut,
}

/// One unit on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Sent first on every connection. `base_seq` is the lowest sequence number the sender can
    /// still replay, so a receiver that restarted and lost its ACK state resumes there.
    Hello { stream_id: u64, base_seq: u64 },
    /// One fragment of message `seq` (`index` of `count`).
    Data {
        stream_id: u64,
        seq: u64,
        index: u16,
        count: u16,
        text: String,
    },
    /// Cumulative acknowledgement: every sequence number below `next_expected` was accepted.
    Ack { stream_id: u64, next_expected: u64 },
    /// Advance past `seq` without delivering it.
    Skip {
        stream_id: u64,
        seq: u64,
        reason: SkipReason,
    },
}
