//! Framing, keyed authentication and the sequenced channel for the RK8 worker
//! wire.
//!
//! Every frame is a 4-byte big-endian length followed by the body. Handshake
//! frames are JSON. After the handshake each body is `[seq:8][tag:32][json]`,
//! where `tag` is a BLAKE3 keyed hash over the direction byte, the sequence
//! number and the JSON (whose `kind` field is the message type). The session
//! key comes from the device key, both nonces, the worker id and the session
//! id, and each direction uses its own subkey, so a frame recorded on one
//! connection cannot be replayed on another and a frame cannot be reflected
//! back at its sender. Within a connection a frame must carry exactly the next
//! sequence number, so a replayed, reordered or dropped frame is rejected.
//!
//! No TLS is used. This layer authenticates every byte but does not encrypt
//! any of them. That is acceptable for loopback only; see `net` for the
//! non-loopback policy. Frame and chunk sizes are bounded before allocation.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Serialize};

use crate::transport::TransportError;

/// Direction byte for frames sent by the controller.
pub const CONTROLLER_TO_WORKER: u8 = b'C';
/// Direction byte for frames sent by the worker.
pub const WORKER_TO_CONTROLLER: u8 = b'W';
/// Largest accepted sealed frame body (after the handshake).
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Largest accepted pre-authentication (handshake) frame body.
pub const MAX_HANDSHAKE_BYTES: usize = 4 * 1024;
/// Largest bundle chunk, in raw bytes. Its hex form must fit one frame.
pub const MAX_CHUNK_BYTES: usize = 256 * 1024;
/// Default bundle chunk size in bytes.
pub const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;

const SESSION_CONTEXT: &str = "rightkit-scenario wire v1 session root";
const AUTH_CONTEXT: &str = "rightkit-scenario wire v1 device auth key";
const FRAME_CONTEXT_C2W: &str = "rightkit-scenario wire v1 frame key controller-to-worker";
const FRAME_CONTEXT_W2C: &str = "rightkit-scenario wire v1 frame key worker-to-controller";
const ENROLMENT_CONTEXT: &str = "rightkit-scenario wire v1 enrolment code";
const ENROL_PROOF_CONTEXT: &str = "rightkit-scenario wire v1 enrolment proof key";
const ENROL_GRANT_CONTEXT: &str = "rightkit-scenario wire v1 enrolment grant key";
const ENROL_WRAP_CONTEXT: &str = "rightkit-scenario wire v1 enrolment wrap key";
const HEADER_BYTES: usize = 8 + 32;

/// Writes one length-prefixed frame in a single `write` call.
pub fn write_frame<W: Write>(w: &mut W, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(body);
    w.write_all(&buf)?;
    w.flush()
}

/// Reads one length-prefixed frame of at most `max` bytes. The length is
/// checked before any allocation, and the buffer grows only as bytes arrive,
/// so a peer that announces a large frame and sends nothing costs nothing.
pub fn read_frame_limited<R: Read>(r: &mut R, max: usize) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = Vec::with_capacity(len.min(16 * 1024));
    r.take(len as u64).read_to_end(&mut body)?;
    if body.len() != len {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    Ok(body)
}

/// Reads one sealed-size frame.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    read_frame_limited(r, MAX_FRAME_BYTES)
}

/// Writes a JSON message as one plain (pre-session) frame.
pub fn write_json<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), TransportError> {
    let body = serde_json::to_vec(msg).map_err(|e| TransportError::Protocol(e.to_string()))?;
    if body.len() > MAX_HANDSHAKE_BYTES {
        return Err(TransportError::Protocol(
            "handshake message too large".into(),
        ));
    }
    write_frame(w, &body)?;
    Ok(())
}

/// Reads a JSON message from one plain (pre-session) frame, bounded to
/// [`MAX_HANDSHAKE_BYTES`] because the peer is not yet authenticated.
pub fn read_json<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<T, TransportError> {
    let body = read_frame_limited(r, MAX_HANDSHAKE_BYTES)?;
    serde_json::from_slice(&body).map_err(|e| TransportError::Protocol(e.to_string()))
}

/// Length-prefixes each part so that distinct part lists never encode alike.
pub fn transcript(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for part in parts {
        out.extend_from_slice(&(part.len() as u64).to_be_bytes());
        out.extend_from_slice(part);
    }
    out
}

/// Subkey for device-key MACs, kept apart from the session root.
pub fn auth_key(device_key: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key(AUTH_CONTEXT, device_key)
}

