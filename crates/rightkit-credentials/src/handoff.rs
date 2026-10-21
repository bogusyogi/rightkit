//! Owner-binary secret hand-off: versioned, length-prefixed messages that carry named secrets
//! from the binary that owns the OS credential store to a binary that must not touch it.
//!
//! Only the owner binary reads the OS store (keychain sharing across binaries is settled as
//! never, rightkit#884). This module is transport-agnostic and compiles on every OS: it encodes
//! and decodes messages over any [`Read`] / [`Write`], and provides the owner-side
//! [`HandoffPolicy`] that answers a request from the owner's own [`Store`].
//!
//! # Frame layout (version 1)
//!
//! ```text
//! magic        4 bytes  b"RKHO"
//! version      1 byte   1
//! kind         1 byte   Request=1, Reply=2, Error=3, Ack=4
//! correlation  8 bytes  big-endian u64
//! length       4 bytes  big-endian u32, byte length of the body
//! body (by kind)
//!   Request: mode u8 (0 strict, 1 snapshot), count u16 BE, then per name: name_len u8, name
//!   Reply:   mode u8, count u16 BE, then per entry: name_len u8, name, status u8, and
//!              status 0 Present: hex_len u32 BE + hex text of the value
//!              status 1 Absent:  nothing
//!              status 2 ReadFailed: code u8 (an ErrorCode), never a value
//!            A strict reply (mode 0) may carry Present entries only.
//!   Error:   code u8 (NotAllowed=1, NotFound=2, StoreError=3, Invalid=4),
//!            name_len u8 (0 = none), name. Never a value.
//!   Ack:     empty
//! ```
//!
//! Names are `[a-z0-9._-]{1,64}`, unique within a message. Every body must be consumed exactly:
//! bytes left inside the declared length are refused. One read consumes exactly one frame, so
//! concatenated frames are read one call at a time and the bytes after a frame are untouched.
//!
//! # Exchange rules
//!
//! * A reply must carry the request's correlation id, mode and exactly the requested names
//!   ([`expect_reply`]); anything else, including a reply to a different request, is refused.
//!   These helpers keep no record of consumed requests: the caller chooses a unique id per
//!   request and accepts one reply for it, and the transport or session owns replay protection.
//! * An owner failure travels as an Error message with the request's id ([`HandoffPolicy::respond`]).
//! * The receiver of an unsolicited reply (a sidecar) confirms with an Ack ([`Message::ack`]);
//!   the spawner checks it with [`expect_ack`].
//! * Snapshot mode ([`Mode::Snapshot`]) reports every requested name as Present, Absent or
//!   ReadFailed. The receiver applies it as a full replacement ([`Snapshot::replace_into`]):
//!   its whole set becomes exactly the Present entries and every other name is wiped.
//!
//! # Transport rules
//!
//! * A sidecar the owner spawns receives the frame on its private stdin, never argv or env, and
//!   the spawner must scrub ambient copies of the same secrets ([`scrub_inherited_env`]).
//! * An independent helper or CLI requests over an authenticated local IPC channel. Verifying the
//!   caller is the transport's job and must happen before [`HandoffPolicy::answer`].
//! * Caller-supplied readers and writers must be unbuffered or wipe their buffers: a `BufReader`
//!   or `BufWriter` keeps an unwiped copy of the frame. [`read_stdin_bundle`] reads an owned,
//!   unbuffered duplicate of stdin for this reason, never `std::io::stdin()`'s global buffer.
//! * All reads are synchronous on a caller-owned reader and carry no deadline. The transport
//!   owns deadlines: use one with its own read timeout, or close it from outside, as HeardRight's
//!   supervisor does with its 3 s acknowledgment timeout.
//! * Values and every buffer this module owns that holds them are zeroized on drop. No error
//!   carries value bytes.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use zeroize::Zeroizing;

use crate::error::Error;
use crate::secret::Secret;
use crate::store::Store;

/// First four bytes of every frame.
pub const MAGIC: [u8; 4] = *b"RKHO";
/// The only frame version this module reads and writes.
pub const VERSION: u8 = 1;
/// Bytes before the body: magic, version, kind, correlation id, length.
pub const HEADER_LEN: usize = 18;
/// Longest accepted name, in bytes.
pub const MAX_NAME_LEN: usize = 64;
/// Ceiling no [`Limits`] value can raise: 1 MiB.
pub const HARD_MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Result of a hand-off operation.
pub type Result<T> = std::result::Result<T, HandoffError>;

/// Errors from the codec and the owner policy. No variant carries value bytes; names that appear
/// were validated against `[a-z0-9._-]{1,64}` first, so they are safe to log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandoffError {
    #[error("hand-off frame has the wrong magic")]
    BadMagic,
    #[error("hand-off frame version {found} is not supported")]
    UnsupportedVersion { found: u8 },
    #[error("hand-off message kind {found} is not known")]
    UnknownKind { found: u8 },
    #[error("hand-off frame is larger than the {limit}-byte limit")]
    Oversize { limit: usize },
    #[error("hand-off frame carries more than {limit} names")]
    TooManyNames { limit: usize },
    #[error("hand-off name is not [a-z0-9._-]{{1,64}}")]
    BadName,
    #[error("hand-off store account is empty, too long or contains NUL")]
    BadAccount,
    #[error("hand-off name {name} appears twice")]
    DuplicateName { name: String },
    #[error("hand-off frame is truncated")]
    Truncated,
    #[error("hand-off frame has bytes after its last entry")]
    TrailingBytes,
    #[error("hand-off frame is malformed: {0}")]
    Malformed(&'static str),
    #[error("hand-off i/o error ({0})")]
    Io(String),
    #[error("another stdin hand-off session is already running in this process")]
    SessionBusy,
    #[error("hand-off message is of an unexpected kind")]
    UnexpectedKind,
    #[error("hand-off reply answers request {found}, expected {expected}")]
    IdMismatch { expected: u64, found: u64 },
    #[error("hand-off reply mode does not match the request")]
    ModeMismatch,
    #[error("hand-off reply names are not exactly the requested names")]
    NamesMismatch,
    #[error("hand-off peer refused: {code:?}")]
    Remote {
        code: ErrorCode,
        name: Option<String>,
    },
    #[error("hand-off name {name} is not on the allowlist")]
    NotAllowed { name: String },
    #[error("hand-off name {name} has no stored credential")]
    NotFound { name: String },
    #[error("credential store error: {0}")]
    Store(Error),
}

