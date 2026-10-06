//! PLACEHOLDER stand-in for `rightkit-framed-sidecar` (publish=false today). Same function
//! signatures and semantics (bounded newline-delimited JSON; oversize/malformed frames are fully
//! consumed so the stream resynchronises). Delete this file and re-export the crate when published.
use std::io::{self, BufRead, Write};

use serde::{de::DeserializeOwned, Serialize};

#[derive(Debug)]
pub enum FrameError {
    TooLarge { limit: usize },
    Json(serde_json::Error),
    Io(io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge { limit } => write!(f, "json frame exceeded {limit}-byte limit"),
            FrameError::Json(e) => write!(f, "json frame failed to deserialize: {e}"),
            FrameError::Io(e) => write!(f, "json frame i/o error: {e}"),
        }
    }
}
impl std::error::Error for FrameError {}
impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        FrameError::Io(e)
    }
}
impl From<serde_json::Error> for FrameError {
    fn from(e: serde_json::Error) -> Self {
        FrameError::Json(e)
    }
}

/// `Ok(None)` is clean EOF.
pub fn read_json_line<R: BufRead, T: DeserializeOwned>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<T>, FrameError> {
    loop {
        buffer.clear();
        if reader.read_until(b'\n', buffer)? == 0 {
            return Ok(None);
        }
        while matches!(buffer.last(), Some(b'\n' | b'\r')) {
            buffer.pop();
        }
        if buffer.is_empty() {
            continue; // skip blank lines
        }
        if buffer.len() > limit {
            return Err(FrameError::TooLarge { limit });
        }
        return Ok(Some(serde_json::from_slice(buffer)?));
    }
}

pub fn write_json_line<W: Write, T: Serialize>(writer: &mut W, value: &T, limit: usize) -> Result<(), FrameError> {
    let encoded = serde_json::to_vec(value)?;
    if encoded.len() > limit {
        return Err(FrameError::TooLarge { limit });
    }
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