fn keyed(key: &[u8; 32], label: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut all: Vec<&[u8]> = Vec::with_capacity(parts.len() + 1);
    all.push(label.as_bytes());
    all.extend_from_slice(parts);
    *blake3::keyed_hash(key, &transcript(&all)).as_bytes()
}

/// Constant-time comparison of two byte strings.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Proof that the worker holds its device key and claims `claim` as its session.
pub fn identify_mac(
    key: &[u8; 32],
    worker_id: &str,
    claim: Option<&str>,
    applied: u64,
    worker_nonce: &str,
    controller_nonce: &str,
) -> [u8; 32] {
    let applied = applied.to_string();
    let present: [u8; 1] = [u8::from(claim.is_some())];
    let claim = claim.unwrap_or("");
    keyed(
        &auth_key(key),
        "identify",
        &[
            worker_id.as_bytes(),
            &present,
            claim.as_bytes(),
            applied.as_bytes(),
            worker_nonce.as_bytes(),
            controller_nonce.as_bytes(),
        ],
    )
}

/// Proof that the controller holds the device key and accepts `session_id`.
pub fn accept_mac(
    key: &[u8; 32],
    session_id: &str,
    worker_id: &str,
    worker_nonce: &str,
    controller_nonce: &str,
) -> [u8; 32] {
    keyed(
        &auth_key(key),
        "accept",
        &[
            session_id.as_bytes(),
            worker_id.as_bytes(),
            worker_nonce.as_bytes(),
            controller_nonce.as_bytes(),
        ],
    )
}

/// Worker proof of an enrolment code for one challenge.
pub fn enrol_proof(
    code_key: &[u8; 32],
    enrolment_id: &str,
    worker_label: &str,
    challenge: &str,
) -> [u8; 32] {
    keyed(
        &blake3::derive_key(ENROL_PROOF_CONTEXT, code_key),
        "enrol",
        &[
            enrolment_id.as_bytes(),
            worker_label.as_bytes(),
            challenge.as_bytes(),
        ],
    )
}

/// Controller MAC over the wrapped device key it hands to a worker. It covers
/// the enrolment id and the challenge, so a grant recorded from one enrolment
/// cannot be replayed into another.
pub fn grant_mac(
    code_key: &[u8; 32],
    enrolment_id: &str,
    challenge: &str,
    worker_id: &str,
    wrapped_hex: &str,
) -> [u8; 32] {
    keyed(
        &blake3::derive_key(ENROL_GRANT_CONTEXT, code_key),
        "grant",
        &[
            enrolment_id.as_bytes(),
            challenge.as_bytes(),
            worker_id.as_bytes(),
            wrapped_hex.as_bytes(),
        ],
    )
}

/// Session root from the device key, both nonces, the worker id and the
/// session id. Each [`Channel`] direction derives its own subkey from it.
pub fn session_key(
    device_key: &[u8; 32],
    controller_nonce: &str,
    worker_nonce: &str,
    worker_id: &str,
    session_id: &str,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(SESSION_CONTEXT);
    hasher.update(&transcript(&[
        device_key,
        controller_nonce.as_bytes(),
        worker_nonce.as_bytes(),
        worker_id.as_bytes(),
        session_id.as_bytes(),
    ]));
    *hasher.finalize().as_bytes()
}

/// Derives the key that wraps a device key, from the enrolment code bytes.
pub fn code_key(code: &[u8]) -> [u8; 32] {
    blake3::derive_key(ENROLMENT_CONTEXT, code)
}

/// One-time pad for the device key, bound to the enrolment challenge.
pub fn wrap_pad(code_key: &[u8; 32], enrolment_id: &str, challenge: &str) -> [u8; 32] {
    keyed(
        &blake3::derive_key(ENROL_WRAP_CONTEXT, code_key),
        "wrap",
        &[enrolment_id.as_bytes(), challenge.as_bytes()],
    )
}

/// XORs two 32-byte values. Used to wrap and unwrap a device key.
pub fn xor32(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = a[i] ^ b[i];
    }
    out
}

/// Fresh random bytes from the operating system.
pub fn random<const N: usize>() -> Result<[u8; N], TransportError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| TransportError::Io(io::Error::other(e.to_string())))?;
    Ok(bytes)
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

/// Decodes a hex string into exactly 32 bytes.
pub fn from_hex_32(text: &str) -> Result<[u8; 32], TransportError> {
    let bytes = from_hex(text).ok_or_else(|| TransportError::Protocol("bad hex".into()))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| TransportError::Protocol("expected 32 bytes".into()))
}

