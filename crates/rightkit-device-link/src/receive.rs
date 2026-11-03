//! The receiving side: in-order delivery from bounded buffers, acknowledged on handler accept.

use std::collections::{BTreeMap, HashMap};

use crate::{Frame, Limits};

/// Why an incoming frame was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveError {
    /// A `Data`, `Skip` or `Ack` arrived before any `Hello`.
    HelloRequired,
    /// The frame names a different stream than the current `Hello`.
    StreamMismatch,
    /// The sequence number is further ahead than the unacknowledged window allows.
    OutOfWindow,
    /// A fragment index, count or size that the protocol does not allow, or one that contradicts
    /// earlier fragments of the same message.
    BadFragment,
    /// The reassembled message is longer than [`Limits::max_message_bytes`]; it was dropped.
    MessageTooLarge,
    /// Admitting the fragment would exceed the reassembly cap (beyond the space reserved for the
    /// message at the head of the queue). The fragment was dropped; the sender recovers by
    /// replaying ([`crate::SendQueue::retransmit`]).
    BufferFull,
    /// [`Receiver::accept`] or [`Receiver::fail`] named a sequence number that is not the message
    /// currently out for handling.
    NotDelivered,
}

impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ReceiveError::HelloRequired => "frame arrived before a hello",
            ReceiveError::StreamMismatch => "frame is for a different stream",
            ReceiveError::OutOfWindow => "sequence number is outside the window",
            ReceiveError::BadFragment => "fragment is not allowed",
            ReceiveError::MessageTooLarge => "reassembled message is larger than the limit",
            ReceiveError::BufferFull => "reassembly buffer is full",
            ReceiveError::NotDelivered => "sequence number is not the message out for handling",
        })
    }
}

impl std::error::Error for ReceiveError {}

/// A message ready for the handler. Call [`Receiver::accept`] (or [`Receiver::fail`]) with `seq`
/// when the handler is done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub seq: u64,
    pub message: String,
}

enum State {
    Partial {
        count: u16,
        parts: BTreeMap<u16, String>,
    },
    Complete(String),
    /// Handed to the handler, not yet accepted.
    InFlight,
    Skipped,
}

struct Slot {
    state: State,
    bytes: usize,
}

/// Incoming side for one peer. Holds the current stream's state; a `Hello` for a different stream
/// id starts it over.
pub struct Receiver {
    limits: Limits,
    stream_id: Option<u64>,
    next_expected: u64,
    slots: HashMap<u64, Slot>,
    reassembly_bytes: usize,
}

impl Receiver {
    pub fn new(limits: Limits) -> Receiver {
        Receiver {
            limits,
            stream_id: None,
            next_expected: 0,
            slots: HashMap::new(),
            reassembly_bytes: 0,
        }
    }

    /// Every sequence number below this was accepted.
    pub fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// Bytes held in the reassembly buffers.
    pub fn reassembly_bytes(&self) -> usize {
        self.reassembly_bytes
    }

    fn ack(&self) -> Option<Frame> {
        self.stream_id.map(|stream_id| Frame::Ack {
            stream_id,
            next_expected: self.next_expected,
        })
    }

    /// Handle a frame from the peer. Returns frames to send back: an `Ack` when the frame was a
    /// duplicate of something already accepted (the first ACK may have been lost), when a `Hello`
    /// resumed the stream, or when skip markers advanced the stream. Completed in-order messages
    /// come out of [`Receiver::poll_delivery`]. An `Ack` frame from the peer is ignored.
    pub fn on_frame(&mut self, frame: Frame) -> Result<Vec<Frame>, ReceiveError> {
        match frame {
            Frame::Hello {
                stream_id,
                base_seq,
            } => {
                if self.stream_id != Some(stream_id) {
                    self.slots.clear();
                    self.reassembly_bytes = 0;
                    self.stream_id = Some(stream_id);
                    // A receiver with no state for this stream resumes where the sender says.
                    self.next_expected = base_seq;
                } else if base_seq > self.next_expected {
                    // The sender no longer holds anything below `base_seq`.
                    self.advance_to(base_seq);
                }
                Ok(self.ack().into_iter().collect())
            }
            Frame::Data {
                stream_id,
                seq,
                index,
                count,
                text,
            } => {
                self.check_stream(stream_id)?;
                if seq < self.next_expected {
                    return Ok(self.ack().into_iter().collect());
                }
                self.check_window(seq)?;
                self.admit_fragment(seq, index, count, text)?;
                Ok(Vec::new())
            }
            Frame::Skip { stream_id, seq, .. } => {
                self.check_stream(stream_id)?;
                if seq < self.next_expected {
                    return Ok(self.ack().into_iter().collect());
                }
                self.check_window(seq)?;
                // A message already out for handling finishes through accept or fail.
                if matches!(
                    self.slots.get(&seq),
                    Some(Slot {
                        state: State::InFlight,
                        ..
                    })
                ) {
                    return Ok(Vec::new());
                }
                if let Some(old) = self.slots.insert(
                    seq,
                    Slot {
                        state: State::Skipped,
                        bytes: 0,
                    },
                ) {
                    self.reassembly_bytes -= old.bytes;
                }
                let before = self.next_expected;
                self.skip_over_marked();
                if self.next_expected != before {
                    Ok(self.ack().into_iter().collect())
                } else {
                    Ok(Vec::new())
                }
            }
            Frame::Ack { .. } => Ok(Vec::new()),
        }
    }