impl From<Error> for HandoffError {
    fn from(e: Error) -> Self {
        HandoffError::Store(e)
    }
}

/// Typed failure code carried by an Error message or a ReadFailed entry. Never a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NotAllowed = 1,
    NotFound = 2,
    StoreError = 3,
    Invalid = 4,
}

impl ErrorCode {
    fn from_u8(b: u8) -> Result<Self> {
        match b {
            1 => Ok(ErrorCode::NotAllowed),
            2 => Ok(ErrorCode::NotFound),
            3 => Ok(ErrorCode::StoreError),
            4 => Ok(ErrorCode::Invalid),
            _ => Err(HandoffError::Malformed("unknown error code")),
        }
    }
}

/// Request and reply mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Every requested name must be present; one failure fails the whole request.
    Strict = 0,
    /// Every requested name is reported as Present, Absent or ReadFailed.
    Snapshot = 1,
}

impl Mode {
    fn from_u8(b: u8) -> Result<Self> {
        match b {
            0 => Ok(Mode::Strict),
            1 => Ok(Mode::Snapshot),
            _ => Err(HandoffError::Malformed("unknown mode")),
        }
    }
}

/// Message kind byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Request = 1,
    Reply = 2,
    Error = 3,
    Ack = 4,
}

impl MessageKind {
    fn from_u8(b: u8) -> Result<Self> {
        match b {
            1 => Ok(MessageKind::Request),
            2 => Ok(MessageKind::Reply),
            3 => Ok(MessageKind::Error),
            4 => Ok(MessageKind::Ack),
            found => Err(HandoffError::UnknownKind { found }),
        }
    }
}

/// Bounds applied while reading or writing a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest accepted body, in bytes. Never above [`HARD_MAX_FRAME_BYTES`].
    pub max_frame_bytes: usize,
    /// Most names a frame may carry.
    pub max_names: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_frame_bytes: 64 * 1024,
            max_names: 32,
        }
    }
}

impl Limits {
    fn body_cap(&self) -> usize {
        self.max_frame_bytes.min(HARD_MAX_FRAME_BYTES)
    }
}

/// Check one name against `[a-z0-9._-]{1,64}`.
pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(HandoffError::BadName)
    }
}

/// A name-to-[`Secret`] map. `Debug` shows names only. Not `Clone`, `Serialize` or `Display`;
/// every value is a [`Secret`] and is zeroized when the bundle drops.
#[derive(Default)]
pub struct SecretBundle {
    entries: BTreeMap<String, Secret>,
}

impl SecretBundle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one secret. The name is validated; a repeated name is refused.
    pub fn insert(&mut self, name: impl Into<String>, secret: Secret) -> Result<()> {
        let name = name.into();
        validate_name(&name)?;
        if self.entries.contains_key(&name) {
            return Err(HandoffError::DuplicateName { name });
        }
        self.entries.insert(name, secret);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Secret> {
        self.entries.get(name)
    }

    /// Remove and return one secret, for a caller that wants to own it.
    pub fn remove(&mut self, name: &str) -> Option<Secret> {
        self.entries.remove(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// View this bundle as an all-Present snapshot.
    pub fn into_snapshot(self) -> Snapshot {
        Snapshot {
            entries: self
                .entries
                .into_iter()
                .map(|(k, v)| (k, Entry::Present(v)))
                .collect(),
        }
    }
}

impl std::fmt::Debug for SecretBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretBundle")
            .field("names", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// One name's state in a snapshot.
pub enum Entry {
    Present(Secret),
    Absent,
    /// The owner could not read the name; the code says why, never the value.
    ReadFailed(ErrorCode),
}

impl Entry {
    fn label(&self) -> &'static str {
        match self {
            Entry::Present(_) => "present",
            Entry::Absent => "absent",
            Entry::ReadFailed(_) => "read-failed",
        }
    }
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Entry::ReadFailed(c) => write!(f, "ReadFailed({c:?})"),
            other => f.write_str(other.label()),
        }
    }
}

/// The reply to a request: each requested name Present, Absent or ReadFailed. `Debug` shows
/// names and states only. Not `Clone`, `Serialize` or `Display`.
#[derive(Default)]
pub struct Snapshot {
    entries: BTreeMap<String, Entry>,
}

impl Snapshot {
    pub fn new() -> Self {
        Self::default()
    }

    fn put(&mut self, name: impl Into<String>, entry: Entry) -> Result<()> {
        let name = name.into();
        validate_name(&name)?;
        if self.entries.contains_key(&name) {
            return Err(HandoffError::DuplicateName { name });
        }
        self.entries.insert(name, entry);
        Ok(())
    }

