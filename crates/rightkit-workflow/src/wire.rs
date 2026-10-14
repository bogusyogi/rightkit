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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::io::Cursor;

    /// A stream that reads from `input` and records writes in `output`.
    struct Pipe {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Pipe {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Pipe {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Msg {
        n: u32,
    }

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn sealed_frames_round_trip_in_order() {
        let mut sender = Channel::new(
            Pipe {
                input: Cursor::new(Vec::new()),
                output: Vec::new(),
            },
            KEY,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        sender.send(&Msg { n: 1 }).unwrap();
        sender.send(&Msg { n: 2 }).unwrap();
        let bytes = sender.into_stream().output;

        let mut receiver = Channel::new(
            Pipe {
                input: Cursor::new(bytes),
                output: Vec::new(),
            },
            KEY,
            WORKER_TO_CONTROLLER,
            CONTROLLER_TO_WORKER,
        );
        assert_eq!(receiver.recv::<Msg>().unwrap(), Msg { n: 1 });
        assert_eq!(receiver.recv::<Msg>().unwrap(), Msg { n: 2 });
    }

    #[test]
    fn a_replayed_frame_is_rejected() {
        let mut sender = Channel::new(
            Pipe {
                input: Cursor::new(Vec::new()),
                output: Vec::new(),
            },
            KEY,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        sender.send(&Msg { n: 9 }).unwrap();
        let frame = sender.into_stream().output;

        // The same authentic frame delivered twice on one connection.
        let mut doubled = frame.clone();
        doubled.extend_from_slice(&frame);
        let mut receiver = Channel::new(
            Pipe {
                input: Cursor::new(doubled),
                output: Vec::new(),
            },
            KEY,
            WORKER_TO_CONTROLLER,
            CONTROLLER_TO_WORKER,
        );
        assert_eq!(receiver.recv::<Msg>().unwrap(), Msg { n: 9 });
        match receiver.recv::<Msg>() {
            Err(TransportError::Protocol(reason)) => assert!(reason.contains("replayed")),
            other => panic!("replay was not rejected: {other:?}"),
        }
    }

    #[test]
    fn a_frame_from_another_session_key_fails_authentication() {
        let mut sender = Channel::new(
            Pipe {
                input: Cursor::new(Vec::new()),
                output: Vec::new(),
            },
            [9; 32],
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        sender.send(&Msg { n: 3 }).unwrap();
        let mut receiver = Channel::new(
            Pipe {
                input: Cursor::new(sender.into_stream().output),
                output: Vec::new(),
            },
            KEY,
            WORKER_TO_CONTROLLER,
            CONTROLLER_TO_WORKER,
        );
        assert!(matches!(
            receiver.recv::<Msg>(),
            Err(TransportError::Auth(_))
        ));
    }

    #[test]
    fn transcripts_do_not_collide_across_part_boundaries() {
        assert_ne!(transcript(&[b"ab", b"c"]), transcript(&[b"a", b"bc"]));
    }

    fn sealed(msg: &Msg, key: [u8; 32], send: u8, recv: u8) -> Vec<u8> {
        let mut sender = Channel::new(
            Pipe {
                input: Cursor::new(Vec::new()),
                output: Vec::new(),
            },
            key,
            send,
            recv,
        );
        sender.send(msg).unwrap();
        sender.into_stream().output
    }

    fn receiver(bytes: Vec<u8>, send: u8, recv: u8) -> Channel<Pipe> {
        Channel::new(
            Pipe {
                input: Cursor::new(bytes),
                output: Vec::new(),
            },
            KEY,
            send,
            recv,
        )
    }

    #[test]
    fn a_frame_reflected_back_to_its_sender_is_rejected() {
        // The controller's own frame, fed back to the controller as if the
        // worker had sent it.
        let frame = sealed(
            &Msg { n: 1 },
            KEY,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        let mut reflected = receiver(frame, CONTROLLER_TO_WORKER, WORKER_TO_CONTROLLER);
        assert!(matches!(
            reflected.recv::<Msg>(),
            Err(TransportError::Auth(_))
        ));
    }

    #[test]
    fn directions_use_distinct_subkeys() {
        assert_ne!(
            direction_key(&KEY, CONTROLLER_TO_WORKER),
            direction_key(&KEY, WORKER_TO_CONTROLLER)
        );
        // A tag computed with the other direction's byte alone also fails.
        let mut frame = sealed(
            &Msg { n: 1 },
            KEY,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        // Flip a body byte: the MAC must catch tampering with the message.
        let last = frame.len() - 1;
        frame[last] ^= 1;
        let mut r = receiver(frame, WORKER_TO_CONTROLLER, CONTROLLER_TO_WORKER);
        assert!(matches!(r.recv::<Msg>(), Err(TransportError::Auth(_))));
    }

    #[test]
    fn a_tampered_sequence_number_fails_authentication() {
        let mut frame = sealed(
            &Msg { n: 1 },
            KEY,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        // Bytes 0..4 are the length prefix; the sequence follows it.
        frame[4 + 7] = 5;
        let mut r = receiver(frame, WORKER_TO_CONTROLLER, CONTROLLER_TO_WORKER);
        assert!(matches!(r.recv::<Msg>(), Err(TransportError::Auth(_))));
    }

    #[test]
    fn a_skipped_frame_is_rejected_as_out_of_order() {
        let mut sender = Channel::new(
            Pipe {
                input: Cursor::new(Vec::new()),
                output: Vec::new(),
            },
            KEY,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        sender.send(&Msg { n: 1 }).unwrap();
        let first_len = sender.stream.output.len();
        sender.send(&Msg { n: 2 }).unwrap();
        let bytes = sender.into_stream().output;
        // Deliver only the second frame: the first was dropped in transit.
        let mut r = receiver(
            bytes[first_len..].to_vec(),
            WORKER_TO_CONTROLLER,
            CONTROLLER_TO_WORKER,
        );
        match r.recv::<Msg>() {
            Err(TransportError::Protocol(reason)) => assert!(reason.contains("out of order")),
            other => panic!("a gap was not rejected: {other:?}"),
        }
    }

    #[test]
    fn sequence_exhaustion_fails_closed_instead_of_wrapping() {
        let mut sender = Channel::new(
            Pipe {
                input: Cursor::new(Vec::new()),
                output: Vec::new(),
            },
            KEY,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        sender.send_seq = u64::MAX;
        assert!(matches!(
            sender.send(&Msg { n: 1 }),
            Err(TransportError::Protocol(_))
        ));
        assert!(sender.stream.output.is_empty(), "nothing may be written");
        assert!(matches!(
            sender.send(&Msg { n: 2 }),
            Err(TransportError::NotConnected)
        ));

        let frame = sealed(
            &Msg { n: 1 },
            KEY,
            CONTROLLER_TO_WORKER,
            WORKER_TO_CONTROLLER,
        );
        let mut r = receiver(frame, WORKER_TO_CONTROLLER, CONTROLLER_TO_WORKER);
        r.recv_seq = u64::MAX;
        assert!(matches!(r.recv::<Msg>(), Err(TransportError::Protocol(_))));
    }

    #[test]
    fn an_oversized_length_prefix_is_refused_without_allocating() {
        // Announces 3 GiB and supplies nothing.
        let mut announce = Cursor::new(0xC000_0000u32.to_be_bytes().to_vec());
        assert!(read_frame(&mut announce).is_err());
        // A handshake frame above its tight bound is refused even if sealed-size.
        let mut body = ((MAX_HANDSHAKE_BYTES + 1) as u32).to_be_bytes().to_vec();
        body.extend(vec![b' '; MAX_HANDSHAKE_BYTES + 1]);
        let r: Result<serde_json::Value, _> = read_json(&mut Cursor::new(body));
        assert!(r.is_err());
        // A frame that announces more than it delivers is a short read.
        let mut short = 100u32.to_be_bytes().to_vec();
        short.extend([0u8; 10]);
        assert!(read_frame(&mut Cursor::new(short)).is_err());
    }

    #[test]
    fn a_failed_write_poisons_the_channel() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("down"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut c = Channel::new(Broken, KEY, CONTROLLER_TO_WORKER, WORKER_TO_CONTROLLER);
        assert!(matches!(c.send(&Msg { n: 1 }), Err(TransportError::Io(_))));
        assert!(matches!(
            c.send(&Msg { n: 2 }),
            Err(TransportError::NotConnected)
        ));
    }

    #[test]
    fn grant_mac_binds_enrolment_id_and_challenge() {
        let ck = code_key(&[3u8; 32]);
        let a = grant_mac(&ck, "e-1", "c1", "w-1", "00");
        assert_ne!(a, grant_mac(&ck, "e-1", "c2", "w-1", "00"));
        assert_ne!(a, grant_mac(&ck, "e-2", "c1", "w-1", "00"));
        assert_ne!(wrap_pad(&ck, "e-1", "c1"), wrap_pad(&ck, "e-1", "c2"));
    }

    #[test]
    fn handshake_macs_and_session_keys_are_domain_separated() {
        let dk = [5u8; 32];
        assert_ne!(auth_key(&dk), dk);
        let a = session_key(&dk, "cn", "wn", "w-1", "s1");
        assert_ne!(a, session_key(&dk, "cn2", "wn", "w-1", "s1"));
        assert_ne!(a, session_key(&dk, "cn", "wn2", "w-1", "s1"));
        assert_ne!(a, session_key(&dk, "cn", "wn", "w-2", "s1"));
        assert_ne!(a, session_key(&dk, "cn", "wn", "w-1", "s2"));
        // identify and accept never produce the same tag for similar inputs.
        assert_ne!(
            identify_mac(&dk, "w-1", Some("s1"), 0, "wn", "cn"),
            accept_mac(&dk, "s1", "w-1", "wn", "cn")
        );
    }

    #[test]
    fn constant_time_compare_checks_length_and_content() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }
}
