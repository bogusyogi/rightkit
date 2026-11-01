//! Length-prefixed frames on a QUIC stream: `u32 header length | u32 body length | JSON header | body`.
//!
//! Headers are small JSON; bodies are opaque bytes (sealed items, record bodies). Both lengths
//! are checked before anything is allocated, so a peer cannot make the other end reserve memory.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::MeshError;
use crate::util::stream_err;

/// Longest JSON header a frame may carry.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

pub async fn write_frame<W: AsyncWrite + Unpin>(
    out: &mut W,
    header: &impl Serialize,
    body: &[u8],
) -> Result<(), MeshError> {
    let header = serde_json::to_vec(header).map_err(|_| MeshError::Malformed("header encode"))?;
    if header.len() > MAX_HEADER_BYTES || body.len() > u32::MAX as usize {
        return Err(MeshError::Malformed("frame too large"));
    }
    let mut prefix = [0u8; 8];
    prefix[..4].copy_from_slice(&(header.len() as u32).to_be_bytes());
    prefix[4..].copy_from_slice(&(body.len() as u32).to_be_bytes());
    out.write_all(&prefix).await.map_err(stream_err)?;
    out.write_all(&header).await.map_err(stream_err)?;
    out.write_all(body).await.map_err(stream_err)?;
    Ok(())
}

/// Read one frame. A body longer than `max_body` is refused before it is read.
pub async fn read_frame<R: AsyncRead + Unpin, H: DeserializeOwned>(
    input: &mut R,
    max_body: usize,
) -> Result<(H, Vec<u8>), MeshError> {
    let mut prefix = [0u8; 8];
    input.read_exact(&mut prefix).await.map_err(stream_err)?;
    let header_len = u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) as usize;
    let body_len = u32::from_be_bytes([prefix[4], prefix[5], prefix[6], prefix[7]]) as usize;
    if header_len > MAX_HEADER_BYTES {
        return Err(MeshError::Malformed("header too long"));
    }
    if body_len > max_body {
        return Err(MeshError::Malformed("body too long"));
    }
    let mut header = vec![0u8; header_len];
    input.read_exact(&mut header).await.map_err(stream_err)?;
    let mut body = vec![0u8; body_len];
    input.read_exact(&mut body).await.map_err(stream_err)?;
    let header =
        serde_json::from_slice(&header).map_err(|_| MeshError::Malformed("header decode"))?;
    Ok((header, body))
}