    pub fn insert_present(&mut self, name: impl Into<String>, secret: Secret) -> Result<()> {
        self.put(name, Entry::Present(secret))
    }

    pub fn insert_absent(&mut self, name: impl Into<String>) -> Result<()> {
        self.put(name, Entry::Absent)
    }

    pub fn insert_failed(&mut self, name: impl Into<String>, code: ErrorCode) -> Result<()> {
        self.put(name, Entry::ReadFailed(code))
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Strict view: every entry must be Present.
    pub fn into_bundle(self) -> Result<SecretBundle> {
        let mut bundle = SecretBundle::new();
        for (name, entry) in self.entries {
            match entry {
                Entry::Present(s) => bundle.insert(name, s)?,
                _ => return Err(HandoffError::Malformed("strict view needs Present entries")),
            }
        }
        Ok(bundle)
    }

    /// Full replacement. `current` becomes exactly this snapshot's Present entries; the old set
    /// is dropped (zeroized) and every name that is Absent, ReadFailed or not mentioned is gone.
    /// An empty snapshot clears the set. Returns the names that were ReadFailed, with their
    /// codes, so the caller can report them; their old values are wiped too (fail closed).
    pub fn replace_into(self, current: &mut SecretBundle) -> Vec<(String, ErrorCode)> {
        let mut fresh = SecretBundle::new();
        let mut failed = Vec::new();
        for (name, entry) in self.entries {
            match entry {
                Entry::Present(s) => {
                    fresh.entries.insert(name, s);
                }
                Entry::Absent => {}
                Entry::ReadFailed(code) => failed.push((name, code)),
            }
        }
        *current = fresh;
        failed
    }
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.entries.iter()).finish()
    }
}

/// One hand-off message. Not `Clone`, `Serialize` or `Display`; `Debug` shows no values.
#[derive(Debug)]
pub enum Message {
    Request {
        id: u64,
        mode: Mode,
        names: Vec<String>,
    },
    Reply {
        id: u64,
        mode: Mode,
        snapshot: Snapshot,
    },
    Error {
        id: u64,
        code: ErrorCode,
        name: Option<String>,
    },
    Ack {
        id: u64,
    },
}

impl Message {
    /// Build a request. Names are validated and must be unique.
    pub fn request<S: AsRef<str>>(id: u64, mode: Mode, names: &[S]) -> Result<Message> {
        let mut out: Vec<String> = Vec::with_capacity(names.len());
        let mut seen: HashSet<&str> = HashSet::with_capacity(names.len());
        for n in names {
            let n = n.as_ref();
            validate_name(n)?;
            if !seen.insert(n) {
                return Err(HandoffError::DuplicateName { name: n.to_owned() });
            }
            out.push(n.to_owned());
        }
        Ok(Message::Request {
            id,
            mode,
            names: out,
        })
    }

    /// A strict reply carrying every secret of `bundle`.
    pub fn reply_bundle(id: u64, bundle: SecretBundle) -> Message {
        Message::Reply {
            id,
            mode: Mode::Strict,
            snapshot: bundle.into_snapshot(),
        }
    }

    /// A snapshot reply.
    pub fn reply_snapshot(id: u64, snapshot: Snapshot) -> Message {
        Message::Reply {
            id,
            mode: Mode::Snapshot,
            snapshot,
        }
    }

    /// An Error message. A `name` that is not a valid name is dropped, never sent.
    pub fn error(id: u64, code: ErrorCode, name: Option<&str>) -> Message {
        Message::Error {
            id,
            code,
            name: name.filter(|n| validate_name(n).is_ok()).map(str::to_owned),
        }
    }

    pub fn ack(id: u64) -> Message {
        Message::Ack { id }
    }

    pub fn id(&self) -> u64 {
        match self {
            Message::Request { id, .. }
            | Message::Reply { id, .. }
            | Message::Error { id, .. }
            | Message::Ack { id } => *id,
        }
    }

    pub fn kind(&self) -> MessageKind {
        match self {
            Message::Request { .. } => MessageKind::Request,
            Message::Reply { .. } => MessageKind::Reply,
            Message::Error { .. } => MessageKind::Error,
            Message::Ack { .. } => MessageKind::Ack,
        }
    }

    /// Owner side: take apart a request, `(id, mode, names)`.
    pub fn into_request(self) -> Result<(u64, Mode, Vec<String>)> {
        match self {
            Message::Request { id, mode, names } => Ok((id, mode, names)),
            _ => Err(HandoffError::UnexpectedKind),
        }
    }
}

