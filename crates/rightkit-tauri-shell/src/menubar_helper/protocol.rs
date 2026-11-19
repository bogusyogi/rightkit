//! Wire protocol and pure checks of the authenticated action channel. No OS calls: everything
//! here is decided from bytes and configuration, so it is unit-testable on any host.
//!
//! One connection, newline-delimited JSON, every line at most [`MAX_FRAME_BYTES`]:
//!
//! ```text
//! server -> client   {"v":1,"nonce":"<64 lowercase hex>"}          (once, after the OS identified the peer)
//! client -> server   {"nonce":"<echo>","seq":1,"id":"<token>","action":"<name>"}
//! server -> client   {"seq":1,"ok":true}   or   {"seq":1,"ok":false,"reason":"<code>"}
//! ```
//!
//! The nonce is fresh per connection and the sequence number starts at 1 and must advance by exactly
//! one, so a frame cannot be replayed on another connection or twice on this one. Neither is what
//! authenticates the sender: the operating system did that (code signature of the connecting
//! process) before the first byte was read. They only make a captured frame useless.

use std::collections::{HashSet, VecDeque};
use std::io::{self, Read};

/// Protocol version carried in the greeting.
pub const PROTOCOL_VERSION: u32 = 1;
/// Longest accepted frame, newline excluded. A longer line closes the connection.
pub const MAX_FRAME_BYTES: usize = 2048;
/// Longest accepted frame id.
pub const MAX_ID_BYTES: usize = 80;
/// Longest accepted action name.
pub const MAX_ACTION_BYTES: usize = 64;
/// Frame ids remembered per sender class for duplicate rejection.
pub const REPLAY_WINDOW: usize = 256;

/// The one request frame. Unknown fields refuse the frame.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub nonce: String,
    pub seq: u64,
    pub id: String,
    pub action: String,
}

/// Why the server refused a frame. The codes are the only text that ever goes back to a peer or
/// into the event sink for a refused frame: never the frame's contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    BadFrame,
    InvalidId,
    WrongNonce,
    OutOfOrder,
    Replay,
    ActionNotAllowed,
}

impl Refusal {
    pub fn code(self) -> &'static str {
        match self {
            Refusal::BadFrame => "bad_frame",
            Refusal::InvalidId => "invalid_id",
            Refusal::WrongNonce => "wrong_nonce",
            Refusal::OutOfOrder => "out_of_order",
            Refusal::Replay => "replay",
            Refusal::ActionNotAllowed => "action_not_allowed",
        }
    }
}

/// One allowed sender class: the signing identifiers that may connect as it and the action names
/// it may send. Both come from the host.
#[derive(Clone, Debug)]
pub struct Sender {
    pub(super) identifiers: Vec<String>,
    pub(super) actions: Vec<String>,
}

