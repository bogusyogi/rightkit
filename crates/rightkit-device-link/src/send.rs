//! The sending side: numbering, fragmenting, replay and skip markers.

use std::collections::VecDeque;

use crate::{Frame, Limits, SkipReason};

/// Why a message was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueError {
    /// Longer than [`Limits::max_message_bytes`] (or needs more than `max_fragments` fragments).
    TooLarge,
    /// [`Limits::max_unacked`] messages are already waiting for an acknowledgement.
    WindowFull,
    /// Queuing this message would exceed [`Limits::max_pending_bytes`].
    PendingBytesFull,
}

impl std::fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EnqueueError::TooLarge => "message is larger than the limit",
            EnqueueError::WindowFull => "too many unacknowledged messages",
            EnqueueError::PendingBytesFull => "too many pending bytes",
        })
    }
}

impl std::error::Error for EnqueueError {}

/// Why an incoming frame was refused by the sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// An acknowledgement for a sequence number this sender never sent.
    AckBeyondSent,
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::AckBeyondSent => {
                f.write_str("acknowledgement is beyond the last sent sequence")
            }
        }
    }
}

impl std::error::Error for SendError {}

enum Body {
    Pending(Vec<String>),
    Skipped(SkipReason),
}

struct Entry {
    seq: u64,
    body: Body,
    bytes: usize,
    enqueued_ms: u64,
}

/// Outgoing queue for one stream. Every message keeps its original sequence number until it is
/// acknowledged, so a reconnect replays it unchanged.
pub struct SendQueue {
    stream_id: u64,
    limits: Limits,
    queue: VecDeque<Entry>,
    next_seq: u64,
    pending_bytes: usize,
    /// Entries before this index in `queue` were sent on the current connection.
    cursor: usize,
    hello_sent: bool,
}

impl SendQueue {
    /// A new stream starting at sequence 0. `stream_id` should be unique per stream and per
    /// sender restart (a random 64-bit value).
    pub fn new(stream_id: u64, limits: Limits) -> SendQueue {
        SendQueue::resume(stream_id, 0, limits)
    }

    /// A stream whose first sequence number is `base_seq` (restored from the caller's storage).
    pub fn resume(stream_id: u64, base_seq: u64, limits: Limits) -> SendQueue {
        SendQueue {
            stream_id,
            limits,
            queue: VecDeque::new(),
            next_seq: base_seq,
            pending_bytes: 0,
            cursor: 0,
            hello_sent: false,
        }
    }

    pub fn stream_id(&self) -> u64 {
        self.stream_id
    }

    /// The lowest sequence number not yet acknowledged.
    pub fn base_seq(&self) -> u64 {
        self.queue.front().map_or(self.next_seq, |e| e.seq)
    }

    /// The sequence number the next message will get.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Messages waiting for an acknowledgement (skip markers included).
    pub fn unacked(&self) -> usize {
        self.queue.len()
    }