/// Client side: accept a reply only if it answers request `id` in `mode` with exactly the
/// `requested` names. An Error with the same id becomes [`HandoffError::Remote`]. A wrong id
/// (including a stale reply to an earlier request), mode or name set is refused.
pub fn expect_reply<S: AsRef<str>>(
    msg: Message,
    id: u64,
    mode: Mode,
    requested: &[S],
) -> Result<Snapshot> {
    match msg {
        Message::Reply {
            id: rid,
            mode: rmode,
            snapshot,
        } => {
            if rid != id {
                return Err(HandoffError::IdMismatch {
                    expected: id,
                    found: rid,
                });
            }
            if rmode != mode {
                return Err(HandoffError::ModeMismatch);
            }
            let mut want: Vec<&str> = requested.iter().map(|s| s.as_ref()).collect();
            want.sort_unstable();
            if want.windows(2).any(|w| w[0] == w[1]) {
                return Err(HandoffError::NamesMismatch);
            }
            // Snapshot keys are already sorted and unique.
            if !want.iter().copied().eq(snapshot.names()) {
                return Err(HandoffError::NamesMismatch);
            }
            // A directly constructed strict reply could carry Absent or ReadFailed entries, which
            // the encoder and decoder both refuse: refuse them here too.
            if mode == Mode::Strict
                && snapshot
                    .entries
                    .values()
                    .any(|e| !matches!(e, Entry::Present(_)))
            {
                return Err(HandoffError::Malformed(
                    "strict reply needs Present entries",
                ));
            }
            Ok(snapshot)
        }
        Message::Error {
            id: rid,
            code,
            name,
        } => {
            if rid != id {
                return Err(HandoffError::IdMismatch {
                    expected: id,
                    found: rid,
                });
            }
            Err(HandoffError::Remote { code, name })
        }
        _ => Err(HandoffError::UnexpectedKind),
    }
}

/// Accept an Ack for `id`. An Error with the same id becomes [`HandoffError::Remote`].
pub fn expect_ack(msg: Message, id: u64) -> Result<()> {
    match msg {
        Message::Ack { id: rid } if rid == id => Ok(()),
        Message::Ack { id: rid } => Err(HandoffError::IdMismatch {
            expected: id,
            found: rid,
        }),
        Message::Error {
            id: rid,
            code,
            name,
        } => {
            if rid != id {
                return Err(HandoffError::IdMismatch {
                    expected: id,
                    found: rid,
                });
            }
            Err(HandoffError::Remote { code, name })
        }
        _ => Err(HandoffError::UnexpectedKind),
    }
}

fn io_err(e: std::io::Error) -> HandoffError {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        HandoffError::Truncated
    } else {
        HandoffError::Io(format!("{:?}", e.kind()))
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn body_size(msg: &Message) -> usize {
    match msg {
        Message::Request { names, .. } => 3 + names.iter().map(|n| 1 + n.len()).sum::<usize>(),
        Message::Reply { snapshot, .. } => {
            let mut n: usize = 3;
            for (name, entry) in &snapshot.entries {
                n = n.saturating_add(1 + name.len() + 1);
                n = n.saturating_add(match entry {
                    Entry::Present(s) => 4usize.saturating_add(s.len().saturating_mul(2)),
                    Entry::Absent => 0,
                    Entry::ReadFailed(_) => 1,
                });
            }
            n
        }
        Message::Error { name, .. } => 2 + name.as_ref().map_or(0, String::len),
        Message::Ack { .. } => 0,
    }
}

fn write_name(out: &mut Vec<u8>, name: &str) -> Result<()> {
    let len = u8::try_from(name.len()).map_err(|_| HandoffError::BadName)?;
    out.push(len);
    out.extend_from_slice(name.as_bytes());
    Ok(())
}

fn wire_count(count: usize, limits: &Limits) -> Result<u16> {
    if count > limits.max_names {
        return Err(HandoffError::TooManyNames {
            limit: limits.max_names,
        });
    }
    u16::try_from(count).map_err(|_| HandoffError::TooManyNames {
        limit: usize::from(u16::MAX),
    })
}

/// Check every invariant the decoder enforces, so the encoder never writes a frame the receiver
/// would refuse: valid unique names, count within the limits and `u16`, Present-only strict
/// replies, valid error names.
fn validate_message(msg: &Message, limits: &Limits) -> Result<()> {
    match msg {
        Message::Request { names, .. } => {
            wire_count(names.len(), limits)?;
            let mut seen: HashSet<&str> = HashSet::with_capacity(names.len());
            for n in names {
                validate_name(n)?;
                if !seen.insert(n.as_str()) {
                    return Err(HandoffError::DuplicateName { name: n.clone() });
                }
            }
        }
        Message::Reply { mode, snapshot, .. } => {
            wire_count(snapshot.len(), limits)?;
            // Snapshot names are validated and unique by construction (private map).
            if *mode == Mode::Strict
                && snapshot
                    .entries
                    .values()
                    .any(|e| !matches!(e, Entry::Present(_)))
            {
                return Err(HandoffError::Malformed(
                    "strict reply needs Present entries",
                ));
            }
        }
        Message::Error { name, .. } => {
            if let Some(n) = name {
                validate_name(n)?;
            }
        }
        Message::Ack { .. } => {}
    }
    Ok(())
}

/// Validate, then encode into one frame held in a zeroizing buffer sized exactly once, so no
/// growth leaves a stale copy of a value behind.
fn encode(msg: &Message, limits: &Limits) -> Result<Zeroizing<Vec<u8>>> {
    validate_message(msg, limits)?;
    let count = match msg {
        Message::Request { names, .. } => wire_count(names.len(), limits)?,
        Message::Reply { snapshot, .. } => wire_count(snapshot.len(), limits)?,
        _ => 0,
    };
    let size = body_size(msg);
    let cap = limits.body_cap();
    if size > cap {
        return Err(HandoffError::Oversize { limit: cap });
    }
    let size_u32 = u32::try_from(size).map_err(|_| HandoffError::Oversize { limit: cap })?;
    let mut out: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(HEADER_LEN + size));
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(msg.kind() as u8);
    out.extend_from_slice(&msg.id().to_be_bytes());
    out.extend_from_slice(&size_u32.to_be_bytes());
    match msg {
        Message::Request { mode, names, .. } => {
            out.push(*mode as u8);
            out.extend_from_slice(&count.to_be_bytes());
            for n in names {
                write_name(&mut out, n)?;
            }
        }
        Message::Reply { mode, snapshot, .. } => {
            out.push(*mode as u8);
            out.extend_from_slice(&count.to_be_bytes());
            for (name, entry) in &snapshot.entries {
                write_name(&mut out, name)?;
                match entry {
                    Entry::Present(s) => {
                        let hex_len = s
                            .len()
                            .checked_mul(2)
                            .and_then(|n| u32::try_from(n).ok())
                            .ok_or(HandoffError::Oversize { limit: cap })?;
                        out.push(0);
                        out.extend_from_slice(&hex_len.to_be_bytes());
                        for &b in s.expose_bytes() {
                            out.push(HEX[usize::from(b >> 4)]);
                            out.push(HEX[usize::from(b & 0x0f)]);
                        }
                    }
                    Entry::Absent => out.push(1),
                    Entry::ReadFailed(code) => {
                        out.push(2);
                        out.push(*code as u8);
                    }
                }
            }
        }
        Message::Error { code, name, .. } => {
            out.push(*code as u8);
            match name {
                Some(n) => write_name(&mut out, n)?,
                None => out.push(0),
            }
        }
        Message::Ack { .. } => {}
    }
    debug_assert_eq!(out.len(), HEADER_LEN + size);
    Ok(out)
}