impl Sender {
    /// `identifiers` are code-signing identifiers (`codesign -dvv` `Identifier=`); `actions` are
    /// the action names this class may send. Validated by `MenubarHelper` at build time.
    pub fn new(identifiers: &[&str], actions: &[&str]) -> Self {
        Self {
            identifiers: identifiers.iter().map(|s| (*s).to_string()).collect(),
            actions: actions.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    pub(super) fn allows(&self, action: &str) -> bool {
        self.actions.iter().any(|a| a == action)
    }

    pub(super) fn validate(&self, what: &str) -> Result<(), String> {
        if self.identifiers.is_empty() {
            return Err(format!("{what}: no signing identifier"));
        }
        if let Some(bad) = self
            .identifiers
            .iter()
            .find(|i| !valid_signing_identifier(i))
        {
            return Err(format!(
                "{what}: malformed signing identifier ({} bytes)",
                bad.len()
            ));
        }
        if self.actions.is_empty() {
            return Err(format!("{what}: no action"));
        }
        if let Some(bad) = self.actions.iter().find(|a| !valid_action(a)) {
            return Err(format!(
                "{what}: malformed action name ({} bytes)",
                bad.len()
            ));
        }
        Ok(())
    }
}

/// An Apple team identifier: exactly ten upper-case letters or digits.
pub fn valid_team_id(team: &str) -> bool {
    team.len() == 10
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// A code-signing identifier safe to splice into a requirement string: 1..=155 bytes of ASCII
/// letters, digits, `.`, `-`, `_`. Refused rather than escaped.
pub fn valid_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 155
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// A code-signing identifier safe to splice into a requirement string: 1..=155 bytes of ASCII
/// letters, digits, `.`, `-` (no `_`: `rightkit_daemon::SigningIdentity::MacDeveloperId` refuses it,
/// so a host that names one learns at build time, not as a runtime refusal of every peer). Refused
/// rather than escaped.
pub fn valid_signing_identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 155
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
}

/// An action name: 1..=64 bytes of lower-case letters, digits, `_`.
pub fn valid_action(action: &str) -> bool {
    !action.is_empty()
        && action.len() <= MAX_ACTION_BYTES
        && action
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A socket file name: 1..=48 bytes of ASCII letters, digits, `.`, `-`, `_`, not starting with `.`.
pub fn valid_socket_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 48 && !name.starts_with('.') && valid_identifier(name)
}

/// A frame id: 1..=80 bytes of ASCII letters, digits, `.`, `-`, `_`.
pub fn valid_frame_id(id: &str) -> bool {
    id.len() <= MAX_ID_BYTES && valid_identifier(id)
}

/// Per-connection frame state: the server nonce and the next expected sequence number.
pub struct Exchange {
    nonce: String,
    next_seq: u64,
}

impl Exchange {
    pub fn new(nonce: String) -> Self {
        Self { nonce, next_seq: 1 }
    }

    /// The greeting line to send (no newline).
    pub fn greeting(&self) -> String {
        format!("{{\"v\":{PROTOCOL_VERSION},\"nonce\":\"{}\"}}", self.nonce)
    }

    /// Check `request` against this connection: nonce (constant time), then sequence. On success
    /// the sequence advances.
    pub fn admit(&mut self, request: &Request) -> Result<(), Refusal> {
        if !constant_time_eq(request.nonce.as_bytes(), self.nonce.as_bytes()) {
            return Err(Refusal::WrongNonce);
        }
        if request.seq != self.next_seq {
            return Err(Refusal::OutOfOrder);
        }
        self.next_seq += 1;
        Ok(())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Parse one frame line. Anything that is not exactly a [`Request`] is `BadFrame`; an id outside
/// the allowed shape is `InvalidId`; an action name outside the allowed shape is `BadFrame`.
pub fn parse_request(line: &[u8]) -> Result<Request, Refusal> {
    let request: Request = serde_json::from_slice(line).map_err(|_| Refusal::BadFrame)?;
    if !valid_frame_id(&request.id) {
        return Err(Refusal::InvalidId);
    }
    if !valid_action(&request.action) {
        return Err(Refusal::BadFrame);
    }
    Ok(request)
}

/// The reply line for `seq` (no newline).
pub fn reply_line(seq: u64, outcome: Result<(), Refusal>) -> String {
    match outcome {
        Ok(()) => format!("{{\"seq\":{seq},\"ok\":true}}"),
        Err(refusal) => format!(
            "{{\"seq\":{seq},\"ok\":false,\"reason\":\"{}\"}}",
            refusal.code()
        ),
    }
}

/// Remembers the last [`REPLAY_WINDOW`] frame ids of one sender class and refuses a repeat.
#[derive(Default)]
pub struct ReplayGuard {
    order: VecDeque<String>,
    seen: HashSet<String>,
}

impl ReplayGuard {
    /// `true` the first time `id` is presented within the window, `false` for a repeat.
    pub fn admit(&mut self, id: &str) -> bool {
        if self.seen.contains(id) {
            return false;
        }
        if self.order.len() == REPLAY_WINDOW {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        self.order.push_back(id.to_string());
        self.seen.insert(id.to_string());
        true
    }
}

/// Why a frame could not be read.
#[derive(Debug)]
pub enum FrameError {
    /// The line grew past [`MAX_FRAME_BYTES`] without a newline.
    TooLong,
    /// The underlying read failed after retrying interrupted reads.
    Io,
}

/// Reads newline-delimited frames from a stream with a hard size cap.
pub struct FrameReader<R: Read> {
    inner: R,
    pending: Vec<u8>,
}

impl<R: Read> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            pending: Vec::new(),
        }
    }

    /// The next frame without its newline, `Ok(None)` at end of stream.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        loop {
            if let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                if end > MAX_FRAME_BYTES {
                    return Err(FrameError::TooLong);
                }
                let mut line: Vec<u8> = self.pending.drain(..=end).collect();
                line.pop();
                return Ok(Some(line));
            }
            if self.pending.len() > MAX_FRAME_BYTES {
                return Err(FrameError::TooLong);
            }
            let mut chunk = [0u8; 512];
            match self.inner.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => self.pending.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(FrameError::Io),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(nonce: &str, seq: u64, id: &str, action: &str) -> Request {
        Request {
            nonce: nonce.into(),
            seq,
            id: id.into(),
            action: action.into(),
        }
    }

    #[test]
    fn signing_identifiers_are_stricter_than_socket_names() {
        assert!(valid_signing_identifier("app.example.next-1"));
        assert!(!valid_signing_identifier("a_b"));
        assert!(!valid_signing_identifier(""));
        assert!(valid_socket_name("app.example.menubar_1"));
    }

    #[test]
    fn exchange_rejects_wrong_nonce_and_out_of_order_and_replayed_seq() {
        let mut ex = Exchange::new("n".repeat(64));
        assert_eq!(
            ex.admit(&request(&"x".repeat(64), 1, "a", "go")),
            Err(Refusal::WrongNonce)
        );
        assert_eq!(
            ex.admit(&request(&"n".repeat(64), 2, "a", "go")),
            Err(Refusal::OutOfOrder)
        );
        assert_eq!(ex.admit(&request(&"n".repeat(64), 1, "a", "go")), Ok(()));
        assert_eq!(
            ex.admit(&request(&"n".repeat(64), 1, "b", "go")),
            Err(Refusal::OutOfOrder)
        );
        assert_eq!(ex.admit(&request(&"n".repeat(64), 2, "b", "go")), Ok(()));
    }

    #[test]
    fn replay_guard_refuses_a_repeat_and_forgets_past_the_window() {
        let mut guard = ReplayGuard::default();
        assert!(guard.admit("one"));
        assert!(!guard.admit("one"));
        for i in 0..REPLAY_WINDOW {
            assert!(guard.admit(&format!("id{i}")));
        }
        assert!(guard.admit("one"));
    }

    #[test]
    fn parse_refuses_unknown_fields_bad_ids_and_bad_actions() {
        let ok = br#"{"nonce":"n","seq":1,"id":"a-1","action":"toggle"}"#;
        assert!(parse_request(ok).is_ok());
        let extra = br#"{"nonce":"n","seq":1,"id":"a","action":"go","x":1}"#;
        assert_eq!(parse_request(extra).err(), Some(Refusal::BadFrame));
        let id = br#"{"nonce":"n","seq":1,"id":"a b","action":"go"}"#;
        assert_eq!(parse_request(id).err(), Some(Refusal::InvalidId));
        let action = br#"{"nonce":"n","seq":1,"id":"a","action":"Go Now"}"#;
        assert_eq!(parse_request(action).err(), Some(Refusal::BadFrame));
        assert_eq!(parse_request(b"not json").err(), Some(Refusal::BadFrame));
    }

    #[test]
    fn frame_reader_splits_lines_and_caps_length() {
        let mut reader = FrameReader::new(&b"one\ntwo\n"[..]);
        assert_eq!(reader.next_frame().unwrap().unwrap(), b"one");
        assert_eq!(reader.next_frame().unwrap().unwrap(), b"two");
        assert!(reader.next_frame().unwrap().is_none());
        let long = vec![b'a'; MAX_FRAME_BYTES + 600];
        let mut reader = FrameReader::new(&long[..]);
        assert!(matches!(reader.next_frame(), Err(FrameError::TooLong)));
    }

    #[test]
    fn reply_lines_carry_only_a_code() {
        assert_eq!(reply_line(3, Ok(())), r#"{"seq":3,"ok":true}"#);
        assert_eq!(
            reply_line(4, Err(Refusal::Replay)),
            r#"{"seq":4,"ok":false,"reason":"replay"}"#
        );
    }
}