    /// The message at the head of the queue, when it is complete and not already out for
    /// handling. Messages come out strictly in sequence order, one at a time.
    pub fn poll_delivery(&mut self) -> Option<Delivery> {
        let seq = self.next_expected;
        let slot = self.slots.get_mut(&seq)?;
        match std::mem::replace(&mut slot.state, State::InFlight) {
            State::Complete(message) => Some(Delivery { seq, message }),
            other => {
                slot.state = other;
                None
            }
        }
    }

    /// The handler accepted `seq`: advance and return the cumulative `Ack` to send. The ACK is
    /// never produced before this call, so the sender keeps the message until the handler has it.
    pub fn accept(&mut self, seq: u64) -> Result<Frame, ReceiveError> {
        self.finish(seq)
    }

    /// The handler failed permanently on `seq`: advance exactly as [`Receiver::accept`] does, so
    /// the stream does not stall on a message that can never be handled. The caller reports the
    /// failure through its own channel.
    pub fn fail(&mut self, seq: u64) -> Result<Frame, ReceiveError> {
        self.finish(seq)
    }

    fn finish(&mut self, seq: u64) -> Result<Frame, ReceiveError> {
        if seq != self.next_expected {
            return Err(ReceiveError::NotDelivered);
        }
        match self.slots.get(&seq) {
            Some(Slot {
                state: State::InFlight,
                ..
            }) => {}
            _ => return Err(ReceiveError::NotDelivered),
        }
        if let Some(slot) = self.slots.remove(&seq) {
            self.reassembly_bytes -= slot.bytes;
        }
        self.next_expected += 1;
        self.skip_over_marked();
        self.ack().ok_or(ReceiveError::HelloRequired)
    }

    fn check_stream(&self, stream_id: u64) -> Result<(), ReceiveError> {
        match self.stream_id {
            None => Err(ReceiveError::HelloRequired),
            Some(current) if current != stream_id => Err(ReceiveError::StreamMismatch),
            Some(_) => Ok(()),
        }
    }

    fn check_window(&self, seq: u64) -> Result<(), ReceiveError> {
        if seq - self.next_expected >= self.limits.max_unacked as u64 {
            return Err(ReceiveError::OutOfWindow);
        }
        Ok(())
    }

    /// Consume skip markers at the head.
    fn skip_over_marked(&mut self) {
        while matches!(
            self.slots.get(&self.next_expected),
            Some(Slot {
                state: State::Skipped,
                ..
            })
        ) {
            self.slots.remove(&self.next_expected);
            self.next_expected += 1;
        }
    }

    fn advance_to(&mut self, target: u64) {
        let stale: Vec<u64> = self
            .slots
            .keys()
            .copied()
            .filter(|seq| *seq < target)
            .collect();
        for seq in stale {
            if let Some(slot) = self.slots.remove(&seq) {
                self.reassembly_bytes -= slot.bytes;
            }
        }
        self.next_expected = target;
        self.skip_over_marked();
    }

    fn admit_fragment(
        &mut self,
        seq: u64,
        index: u16,
        count: u16,
        text: String,
    ) -> Result<(), ReceiveError> {
        if count == 0
            || count as usize > self.limits.max_fragments
            || index >= count
            || text.len() > self.limits.fragment_bytes
        {
            return Err(ReceiveError::BadFragment);
        }
        match self.slots.get(&seq) {
            // A fragment for a message already settled (skipped, complete or out for handling).
            Some(Slot {
                state: State::Skipped | State::Complete(_) | State::InFlight,
                ..
            }) => return Ok(()),
            Some(Slot {
                state:
                    State::Partial {
                        count: existing,
                        parts,
                    },
                ..
            }) => {
                if *existing != count {
                    return Err(ReceiveError::BadFragment);
                }
                if parts.contains_key(&index) {
                    return Ok(());
                }
            }
            None => {}
        }
        // The head message may use all the reassembly space; every other message leaves room for
        // one maximum-size head message, so a full buffer can always finish the head.
        let cap = if seq == self.next_expected {
            self.limits.max_reassembly_bytes
        } else {
            self.limits
                .max_reassembly_bytes
                .saturating_sub(self.limits.max_message_bytes)
        };
        if self.reassembly_bytes + text.len() > cap {
            return Err(ReceiveError::BufferFull);
        }
        let held = self.slots.get(&seq).map_or(0, |s| s.bytes);
        if held + text.len() > self.limits.max_message_bytes {
            self.drop_slot(seq);
            return Err(ReceiveError::MessageTooLarge);
        }
        let slot = self.slots.entry(seq).or_insert_with(|| Slot {
            state: State::Partial {
                count,
                parts: BTreeMap::new(),
            },
            bytes: 0,
        });
        slot.bytes += text.len();
        self.reassembly_bytes += text.len();
        let mut complete = None;
        if let State::Partial { count, parts } = &mut slot.state {
            parts.insert(index, text);
            if parts.len() == *count as usize {
                let mut message = String::with_capacity(slot.bytes);
                for part in parts.values() {
                    message.push_str(part);
                }
                complete = Some(message);
            }
        }
        if let Some(message) = complete {
            slot.state = State::Complete(message);
        }
        Ok(())
    }

    fn drop_slot(&mut self, seq: u64) {
        if let Some(slot) = self.slots.remove(&seq) {
            self.reassembly_bytes -= slot.bytes;
        }
    }
}