struct Cursor<'a> {
    body: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(HandoffError::Truncated)?;
        let slice = self
            .body
            .get(self.pos..end)
            .ok_or(HandoffError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn name(&mut self) -> Result<&'a str> {
        let len = self.u8()? as usize;
        if len == 0 || len > MAX_NAME_LEN {
            return Err(HandoffError::BadName);
        }
        let bytes = self.take(len)?;
        let name = std::str::from_utf8(bytes).map_err(|_| HandoffError::BadName)?;
        validate_name(name)?;
        Ok(name)
    }
    fn finish(&self) -> Result<()> {
        if self.pos == self.body.len() {
            Ok(())
        } else {
            Err(HandoffError::TrailingBytes)
        }
    }
}

fn decode_body(kind: MessageKind, id: u64, body: &[u8], limits: &Limits) -> Result<Message> {
    let mut c = Cursor { body, pos: 0 };
    let msg = match kind {
        MessageKind::Request => {
            let mode = Mode::from_u8(c.u8()?)?;
            let count = c.u16()? as usize;
            if count > limits.max_names {
                return Err(HandoffError::TooManyNames {
                    limit: limits.max_names,
                });
            }
            let mut names: Vec<String> = Vec::with_capacity(count);
            let mut seen: HashSet<&str> = HashSet::with_capacity(count);
            for _ in 0..count {
                let n = c.name()?;
                if !seen.insert(n) {
                    return Err(HandoffError::DuplicateName { name: n.to_owned() });
                }
                names.push(n.to_owned());
            }
            Message::Request { id, mode, names }
        }
        MessageKind::Reply => {
            let mode = Mode::from_u8(c.u8()?)?;
            let count = c.u16()? as usize;
            if count > limits.max_names {
                return Err(HandoffError::TooManyNames {
                    limit: limits.max_names,
                });
            }
            let mut snapshot = Snapshot::new();
            for _ in 0..count {
                let name = c.name()?;
                let status = c.u8()?;
                if mode == Mode::Strict && status != 0 {
                    return Err(HandoffError::Malformed(
                        "strict reply needs Present entries",
                    ));
                }
                match status {
                    0 => {
                        let hex_len = c.u32()? as usize;
                        let hex = c.take(hex_len)?;
                        if !hex_len.is_multiple_of(2) {
                            return Err(HandoffError::Malformed("odd-length value"));
                        }
                        let mut value: Zeroizing<Vec<u8>> =
                            Zeroizing::new(Vec::with_capacity(hex_len / 2));
                        for pair in hex.as_chunks::<2>().0 {
                            match (nibble(pair[0]), nibble(pair[1])) {
                                (Some(hi), Some(lo)) => value.push((hi << 4) | lo),
                                _ => return Err(HandoffError::Malformed("value is not hex")),
                            }
                        }
                        let secret = Secret::from_bytes(std::mem::take(&mut *value));
                        snapshot.insert_present(name, secret)?;
                    }
                    1 => snapshot.insert_absent(name)?,
                    2 => {
                        let code = ErrorCode::from_u8(c.u8()?)?;
                        snapshot.insert_failed(name, code)?;
                    }
                    _ => return Err(HandoffError::Malformed("unknown entry status")),
                }
            }
            Message::Reply { id, mode, snapshot }
        }
        MessageKind::Error => {
            let code = ErrorCode::from_u8(c.u8()?)?;
            let name_len = c.u8()? as usize;
            let name = if name_len == 0 {
                None
            } else {
                if name_len > MAX_NAME_LEN {
                    return Err(HandoffError::BadName);
                }
                let bytes = c.take(name_len)?;
                let n = std::str::from_utf8(bytes).map_err(|_| HandoffError::BadName)?;
                validate_name(n)?;
                Some(n.to_owned())
            };
            Message::Error { id, code, name }
        }
        MessageKind::Ack => Message::Ack { id },
    };
    c.finish()?;
    Ok(msg)
}

/// Write one message, using [`Limits::default`]. The writer must be unbuffered or wipe its
/// buffer; this function flushes it.
pub fn write_message<W: Write>(w: &mut W, msg: &Message) -> Result<()> {
    write_message_with_limits(w, msg, &Limits::default())
}

