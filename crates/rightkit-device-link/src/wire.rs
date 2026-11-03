//! Frame encoding: the magic `RKDL`, a version byte, a type byte, then big-endian fields and a
//! length-prefixed UTF-8 text.

use crate::{Frame, SkipReason, PROTOCOL_VERSION};

const MAGIC: &[u8; 4] = b"RKDL";
const TYPE_HELLO: u8 = 1;
const TYPE_DATA: u8 = 2;
const TYPE_ACK: u8 = 3;
const TYPE_SKIP: u8 = 4;

/// Why bytes did not decode to a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// No device-link magic: the bytes are the caller's plain payload (an old peer).
    NotAFrame,
    /// A device-link frame of another protocol version. Fail closed; do not fall back.
    UnsupportedVersion(u8),
    /// Magic and version matched but the frame is truncated, has an unknown type, carries more
    /// text than the limit or text that is not UTF-8.
    Malformed,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::NotAFrame => f.write_str("not a device-link frame"),
            DecodeError::UnsupportedVersion(v) => {
                write!(f, "device-link protocol version {v} is not supported")
            }
            DecodeError::Malformed => f.write_str("malformed device-link frame"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Encode a frame.
pub fn encode(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(MAGIC);
    out.push(PROTOCOL_VERSION);
    match frame {
        Frame::Hello {
            stream_id,
            base_seq,
        } => {
            out.push(TYPE_HELLO);
            out.extend_from_slice(&stream_id.to_be_bytes());
            out.extend_from_slice(&base_seq.to_be_bytes());
        }
        Frame::Data {
            stream_id,
            seq,
            index,
            count,
            text,
        } => {
            out.reserve(text.len() + 24);
            out.push(TYPE_DATA);
            out.extend_from_slice(&stream_id.to_be_bytes());
            out.extend_from_slice(&seq.to_be_bytes());
            out.extend_from_slice(&index.to_be_bytes());
            out.extend_from_slice(&count.to_be_bytes());
            out.extend_from_slice(&(text.len() as u32).to_be_bytes());
            out.extend_from_slice(text.as_bytes());
        }
        Frame::Ack {
            stream_id,
            next_expected,
        } => {
            out.push(TYPE_ACK);
            out.extend_from_slice(&stream_id.to_be_bytes());
            out.extend_from_slice(&next_expected.to_be_bytes());
        }
        Frame::Skip {
            stream_id,
            seq,
            reason,
        } => {
            out.push(TYPE_SKIP);
            out.extend_from_slice(&stream_id.to_be_bytes());
            out.extend_from_slice(&seq.to_be_bytes());
            out.push(match reason {
                SkipReason::Failed => 0,
                SkipReason::TimedOut => 1,
            });
        }
    }
    out
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.bytes.len() < n {
            return Err(DecodeError::Malformed);
        }
        let (head, tail) = self.bytes.split_at(n);
        self.bytes = tail;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

/// Decode one frame. `max_text_bytes` caps a `Data` fragment's text (use
/// [`crate::Limits::fragment_bytes`]).
pub fn decode(bytes: &[u8], max_text_bytes: usize) -> Result<Frame, DecodeError> {
    let Some(rest) = bytes.strip_prefix(MAGIC.as_slice()) else {
        return Err(DecodeError::NotAFrame);
    };
    let mut r = Reader { bytes: rest };
    let version = r.u8()?;
    if version != PROTOCOL_VERSION {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    let frame = match r.u8()? {
        TYPE_HELLO => Frame::Hello {
            stream_id: r.u64()?,
            base_seq: r.u64()?,
        },
        TYPE_DATA => {
            let stream_id = r.u64()?;
            let seq = r.u64()?;
            let index = r.u16()?;
            let count = r.u16()?;
            let len = r.u32()? as usize;
            if len > max_text_bytes {
                return Err(DecodeError::Malformed);
            }
            let text = std::str::from_utf8(r.take(len)?)
                .map_err(|_| DecodeError::Malformed)?
                .to_string();
            Frame::Data {
                stream_id,
                seq,
                index,
                count,
                text,
            }
        }
        TYPE_ACK => Frame::Ack {
            stream_id: r.u64()?,
            next_expected: r.u64()?,
        },
        TYPE_SKIP => Frame::Skip {
            stream_id: r.u64()?,
            seq: r.u64()?,
            reason: match r.u8()? {
                0 => SkipReason::Failed,
                1 => SkipReason::TimedOut,
                _ => return Err(DecodeError::Malformed),
            },
        },
        _ => return Err(DecodeError::Malformed),
    };
    if !r.bytes.is_empty() {
        return Err(DecodeError::Malformed);
    }
    Ok(frame)
}