    /// Bytes of pending message text.
    pub fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    /// Queue a message and return its sequence number. `now_ms` is any monotonic millisecond
    /// clock the caller also passes to [`SendQueue::expire`].
    pub fn enqueue(&mut self, message: &str, now_ms: u64) -> Result<u64, EnqueueError> {
        if message.len() > self.limits.max_message_bytes {
            return Err(EnqueueError::TooLarge);
        }
        if self.queue.len() >= self.limits.max_unacked {
            return Err(EnqueueError::WindowFull);
        }
        if self.pending_bytes + message.len() > self.limits.max_pending_bytes {
            return Err(EnqueueError::PendingBytesFull);
        }
        let fragments = split_utf8(message, self.limits.fragment_bytes);
        if fragments.len() > self.limits.max_fragments || fragments.len() > u16::MAX as usize {
            return Err(EnqueueError::TooLarge);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending_bytes += message.len();
        self.queue.push_back(Entry {
            seq,
            body: Body::Pending(fragments),
            bytes: message.len(),
            enqueued_ms: now_ms,
        });
        Ok(seq)
    }

    /// Frames not yet sent on the current connection, in order: a `Hello` first on a new
    /// connection, then every queued message's fragments (or skip marker).
    pub fn transmit(&mut self) -> Vec<Frame> {
        let mut frames = Vec::new();
        if !self.hello_sent {
            frames.push(Frame::Hello {
                stream_id: self.stream_id,
                base_seq: self.base_seq(),
            });
            self.hello_sent = true;
        }
        for entry in self.queue.iter().skip(self.cursor) {
            match &entry.body {
                Body::Pending(fragments) => {
                    let count = fragments.len() as u16;
                    for (index, text) in fragments.iter().enumerate() {
                        frames.push(Frame::Data {
                            stream_id: self.stream_id,
                            seq: entry.seq,
                            index: index as u16,
                            count,
                            text: text.clone(),
                        });
                    }
                }
                Body::Skipped(reason) => frames.push(Frame::Skip {
                    stream_id: self.stream_id,
                    seq: entry.seq,
                    reason: *reason,
                }),
            }
        }
        self.cursor = self.queue.len();
        frames
    }

    /// The transport reconnected: the next [`SendQueue::transmit`] starts with a `Hello` carrying
    /// the current base sequence and replays every unacknowledged message under its original
    /// sequence number.
    pub fn reconnect(&mut self) {
        self.cursor = 0;
        self.hello_sent = false;
    }

    /// Resend every unacknowledged message on the current connection (for example when the
    /// receiver reported [`crate::ReceiveError::BufferFull`]) without a new `Hello`.
    pub fn retransmit(&mut self) {
        self.cursor = 0;
    }

    /// Handle a frame from the peer. Only `Ack` matters to a sender; other frames are ignored, as
    /// are acknowledgements for another stream.
    pub fn on_frame(&mut self, frame: &Frame) -> Result<(), SendError> {
        let Frame::Ack {
            stream_id,
            next_expected,
        } = frame
        else {
            return Ok(());
        };
        if *stream_id != self.stream_id {
            return Ok(());
        }
        if *next_expected > self.next_seq {
            return Err(SendError::AckBeyondSent);
        }
        while let Some(front) = self.queue.front() {
            if front.seq >= *next_expected {
                break;
            }
            if let Some(entry) = self.queue.pop_front() {
                if let Body::Pending(_) = entry.body {
                    self.pending_bytes -= entry.bytes;
                }
            }
            self.cursor = self.cursor.saturating_sub(1);
        }
        Ok(())
    }

    /// Give up on `seq` (a permanent failure): its text is freed and a skip marker takes its
    /// place under the same sequence number, so the receiver advances past it. Returns false when
    /// `seq` is not queued or is already a skip marker.
    pub fn skip(&mut self, seq: u64, reason: SkipReason) -> bool {
        let Some(front_seq) = self.queue.front().map(|e| e.seq) else {
            return false;
        };
        if seq < front_seq {
            return false;
        }
        let index = (seq - front_seq) as usize;
        let Some(entry) = self.queue.get_mut(index) else {
            return false;
        };
        if matches!(entry.body, Body::Skipped(_)) {
            return false;
        }
        self.pending_bytes -= entry.bytes;
        entry.body = Body::Skipped(reason);
        // Send the marker (and what follows) again even if the fragments went out already.
        self.cursor = self.cursor.min(index);
        true
    }

    /// Skip every message from the front that has waited `timeout_ms` or more since it was
    /// queued. Returns the skipped sequence numbers.
    pub fn expire(&mut self, now_ms: u64, timeout_ms: u64) -> Vec<u64> {
        let mut expired = Vec::new();
        let mut index = 0;
        while let Some(entry) = self.queue.get(index) {
            if now_ms.saturating_sub(entry.enqueued_ms) < timeout_ms {
                break;
            }
            if matches!(entry.body, Body::Pending(_)) {
                expired.push(entry.seq);
            }
            index += 1;
        }
        for seq in &expired {
            self.skip(*seq, SkipReason::TimedOut);
        }
        expired
    }
}

/// Split text into pieces of at most `max` bytes, cut only at character boundaries. An empty
/// message is one empty fragment, so it still occupies a sequence number.
fn split_utf8(text: &str, max: usize) -> Vec<String> {
    if text.len() <= max {
        return vec![text.to_string()];
    }
    let mut out = Vec::with_capacity(text.len() / max + 1);
    let mut rest = text;
    while !rest.is_empty() {
        if rest.len() <= max {
            out.push(rest.to_string());
            break;
        }
        let mut end = max;
        while end > 0 && !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            // A single character wider than the fragment size: take it whole.
            end = rest.chars().next().map_or(rest.len(), |c| c.len_utf8());
        }
        out.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    out
}