/// Write one message under explicit limits.
pub fn write_message_with_limits<W: Write>(
    w: &mut W,
    msg: &Message,
    limits: &Limits,
) -> Result<()> {
    let frame = encode(msg, limits)?;
    w.write_all(&frame).map_err(io_err)?;
    w.flush().map_err(io_err)
}

/// Read exactly one message and nothing after it, synchronously, from a caller-owned reader.
///
/// This call has no deadline. The transport owns deadlines: use a transport with its own read
/// timeout, or close it from another thread (as HeardRight's supervisor does with its 3 s
/// acknowledgment timeout); the pending read then fails with an i/o error. The reader must be
/// unbuffered or wipe its buffers. Every scratch buffer here is zeroized when dropped, on every
/// path, including each error return.
pub fn read_message<R: Read>(r: &mut R, limits: &Limits) -> Result<Message> {
    let mut header = Zeroizing::new([0u8; HEADER_LEN]);
    r.read_exact(&mut header[..]).map_err(io_err)?;
    if header[..4] != MAGIC {
        return Err(HandoffError::BadMagic);
    }
    if header[4] != VERSION {
        return Err(HandoffError::UnsupportedVersion { found: header[4] });
    }
    let kind = MessageKind::from_u8(header[5])?;
    let mut idb = [0u8; 8];
    idb.copy_from_slice(&header[6..14]);
    let id = u64::from_be_bytes(idb);
    let len = u32::from_be_bytes([header[14], header[15], header[16], header[17]]) as usize;
    let cap = limits.body_cap();
    if len > cap {
        return Err(HandoffError::Oversize { limit: cap });
    }
    let mut body = Zeroizing::new(vec![0u8; len]);
    r.read_exact(&mut body[..]).map_err(io_err)?;
    decode_body(kind, id, &body, limits)
}

/// Like [`read_message`], then also requires the stream to end: any byte after the frame is
/// [`HandoffError::TrailingBytes`]. Blocks until EOF, so use it only on a stream the writer closes.
pub fn read_single_message<R: Read>(r: &mut R, limits: &Limits) -> Result<Message> {
    let msg = read_message(r, limits)?;
    let mut probe = Zeroizing::new([0u8; 1]);
    loop {
        match r.read(&mut probe[..]) {
            Ok(0) => return Ok(msg),
            Ok(_) => return Err(HandoffError::TrailingBytes),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_err(e)),
        }
    }
}

/// A strict delivery: the correlation id the owner chose, and the secrets. Acknowledge it with
/// [`Delivery::ack`] (or `Message::ack(id)` after moving `bundle` out) once the secrets are
/// installed. `Debug` shows names only.
#[derive(Debug)]
pub struct Delivery {
    pub id: u64,
    pub bundle: SecretBundle,
}

impl Delivery {
    /// The Ack that confirms this delivery to the owner ([`expect_ack`] checks it).
    pub fn ack(&self) -> Message {
        Message::ack(self.id)
    }
}

/// A snapshot delivery: the correlation id and the snapshot to apply with
/// [`Snapshot::replace_into`], which consumes the snapshot: save the id first, apply, then send
/// `Message::ack(id)`:
///
/// ```ignore
/// let SnapshotDelivery { id, snapshot } = read_stdin_snapshot()?;
/// let failed = snapshot.replace_into(&mut current);
/// write_message(&mut out, &Message::ack(id))?;
/// ```
///
/// `Debug` shows names and states only.
#[derive(Debug)]
pub struct SnapshotDelivery {
    pub id: u64,
    pub snapshot: Snapshot,
}

impl SnapshotDelivery {
    pub fn ack(&self) -> Message {
        Message::ack(self.id)
    }
}

/// Receive one unsolicited Reply of any mode, keeping its correlation id.
pub fn receive_snapshot_delivery<R: Read>(r: &mut R, limits: &Limits) -> Result<SnapshotDelivery> {
    match read_message(r, limits)? {
        Message::Reply { id, snapshot, .. } => Ok(SnapshotDelivery { id, snapshot }),
        _ => Err(HandoffError::UnexpectedKind),
    }
}

/// Receive one unsolicited strict Reply (every entry Present), keeping its correlation id.
pub fn receive_delivery<R: Read>(r: &mut R, limits: &Limits) -> Result<Delivery> {
    match read_message(r, limits)? {
        Message::Reply {
            id,
            mode: Mode::Strict,
            snapshot,
        } => Ok(Delivery {
            id,
            bundle: snapshot.into_bundle()?,
        }),
        Message::Reply { .. } => Err(HandoffError::ModeMismatch),
        _ => Err(HandoffError::UnexpectedKind),
    }
}

/// Process-wide one-shot stdin session flag.
static STDIN_SESSION: AtomicBool = AtomicBool::new(false);

/// Holds the stdin session; releases it on drop.
struct StdinSession;

impl StdinSession {
    fn begin() -> Result<StdinSession> {
        if STDIN_SESSION.swap(true, Ordering::SeqCst) {
            Err(HandoffError::SessionBusy)
        } else {
            Ok(StdinSession)
        }
    }
}

impl Drop for StdinSession {
    fn drop(&mut self) {
        STDIN_SESSION.store(false, Ordering::SeqCst);
    }
}