fn frame_tag(key: &[u8; 32], direction: u8, seq: &[u8; 8], body: &[u8]) -> [u8; 32] {
    let mut input = Vec::with_capacity(9 + body.len());
    input.push(direction);
    input.extend_from_slice(seq);
    input.extend_from_slice(body);
    *blake3::keyed_hash(key, &input).as_bytes()
}

fn direction_key(root: &[u8; 32], direction: u8) -> [u8; 32] {
    let context = if direction == CONTROLLER_TO_WORKER {
        FRAME_CONTEXT_C2W
    } else {
        FRAME_CONTEXT_W2C
    };
    blake3::derive_key(context, root)
}

/// A post-handshake channel. Each direction has its own key and sequence
/// counter. A failed send or receive poisons the channel, so a half-written
/// frame can never be followed by a reused sequence number.
pub struct Channel<S> {
    stream: S,
    send_key: [u8; 32],
    recv_key: [u8; 32],
    send_dir: u8,
    recv_dir: u8,
    send_seq: u64,
    recv_seq: u64,
    poisoned: bool,
}

impl<S: Read + Write> Channel<S> {
    pub fn new(stream: S, root_key: [u8; 32], send_dir: u8, recv_dir: u8) -> Self {
        Self {
            stream,
            send_key: direction_key(&root_key, send_dir),
            recv_key: direction_key(&root_key, recv_dir),
            send_dir,
            recv_dir,
            send_seq: 0,
            recv_seq: 0,
            poisoned: false,
        }
    }

    /// Seals and writes one message.
    pub fn send<T: Serialize>(&mut self, msg: &T) -> Result<(), TransportError> {
        if self.poisoned {
            return Err(TransportError::NotConnected);
        }
        let body = serde_json::to_vec(msg).map_err(|e| TransportError::Protocol(e.to_string()))?;
        if HEADER_BYTES + body.len() > MAX_FRAME_BYTES {
            return Err(TransportError::Protocol(
                "message exceeds the frame limit".into(),
            ));
        }
        let Some(seq) = self.send_seq.checked_add(1) else {
            self.poisoned = true;
            return Err(TransportError::Protocol("send sequence exhausted".into()));
        };
        let seq_bytes = seq.to_be_bytes();
        let tag = frame_tag(&self.send_key, self.send_dir, &seq_bytes, &body);
        let mut frame = Vec::with_capacity(HEADER_BYTES + body.len());
        frame.extend_from_slice(&seq_bytes);
        frame.extend_from_slice(&tag);
        frame.extend_from_slice(&body);
        if let Err(e) = write_frame(&mut self.stream, &frame) {
            self.poisoned = true;
            return Err(e.into());
        }
        self.send_seq = seq;
        Ok(())
    }

    /// Reads and opens one message. The tag is checked before the sequence
    /// number, so a forged sequence number reads as an authentication failure.
    /// Any failure poisons the channel.
    pub fn recv<T: DeserializeOwned>(&mut self) -> Result<T, TransportError> {
        if self.poisoned {
            return Err(TransportError::NotConnected);
        }
        let result = self.recv_inner();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn recv_inner<T: DeserializeOwned>(&mut self) -> Result<T, TransportError> {
        let frame = read_frame(&mut self.stream)?;
        if frame.len() < HEADER_BYTES {
            return Err(TransportError::Protocol("short frame".into()));
        }
        let seq_bytes: [u8; 8] = frame[0..8]
            .try_into()
            .map_err(|_| TransportError::Protocol("short sequence".into()))?;
        let tag = &frame[8..HEADER_BYTES];
        let body = &frame[HEADER_BYTES..];
        let expected = frame_tag(&self.recv_key, self.recv_dir, &seq_bytes, body);
        if !ct_eq(&expected, tag) {
            return Err(TransportError::Auth("frame authentication failed".into()));
        }
        let seq = u64::from_be_bytes(seq_bytes);
        let next = self
            .recv_seq
            .checked_add(1)
            .ok_or_else(|| TransportError::Protocol("receive sequence exhausted".into()))?;
        if seq < next {
            return Err(TransportError::Protocol(format!(
                "replayed frame {seq} (last accepted {})",
                self.recv_seq
            )));
        }
        if seq != next {
            return Err(TransportError::Protocol(format!(
                "frame {seq} out of order (expected {next})"
            )));
        }
        self.recv_seq = seq;
        serde_json::from_slice(body).map_err(|e| TransportError::Protocol(e.to_string()))
    }

    /// Returns the underlying stream, for shutdown or inspection.
    pub fn into_stream(self) -> S {
        self.stream
    }
}