/// An owned, unbuffered duplicate of this process's stdin. Fails cleanly (a typed error) when
/// stdin is closed or null.
fn open_stdin() -> Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        let owned = std::io::stdin()
            .as_fd()
            .try_clone_to_owned()
            .map_err(io_err)?;
        Ok(File::from(owned))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        let owned = std::io::stdin()
            .as_handle()
            .try_clone_to_owned()
            .map_err(io_err)?;
        Ok(File::from(owned))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(HandoffError::Io(
            "stdin is not supported on this target".to_owned(),
        ))
    }
}

fn with_stdin<T>(f: impl FnOnce(&mut File) -> Result<T>) -> Result<T> {
    let _session = StdinSession::begin()?;
    let mut file = open_stdin()?;
    f(&mut file)
}

/// Child side: read exactly one strict Reply from this process's stdin with default limits, as a
/// [`Delivery`] (id plus bundle). Only one stdin session may run at a time in a process; a second
/// concurrent one gets [`HandoffError::SessionBusy`]. The stdin used is an owned, unbuffered
/// duplicate, never `std::io::stdin()`'s process-wide buffer, and it reads only the one frame,
/// so the parent may keep using stdin. The read has no deadline: the parent must close the pipe
/// or the child's supervisor must kill the child (see [`read_message`]).
pub fn read_stdin_bundle() -> Result<Delivery> {
    with_stdin(|f| receive_delivery(f, &Limits::default()))
}

/// Child side: like [`read_stdin_bundle`] but accepts a snapshot Reply of either mode.
pub fn read_stdin_snapshot() -> Result<SnapshotDelivery> {
    with_stdin(|f| receive_snapshot_delivery(f, &Limits::default()))
}

/// Spawner side: remove ambient copies of the secrets from the child's inherited environment.
/// A parent that already holds `GROQ_API_KEY` in its environment would otherwise hand the child
/// that value no matter how the hand-off frame is delivered. Call this on every command that
/// spawns a sidecar, with every name the child may receive.
pub fn scrub_inherited_env<'a>(cmd: &'a mut Command, names: &[&str]) -> &'a mut Command {
    for n in names {
        cmd.env_remove(n);
    }
    cmd
}

/// Owner side: the pairs of logical name and store account a peer may fetch from the owner's own
/// [`Store`]. A logical name is never treated as an account: only the mapped account is read.
///
/// The transport must verify the caller (same app, same user, signed path) before calling
/// [`HandoffPolicy::answer`] or [`HandoffPolicy::respond`]; this type decides only which names
/// may be released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffPolicy {
    /// `(logical name, store account)`, logical names unique.
    allowed: Vec<(String, String)>,
}

impl HandoffPolicy {
    /// Build a policy from `(logical name, account)` pairs. Logical names are validated and must
    /// be unique; accounts pass the crate's own account check (non-empty, at most 250 bytes, no
    /// control character), a rejection being `BadAccount`.
    pub fn new<I, L, A>(pairs: I) -> Result<Self>
    where
        I: IntoIterator<Item = (L, A)>,
        L: Into<String>,
        A: Into<String>,
    {
        let mut allowed: Vec<(String, String)> = Vec::new();
        for (logical, account) in pairs {
            let (logical, account) = (logical.into(), account.into());
            validate_name(&logical)?;
            if crate::namespace::check_account(&account).is_err() {
                return Err(HandoffError::BadAccount);
            }
            if allowed.iter().any(|(l, _)| *l == logical) {
                return Err(HandoffError::DuplicateName { name: logical });
            }
            allowed.push((logical, account));
        }
        Ok(HandoffPolicy { allowed })
    }

    pub fn logical_names(&self) -> impl Iterator<Item = &str> {
        self.allowed.iter().map(|(l, _)| l.as_str())
    }

    /// Validate the whole request (shape, count, duplicates, allowlist) before any read, and
    /// return the account for each name in request order.
    fn resolve<'p, S: AsRef<str>>(
        &'p self,
        request_names: &'p [S],
    ) -> Result<Vec<(&'p str, &'p str)>> {
        let max = Limits::default().max_names;
        if request_names.len() > max {
            return Err(HandoffError::TooManyNames { limit: max });
        }
        let mut out: Vec<(&str, &str)> = Vec::with_capacity(request_names.len());
        for n in request_names {
            let n = n.as_ref();
            validate_name(n)?;
            if out.iter().any(|(l, _)| *l == n) {
                return Err(HandoffError::DuplicateName { name: n.to_owned() });
            }
            match self.allowed.iter().find(|(l, _)| l == n) {
                Some((l, a)) => out.push((l.as_str(), a.as_str())),
                None => return Err(HandoffError::NotAllowed { name: n.to_owned() }),
            }
        }
        Ok(out)
    }

    /// Strict answer. Reads only mapped accounts, from `store` forced non-interactive whatever
    /// mode the caller's store has, so it can never raise an OS prompt. A name with no stored
    /// credential is [`HandoffError::NotFound`], never skipped; a store error fails the request.
    pub fn answer<S: AsRef<str>>(
        &self,
        store: &Store,
        request_names: &[S],
    ) -> Result<SecretBundle> {
        let resolved = self.resolve(request_names)?;
        let store = store.non_interactive();
        let mut bundle = SecretBundle::new();
        for (logical, account) in resolved {
            match store.get(account)? {
                Some(secret) => bundle.insert(logical, secret)?,
                None => {
                    return Err(HandoffError::NotFound {
                        name: logical.to_owned(),
                    })
                }
            }
        }
        Ok(bundle)
    }

    /// Snapshot answer for optional keys: each requested name is Present, Absent (the store
    /// reports no item) or ReadFailed (the store failed; the code, never a value). A name that is
    /// malformed, duplicated or not allowlisted still fails the whole request before any read.
    /// Reads are forced non-interactive, as in [`HandoffPolicy::answer`]. The receiver applies it
    /// with [`Snapshot::replace_into`] (full replacement).
    pub fn answer_snapshot<S: AsRef<str>>(
        &self,
        store: &Store,
        request_names: &[S],
    ) -> Result<Snapshot> {
        let resolved = self.resolve(request_names)?;
        let store = store.non_interactive();
        let mut snapshot = Snapshot::new();
        for (logical, account) in resolved {
            match store.get(account) {
                Ok(Some(secret)) => snapshot.insert_present(logical, secret)?,
                Ok(None) => snapshot.insert_absent(logical)?,
                Err(_) => snapshot.insert_failed(logical, ErrorCode::StoreError)?,
            }
        }
        Ok(snapshot)
    }

    /// Answer a request message: a Reply with the request's id and mode, or an Error with the
    /// request's id and a typed code. Anything that is not a Request gets an Invalid error.
    pub fn respond(&self, store: &Store, request: &Message) -> Message {
        let Message::Request { id, mode, names } = request else {
            return Message::error(request.id(), ErrorCode::Invalid, None);
        };
        let outcome = match mode {
            Mode::Strict => self
                .answer(store, names.as_slice())
                .map(|b| Message::reply_bundle(*id, b)),
            Mode::Snapshot => self
                .answer_snapshot(store, names.as_slice())
                .map(|s| Message::reply_snapshot(*id, s)),
        };
        match outcome {
            Ok(m) => m,
            Err(HandoffError::NotAllowed { name }) => {
                Message::error(*id, ErrorCode::NotAllowed, Some(name.as_str()))
            }
            Err(HandoffError::NotFound { name }) => {
                Message::error(*id, ErrorCode::NotFound, Some(name.as_str()))
            }
            Err(HandoffError::Store(_)) => Message::error(*id, ErrorCode::StoreError, None),
            Err(_) => Message::error(*id, ErrorCode::Invalid, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> SecretBundle {
        let mut b = SecretBundle::new();
        b.insert("provider.openai", Secret::from_string("sk-test-VALUE-1"))
            .unwrap();
        b.insert("token_2", Secret::from_bytes(vec![0u8, 255, 16, 1]))
            .unwrap();
        b
    }

    fn encoded(m: &Message) -> Vec<u8> {
        let mut v = Vec::new();
        write_message(&mut v, m).unwrap();
        v
    }

    #[test]
    fn reply_round_trip() {
        let bytes = encoded(&Message::reply_bundle(7, bundle()));
        let msg = read_single_message(&mut bytes.as_slice(), &Limits::default()).unwrap();
        let (id, snap) = match msg {
            Message::Reply { id, snapshot, .. } => (id, snapshot),
            other => panic!("{other:?}"),
        };
        assert_eq!(id, 7);
        let got = snap.into_bundle().unwrap();
        assert_eq!(
            got.get("provider.openai").unwrap().expose_str().unwrap(),
            "sk-test-VALUE-1"
        );
        assert_eq!(got.get("token_2").unwrap().expose_bytes(), &[0, 255, 16, 1]);
    }

    #[test]
    fn debug_shows_names_only() {
        let s = format!("{:?}", bundle());
        assert!(s.contains("provider.openai") && !s.contains("sk-test-VALUE-1"));
        let m = format!("{:?}", Message::reply_bundle(1, bundle()));
        assert!(m.contains("provider.openai") && !m.contains("sk-test-VALUE-1"));
    }

    #[test]
    fn a_second_concurrent_stdin_session_is_refused() {
        let first = StdinSession::begin().unwrap();
        assert_eq!(StdinSession::begin().err(), Some(HandoffError::SessionBusy));
        drop(first);
        let again = StdinSession::begin().unwrap();
        drop(again);
    }

    #[test]
    fn a_directly_built_strict_reply_with_a_non_present_entry_is_refused() {
        let mut snap = Snapshot::new();
        snap.insert_present("a", Secret::from_string("x")).unwrap();
        snap.insert_absent("b").unwrap();
        let msg = Message::Reply {
            id: 3,
            mode: Mode::Strict,
            snapshot: snap,
        };
        let err = expect_reply(msg, 3, Mode::Strict, &["a", "b"]).err();
        assert_eq!(
            err,
            Some(HandoffError::Malformed(
                "strict reply needs Present entries"
            ))
        );
    }

    #[test]
    fn a_request_with_thousands_of_distinct_names_is_checked_in_linear_time() {
        let names: Vec<String> = (0..20_000).map(|i| format!("n{i}")).collect();
        let limits = Limits {
            max_frame_bytes: HARD_MAX_FRAME_BYTES,
            max_names: 30_000,
        };
        let msg = Message::request(1, Mode::Strict, &names).unwrap();
        let mut bytes = Vec::new();
        write_message_with_limits(&mut bytes, &msg, &limits).unwrap();
        let back = read_single_message(&mut bytes.as_slice(), &limits).unwrap();
        let (_, _, got) = back.into_request().unwrap();
        assert_eq!(got.len(), 20_000);
    }

    #[test]
    fn header_is_eighteen_bytes() {
        let bytes = encoded(&Message::ack(9));
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(&bytes[..4], b"RKHO");
        assert_eq!(bytes[5], 4);
    }
}
